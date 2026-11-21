/**
 * SL-3.FONT.10 — the editor's CJK render-path integration.
 *
 * `buildPage` calls {@link renderWithCjk} where it used to call
 * `client.render` directly. The first render still paints immediately (a CJK
 * code the resident set cannot serve is a `.notdef` box, which is missing ink
 * in the right place); when the guest's typed report names chunks, the hook
 * drains them from the local Cache Storage adapter and, if the revision moved,
 * re-renders and replaces the page's pixels — with a typed disclosure in the
 * page DOM. No font bytes are embedded and no document bytes leave the device.
 *
 * The loader can only be installed when the shell has a `selis-cjk/1` manifest
 * and core bytes installed in Cache Storage; this checkout ships no CJK font
 * payload, so a document with no CJK loader simply takes the unchanged path.
 * The wiring is real and typed; the payload remains the honest open gap.
 */

import type { EngineClient, RenderedPage } from "./engine.js";
import {
	attachViewerCjkDrain,
	createViewerCjkRenderLoop,
	type CacheLike,
	type CjkClaim,
	type DrainOutcome,
} from "./cjk-render-loop.js";

/** The drains installed per document handle. */
const drains = new Map<number, (needs: readonly string[]) => Promise<DrainOutcome>>();

/** Options for installing a CJK loader on an open document. */
export interface ViewerCjkInstall {
	readonly doc: number;
	readonly claims: readonly CjkClaim[];
	readonly core: Uint8Array;
	readonly absent?: readonly string[];
	readonly cache?: CacheLike;
	readonly cacheName?: string;
}

/**
 * Bind a CJK loader to an open document. Called by a shell that has a shipped
 * `selis-cjk/1` payload; after this returns, renders carry the typed report
 * and {@link renderWithCjk} drives the notdef → repaint sequence.
 */
export async function installViewerCjk(
	engine: EngineClient,
	options: ViewerCjkInstall,
): Promise<void> {
	const drain = await attachViewerCjkDrain(engine, {
		doc: options.doc,
		claims: options.claims,
		core: options.core,
		...(options.absent ? { absent: options.absent } : {}),
		cache: options.cache ?? browserCache(),
		cacheName: options.cacheName ?? "selis-cjk-v1",
	});
	drains.set(options.doc, drain);
}

/** The page's own Cache Storage, or an empty in-memory double when refused. */
function browserCache(): CacheLike {
	const caches = (globalThis as { caches?: CacheLike }).caches;
	if (caches !== undefined) return caches;
	return {
		async open() {
			return {
				async match() {
					return undefined;
				},
				async put() {},
			};
		},
	};
}

/** Forget a document's loader when it is closed. */
export function forgetViewerCjk(doc: number): void {
	drains.delete(doc);
}

/** Where a typed disclosure is shown on a page. */
const NOTICE_CLASS = "selis-editor-cjk-notice";

function clearNotice(wrap: HTMLElement): void {
	for (const old of Array.from(wrap.querySelectorAll(`.${NOTICE_CLASS}`))) old.remove();
}

function showNotice(wrap: HTMLElement, text: string): void {
	clearNotice(wrap);
	const notice = document.createElement("p");
	notice.className = NOTICE_CLASS;
	notice.setAttribute("role", "status");
	notice.textContent = text;
	wrap.appendChild(notice);
}

/**
 * Render one page through the CJK hook.
 *
 * Returns the FIRST render synchronously so the page paints without waiting on
 * any byte source; if the guest named missing chunks, an async leg drains them
 * and repaints the canvas in place, disclosing each substituted face.
 *
 * @param paint redraw the page's canvas from a fresh render's pixels
 */
export function renderWithCjk(
	engine: EngineClient,
	doc: number,
	page: number,
	dpi: number,
	wrap: HTMLElement,
	paint: (page: RenderedPage) => void,
): RenderedPage {
	const first = engine.render(doc, page, dpi);
	const needs = first.cjk?.needs ?? [];
	if (needs.length === 0) {
		clearNotice(wrap);
		return first;
	}
	const drain = drains.get(doc);
	if (drain === undefined) {
		// No loader installed: the guest would not have reported `needs` in the
		// first place, but be explicit rather than silently painting tofu.
		return first;
	}
	void runDrainAndRepaint(engine, doc, page, dpi, wrap, paint, drain);
	return first;
}

async function runDrainAndRepaint(
	engine: EngineClient,
	doc: number,
	page: number,
	dpi: number,
	wrap: HTMLElement,
	paint: (page: RenderedPage) => void,
	drain: (needs: readonly string[]) => Promise<DrainOutcome>,
): Promise<void> {
	const loop = createViewerCjkRenderLoop({
		render: (p, d) => engine.render(doc, p, d),
		drain,
		view: {
			showDisclosure(_p, text) {
				showNotice(wrap, text);
			},
			clearDisclosure() {
				clearNotice(wrap);
			},
			noteRepaint() {
				// The repaint itself is the fresh render below; the seam records
				// the intent so a shell can announce it (aria-live on the notice).
			},
		},
		// The render report names chunks, not the drawn code or /BaseFont —
		// those come from the text path's `selis-font-substitution/1` notes.
		// Naming the on-demand chunk and the subset fact is still a disclosure,
		// never a silent swap.
		noteFor: (chunk) => ({
			requestedBaseFont: "unembedded CJK font",
			code: 0,
			chunk,
			subset: true,
		}),
	});
	const result = await loop.renderPage(page, dpi);
	if (result.telemetry.repainted) paint(result.page);
}
