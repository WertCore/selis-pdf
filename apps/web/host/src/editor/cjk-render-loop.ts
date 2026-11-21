/**
 * SL-3.FONT.10 — the CJK render-loop hook in the shipped web viewer.
 *
 * This is the production caller that closes FONT.10's render half. The editor
 * engine renders a page and now reports the guest's typed CJK notdef block
 * ({@link CjkRenderReport}); this module is what the render path calls instead
 * of a raw `engine.render`:
 *
 *   render (tofu) → read `needs` → drain only those ids through the local
 *   Cache Storage adapter → if `revision` moved, re-render → show the typed
 *   {@link CjkSubstitutionNote} in the DOM seam.
 *
 * ## Why it is self-contained
 *
 * The web host is served bundle-free and same-origin (ADR-P0016/ADR-P0028): a
 * bare workspace import would be rejected by `place-assets.mjs`. The logic is
 * therefore the small, typed sibling of `@selis/wasm-loader`'s
 * `createCjkRenderLoop`; both implement the same contract, and both are tested.
 * The canonical, transport-bound client remains `LazyPayloadClient.drainCjk`
 * in `packages/wasm-loader`; here the same protocol is driven over the engine's
 * own `dispatch` so the shipped page needs no new dependency.
 *
 * ## Local-only
 *
 * Subset bytes are read from an injected Cache Storage-like object under a
 * `payload:id:digest` key. A miss returns status `0` — there is no fetch port
 * at all, so no document bytes leave the device and no network request is
 * implied. Chunk installation (consent) is a separate shell concern.
 */

import type { EngineClient, RenderedPage } from "./engine.js";

/** A typed disclosure note — mirrors `selis_font::cjk::CjkSubstitutionNote`. */
export interface CjkSubstitutionNote {
	readonly requestedBaseFont: string;
	readonly code: number;
	readonly chunk: string | null;
	readonly subset: boolean;
}

/** The same one-line disclosure the engine emits in `selis-font-substitution/1`. */
export function cjkDisclosure(note: CjkSubstitutionNote): string {
	const source = note.chunk === null ? "bundled subset core" : `on-demand subset chunk ${note.chunk}`;
	return `font ${note.requestedBaseFont} substituted with CJK ${source} for U+${note.code.toString(16).toUpperCase().padStart(4, "0")}`;
}

/**
 * The typed DOM seam. It names outcomes, not DOM APIs, so a real page, a
 * shadow root and a headless vitest double can all bind it.
 */
export interface CjkViewSeam {
	/** Show (replace) the substitution disclosure for a page. */
	showDisclosure(page: number, text: string): void;
	/** Clear a stale disclosure when a page no longer substitutes. */
	clearDisclosure(page: number): void;
	/** Announce a repaint, with the revision that caused it. */
	noteRepaint(page: number, revision: number): void;
}

/** The cache key a byte cache must use: identity paired with the guest's digest. */
export function cjkCacheKey(id: string, sha256: string): string {
	return `cjk:${id}:${sha256}`;
}

/** The minimal Cache Storage surface the byte source needs, injected. */
export interface CacheLike {
	open(name: string): Promise<{
		match(key: string): Promise<unknown>;
		put(key: string, value: unknown): Promise<unknown>;
	}>;
}

/** The minimal `Response` shape Cache Storage returns, injected. */
export interface ResponseLike {
	readonly status: number;
	arrayBuffer(): Promise<ArrayBuffer>;
}

/** A claim the shell sends at `cjkOpen` and resolves from at drain time. */
export interface CjkClaim {
	readonly id: string;
	readonly sha256: string;
	readonly rawBytes: number;
	readonly url: string;
}

/** What a drain handed back, narrowed to what the render loop needs. */
export interface DrainOutcome {
	readonly revision: number;
	readonly residentBytes: number;
	readonly delivered: readonly string[];
	readonly refused: readonly string[];
}

/** The ports the viewer render-loop hook needs. */
export interface ViewerCjkPorts {
	/** Render one page; the engine attaches the typed CJK report. */
	readonly render: (page: number, dpi: number) => RenderedPage;
	/** Drain the ids pass 1 named; the production binding is the local cache drain. */
	readonly drain: (needs: readonly string[]) => Promise<DrainOutcome>;
	readonly view: CjkViewSeam;
	/** Build the typed note for a chunk the guest adopted. */
	readonly noteFor: (chunk: string) => CjkSubstitutionNote;
}

/** What one hooked render observed — asserted by vitest and the browser leg. */
export interface ViewerCjkTelemetry {
	readonly page: number;
	readonly pass1Needs: readonly string[];
	readonly pass1Revision: number;
	readonly delivered: readonly string[];
	readonly refused: readonly string[];
	readonly repainted: boolean;
	readonly finalRevision: number;
	readonly disclosure: string | null;
}

export interface ViewerCjkResult {
	readonly page: RenderedPage;
	readonly telemetry: ViewerCjkTelemetry;
}

/** A page render through the notdef → drain → repaint → disclose loop. */
export function createViewerCjkRenderLoop(ports: ViewerCjkPorts): {
	renderPage(page: number, dpi: number): Promise<ViewerCjkResult>;
} {
	return {
		async renderPage(page: number, dpi: number): Promise<ViewerCjkResult> {
			const first = ports.render(page, dpi);
			const needs = first.cjk?.needs ?? [];
			const pass1Revision = first.cjk?.revision ?? 0;
			if (needs.length === 0) {
				ports.view.clearDisclosure(page);
				return {
					page: first,
					telemetry: {
						page,
						pass1Needs: [],
						pass1Revision,
						delivered: [],
						refused: [],
						repainted: false,
						finalRevision: pass1Revision,
						disclosure: null,
					},
				};
			}

			const outcome = await ports.drain(needs);
			const repainted = outcome.revision !== pass1Revision;
			let shown = first;
			let disclosure: string | null = null;
			if (repainted) {
				ports.view.noteRepaint(page, outcome.revision);
				shown = ports.render(page, dpi);
				for (const chunk of outcome.delivered) {
					const note = ports.noteFor(chunk);
					disclosure = cjkDisclosure(note);
					ports.view.showDisclosure(page, disclosure);
				}
			}
			return {
				page: shown,
				telemetry: {
					page,
					pass1Needs: [...needs],
					pass1Revision,
					delivered: [...outcome.delivered],
					refused: [...outcome.refused],
					repainted,
					finalRevision: outcome.revision,
					disclosure,
				},
			};
		},
	};
}

/** Options for {@link attachViewerCjkDrain}. */
export interface ViewerDrainOptions {
	readonly doc: number;
	readonly claims: readonly CjkClaim[];
	readonly core: Uint8Array;
	readonly cache: CacheLike;
	readonly cacheName: string;
	readonly absent?: readonly string[];
}

/** Read the guest's state from a `cjkOpen`/`cjkChunk` response value. */
function readState(value: unknown): { revision: number; residentBytes: number; request: CjkClaim | null; adopted: boolean } {
	if (typeof value !== "object" || value === null) {
		throw new Error("the engine's CJK loader reply is malformed");
	}
	const v = value as Record<string, unknown>;
	const num = (key: string): number => {
		const n = v[key];
		if (typeof n !== "number" || !Number.isFinite(n)) throw new Error(`CJK reply has no ${key}`);
		return n;
	};
	let request: CjkClaim | null = null;
	if (v.request !== null && typeof v.request === "object") {
		const r = v.request as Record<string, unknown>;
		request = {
			id: String(r.id),
			sha256: String(r.sha256),
			rawBytes: Number(r.rawBytes),
			url: String(r.url),
		};
	}
	return {
		revision: num("revision"),
		residentBytes: num("residentBytes"),
		request,
		adopted: v.adopted === true,
	};
}

/**
 * Bind the drain to the engine guest over a **local-only** byte source.
 *
 * The returned `drain` moves bytes only for ids the guest named, keyed on the
 * claim's digest, and never fetches: a cache miss hands the guest status `0`.
 * It is the viewer-side counterpart of `LazyPayloadClient.drainCjk` — the same
 * render-then-fetch ordering, expressed over `EngineClient.dispatch` so the
 * shipped page keeps its closed import set.
 */
export async function attachViewerCjkDrain(
	engine: EngineClient,
	options: ViewerDrainOptions,
): Promise<(needs: readonly string[]) => Promise<DrainOutcome>> {
	const claims = new Map(options.claims.map((claim) => [claim.id, claim]));
	const openReply = engine.dispatch(
		{
			op: "cjkOpen",
			doc: options.doc,
			claims: options.claims.map((c) => ({
				id: c.id,
				sha256: c.sha256,
				rawBytes: c.rawBytes,
				url: c.url,
			})),
			coreLen: options.core.byteLength,
			...(options.absent && options.absent.length > 0 ? { unserved: [...options.absent] } : {}),
		},
		options.core,
	);
	if (openReply.response.ok !== true) {
		throw new Error(openReply.response.message ?? "the engine refused cjkOpen");
	}
	let state = readState(openReply.response.value);

	const deliver = async (claim: CjkClaim): Promise<boolean> => {
		const key = cjkCacheKey(claim.id, claim.sha256);
		const hit = await (await options.cache.open(options.cacheName)).match(key);
		let bytes = new Uint8Array(0);
		let status = 0;
		if (hit && typeof (hit as ResponseLike).arrayBuffer === "function") {
			const response = hit as ResponseLike;
			if (response.status === 200) {
				bytes = new Uint8Array(await response.arrayBuffer());
				status = 200;
			}
		}
		const reply = engine.dispatch(
			{
				op: "cjkChunk",
				doc: options.doc,
				chunk: claim.id,
				status,
				len: bytes.byteLength,
			},
			bytes,
		);
		if (reply.response.ok !== true) {
			throw new Error(reply.response.message ?? "the engine refused cjkChunk");
		}
		state = readState(reply.response.value);
		return state.adopted;
	};

	return async (needs: readonly string[]): Promise<DrainOutcome> => {
		const delivered: string[] = [];
		const refused: string[] = [];
		const queue = [...needs];
		const seen = new Set<string>();
		while (queue.length > 0) {
			if (delivered.length + refused.length >= 64) {
				throw new Error("CJK delivery loop exceeded its guard");
			}
			const id = queue.shift() as string;
			if (seen.has(id)) continue;
			seen.add(id);
			const claim = claims.get(id);
			if (claim === undefined) continue; // the guest named nothing we can address
			if (await deliver(claim)) {
				delivered.push(id);
				if (state.request !== null) queue.push(state.request.id);
			} else {
				refused.push(id);
			}
		}
		return {
			revision: state.revision,
			residentBytes: state.residentBytes,
			delivered,
			refused,
		};
	};
}
