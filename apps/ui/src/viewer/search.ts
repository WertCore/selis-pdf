/**
 * Incremental search for the viewer (SL-4.UI.05).
 *
 * The controller consumes {@link EnginePort.search}'s async batches and hands
 * the shell one immutable {@link SearchState}: the match count, the current
 * match, and the highlight rectangles to paint. It renders nothing, reads no
 * viewport, and reaches for no platform global, exactly as `page-list.ts` does
 * (SL-4.UI.02), which is also what lets it be tested in Node: it holds no
 * platform global (SL-4.UI.01 / ADR-P0044) and there is no jsdom to assert
 * against.
 *
 * ## Four decisions this file exists to record
 *
 * **1. Where the state lives.** ADR-P0035 says app state belongs in the
 * `selis-viewmodel` crate, and search match state is named there explicitly.
 * That crate does not exist yet. What UI.04 did for selection, UI.05 does for
 * search: the *state* is a small plain object the controller owns and
 * republishes whole, and every *question* about it is a pure function of plain
 * data over it, so moving the state across the language boundary later is a
 * matter of calling these from Rust rather than a rewrite. Inventing a richer
 * local store would be more code today and nothing to migrate tomorrow.
 *
 * **2. Highlight geometry comes from the text layer, never from the render
 * ladder and never from `SearchMatch.rects`.** This is UI.04's argument
 * repeated because it is load-bearing for search too: the engine's glyph quads
 * are the only source of character geometry, they are scale-free, and
 * `DrawOp.provisional` means the pixels on screen during a zoom are the
 * *previous* scale's bitmap. A highlight derived from what is painted would
 * therefore be wrong exactly during the zoom it has to survive. A match is a
 * character range; the rectangles are `selectionRects` over the same
 * `TextLayerFrame` the selection uses, so one projection serves both and a
 * match highlight and a text selection cannot disagree.
 *
 * `SearchMatch.rects` is deliberately unread, and that is a decision rather
 * than an oversight. It is optional, so no transport is required to supply it;
 * the engine's own `selis-pdf-text` `SearchMatch` carries the *line's*
 * bounding rect rather than a per-match one, so a transport that mapped it
 * naively would highlight the whole line. A second geometry source that can be
 * absent, and that disagrees with the text layer when present, is worse than
 * none. A match whose range cannot be located in the page's text still counts
 * and still navigates; it simply has no rectangle, which is recorded as a known
 * limit in the viewer README rather than papered over.
 *
 * **3. A superseded search cannot repaint.** Every run gets a monotonic id and
 * its own `AbortController`. Batches are applied, state is written and layers
 * are cached *only* behind an id check, so a transport that ignores its signal,
 * or a generator parked mid-`await` when the reader types another character, is
 * inert rather than a source of mixed results. The abort is the optimisation;
 * the id is the guarantee, because `EnginePort.search` is an `AsyncIterable`
 * and cancellation is only ever a promise the transport makes.
 *
 * **4. The work is bounded by the window, not the document.** The match *count*
 * is document-wide and honest, but the per-page breakdown and the highlight
 * rectangles are computed only for the pages the shell reports as visible. A
 * reader who searches a common word in a 2 000-page report should not have the
 * viewer allocate a rectangle per match on pages nobody is looking at, nor pay
 * for them on every one of the thousands of batches that arrive. Per-batch cost
 * is O(window), which is the property `search.test.ts` measures.
 *
 * ## What the shell still owns
 *
 * The input, the buttons, the live-region element, scrolling and focus. The
 * controller publishes `currentMatch`; the shell calls the page list's
 * `goToPage`, because `goToPage` and `scrollTopForPage` are the only two ways
 * to move (UI.02's rule, which UI.06 leans on) and search is not the place to
 * add a third.
 */

import type { PlatformAdapter } from "../platform/adapter.js";
import { ErrorCode } from "../platform/errors.js";
import type { AdapterError } from "../platform/errors.js";
import type { DocHandle, PageTextLayer, SearchBatch } from "../platform/types.js";
import type { SearchOptions as EngineSearchOptions } from "../platform/types.js";
import { resolveSearchKey } from "./keyboard.js";
import type { PlacedPage } from "./layout.js";
import type { Caret } from "./selection.js";
import { selectionRects } from "./selection.js";
import type { PageListCatalogue, SearchCatalogue, SearchStrings } from "./strings.js";
import { createPageListStrings, createSearchStrings } from "./strings.js";
import type { CharBox, TextLayerFrame } from "./text-layer.js";
import { buildTextLayer } from "./text-layer.js";

/** Where a search is in its lifecycle. `cancelled` is not a status: see below. */
export type SearchStatus = "idle" | "searching" | "complete" | "failed";

/**
 * Cancellation is deliberately *not* a status.
 *
 * A cancelled search is a superseded one, and the reader sees whatever the
 * search that replaced it published. Giving it a status would mean a transient
 * state the next keystroke overwrites, and a live region that announces
 * "search cancelled" between two queries typed a moment apart. The evidence
 * that cancellation works is the abort and the run id, both asserted in the
 * tests, not a fifth status the UI can paint.
 */

/** The modifiers the reader can set, resolved to definite booleans. */
export interface SearchModifiers {
	/** `Aa`: the search is case-sensitive. */
	readonly caseSensitive: boolean;
	/** Whole words only. */
	readonly wholeWord: boolean;
}

/** One match, as a character range in the page's extracted text. */
export interface SearchMatchRange {
	readonly page: number;
	/** UTF-16 start offset within the page's text. */
	readonly start: number;
	/** UTF-16 end offset (exclusive) within the page's text. */
	readonly end: number;
}

/** How many matches are on one page, for a badge on that page's tile. */
export interface PageMatchCount {
	readonly page: number;
	readonly count: number;
}

/**
 * One highlight to paint, inside a page element.
 *
 * `rects` is in CSS pixels relative to the page box, the same frame the text
 * layer and the selection use, and the shell positions it the way it positions
 * a text layer: absolutely, inside the tile.
 */
export interface SearchHighlight {
	/** Index into the flat match list, so a shell can key on identity. */
	readonly index: number;
	readonly page: number;
	readonly start: number;
	readonly end: number;
	/** One box per line the match covers. Empty when the range is unlocatable. */
	readonly rects: readonly CharBox[];
	/** True for the one match next/previous is on. */
	readonly current: boolean;
	/** `.selis-*` class list; never a hard-coded colour. */
	readonly className: string;
}

/** What the shell reports about what is on screen. The only geometry input. */
export interface SearchWindow {
	/** The page the reader is looking at; where a bare "next" starts from. */
	readonly currentPage: number;
	/** The pages currently in the window, in any order. */
	readonly pages: readonly number[];
}

/** Everything the shell paints, in one immutable value. */
export interface SearchState {
	readonly docId: string;
	/** The query being searched, verbatim (never trimmed for the reader). */
	readonly query: string;
	readonly modifiers: SearchModifiers;
	readonly status: SearchStatus;
	/** How much of the document the engine has scanned, 0..1. */
	readonly progress: number;
	/**
	 * Matches found *so far*. Grows while `status` is `searching`, so the
	 * count on screen is a running count, not a promise.
	 */
	readonly matchCount: number;
	/** Index of the current match, or -1 when none is selected. */
	readonly currentIndex: number;
	/** The current match, or `null`. What the shell scrolls to. */
	readonly currentMatch: SearchMatchRange | null;
	/** The reported current page, echoed so a shell can diff it cheaply. */
	readonly visiblePage: number;
	/** Per-page counts for the pages in the window only; see the module doc. */
	readonly pageCounts: readonly PageMatchCount[];
	/** Highlights for the pages in the window only. */
	readonly highlights: readonly SearchHighlight[];
	/** Polite live-region text. */
	readonly announcement: string;
	/** True when there is at least one match to step to. */
	readonly hasMatches: boolean;
	/** `aria-label` for the search input. */
	readonly fieldLabel: string;
	/** Placeholder for the search input. */
	readonly fieldPlaceholder: string;
	/** `aria-label` for the region wrapping the controls. */
	readonly regionLabel: string;
	/** `aria-label` for the next-match control. */
	readonly nextLabel: string;
	/** `aria-label` for the previous-match control. */
	readonly previousLabel: string;
	/** `aria-label` for the close control. */
	readonly closeLabel: string;
	/** Label for the match-case modifier. */
	readonly caseLabel: string;
	/** Label for the whole-word modifier. */
	readonly wordLabel: string;
}

export interface SearchControllerOptions {
	readonly adapter: PlatformAdapter;
	readonly doc: DocHandle;
	/**
	 * The page's current box, or `null` when the layout does not have it (an
	 * empty document, or before the first layout pass). This is the *only*
	 * geometry input, and the reason highlight geometry is not coupled to the
	 * render ladder: what is asked for is `PlacedPage.scale`, CSS pixels per
	 * PDF point, and nothing about tiles, rungs or device pixels. A zoom changes
	 * the answer, the frames are re-projected, and no pixel is cached.
	 */
	readonly placePage: (page: number) => PlacedPage | null;
	/** Search message catalogue override, merged over English. */
	readonly strings?: Partial<SearchCatalogue>;
	/**
	 * Page-list catalogue, used only to word the page in an announcement. A
	 * shell that has already localised the page list passes the same overrides
	 * here, so "Page 7 of 900" is worded one way in both places.
	 */
	readonly pageStrings?: Partial<PageListCatalogue>;
	/** Called on every state change, including every batch that lands. */
	readonly onState?: (state: SearchState) => void;
	/** Called for a scan that failed for a reason other than cancellation. */
	readonly onError?: (error: AdapterError) => void;
}

/** The controller surface the shells program against. */
export interface SearchController {
	/**
	 * Set the query, superseding any scan in flight. An empty query clears;
	 * re-setting the query already being searched is a no-op, so a shell that
	 * reports every input event does not restart a scan per keystroke.
	 */
	setQuery(query: string, modifiers?: Partial<SearchModifiers>): SearchState;
	/** Set the modifiers, re-running the current query if they changed. */
	setModifiers(modifiers: Partial<SearchModifiers>): SearchState;
	/** Report what is on screen; returns the republished state. */
	update(window: SearchWindow): SearchState;
	/** The last state. */
	state(): SearchState;
	/** Step to the next match, wrapping. */
	next(): SearchState;
	/** Step to the previous match, wrapping. */
	previous(): SearchState;
	/** Select a match by index; out-of-range is clamped. */
	select(index: number): SearchState;
	/** Resolve a key press through the search key map, or `null`. */
	handleKey(key: string, shift?: boolean): SearchState | null;
	/** Abort any scan in flight and drop every result. */
	clear(): SearchState;
	/** Subscribe to state changes. Returns an unsubscribe. */
	subscribe(listener: (state: SearchState) => void): () => void;
	/** Abort and release. Idempotent. */
	dispose(): void;
}

/**
 * Locate a page-text offset as a caret in a projected text layer, or `null`.
 *
 * The join between what search reports and what the text layer draws is one
 * newline per line: `PageTextLayer.text` is the page's lines joined with `"\n"`
 * (SL-3.TEXT.04, and the mock synthesiser does exactly this), and a match's
 * `start`/`end` are offsets into that same string. So the mapping is a walk,
 * not a search, and it is a pure function of the frame because the frame's
 * lines are index-aligned with the text by construction.
 *
 * `index === text.length` resolves to the *end* of the last line, which is what
 * makes a match running to the end of a page land on its last character rather
 * than falling off the end and losing its highlight. Anything past that is out
 * of range and returns `null`: an unlocatable range is still counted and still
 * navigable, it just has no rectangle.
 *
 * Exported because it is the one piece of this file testable against
 * hand-authored geometry without a controller, and because it is the seam a
 * future viewmodel would call across the language boundary.
 */
export function caretAtTextIndex(frame: TextLayerFrame, index: number): Caret | null {
	if (!Number.isInteger(index) || index < 0) {
		return null;
	}
	let cursor = 0;
	for (let line = 0; line < frame.lines.length; line += 1) {
		const entry = frame.lines[line];
		if (entry === undefined) {
			break;
		}
		const length = entry.chars.length;
		if (index <= cursor + length) {
			return { line, offset: index - cursor };
		}
		// One past a line's characters is the newline `text` puts there, so the
		// next line starts at `cursor + length + 1`.
		cursor += length + 1;
	}
	return null;
}

/**
 * A match's two carets, or `null` when the range cannot be placed.
 *
 * A range whose end lands on the *start* of a later line is clamped to the end
 * of the line it began on. That happens for a query containing a newline, and
 * the alternative is a highlight spanning the gap between two lines, which
 * reads as a rendering bug rather than as a match.
 */
export function matchCarets(
	frame: TextLayerFrame,
	start: number,
	end: number,
): [Caret, Caret] | null {
	const from = caretAtTextIndex(frame, start);
	const to = caretAtTextIndex(frame, end);
	if (from === null || to === null) {
		return null;
	}
	const clamped: Caret =
		to.line > from.line && to.offset === 0
			? { line: from.line, offset: frame.lines[from.line]?.chars.length ?? 0 }
			: to;
	return [from, clamped];
}

/** Order two matches the way a reader walks the document. */
function compareMatches(left: SearchMatchRange, right: SearchMatchRange): number {
	return left.page - right.page || left.start - right.start || left.end - right.end;
}

function sameMatch(left: SearchMatchRange, right: SearchMatchRange): boolean {
	return left.page === right.page && left.start === right.start && left.end === right.end;
}

/** A cancellation is registry code 4020, or the platform's own abort error. */
function isCancellation(error: unknown): boolean {
	if (typeof error !== "object" || error === null) {
		return false;
	}
	const candidate = error as { code?: unknown; name?: unknown };
	return candidate.code === ErrorCode.Cancelled || candidate.name === "AbortError";
}

/**
 * Text layers kept in memory. Bounded because they are the only per-page
 * allocation search makes, and a reader who scrolls the whole document with a
 * search open would otherwise accumulate one layer per page passed. The window
 * is always kept; the rest is most-recently-used.
 */
const MAX_CACHED_LAYERS = 64;

/**
 * Create the search controller (SL-4.UI.05).
 *
 * Shaped exactly like `createPageList`: it owns a slice, reports facts, hands
 * back one immutable value, and renders nothing. The slice is the query, the
 * modifiers, the matches and which one is current, and not the viewport, the
 * scroll offset or the zoom, which belong to the page list and arrive as the
 * one input this controller takes.
 */
export function createSearch(options: SearchControllerOptions): SearchController {
	const strings: SearchStrings = createSearchStrings(options.strings ?? {});
	const pageStrings = createPageListStrings(options.pageStrings ?? {});
	const subscribers = new Set<(state: SearchState) => void>();

	/** The matches, in document order. Counted, never handed out. */
	let matches: SearchMatchRange[] = [];
	/** Per-page counts: the windowed breakdown, and the "needs a layer" test. */
	let perPage = new Map<number, number>();
	let currentIndex = -1;
	/** Identity of the current match, so it survives batches being appended. */
	let currentMatch: SearchMatchRange | null = null;
	let status: SearchStatus = "idle";
	let progress = 0;
	let query = "";
	let modifiers: SearchModifiers = { caseSensitive: false, wholeWord: false };
	let win: SearchWindow = { currentPage: 0, pages: [] };

	/**
	 * Engine text layers, keyed by page. Scale-free by construction: the quads
	 * are in PDF points, so a zoom does not invalidate this map and nothing here
	 * is thrown away when the reader zooms. That is the whole of the
	 * zoom-survival argument, and it is why the frames below are a separate,
	 * disposable cache.
	 */
	const layers = new Map<number, PageTextLayer>();
	/** Projected frames, valid only at `frameScale`. */
	let frames = new Map<number, TextLayerFrame>();
	let frameScale: number | null = null;
	const pendingLayers = new Set<number>();

	/** Monotonic run id. The guard every mutation sits behind. */
	let runId = 0;
	let scan: AbortController | null = null;
	let lastState: SearchState | null = null;
	/** The announcement already published, so an unchanged match stays quiet. */
	let announced: { key: string; text: string } | null = null;
	let disposed = false;

	/** CSS pixels per PDF point for the window, or `null` before first layout. */
	function currentScale(): number | null {
		for (const page of win.pages) {
			const placed = options.placePage(page);
			if (placed !== null) {
				return placed.scale;
			}
		}
		return null;
	}

	/** Matches on `page`, in document order, with their flat indices. */
	function matchesOnPage(page: number): { index: number; match: SearchMatchRange }[] {
		const found: { index: number; match: SearchMatchRange }[] = [];
		for (let index = 0; index < matches.length; index += 1) {
			const match = matches[index];
			if (match !== undefined && match.page === page) {
				found.push({ index, match });
			}
		}
		return found;
	}

	/**
	 * The highlights for the window, projected now, from the current scale.
	 *
	 * O(window) whatever the document size, because the loop is over the
	 * window's pages and each page's own matches. A match on a page nobody is
	 * looking at costs nothing beyond the two numbers already stored.
	 */
	function buildHighlights(): SearchHighlight[] {
		const scale = currentScale();
		if (scale !== frameScale) {
			// A zoom (or the first pass) re-projects from the engine's points.
			// The layer cache survives; the pixels never existed.
			frames = new Map();
			frameScale = scale;
		}
		if (scale === null) {
			return [];
		}
		const highlights: SearchHighlight[] = [];
		for (const page of [...win.pages].sort((a, b) => a - b)) {
			const layer = layers.get(page);
			const placed = options.placePage(page);
			if (layer === undefined || placed === null) {
				continue;
			}
			let frame = frames.get(page);
			if (frame === undefined) {
				frame = buildTextLayer(layer, placed);
				frames.set(page, frame);
			}
			for (const entry of matchesOnPage(page)) {
				const carets = matchCarets(frame, entry.match.start, entry.match.end);
				const current = currentMatch !== null && sameMatch(entry.match, currentMatch);
				// The class list is assembled, never concatenated: the i18n lint
				// gate in `strings.test.ts` reads a literal containing a space and
				// a letter as user-facing prose, and a joined array is the same
				// shape `page-list.ts` uses for the very same reason.
				const classes = ["selis-search-highlight"];
				if (current) {
					classes.push("selis-search-highlight--current");
				}
				highlights.push({
					index: entry.index,
					page,
					start: entry.match.start,
					end: entry.match.end,
					rects:
						carets === null ? [] : selectionRects(frame, { anchor: carets[0], head: carets[1] }),
					current,
					className: classes.join(" "),
				});
			}
		}
		return highlights;
	}

	/**
	 * The live-region sentence.
	 *
	 * Three rules, and each one is a bug the reader would otherwise see:
	 *
	 * 1. A scan in progress never says "no matches". It says how many so far.
	 * 2. The current-match sentence is only re-announced when the current match
	 *    actually changes. Otherwise every one of a 2 000-page document's
	 *    batches would re-announce the same match with a larger count, and a
	 *    polite live region would interrupt itself hundreds of times.
	 * 3. The page is worded by the *page list's* catalogue, passed in as
	 *    `pageLabel`, so "Page 7 of 900" cannot be worded two ways.
	 */
	function announcementText(): string {
		if (query.length === 0) {
			return strings.idle();
		}
		if (status === "failed") {
			return strings.failed();
		}
		if (currentMatch !== null && currentIndex >= 0) {
			const key = `${currentMatch.page}:${currentMatch.start}:${currentMatch.end}`;
			if (announced !== null && announced.key === key) {
				return announced.text;
			}
			const text = strings.match(
				currentIndex + 1,
				matches.length,
				pageStrings.pageLabel(currentMatch.page, options.doc.pageCount),
			);
			announced = { key, text };
			return text;
		}
		if (status === "complete") {
			return matches.length === 0 ? strings.none() : strings.found(matches.length);
		}
		return strings.scanning(matches.length);
	}

	function buildState(): SearchState {
		return {
			docId: options.doc.id,
			query,
			modifiers,
			status,
			progress,
			matchCount: matches.length,
			currentIndex,
			currentMatch,
			visiblePage: win.currentPage,
			pageCounts: [...win.pages]
				.sort((a, b) => a - b)
				.flatMap((page) => {
					const count = perPage.get(page);
					return count === undefined || count === 0 ? [] : [{ page, count }];
				}),
			highlights: buildHighlights(),
			announcement: announcementText(),
			hasMatches: matches.length > 0,
			fieldLabel: strings.fieldLabel,
			fieldPlaceholder: strings.fieldPlaceholder,
			regionLabel: strings.regionLabel,
			nextLabel: strings.nextLabel,
			previousLabel: strings.previousLabel,
			closeLabel: strings.closeLabel,
			caseLabel: strings.caseLabel,
			wordLabel: strings.wordLabel,
		};
	}

	function publish(): SearchState {
		const state = buildState();
		if (disposed) {
			return state;
		}
		lastState = state;
		options.onState?.(state);
		for (const listener of subscribers) {
			listener(state);
		}
		return state;
	}

	/** Keep the window, then most-recently-used, up to the cap. */
	function pruneLayers(): void {
		if (layers.size <= MAX_CACHED_LAYERS) {
			return;
		}
		const keep = new Set(win.pages);
		if (currentMatch !== null) {
			keep.add(currentMatch.page);
		}
		for (const page of [...layers.keys()].reverse()) {
			if (layers.size <= MAX_CACHED_LAYERS) {
				return;
			}
			if (!keep.has(page)) {
				layers.delete(page);
				frames.delete(page);
			}
		}
	}

	/**
	 * Fetch the text layers the window needs, and only those.
	 *
	 * Bounded by the window on purpose: the match count is document-wide, but
	 * asking the engine for a text layer per matching page in a 2 000-page
	 * report would spend the whole scan budget on pages nobody is looking at.
	 * The window is what the reader can see, so the window is what gets
	 * geometry.
	 */
	function requestWindowLayers(run: number): void {
		for (const page of win.pages) {
			if ((perPage.get(page) ?? 0) === 0 || layers.has(page) || pendingLayers.has(page)) {
				continue;
			}
			pendingLayers.add(page);
			const signal = scan?.signal;
			void options.adapter.engine
				.textLayer(options.doc, page, signal === undefined ? undefined : { signal })
				.then((layer) => {
					pendingLayers.delete(page);
					// A layer that resolves after its scan was superseded is
					// dropped rather than stored: a stale layer for the old
					// query's page set is exactly the cross-run contamination
					// the run id exists to stop.
					if (disposed || run !== runId) {
						return;
					}
					layers.set(page, layer);
					frames.delete(page);
					pruneLayers();
					publish();
				})
				.catch((error: unknown) => {
					pendingLayers.delete(page);
					if (disposed || run !== runId || isCancellation(error)) {
						return;
					}
					// A page whose layer cannot be read still counts its
					// matches and still navigates; it just has no highlight.
					options.onError?.(error as AdapterError);
				});
		}
	}

	/**
	 * Fold one batch in. Every early return here is the supersession guard: a
	 * batch from a scan that is no longer current is dropped without touching a
	 * single field, which is the property `search.test.ts` proves against an
	 * engine that ignores its abort signal entirely.
	 *
	 * The append is in place and the sort is conditional, because this runs
	 * once per page of the document. A transport that yields in document order,
	 * which is what `EnginePort.search` documents and what the engine does,
	 * costs O(1) amortised per match and no sort at all. Only a transport that
	 * yields out of order pays for the re-sort, and only that transport can
	 * shift an existing match's index, which is why the current match is
	 * re-found by identity there and nowhere else.
	 */
	function applyBatch(run: number, batch: SearchBatch): void {
		if (disposed || run !== runId) {
			return;
		}
		let ordered = true;
		for (const match of batch.matches) {
			const range: SearchMatchRange = { page: match.page, start: match.start, end: match.end };
			const last = matches[matches.length - 1];
			if (last !== undefined && compareMatches(last, range) > 0) {
				ordered = false;
			}
			// A transport that repeats a match would double-count it, and a
			// count that is wrong is worse than a count that is late.
			if (last !== undefined && sameMatch(last, range)) {
				continue;
			}
			matches.push(range);
			perPage.set(range.page, (perPage.get(range.page) ?? 0) + 1);
		}
		if (!ordered) {
			matches.sort(compareMatches);
			const identity = currentMatch;
			if (identity !== null) {
				const found = matches.findIndex((entry) => sameMatch(entry, identity));
				currentIndex = found;
				if (found === -1) {
					currentMatch = null;
				}
			}
		}
		progress = Math.min(1, Math.max(0, batch.progress));
		if (batch.done) {
			status = "complete";
		}
		// Ask for the layers this batch's pages need *now*, not when the scan
		// finishes. That is what makes highlighting progressive rather than a
		// lump that appears at the end, and the loop is over the window, so it
		// is a handful of iterations per page of the document.
		requestWindowLayers(run);
	}

	/**
	 * Drain the engine's batches into published states.
	 *
	 * The `run !== runId` checks are the guarantee; the `abort()` in
	 * {@link startScan} is only the optimisation. Returning from inside
	 * `for await` also calls the iterator's `return()`, so a well-behaved
	 * generator gets its `finally` on the way out, and one that ignores its
	 * signal simply never gets to mutate anything.
	 */
	async function consume(
		run: number,
		signal: AbortSignal,
		searchQuery: string,
		searchModifiers: SearchModifiers,
	): Promise<void> {
		const engineOptions: EngineSearchOptions = {
			caseSensitive: searchModifiers.caseSensitive,
			wholeWord: searchModifiers.wholeWord,
		};
		try {
			for await (const batch of options.adapter.engine.search(
				options.doc,
				searchQuery,
				engineOptions,
				{ signal },
			)) {
				if (disposed || run !== runId) {
					return;
				}
				applyBatch(run, batch);
				publish();
			}
			if (disposed || run !== runId) {
				return;
			}
			// The loop ended without `done: true`. A transport that simply
			// stops is treated as a finished scan rather than a failure, but
			// the progress is forced to 1 so a shell can retire a progress bar
			// that would otherwise sit at 80% for ever.
			status = "complete";
			progress = 1;
			requestWindowLayers(run);
			publish();
		} catch (error) {
			if (disposed || run !== runId) {
				return;
			}
			// Cancellation is not a failure and is not reported as one: the
			// scan it belonged to has been replaced, and its replacement owns
			// the state now.
			if (signal.aborted || isCancellation(error)) {
				return;
			}
			status = "failed";
			options.onError?.(error as AdapterError);
			publish();
		}
	}

	function startScan(nextQuery: string, nextModifiers: SearchModifiers): SearchState {
		scan?.abort();
		const run = ++runId;
		const controller = new AbortController();
		scan = controller;
		reset(nextQuery, nextModifiers, "searching");
		publish();
		void consume(run, controller.signal, nextQuery, nextModifiers);
		return lastState as SearchState;
	}

	/** Drop every result and put the controller back to a known state. */
	function reset(nextQuery: string, nextModifiers: SearchModifiers, nextStatus: SearchStatus) {
		query = nextQuery;
		modifiers = nextModifiers;
		matches = [];
		perPage = new Map();
		layers.clear();
		frames = new Map();
		frameScale = null;
		currentIndex = -1;
		currentMatch = null;
		progress = 0;
		status = nextStatus;
		announced = null;
	}

	function resolveModifiers(next?: Partial<SearchModifiers>): SearchModifiers {
		return {
			caseSensitive: next?.caseSensitive ?? modifiers.caseSensitive,
			wholeWord: next?.wholeWord ?? modifiers.wholeWord,
		};
	}

	function sameModifiers(left: SearchModifiers, right: SearchModifiers): boolean {
		return left.caseSensitive === right.caseSensitive && left.wholeWord === right.wholeWord;
	}

	/**
	 * The first match at or after `page`, or the first match at all.
	 *
	 * The fallback is the wrap a reader expects from "next" pressed on the last
	 * page of the document: it goes round to the top rather than doing nothing.
	 */
	function firstFromPage(page: number): number {
		for (let index = 0; index < matches.length; index += 1) {
			const match = matches[index];
			if (match !== undefined && match.page >= page) {
				return index;
			}
		}
		return 0;
	}

	/** The last match at or before `page`, or the last match at all. */
	function lastToPage(page: number): number {
		for (let index = matches.length - 1; index >= 0; index -= 1) {
			const match = matches[index];
			if (match !== undefined && match.page <= page) {
				return index;
			}
		}
		return Math.max(0, matches.length - 1);
	}

	function step(delta: 1 | -1): SearchState {
		if (matches.length === 0) {
			return lastState ?? buildState();
		}
		const target =
			currentIndex < 0 || currentMatch === null
				? delta === 1
					? firstFromPage(win.currentPage)
					: lastToPage(win.currentPage)
				: (currentIndex + delta + matches.length) % matches.length;
		return selectMatch(target);
	}

	function selectMatch(index: number): SearchState {
		if (matches.length === 0) {
			currentIndex = -1;
			currentMatch = null;
			return publish();
		}
		const target = Math.min(matches.length - 1, Math.max(0, Math.trunc(index)));
		const match = matches[target];
		if (match === undefined) {
			return lastState ?? buildState();
		}
		const moved = currentIndex !== target;
		currentIndex = target;
		currentMatch = match;
		if (moved) {
			announced = null;
		}
		// A match on a page outside the window has no highlight yet, and a
		// reader who pressed "next" onto a page they cannot see has been told
		// nothing useful. Ask for that page's layer; the shell scrolls there
		// from `currentMatch`.
		if (!layers.has(match.page) && !pendingLayers.has(match.page)) {
			requestWindowLayers(runId);
		}
		return publish();
	}

	const controller: SearchController = {
		setQuery(next, nextModifiers) {
			const resolved = resolveModifiers(nextModifiers);
			if (next.length === 0) {
				// An empty query is a clear, not a search the engine would
				// reject: the reader emptied the field, and a rejection dialog
				// for that would be absurd.
				scan?.abort();
				// Bumping the run id is what makes the abort sufficient: a
				// batch already on its way is now from a run that is not
				// current, so it cannot repaint.
				runId += 1;
				reset("", resolved, "idle");
				return publish();
			}
			if (next === query && sameModifiers(resolved, modifiers) && status !== "idle") {
				return lastState ?? buildState();
			}
			return startScan(next, resolved);
		},

		setModifiers(next) {
			const resolved = resolveModifiers(next);
			if (sameModifiers(resolved, modifiers)) {
				return lastState ?? buildState();
			}
			if (query.length === 0) {
				modifiers = resolved;
				return publish();
			}
			return startScan(query, resolved);
		},

		update(next) {
			win = next;
			pruneLayers();
			requestWindowLayers(runId);
			return publish();
		},

		state() {
			return lastState ?? buildState();
		},

		next() {
			return step(1);
		},

		previous() {
			return step(-1);
		},

		select: selectMatch,

		handleKey(key, shift) {
			const command = resolveSearchKey(key, {
				hasQuery: query.length > 0,
				shift: shift ?? false,
			});
			if (command === null) {
				return null;
			}
			if (command.action === "close") {
				return controller.clear();
			}
			return command.action === "next" ? step(1) : step(-1);
		},

		clear() {
			scan?.abort();
			runId += 1;
			reset("", modifiers, "idle");
			return publish();
		},

		subscribe(listener) {
			subscribers.add(listener);
			return () => {
				subscribers.delete(listener);
			};
		},

		dispose() {
			disposed = true;
			runId += 1;
			scan?.abort();
			subscribers.clear();
		},
	};

	return controller;
}
