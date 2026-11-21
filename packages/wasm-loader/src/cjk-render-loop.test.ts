/**
 * FONT.10 — vitest coverage of the production render-loop hook.
 *
 * This is the render-side counterpart of `cjk-cache-source.test.ts`: where that
 * test proves a typed disclosure can fulfil a chunk, this one drives the whole
 * notdef → drain → repaint → disclose sequence through the hook a shipped
 * viewer calls. The drain port is the REAL {@link LazyPayloadClient.drainCjk}
 * over a cache adapter with an injected Cache Storage double, so the bytes are
 * handled by the same client the browser uses — only the guest's render is
 * faked (there is no font payload in this checkout).
 */

import { describe, expect, it } from "vitest";

import {
	attachCjkDrain,
	cjkSubstitutionDisclosure,
	createCjkRenderLoop,
	type CjkDomSeam,
	type CjkDrainOutcome,
	type CjkRenderPort,
	type CjkRenderReport,
	type CjkSubstitutionNote,
} from "./cjk-render-loop.js";
import type { CacheStorageLike } from "./cjk-cache-source.js";
import { payloadCacheKey, type LazyTransport } from "./lazy-payload.js";

/** A render result: pixels plus the guest's typed CJK block. */
interface Page {
	readonly pixels: readonly number[];
	readonly cjk?: CjkRenderReport;
}

const CHUNK = "ideographs-1";
const SHA = "d".repeat(64);
const CLAIM = { id: CHUNK, sha256: SHA, rawBytes: 3, url: `cjk/${CHUNK}.ttf` };

function note(): CjkSubstitutionNote {
	return { requestedBaseFont: "STSong-Light", code: 0x4e2d, chunk: CHUNK, subset: true };
}

class FakeResponse {
	readonly status: number;
	readonly #bytes: Uint8Array;
	constructor(bytes: Uint8Array, init: { status?: number } = {}) {
		this.#bytes = bytes;
		this.status = init.status ?? 200;
	}
	async arrayBuffer(): Promise<ArrayBuffer> {
		return this.#bytes.slice().buffer as ArrayBuffer;
	}
}

function cacheDouble(seed = true): CacheStorageLike {
	const stores = new Map<string, Map<string, unknown>>();
	const key = payloadCacheKey("cjk", CLAIM.id, CLAIM.sha256);
	return {
		async open(name) {
			let store = stores.get(name);
			if (!store) {
				store = new Map();
				stores.set(name, store);
				if (seed) await store.set(key, new FakeResponse(new Uint8Array([1, 2, 3])));
			}
			return {
				async match(k: string) {
					return store?.get(k);
				},
				async put(k: string, value: unknown) {
					await store?.set(k, value);
				},
			};
		},
	};
}

/** A guest that opens, then adopts one chunk on delivery. */
function adoptingGuest(): LazyTransport {
	let at = 0;
	const steps: Array<{ op: string; value: Record<string, unknown> }> = [
		{
			op: "cjkOpen",
			value: {
				revision: 0,
				residentBytes: 0,
				needs: [],
				unavailable: [],
				exhausted: [],
				request: null,
				loaded: [],
			},
		},
		{
			op: "cjkChunk",
			value: {
				revision: 7,
				residentBytes: 3,
				needs: [],
				unavailable: [],
				exhausted: [],
				request: null,
				loaded: [CHUNK],
				adopted: true,
			},
		},
	];
	return {
		async call(op) {
			const step = steps[at];
			if (!step || step.op !== op) throw new Error(`unexpected op ${op} at ${at}`);
			at += 1;
			return step.value;
		},
	};
}

function domSpy(): CjkDomSeam & {
	readonly shown: CjkSubstitutionNote[];
	readonly cleared: number[];
	readonly repaints: Array<{ page: number; revision: number }>;
} {
	const shown: CjkSubstitutionNote[] = [];
	const cleared: number[] = [];
	const repaints: Array<{ page: number; revision: number }> = [];
	return {
		shown,
		cleared,
		repaints,
		showDisclosure(_page, n) {
			shown.push(n);
		},
		clearDisclosure(page) {
			cleared.push(page);
		},
		noteRepaint(page, revision) {
			repaints.push({ page, revision });
		},
	};
}

/** A renderer that paints tofu on pass 1 and the real glyph after the drain. */
function renderPort(needs: readonly string[]): CjkRenderPort<Page> & { readonly calls: number } {
	let calls = 0;
	return {
		get calls() {
			return calls;
		},
		async render() {
			calls += 1;
			if (calls === 1) {
				return { pixels: [0, 0, 0, 0], cjk: { needs: [...needs], revision: 0, residentBytes: 0 } };
			}
			return { pixels: [1, 2, 3, 4], cjk: { needs: [], revision: 7, residentBytes: 3 } };
		},
		cjkOf(result) {
			return result.cjk;
		},
	};
}

async function realDrain(): Promise<(needs: readonly string[]) => Promise<CjkDrainOutcome>> {
	const attached = await attachCjkDrain(1, {
		transport: adoptingGuest(),
		cache: cacheDouble(),
		cacheName: "selis-cjk-v1",
		claims: [CLAIM],
		core: new Uint8Array(1),
		ResponseCtor: FakeResponse,
	});
	return attached.drain;
}

describe("FONT.10 — the CJK render-loop hook (production drainCjk caller)", () => {
	it("paints notdef on pass 1, drains the named chunk, repaints and discloses", async () => {
		const render = renderPort([CHUNK]);
		const dom = domSpy();
		const loop = createCjkRenderLoop({
			render,
			drain: await realDrain(),
			dom,
			noteFor: note,
		});

		const result = await loop.renderPage(2);

		// Pass 1 named the chunk before any bytes moved; the guest adopted it and
		// moved its revision, so a second render replaced the tofu.
		expect(result.telemetry.pass1Needs).toEqual([CHUNK]);
		expect(result.telemetry.delivered).toEqual([CHUNK]);
		expect(result.telemetry.repainted).toBe(true);
		expect(result.telemetry.finalRevision).toBe(7);
		expect(render.calls).toBe(2);
		expect(result.page.pixels).toEqual([1, 2, 3, 4]);

		// The typed note is shown, not a silent swap.
		expect(dom.shown).toHaveLength(1);
		expect(dom.shown[0]).toEqual(note());
		expect(dom.repaints).toEqual([{ page: 2, revision: 7 }]);
		expect(dom.cleared).toEqual([]);
		expect(result.telemetry.disclosure).toBe(
			"font STSong-Light substituted with CJK on-demand subset chunk ideographs-1 for U+4E2D",
		);
		expect(cjkSubstitutionDisclosure(note())).toBe(result.telemetry.disclosure);
	});

	it("leaves ordinary pages untouched and clears a stale disclosure", async () => {
		const render = renderPort([]);
		const dom = domSpy();
		const loop = createCjkRenderLoop({
			render,
			drain: async () => {
				throw new Error("no drain for a page that needs nothing");
			},
			dom,
			noteFor: note,
		});

		const result = await loop.renderPage(3);

		expect(render.calls).toBe(1);
		expect(result.telemetry.repainted).toBe(false);
		expect(result.telemetry.disclosure).toBeNull();
		expect(dom.cleared).toEqual([3]);
		expect(dom.shown).toEqual([]);
	});

	it("does not repaint or disclose when the guest refused the chunk", async () => {
		const render = renderPort([CHUNK]);
		const dom = domSpy();
		// A refusal leaves the revision unchanged — the guest charged an attempt
		// but adopted nothing, so there are no real glyphs to paint yet.
		const refused: CjkDrainOutcome = {
			state: {
				revision: 0,
				residentBytes: 0,
				loaded: [],
				needs: [CHUNK],
				unavailable: [],
				exhausted: [],
				request: null,
			},
			delivered: [],
			refused: [CHUNK],
		};
		const loop = createCjkRenderLoop({
			render,
			drain: async () => refused,
			dom,
			noteFor: note,
		});

		const result = await loop.renderPage(4);

		expect(result.telemetry.refused).toEqual([CHUNK]);
		expect(result.telemetry.repainted).toBe(false);
		expect(render.calls).toBe(1);
		expect(dom.shown).toEqual([]);
		expect(dom.repaints).toEqual([]);
	});
});
