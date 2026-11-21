/**
 * FONT.10 — host vitest coverage of the viewer render-loop hook.
 *
 * The hook lives in the shipped viewer's render path (`editor-page.ts` calls
 * `renderWithCjk`, which calls `createViewerCjkRenderLoop`). These tests drive
 * that hook and its local cache-backed drain with injected ports — a typed DOM
 * seam stands in for the page, so nothing here needs a real `document`. The
 * browser harness leg observes the same sequence with real Cache Storage.
 */

import { describe, expect, it } from "vitest";

import type { CjkRenderReport, RenderedPage } from "./engine.js";
import {
	attachViewerCjkDrain,
	cjkCacheKey,
	cjkDisclosure,
	createViewerCjkRenderLoop,
	type CacheLike,
	type CjkClaim,
	type CjkViewSeam,
	type DrainOutcome,
} from "./cjk-render-loop.js";

const CHUNK = "ideographs-1";
const SHA = "d".repeat(64);
const CLAIM: CjkClaim = { id: CHUNK, sha256: SHA, rawBytes: 3, url: `cjk/${CHUNK}.ttf` };

function page(needs: readonly string[], revision: number, pixels: number[]): RenderedPage {
	const cjk: CjkRenderReport = { needs: [...needs], revision, residentBytes: needs.length ? 0 : 3 };
	return {
		width: 1,
		height: 1,
		pixels: new Uint8Array(pixels),
		ctm: { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 },
		cjk,
	};
}

function seam(): CjkViewSeam & {
	readonly shown: string[];
	readonly cleared: number[];
	readonly repaints: Array<{ page: number; revision: number }>;
} {
	const shown: string[] = [];
	const cleared: number[] = [];
	const repaints: Array<{ page: number; revision: number }> = [];
	return {
		shown,
		cleared,
		repaints,
		showDisclosure(_page, text) {
			shown.push(text);
		},
		clearDisclosure(p) {
			cleared.push(p);
		},
		noteRepaint(p, revision) {
			repaints.push({ page: p, revision });
		},
	};
}

function byteCache(seed = true): CacheLike {
	return {
		async open() {
			return {
				async match(key: string) {
					if (!seed || key !== cjkCacheKey(CLAIM.id, CLAIM.sha256)) return undefined;
					return { status: 200, arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer };
				},
				async put() {},
			};
		},
	};
}

describe("FONT.10 — viewer render-loop hook", () => {
	it("renders notdef, drains the named chunk, repaints and discloses", async () => {
		let renders = 0;
		const render = () => {
			renders += 1;
			return renders === 1 ? page([CHUNK], 0, [0]) : page([], 7, [9]);
		};
		const view = seam();
		const drain = async (): Promise<DrainOutcome> => ({
			revision: 7,
			residentBytes: 3,
			delivered: [CHUNK],
			refused: [],
		});
		const loop = createViewerCjkRenderLoop({
			render,
			drain,
			view,
			noteFor: (chunk) => ({ requestedBaseFont: "STSong-Light", code: 0x4e2d, chunk, subset: true }),
		});

		const result = await loop.renderPage(1, 150);

		expect(result.telemetry.pass1Needs).toEqual([CHUNK]);
		expect(result.telemetry.delivered).toEqual([CHUNK]);
		expect(result.telemetry.repainted).toBe(true);
		expect(renders).toBe(2);
		expect([...result.page.pixels]).toEqual([9]);
		expect(view.repaints).toEqual([{ page: 1, revision: 7 }]);
		expect(view.shown).toEqual([
			"font STSong-Light substituted with CJK on-demand subset chunk ideographs-1 for U+4E2D",
		]);
		expect(cjkDisclosure({
			requestedBaseFont: "STSong-Light",
			code: 0x4e2d,
			chunk: CHUNK,
			subset: true,
		})).toBe(view.shown[0]);
	});

	it("skips ordinary pages and clears a stale disclosure", async () => {
		const view = seam();
		let renders = 0;
		const loop = createViewerCjkRenderLoop({
			render: () => {
				renders += 1;
				return page([], 2, [1]);
			},
			drain: async () => {
				throw new Error("no drain for a page that needs nothing");
			},
			view,
			noteFor: (chunk) => ({ requestedBaseFont: "x", code: 0, chunk, subset: true }),
		});

		const result = await loop.renderPage(2, 150);

		expect(renders).toBe(1);
		expect(result.telemetry.repainted).toBe(false);
		expect(view.cleared).toEqual([2]);
		expect(view.shown).toEqual([]);
	});

	it("does not repaint when the guest refused the chunk", async () => {
		const view = seam();
		let renders = 0;
		const loop = createViewerCjkRenderLoop({
			render: () => {
				renders += 1;
				return renders === 1 ? page([CHUNK], 0, [0]) : page([], 7, [9]);
			},
			drain: async () => ({
				revision: 0,
				residentBytes: 0,
				delivered: [],
				refused: [CHUNK],
			}),
			view,
			noteFor: (chunk) => ({ requestedBaseFont: "x", code: 0, chunk, subset: true }),
		});

		const result = await loop.renderPage(3, 150);

		expect(result.telemetry.refused).toEqual([CHUNK]);
		expect(result.telemetry.repainted).toBe(false);
		expect(renders).toBe(1);
		expect(view.shown).toEqual([]);
	});
});

/** A minimal EngineClient-shaped double over the JSON dispatch protocol. */
function fakeEngine(states: Array<Record<string, unknown>>): {
	dispatch: (body: Record<string, unknown>, payload: Uint8Array | null) => {
		response: { ok: boolean; value?: unknown };
		attachment: null;
	};
	reads: Array<{ op: string; len: number }>;
} {
	const reads: Array<{ op: string; len: number }> = [];
	let at = 0;
	return {
		reads,
		dispatch(body, payload) {
			reads.push({ op: String(body.op), len: payload?.byteLength ?? 0 });
			const value = states[at];
			if (!value) throw new Error(`unexpected op ${String(body.op)} at ${at}`);
			at += 1;
			return { response: { ok: true, value }, attachment: null };
		},
	};
}

const OPEN_STATE = {
	revision: 0,
	residentBytes: 0,
	needs: [],
	loaded: [],
	unavailable: [],
	exhausted: [],
	request: null,
};
const ADOPT_STATE = {
	revision: 7,
	residentBytes: 3,
	needs: [],
	loaded: [CHUNK],
	unavailable: [],
	exhausted: [],
	request: null,
	adopted: true,
};

describe("FONT.10 — local-only cache drain", () => {
	it("opens the loader and delivers a cache-seeded chunk through the guest", async () => {
		const engine = fakeEngine([OPEN_STATE, ADOPT_STATE]);
		const drain = await attachViewerCjkDrain(engine as never, {
			doc: 1,
			claims: [CLAIM],
			core: new Uint8Array([0]),
			cache: byteCache(),
			cacheName: "selis-cjk-v1",
		});

		const outcome = await drain([CHUNK]);

		expect(outcome.delivered).toEqual([CHUNK]);
		expect(outcome.revision).toBe(7);
		expect(engine.reads.map((r) => r.op)).toEqual(["cjkOpen", "cjkChunk"]);
		expect(engine.reads[1]?.len).toBe(3); // the cached decompressed bytes
	});

	it("treats a cache miss as status 0 with no bytes — never a network fetch", async () => {
		const engine = fakeEngine([OPEN_STATE, { ...ADOPT_STATE, adopted: false, revision: 0, loaded: [] }]);
		const drain = await attachViewerCjkDrain(engine as never, {
			doc: 2,
			claims: [CLAIM],
			core: new Uint8Array([0]),
			cache: byteCache(false),
			cacheName: "selis-cjk-v1",
		});

		const outcome = await drain([CHUNK]);

		expect(outcome.refused).toEqual([CHUNK]);
		expect(outcome.delivered).toEqual([]);
		expect(engine.reads[1]?.len).toBe(0);
	});
});
