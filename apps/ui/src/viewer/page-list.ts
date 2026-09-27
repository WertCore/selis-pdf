/**
 * The page-list controller (SL-4.UI.02).
 *
 * This is the primitive UI.03/UI.04/UI.05/UI.06 and the extension's viewer
 * (EXT.06) consume: one object that owns the fit mode, the zoom, the scroll
 * offset, the window, the tile scheduler and the current page, and that hands
 * the shell a plain, immutable {@link PageListState} describing exactly what
 * to paint. It renders nothing itself — that is the shell's job, and it is why
 * this package is testable without a DOM (the repo ships no jsdom, by
 * ADR-P0021's zero-dependency rule) and reusable verbatim by every host.
 *
 * Seams, kept where the plan puts them:
 * - **Host seam** — the engine arrives as an injected `PlatformAdapter`; no
 *   globals, no sniffing (SL-4.UI.01, enforced by the `platform-globals` test).
 * - **Host-specific input** — viewport metrics (size, scroll offset, device
 *   pixel ratio) and the two density-derived pixel values are *passed in*, not
 *   measured. Measuring needs `getBoundingClientRect` and `devicePixelRatio`,
 *   which are host facts; the shell reads them and hands them over. That keeps
 *   this file pure geometry plus one async boundary.
 * - **Logic seam** — no app state lives here beyond what the page list owns
 *   (current page, zoom, mode). The viewmodel (ADR-P0035) publishes diffs
 *   above this; the shell maps `onState` into them.
 *
 * The DoD's two clauses map to specific guarantees here, each asserted in
 * `page-list.test.ts`:
 * - *"60 fps sustained scroll on a 2 000-page document"* — the window is
 *   bounded by rows (see `windowing.ts`), the tile ladder is bounded per
 *   frame, and a relayout is memoised so scrolling never triggers one.
 * - *"No layout shift when a tile resolves"* — the only field a tile
 *   resolution changes is `stage`, which no layout input reads, and a page's
 *   box is identical across all three rungs of the ladder.
 */

import type { PlatformAdapter } from "../platform/adapter.js";
import type { AdapterError } from "../platform/errors.js";
import type { DocHandle } from "../platform/types.js";
import { anchorScrollTop, captureAnchor, proportionalScrollTop } from "./anchoring.js";
import type { PageNavigationCommand } from "./keyboard.js";
import { announcement, pageLabel, resolvePageKey, stepFor } from "./keyboard.js";
import type { PageFitMode, PageLayout, PlacedPage } from "./layout.js";
import { layoutPages, pageAtScrollTop } from "./layout.js";
import type { TileEntry, TileStage } from "./tile-ladder.js";
import { TileScheduler, planLadder } from "./tile-ladder.js";
import type { PageWindow } from "./windowing.js";
import { prefetchRows, scrollTopForPage, visibleWindow } from "./windowing.js";

/** What the shell measures and hands in. All host-specific, none guessed. */
export interface PageListViewport {
	/** Scroll viewport width in CSS pixels. */
	readonly width: number;
	/** Scroll viewport height in CSS pixels. */
	readonly height: number;
	/** Current scroll offset in CSS pixels. */
	readonly scrollTop: number;
	/** Device pixels per CSS pixel. */
	readonly devicePixelRatio: number;
}

/**
 * Pixel values the shell reads from the ui-kit density tokens. They are passed
 * in rather than imported because they are CSS lengths, resolved at runtime
 * from `getComputedStyle` — the same host fact as the viewport metrics.
 */
export interface PageListMetrics {
	/** `--selis-density-page-gap`. */
	readonly pageGap: number;
	/** `--selis-space-6`, the document area's padding. */
	readonly padding: number;
}

/** The default metrics match the ui-kit comfortable-density token values. */
export const DEFAULT_METRICS: PageListMetrics = { pageGap: 16, padding: 16 };

/** Prefetch rows beyond the window: enough to hide a page turn. */
const DEFAULT_PREFETCH_ROWS = 2;

/** One page tile to paint, with its ladder rung. */
export interface PageTileView {
	readonly page: number;
	readonly row: number;
	readonly column: number;
	/** List-space box, CSS pixels. Identical for every rung of the ladder. */
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
	/** Which rung is available now. */
	readonly stage: TileStage;
	/** CSS pixels per PDF point (the layout scale). */
	readonly scale: number;
	/** Device pixels per PDF point the current rung was rendered at. */
	readonly deviceScale: number;
	/** `.selis-*` class list; never a hard-coded colour or size. */
	readonly className: string;
	/** `"Page N of M"`. */
	readonly label: string;
	/** Roving tabindex: the current page is the one in the tab order. */
	readonly tabIndex: 0 | -1;
	readonly current: boolean;
}

/** Everything the shell paints, in one immutable value. */
export interface PageListState {
	readonly docId: string;
	readonly pageCount: number;
	readonly mode: PageFitMode;
	readonly zoom: number;
	/** Effective CSS pixels per PDF point. */
	readonly scale: number;
	/** 0-based current page (the topmost page in view). */
	readonly currentPage: number;
	readonly contentWidth: number;
	readonly contentHeight: number;
	readonly scrollTop: number;
	readonly viewportHeight: number;
	readonly tiles: readonly PageTileView[];
	/** Polite live-region text. */
	readonly announcement: string;
	/** `aria-label` for the scrolling region that contains the list. */
	readonly regionLabel: string;
	/** `aria-label` for the page list itself. */
	readonly listLabel: string;
	/** True when the window hit its row cap. */
	readonly windowClamped: boolean;
}

export interface PageListOptions {
	readonly adapter: PlatformAdapter;
	readonly doc: DocHandle;
	readonly mode?: PageFitMode;
	readonly zoom?: number;
	readonly metrics?: Partial<PageListMetrics>;
	/** Pixels of context either side of the viewport; see `windowing.ts`. */
	readonly overscanPx?: number;
	/** Rows of low-res prefetch beyond the window. */
	readonly prefetchRows?: number;
	/** Engine tasks submitted per frame. */
	readonly frameBudget?: number;
	/** Called on every state change (including tile resolutions). */
	readonly onState?: (state: PageListState) => void;
	/** Called for a tile that failed for a reason other than cancellation. */
	readonly onError?: (error: AdapterError) => void;
}

/** The controller surface the shells program against. */
export interface PageList {
	/** Report new viewport metrics (from a resize, scroll, or DPR change). */
	update(viewport: PageListViewport): PageListState;
	/** The last state; identical to the one `onState` last received. */
	state(): PageListState;
	/**
	 * Change fit mode. Returns the scroll offset that preserves the reader's
	 * position (anchoring) — the shell must apply it.
	 */
	setMode(mode: PageFitMode): number;
	/** Change zoom. Same contract as {@link PageList.setMode}. */
	setZoom(zoom: number): number;
	/** Scroll offset that puts `page` at the top (or centred). */
	scrollTopFor(page: number, align?: "start" | "centre"): number;
	/** Move the current page, returning the state to apply. */
	goToPage(page: number, align?: "start" | "centre"): PageListState;
	/** Apply a key press; `null` when the key is not the page list's. */
	handleKey(key: string): PageListState | null;
	/** The cached tile for a page, for the compositor (UI.03). */
	tileAt(page: number): TileEntry | null;
	/** The layout the current state was built from (UI.06 navigation). */
	layout(): PageLayout;
	/** Abort in-flight renders and release the cache. */
	dispose(): void;
}

/**
 * Create the controller. Nothing is measured, fetched, or scheduled here: the
 * first `update()` from the shell supplies the viewport, and only then does any
 * engine work start.
 */
export function createPageList(options: PageListOptions): PageList {
	const { adapter, doc } = options;
	const metrics: PageListMetrics = {
		pageGap: options.metrics?.pageGap ?? DEFAULT_METRICS.pageGap,
		padding: options.metrics?.padding ?? DEFAULT_METRICS.padding,
	};
	let mode: PageFitMode = options.mode ?? "page";
	let zoom: number = options.zoom ?? 1;
	let viewport: PageListViewport = { width: 0, height: 0, scrollTop: 0, devicePixelRatio: 1 };
	let currentPage = 0;
	let lastState: PageListState | null = null;
	let cachedLayout: PageLayout | null = null;
	let layoutKey = "";
	let disposed = false;

	const scheduler = new TileScheduler({
		engine: adapter.engine,
		doc,
		// A tile landing is a state change like any other: the shell repaints the
		// same boxes with a better rung, which is why nothing shifts.
		onTile: () => {
			publish(false);
		},
		onError: (error) => {
			options.onError?.(error);
		},
	});

	/**
	 * Relayout only when the inputs change. Scrolling must never land here —
	 * that is the difference between 60 fps and a stutter on a 2 000-page
	 * document — so the key covers everything `layoutPages` reads and nothing
	 * else.
	 */
	function relayout(): PageLayout {
		const key = [
			doc.id,
			mode,
			zoom,
			Math.round(viewport.width),
			Math.round(viewport.height),
			metrics.pageGap,
			metrics.padding,
		].join("|");
		if (cachedLayout === null || key !== layoutKey) {
			layoutKey = key;
			cachedLayout = layoutPages({
				pageSizes: doc.pageSizes,
				viewportWidth: viewport.width,
				viewportHeight: viewport.height,
				mode,
				zoom,
				gap: metrics.pageGap,
				padding: metrics.padding,
			});
		}
		return cachedLayout;
	}

	/** The window for the current scroll position, at the configured overscan. */
	function windowFor(): PageWindow {
		return visibleWindow({
			layout: relayout(),
			scrollTop: viewport.scrollTop,
			viewportHeight: viewport.height,
			// `exactOptionalPropertyTypes`: an unset option is omitted, never
			// passed as `undefined`.
			...(options.overscanPx === undefined ? {} : { overscan: options.overscanPx }),
		});
	}

	/** The window with no overscan: the pages the reader can actually see. */
	function visiblePages(): PageWindow {
		return visibleWindow({
			layout: relayout(),
			scrollTop: viewport.scrollTop,
			viewportHeight: viewport.height,
			overscan: 0,
		});
	}

	function tileView(entry: PlacedPage, current: number): PageTileView {
		const full = scheduler.get(entry.page, "full");
		const low = full === null ? scheduler.get(entry.page, "lowres") : null;
		const stage: TileStage = full !== null ? "full" : low !== null ? "lowres" : "placeholder";
		const deviceScale = full?.deviceScale ?? low?.deviceScale ?? 0;
		const isCurrent = entry.page === current;
		const classes = [
			"selis-page-tile",
			"selis-page-tile--placed",
			stage === "placeholder" ? "selis-page-tile--loading" : "",
			stage === "lowres" ? "selis-page-tile--lowres" : "",
			isCurrent ? "selis-page-tile--current" : "",
		]
			.filter((name) => name.length > 0)
			.join(" ");
		return {
			page: entry.page,
			row: entry.row,
			column: entry.column,
			x: entry.x,
			y: entry.y,
			width: entry.width,
			height: entry.height,
			stage,
			scale: entry.scale,
			deviceScale,
			className: classes,
			label: pageLabel(entry.page, doc.pageCount),
			// Roving tabindex: exactly one page is in the tab order at a time.
			tabIndex: isCurrent ? 0 : -1,
			current: isCurrent,
		};
	}

	function buildState(): PageListState {
		const layout = relayout();
		const top = pageAtScrollTop(layout, viewport.scrollTop);
		if (top !== null) {
			currentPage = top.page;
		}
		// At the very bottom of the document the last page is what the reader is
		// looking at, even though its top edge is above the fold only by less
		// than the trailing padding. Without this, "go to the last page" (End,
		// or a deep link from UI.06) would report the second-to-last page.
		const atEnd =
			layout.contentHeight - viewport.height - viewport.scrollTop <= 1 && layout.pages.length > 0;
		if (atEnd) {
			currentPage = layout.pages.length - 1;
		}
		const win = windowFor();
		const tiles = win.pages.map((entry) => tileView(entry, currentPage));
		return {
			docId: doc.id,
			pageCount: doc.pageCount,
			mode,
			zoom,
			scale: layout.scale,
			currentPage,
			contentWidth: layout.contentWidth,
			contentHeight: layout.contentHeight,
			scrollTop: viewport.scrollTop,
			viewportHeight: viewport.height,
			tiles,
			announcement: announcement(currentPage, doc.pageCount, mode),
			regionLabel: "Document pages",
			listLabel: "Pages",
			windowClamped: win.clamped,
		};
	}

	/**
	 * One frame: build the state, hand it to the shell, and — unless this is a
	 * tile landing, which changes nothing about what is *wanted* — plan and
	 * submit the engine work for the pages now on screen.
	 */
	function publish(withLadder: boolean): PageListState {
		const state = buildState();
		if (disposed) {
			return state;
		}
		lastState = state;
		options.onState?.(state);
		if (withLadder) {
			scheduleLadder();
		}
		return state;
	}

	function scheduleLadder(): void {
		const layout = relayout();
		const win = windowFor();
		const visible = new Set(visiblePages().pages.map((entry) => entry.page));
		const prefetch = prefetchRows(
			layout,
			win,
			options.prefetchRows ?? DEFAULT_PREFETCH_ROWS,
		).flatMap((row) => layout.pages.filter((entry) => entry.row === row.row));
		const tasks = planLadder({
			doc,
			windowPages: win.pages,
			visiblePages: visible,
			prefetchPages: prefetch,
			devicePixelRatio: viewport.devicePixelRatio,
			cssScale: layout.scale,
			satisfied: scheduler.satisfiedKeys(),
			...(options.frameBudget === undefined ? {} : { budget: options.frameBudget }),
		});
		scheduler.submit(tasks);
	}

	/**
	 * Clamp a page number into the document. UI.06 navigation and a deep link can
	 * both name a page that is not there (a stale bookmark, a changed document);
	 * clamping to the nearest real page is what a reader expects, and it keeps
	 * `scrollTopForPage` from silently resolving to the top of the document.
	 */
	function clampPage(page: number): number {
		const last = doc.pageCount - 1;
		if (!Number.isFinite(page)) {
			return 0;
		}
		return Math.min(last, Math.max(0, Math.round(page)));
	}

	/**
	 * Apply a relayout-triggering change (fit mode or zoom) and keep the reader
	 * where they were. The returned offset must be applied to the scroll
	 * container; the state published through `onState` already assumes it, so a
	 * shell that ignores the number will see the pages jump. That is the
	 * contract the plan's "correct scroll anchoring on zoom" reduces to.
	 */
	function relayoutTo(next: { mode?: PageFitMode; zoom?: number }): number {
		const previous = relayout();
		const anchor = captureAnchor(previous, viewport.scrollTop);
		if (next.mode !== undefined) {
			mode = next.mode;
		}
		if (next.zoom !== undefined) {
			zoom = next.zoom;
		}
		const layout = relayout();
		const target =
			anchorScrollTop(anchor, layout, viewport.height) ??
			proportionalScrollTop(previous, layout, viewport.scrollTop, viewport.height);
		// Every cached tile was rasterised at the old scale and every in-flight
		// one is being asked for the old geometry: both are now waste.
		scheduler.cancelAll();
		scheduler.clearCache();
		viewport = { ...viewport, scrollTop: target };
		publish(true);
		return target;
	}

	const list: PageList = {
		update(next: PageListViewport) {
			viewport = next;
			return publish(true);
		},

		state() {
			return lastState ?? buildState();
		},

		setMode(next: PageFitMode) {
			return relayoutTo({ mode: next });
		},

		setZoom(next: number) {
			if (!Number.isFinite(next) || next <= 0) {
				// A toolbar can emit 0 while the user is mid-drag; the layout
				// clamps anyway, but not changing zoom at all is saner.
				return viewport.scrollTop;
			}
			return relayoutTo({ zoom: next });
		},

		scrollTopFor(page: number, align: "start" | "centre" = "start") {
			return scrollTopForPage(relayout(), clampPage(page), viewport.height, align);
		},

		goToPage(page: number, align: "start" | "centre" = "start") {
			const target = scrollTopForPage(relayout(), clampPage(page), viewport.height, align);
			viewport = { ...viewport, scrollTop: target };
			return publish(true);
		},

		handleKey(key: string) {
			const command: PageNavigationCommand | null = resolvePageKey(key, {
				currentPage,
				pageCount: doc.pageCount,
				mode,
				pagesPerView: visiblePages().pages.length,
				step: stepFor(mode),
			});
			if (command === null) {
				return null;
			}
			return list.goToPage(command.page, command.align);
		},

		tileAt(page: number) {
			return scheduler.bestFor(page);
		},

		layout() {
			return relayout();
		},

		dispose() {
			disposed = true;
			scheduler.dispose();
		},
	};

	// Publish the empty initial state so a shell can paint before the first
	// viewport report; no engine work is scheduled from it.
	publish(false);
	return list;
}
