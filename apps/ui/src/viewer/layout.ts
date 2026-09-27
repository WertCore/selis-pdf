/**
 * Page geometry for the continuous page list (SL-4.UI.02).
 *
 * This is the whole of UI.02's *layout* brain and it is deliberately pure:
 * given page sizes (points, from `DocHandle.pageSizes`), a viewport in CSS
 * pixels, a fit mode, a zoom multiplier and the two pixel values the shell
 * reads out of the ui-kit density tokens, it returns every page's box plus a
 * row index for windowing.
 *
 * Two properties are load-bearing, and `layout.test.ts` asserts both:
 *
 * 1. **The layout is a function of the document and the viewport only.** It
 *    knows nothing about tiles, so a tile resolving (placeholder → low-res →
 *    full-res, UI.02's ladder) cannot move a box. That is the structural
 *    answer to the DoD's "no layout shift when a tile resolves" — there is no
 *    path by which tile state could reach this module.
 * 2. **It is O(pages) with a small constant**, so a 2 000-page document
 *    relayouts well inside a frame. The controller memoises the result and only
 *    recomputes when the key changes (`page-list.ts`); the per-scroll work is
 *    the binary search in `windowing.ts`, never a relayout.
 *
 * Fit modes (the DoD's "page-fit/width/spread"):
 * - `"page"`   — one page centred, whole page visible (fit-height × fit-width).
 * - `"width"`  — pages as wide as the viewport, height follows.
 * - `"spread"` — facing pages, two per row. An odd page count puts the cover
 *   page alone on the first row and pairs the rest, which is what a reader
 *   expects from a duplex-printed document.
 *
 * `zoom` is a multiplier *on top of* the fitted scale, so "fit width, then
 * zoom in 20%" is `mode: "width", zoom: 1.2` — the same state the toolbar's
 * zoom control publishes.
 */

import type { Size } from "../platform/types.js";

/** How pages are fitted to the viewport. */
export type PageFitMode = "page" | "width" | "spread";

/** Everything `layoutPages` needs. All lengths are CSS pixels except sizes. */
export interface LayoutRequest {
	/** Per-page media sizes in points, index-aligned with page numbers. */
	readonly pageSizes: readonly Size[];
	/** Scroll viewport size in CSS pixels. */
	readonly viewportWidth: number;
	readonly viewportHeight: number;
	readonly mode: PageFitMode;
	/** Multiplier applied on top of the fitted scale (1 = exactly fit). */
	readonly zoom: number;
	/** Gap between rows/columns in CSS pixels (`--selis-density-page-gap`). */
	readonly gap: number;
	/** Padding around the whole list in CSS pixels (`--selis-space-6`). */
	readonly padding: number;
}

/** One page's box in list coordinates (CSS pixels, origin top-left). */
export interface PlacedPage {
	/** 0-based page index. */
	readonly page: number;
	/** Row this page sits in; windowing iterates rows, not pages. */
	readonly row: number;
	/** 0 for a single-page row, 0/1 within a spread pair. */
	readonly column: number;
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
	/** Effective CSS pixels per PDF point for this page. */
	readonly scale: number;
}

/** One row's band. Pages run `firstPage..lastPage` inclusive. */
export interface PlacedRow {
	readonly row: number;
	readonly y: number;
	/** Row height = tallest page in the row. */
	readonly height: number;
	/** Full row width including the inter-column gap. */
	readonly width: number;
	readonly firstPage: number;
	readonly lastPage: number;
}

/** The full document geometry. `pages` is index-aligned with `pageSizes`. */
export interface PageLayout {
	readonly mode: PageFitMode;
	readonly zoom: number;
	/** The scale the fit mode alone asked for. */
	readonly fitScale: number;
	/** `fitScale × zoom`, clamped — the scale every page was placed with. */
	readonly scale: number;
	/** Scrollable content size, including padding. */
	readonly contentWidth: number;
	readonly contentHeight: number;
	readonly rows: readonly PlacedRow[];
	readonly pages: readonly PlacedPage[];
}

/**
 * Clamp bounds on the effective scale. A 2 000-page A4 document at `zoom: 64`
 * would place a page 44 000 px wide and push the scroll geometry toward the
 * edge of float precision; the engine's pixel budget would reject the tile
 * anyway. The range is generous (0.05×–16× of fitted) and only helps the
 * 60 fps budget by keeping the numbers small.
 */
export const MIN_SCALE = 0.05;
export const MAX_SCALE = 16;

function clamp(value: number, low: number, high: number): number {
	return Math.min(high, Math.max(low, value));
}

/** Column count for a mode (spread is two-up; the rest are one-up). */
export function columnsFor(mode: PageFitMode): 1 | 2 {
	return mode === "spread" ? 2 : 1;
}

/**
 * Row composition: which pages share a row. Separated from the geometry so the
 * spread rule is stated once and is testable on its own.
 */
export function planRows(pageCount: number, mode: PageFitMode): readonly (readonly number[])[] {
	if (pageCount <= 0) {
		return [];
	}
	if (mode !== "spread") {
		return Array.from({ length: pageCount }, (_, page) => [page]);
	}
	const rows: number[][] = [];
	let next = 0;
	// An odd page count in spread mode: the cover page reads better alone than
	// paired with a blank verso, so it takes the first row to itself.
	if (pageCount % 2 === 1) {
		rows.push([next]);
		next += 1;
	}
	while (next < pageCount) {
		rows.push([next, next + 1]);
		next += 2;
	}
	return rows;
}

/** Lay the whole document out. Pure, and the only place boxes are created. */
export function layoutPages(request: LayoutRequest): PageLayout {
	const { pageSizes, viewportWidth, mode, zoom } = request;
	// Negative metrics would make boxes overlap; normalise the inputs a shell
	// could plausibly get wrong (a zero-height viewport mid-resize).
	const gap = Math.max(0, request.gap);
	const padding = Math.max(0, request.padding);
	const columns = columnsFor(mode);
	const columnGap = columns - 1 === 1 ? gap : 0;
	const viewportHeight = Math.max(0, request.viewportHeight);
	const availableWidth = Math.max(1, viewportWidth - 2 * padding - columnGap);
	const rowsPlan = planRows(pageSizes.length, mode);

	// Natural extents in points: the widest row and the tallest row. Fitting
	// uses the *row* width so a spread pair fits as a unit, rather than each
	// page fitting independently and overflowing the viewport.
	let widestRow = 0;
	let tallestRow = 0;
	for (const rowPages of rowsPlan) {
		let rowWidth = 0;
		let rowHeight = 0;
		for (const page of rowPages) {
			const size = pageSizes[page];
			if (size === undefined) {
				continue;
			}
			rowWidth += size.width;
			rowHeight = Math.max(rowHeight, size.height);
		}
		widestRow = Math.max(widestRow, rowWidth);
		tallestRow = Math.max(tallestRow, rowHeight);
	}
	widestRow = widestRow === 0 ? 1 : widestRow;
	tallestRow = tallestRow === 0 ? 1 : tallestRow;

	const widthScale = availableWidth / widestRow;
	const pageScale = Math.min(widthScale, Math.max(1, viewportHeight - 2 * padding) / tallestRow);
	const fitScale = mode === "width" ? widthScale : pageScale;
	const scale = clamp(
		fitScale * (Number.isFinite(zoom) && zoom > 0 ? zoom : 1),
		MIN_SCALE,
		MAX_SCALE,
	);

	// Two passes per row: measure, then place. Centring needs the row width and
	// column 1 needs column 0's box, so boxes are built one row at a time.
	const pages: PlacedPage[] = [];
	const rows: PlacedRow[] = [];
	let widest = 0;
	let y = padding;
	for (const [row, rowPages] of rowsPlan.entries()) {
		const rowTop = y;
		const boxes = rowPages.flatMap((page) => {
			const size = pageSizes[page];
			if (size === undefined) {
				return [];
			}
			return [
				{
					page,
					width: Math.max(1, size.width * scale),
					height: Math.max(1, size.height * scale),
				},
			];
		});
		let rowWidth = 0;
		let rowHeight = 0;
		for (const box of boxes) {
			rowWidth += box.width;
			rowHeight = Math.max(rowHeight, box.height);
		}
		rowWidth += Math.max(0, boxes.length - 1) * gap;
		widest = Math.max(widest, rowWidth);

		// The content width never shrinks below the viewport's available width,
		// so "fit width" has no horizontal scrollbar, while a zoomed-in spread
		// still overflows horizontally — which is what the reader expects.
		const contentWidth = Math.max(widest, availableWidth) + 2 * padding;
		const innerWidth = contentWidth - 2 * padding;
		let x = padding + Math.max(0, (innerWidth - rowWidth) / 2);
		for (const [column, box] of boxes.entries()) {
			pages.push({
				page: box.page,
				row,
				column,
				x,
				y: rowTop,
				width: box.width,
				height: box.height,
				scale,
			});
			x += box.width + gap;
		}

		const firstPage = boxes[0]?.page ?? 0;
		const lastPage = boxes[boxes.length - 1]?.page ?? firstPage;
		rows.push({
			row,
			y: rowTop,
			height: Math.max(1, rowHeight),
			width: rowWidth,
			firstPage,
			lastPage,
		});
		y = rowTop + Math.max(1, rowHeight) + gap;
	}

	const contentWidth = Math.max(widest, availableWidth) + 2 * padding;
	const contentHeight = rows.length === 0 ? 0 : Math.max(0, y - gap) + padding;
	return {
		mode,
		zoom,
		fitScale,
		scale,
		contentWidth,
		contentHeight,
		rows,
		pages,
	};
}

/**
 * Index of the row containing `y`, or the nearest row when `y` falls in a gap
 * or past the end. Binary search: this is the one computation performed on
 * every scroll step, so it must not be linear in the page count.
 */
export function rowIndexAt(layout: PageLayout, y: number): number {
	const { rows } = layout;
	const first = rows[0];
	if (first === undefined) {
		return -1;
	}
	if (y < first.y) {
		return 0;
	}
	// Find the first row whose bottom edge is below `y`. Binary search: this
	// runs on every scroll step, so it must not be linear in the page count.
	let low = 0;
	let high = rows.length - 1;
	let found = rows.length - 1;
	while (low <= high) {
		const mid = (low + high) >> 1;
		const row = rows[mid];
		if (row === undefined) {
			break;
		}
		if (row.y + row.height > y) {
			found = mid;
			high = mid - 1;
		} else {
			low = mid + 1;
		}
	}
	const owner = rows[found];
	// In an inter-row gap the match is the row *below* the gap, but the row
	// the reader is finishing is the one above it. Handing back the row above
	// is what keeps `aria-current` and the page-number field from flickering
	// while a gap scrolls past.
	return owner !== undefined && owner.y > y ? Math.max(0, found - 1) : found;
}

/** The page occupying a list-space point, or the nearest page in that row. */
export function pageAt(layout: PageLayout, x: number, y: number): PlacedPage | null {
	const rowIndex = rowIndexAt(layout, y);
	if (rowIndex < 0) {
		return null;
	}
	let best: PlacedPage | null = null;
	let bestDistance = Number.POSITIVE_INFINITY;
	for (const entry of layout.pages) {
		if (entry.row !== rowIndex) {
			continue;
		}
		const distance = Math.abs(x - (entry.x + entry.width / 2));
		if (distance < bestDistance) {
			best = entry;
			bestDistance = distance;
		}
	}
	return best;
}

/**
 * The topmost page currently in view — the UI.02 definition of "current page".
 * `aria-current`, the page-number field and the keyboard all follow this one
 * page, so it must be a function of the scroll offset alone (no tile state, no
 * render timing) or assistive tech and the toolbar would disagree.
 */
export function pageAtScrollTop(layout: PageLayout, scrollTop: number): PlacedPage | null {
	const rowIndex = rowIndexAt(layout, scrollTop);
	if (rowIndex < 0) {
		return null;
	}
	return layout.pages.find((entry) => entry.row === rowIndex) ?? null;
}
