/**
 * SL-3.FONT.10 — the CJK *render-loop hook*: a production caller of
 * `LazyPayloadClient.drainCjk`.
 *
 * ## What this closes
 *
 * FONT.10 built the whole lazy-CJK mechanism but recorded honestly that
 * `drainCjk` had no caller in a viewer's render loop. This module is that
 * caller, expressed as a typed hook over three injected ports so it can be
 * exercised in vitest, in a real browser harness and by a shipped shell
 * without importing a platform global:
 *
 * * a **render port** — the guest has already rendered the page and handed
 *   back its typed CJK report (the `cjk` block `worker.rs::op_render` puts on
 *   every render response when a loader is installed);
 * * a **drain port** — backed by {@link LazyPayloadClient.drainCjk} over a
 *   {@link CjkCacheSource}, so bytes move only for chunk ids the guest named,
 *   under a digest-derived cache key, and never over a document-upload path;
 * * a **DOM seam** — the typed surface a shell uses to announce the repaint
 *   and to show the typed {@link CjkSubstitutionNote}. Substitution is always
 *   disclosed; there is no silent swap.
 *
 * ## The sequence (render-then-fetch-then-repaint)
 *
 * 1. render pass 1. An uncovered CJK code paints `.notdef` (tofu) and the
 *    guest names its chunk in `report.needs` — no I/O on the guest side.
 * 2. if `needs` is empty, nothing was substituted: clear any stale disclosure
 *    and return pass 1 untouched. Most pages take this path.
 * 3. otherwise `drain` the named ids through the cache adapter. The guest
 *    adopts a chunk only when its SHA-256 and SFNT parse check out.
 * 4. a changed `revision` is the repaint signal (ADR-P0043 §3): render the
 *    page again, now with the resident subset, and replace the tofu.
 * 5. show the typed disclosure for every adopted chunk. The note is pure data
 *    (it carries no font bytes), so this never uploads or downloads a
 *    document.
 *
 * Every decision that matters — what is missing, whether a delivery was
 * adopted, when to stop retrying — stays the guest's. The hook only renders,
 * drains what was named, watches the revision and discloses.
 */

import {
	CJK_DESCRIPTOR,
	LazyPayloadClient,
	type ClaimBody,
	type LazyState,
	type LazyTransport,
} from "./lazy-payload.js";
import {
	CjkCacheSource,
	cacheStoragePayloadCache,
	type CacheStorageLike,
} from "./cjk-cache-source.js";

/**
 * The typed CJK block a render response carries (SL-4.WASM.07).
 *
 * The guest inserts this only when a CJK loader is installed; a document
 * without one keeps the exact response shape it always had.
 */
export interface CjkRenderReport {
	/** Chunk ids the walk wanted but could not resolve this pass. */
	readonly needs: readonly string[];
	/** The set's counter. A change across a drain is the repaint signal. */
	readonly revision: number;
	/** Raw resident bytes after the walk. */
	readonly residentBytes: number;
}

/**
 * The browser-side mirror of `selis_font::cjk::CjkSubstitutionNote`.
 *
 * Kept field-for-field so a note emitted by the text path
 * (`selis-font-substitution/1`) and a note produced here describe the same
 * fact in the same vocabulary. `chunk: null` means the bundled core served
 * the code; a string names an on-demand subset.
 */
export interface CjkSubstitutionNote {
	/** The `/BaseFont` the document asked for (e.g. `STSong-Light`). */
	readonly requestedBaseFont: string;
	/** The code point that triggered the fallback. */
	readonly code: number;
	/** The chunk that served it, or `null` for the bundled core. */
	readonly chunk: string | null;
	/** Always `true`: the substitute is a subset, never a whole CJK font. */
	readonly subset: boolean;
}

/** The same one-line disclosure the Rust note renders, for a diagnostics panel. */
export function cjkSubstitutionDisclosure(note: CjkSubstitutionNote): string {
	const source = note.chunk === null ? "bundled subset core" : `on-demand subset chunk ${note.chunk}`;
	return `font ${note.requestedBaseFont} substituted with CJK ${source} for U+${note.code.toString(16).toUpperCase().padStart(4, "0")}`;
}

/**
 * The typed DOM seam a shell supplies. No method names a DOM API: an editor,
 * a tools page and a headless harness can each bind it differently.
 */
export interface CjkDomSeam {
	/** Show (replace) the disclosure for one page. */
	showDisclosure(page: number, note: CjkSubstitutionNote): void;
	/** Clear a stale disclosure when a page no longer substitutes. */
	clearDisclosure(page: number): void;
	/** Announce that a repaint was scheduled, with the revision that caused it. */
	noteRepaint(page: number, revision: number): void;
}

/** A no-op seam, for hosts that only want the telemetry (e.g. a harness). */
export const NOOP_CJK_DOM: CjkDomSeam = {
	showDisclosure() {},
	clearDisclosure() {},
	noteRepaint() {},
};

/** The renderer port over one guest render response. */
export interface CjkRenderPort<Page> {
	/** Render (or re-render) one page. */
	render(page: number): Page | Promise<Page>;
	/** Read the typed CJK block from a render result, if the guest reported one. */
	cjkOf(result: Page): CjkRenderReport | undefined;
}

/** What a drain returned, narrowed to what the loop needs. */
export interface CjkDrainOutcome {
	readonly state: LazyState<string>;
	readonly delivered: readonly string[];
	readonly refused: readonly string[];
}

/** What one hooked render observed — the harness asserts on this. */
export interface CjkRenderLoopTelemetry {
	readonly page: number;
	/** `needs` from pass 1 — the notdef report before any bytes moved. */
	readonly pass1Needs: readonly string[];
	readonly pass1Revision: number;
	/** Chunk ids the guest adopted on this pass. */
	readonly delivered: readonly string[];
	/** Chunk ids a delivery was refused for (the guest charged an attempt). */
	readonly refused: readonly string[];
	/** Whether a revision change forced a second render. */
	readonly repainted: boolean;
	readonly finalRevision: number;
	/** The last disclosure shown, or `null` when nothing was substituted. */
	readonly disclosure: string | null;
}

/** The result of a hooked render: the pixels to show and what happened. */
export interface CjkRenderResult<Page> {
	readonly page: Page;
	readonly telemetry: CjkRenderLoopTelemetry;
}

/** The three ports {@link createCjkRenderLoop} needs. */
export interface CjkRenderLoopPorts<Page> {
	readonly render: CjkRenderPort<Page>;
	/** Drain the ids pass 1 named; the production binding is `drainCjk`. */
	readonly drain: (needs: readonly string[]) => Promise<CjkDrainOutcome>;
	readonly dom: CjkDomSeam;
	/** Build the typed disclosure note for a chunk the guest adopted. */
	readonly noteFor: (chunk: string) => CjkSubstitutionNote;
}

/** A page render through the notdef → drain → repaint → disclose loop. */
export interface CjkRenderLoop<Page> {
	renderPage(page: number): Promise<CjkRenderResult<Page>>;
}

/**
 * Attach the render-loop hook. The returned `renderPage` is what a viewer's
 * render path calls in place of a raw render: it still returns the pixels, but
 * the caller can now trust that a CJK tofu frame is only ever a transient.
 */
export function createCjkRenderLoop<Page>(ports: CjkRenderLoopPorts<Page>): CjkRenderLoop<Page> {
	return {
		async renderPage(page: number): Promise<CjkRenderResult<Page>> {
			const first = await ports.render.render(page);
			const report = ports.render.cjkOf(first);
			const needs = report?.needs ?? [];
			const pass1Revision = report?.revision ?? 0;
			if (needs.length === 0) {
				ports.dom.clearDisclosure(page);
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

			// Only ids the guest named reach the drain. The cache adapter keys
			// on `payload:id:digest` and never derives a document URL.
			const outcome = await ports.drain(needs);
			const finalRevision = outcome.state.revision;
			const repainted = finalRevision !== pass1Revision;
			let shown = first;
			let disclosure: string | null = null;
			if (repainted) {
				ports.dom.noteRepaint(page, finalRevision);
				shown = await ports.render.render(page);
				// Disclose every adopted chunk — one note per substituted face,
				// never a silent swap. `noteFor` is injected because the render
				// report names chunks, not the document's /BaseFont or code.
				for (const chunk of outcome.delivered) {
					const note = ports.noteFor(chunk);
					disclosure = cjkSubstitutionDisclosure(note);
					ports.dom.showDisclosure(page, note);
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
					finalRevision,
					disclosure,
				},
			};
		},
	};
}

/** Options for {@link attachCjkDrain}. */
export interface CjkDrainOptions {
	/** The guest's message port (`cjkOpen`/`cjkChunk`/`cjkClose`). */
	readonly transport: LazyTransport;
	/** Injected Cache Storage-like object; this module never names `caches`. */
	readonly cache: CacheStorageLike;
	/** The Cache Storage bucket name for subset bytes. */
	readonly cacheName: string;
	/** The chunk claims the shell sends at open. */
	readonly claims: readonly ClaimBody<string>[];
	/** The bundled core subset, carried as cjkOpen's binary attachment. */
	readonly core: Uint8Array;
	/** Chunk ids this payload ships no file for, if any. */
	readonly absent?: readonly string[];
	/** Injected Response constructor (ADR-P0044: no platform globals). */
	readonly ResponseCtor: new (
		body: Uint8Array,
		init?: { status?: number },
	) => { readonly status: number; arrayBuffer(): Promise<ArrayBuffer> };
}

/** A drain bound to a real {@link LazyPayloadClient} + cache adapter. */
export interface AttachedCjkDrain {
	readonly client: LazyPayloadClient<string>;
	readonly drain: (needs: readonly string[]) => Promise<CjkDrainOutcome>;
}

/**
 * Bind a drain to the real client over a **local-only** byte source.
 *
 * A cache miss is answered with status `0` ("no response arrived"), never by
 * a network fetch: the cache adapter is the only route to subset bytes, so no
 * document bytes leave the device and no request is made without the shell's
 * separate, already-consented chunk installation. This is the production
 * counterpart of the in-memory doubles used in the unit tests.
 */
export async function attachCjkDrain(
	doc: number,
	options: CjkDrainOptions,
): Promise<AttachedCjkDrain> {
	const cache = cacheStoragePayloadCache(options.cache, options.cacheName, options.ResponseCtor);
	const source = new CjkCacheSource<string>({
		cache,
		// Local-only: a miss is a typed "nothing arrived". The guest charges a
		// failed attempt; it never turns into a download and never uploads.
		fetchBytes: async () => ({ status: 0, bytes: new Uint8Array(0) }),
	});
	const client = new LazyPayloadClient(CJK_DESCRIPTOR, options.transport, source);
	await client.open(doc, options.claims, {
		core: options.core,
		...(options.absent ? { absent: options.absent } : {}),
	});
	return {
		client,
		drain: async (needs: readonly string[]): Promise<CjkDrainOutcome> => {
			const before = client.state;
			const result = await client.drainCjk(doc, {
				needs,
				revision: before?.revision ?? 0,
				residentBytes: before?.residentBytes ?? 0,
			});
			return {
				state: result.state,
				delivered: result.delivered,
				refused: result.refused,
			};
		},
	};
}
