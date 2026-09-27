/**
 * Windowing: which rows the scroll viewport shows (SL-4.UI.02).
 *
 * The DoD's 60 fps target on a 2 000-page document is, mechanically, this
 * file: only the rows near the viewport may be materialised, and finding them
 * must be a binary search rather than a scan. `page-list.ts` turns the rows
 * returned here into the handful of DOM nodes the shell paints.
 *
 * The overscan band is what makes sustained scrolling look continuous: rows
 * just outside the viewport are kept in the window so a fast scroll paints
 * from cache before the compositor (UI.03) has to wait on a tile. It is a
 * pixel band, not a row count, so a document of tiny pages and a document of
 * posters both get the same *time* of runway.
 */

import type { PageLayout, PlacedPage, PlacedRow } from "./layout.js";
import { rowIndexAt } from "./layout.js";

/** Default overscan band either side of the viewport, in CSS pixels. */
export const DEFAULT_OVERSCAN_PX = 400;

/**
 * Hard cap on rows in one window. A pathological document (a 2 000-page
 * render of a 1 pt page) could otherwise put a whole document in the window;
 * the cap keeps the node count bounded whatever the input, which is the
 * property the 60 fps budget rests on.
 */
export const MAX_WINDOW_ROWS = 64;

export interface WindowRequest {
	readonly layout: PageLayout;
	readonly scrollTop: number;
	readonly viewportHeight: number;
	/** Pixels of extra context above and below; defaults to {@link DEFAULT_OVERSCAN_PX}. */
	readonly overscan?: number;
	/** Row cap; defaults to {@link MAX_WINDOW_ROWS}. */
	readonly maxRows?: number;
}

/** The materialisable slice of the document for one scroll position. */
export interface PageWindow {
	/** Inclusive row range; `-1`/`-1` for an empty document. */
	readonly firstRow: number;
	readonly lastRow: number;
	readonly rows: readonly PlacedRow[];
	/** `rows` flattened into pages, in document order. */
	readonly pages: readonly PlacedPage[];
	/** True when the window was clipped by {@link MAX_WINDOW_ROWS}. */
	readonly clamped: boolean;
}

const EMPTY_WINDOW: PageWindow = {
	firstRow: -1,
	lastRow: -1,
	rows: [],
	pages: [],
	clamped: false,
};

/** Rows intersecting `[scrollTop − overscan, scrollTop + height + overscan]`. */
export function visibleWindow(request: WindowRequest): PageWindow {
	const { layout, scrollTop } = request;
	if (layout.rows.length === 0) {
		return EMPTY_WINDOW;
	}
	const overscan = Math.max(0, request.overscan ?? DEFAULT_OVERSCAN_PX);
	const maxRows = Math.max(1, request.maxRows ?? MAX_WINDOW_ROWS);
	const top = Math.max(0, scrollTop) - overscan;
	const bottom = Math.max(0, scrollTop) + Math.max(0, request.viewportHeight) + overscan;

	const first = rowIndexAt(layout, top);
	const last = rowIndexAt(layout, bottom);
	const start = Math.max(0, Math.min(first, layout.rows.length - 1));
	const end = Math.min(layout.rows.length - 1, Math.max(start, last));
	const clamped = end - start + 1 > maxRows;
	const lastRow = clamped ? start + maxRows - 1 : end;
	const rows = layout.rows.slice(start, lastRow + 1);
	const pages = layout.pages.filter((entry) => entry.row >= start && entry.row <= lastRow);
	return { firstRow: start, lastRow, rows, pages, clamped };
}

/**
 * Rows to prefetch beyond the window — the first rows *after* it, in reading
 * order. The tile ladder uses this to paint low-res previews slightly ahead of
 * the reader, which is the cheap half of the placeholder→low-res→full ladder.
 *
 * The parameter is named `current` rather than `window` because `window` is a
 * platform global: the SL-4.UI.01 globals lint in `../platform` fails the build
 * on a bare reference to one, and the same discipline should hold for a local.
 */
export function prefetchRows(
	layout: PageLayout,
	current: PageWindow,
	count: number,
): readonly PlacedRow[] {
	const first = current.lastRow + 1;
	return layout.rows.slice(first, first + Math.max(0, count));
}

/**
 * Scroll offset that brings a page's top edge to the top of the viewport,
 * clamped to the document. UI.06 navigation and the keyboard both go through
 * here so "go to page" has exactly one definition.
 */
export function scrollTopForPage(
	layout: PageLayout,
	page: number,
	viewportHeight: number,
	align: "start" | "centre" = "start",
): number {
	const entry = layout.pages.find((candidate) => candidate.page === page);
	if (entry === undefined) {
		return 0;
	}
	const raw = align === "centre" ? entry.y - (viewportHeight - entry.height) / 2 : entry.y;
	return clampScrollTop(layout, raw, viewportHeight);
}

/** Clamp a scroll offset into `[0, contentHeight − viewportHeight]`. */
export function clampScrollTop(
	layout: PageLayout,
	scrollTop: number,
	viewportHeight: number,
): number {
	const max = Math.max(0, layout.contentHeight - Math.max(0, viewportHeight));
	return Math.min(max, Math.max(0, Number.isFinite(scrollTop) ? scrollTop : 0));
}
