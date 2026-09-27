/**
 * Windowing tests (SL-4.UI.02). The DoD's "60 fps sustained scroll on a
 * 2 000-page document" is, mechanically, the claim that the window stays small
 * and is found without a scan — that is what these assert.
 */

import { describe, expect, it } from "vitest";
import type { LayoutRequest } from "./layout.js";
import { layoutPages } from "./layout.js";
import type { PageWindow } from "./windowing.js";
import {
	MAX_WINDOW_ROWS,
	clampScrollTop,
	prefetchRows,
	scrollTopForPage,
	visibleWindow,
} from "./windowing.js";

const LETTER = { width: 612, height: 792 } as const;

function layout(pageCount: number, overrides: Partial<LayoutRequest> = {}) {
	return layoutPages({
		pageSizes: Array.from({ length: pageCount }, () => LETTER),
		viewportWidth: 1000,
		viewportHeight: 800,
		mode: "page",
		zoom: 1,
		gap: 16,
		padding: 16,
		...overrides,
	});
}

const pageOf = (win: PageWindow): number[] => win.pages.map((page) => page.page);

describe("visible window", () => {
	it("covers the viewport and nothing beyond it without overscan", () => {
		const doc = layout(50);
		const win = visibleWindow({ layout: doc, scrollTop: 0, viewportHeight: 800, overscan: 0 });
		// The viewport band is edge-inclusive: page 0 fills it, and page 1's top
		// edge lands exactly on the bottom edge, so it counts as visible. Page 2
		// is one row further down and must not.
		expect(pageOf(win)).toEqual([0, 1]);
		expect(win.firstRow).toBe(0);
		expect(win.clamped).toBe(false);
	});

	it("extends by the overscan band so a fast scroll finds a painted neighbour", () => {
		const doc = layout(50);
		const tight = visibleWindow({ layout: doc, scrollTop: 0, viewportHeight: 800, overscan: 0 });
		const loose = visibleWindow({ layout: doc, scrollTop: 0, viewportHeight: 800, overscan: 2000 });
		expect(loose.pages.length).toBeGreaterThan(tight.pages.length);
		// Still bounded by the cap, however generous the band.
		expect(loose.rows.length).toBeLessThanOrEqual(MAX_WINDOW_ROWS);
	});

	it("stays small on a 2 000-page document at every scroll position", () => {
		const doc = layout(2000);
		for (let step = 0; step < 40; step += 1) {
			const scrollTop = (doc.contentHeight / 40) * step;
			const win = visibleWindow({ layout: doc, scrollTop, viewportHeight: 800 });
			// A 768 px page, an 800 px viewport, 400 px of overscan: a handful.
			expect(win.rows.length).toBeLessThanOrEqual(8);
			expect(win.pages.length).toBeLessThanOrEqual(8);
		}
	});

	it("clamps a pathological window and says so", () => {
		// A viewport tall enough to intersect far more rows than the cap allows.
		const doc = layout(2000);
		const win = visibleWindow({ layout: doc, scrollTop: 0, viewportHeight: 6000, maxRows: 3 });
		expect(win.rows).toHaveLength(3);
		expect(win.clamped).toBe(true);
		expect(win.lastRow).toBe(2);
	});

	it("is empty and harmless for an empty document", () => {
		const doc = layout(0);
		const win = visibleWindow({ layout: doc, scrollTop: 0, viewportHeight: 800 });
		expect(win.firstRow).toBe(-1);
		expect(win.lastRow).toBe(-1);
		expect(win.pages).toEqual([]);
	});

	it("slides the window as the reader scrolls, in document order", () => {
		const doc = layout(2000);
		const first = visibleWindow({ layout: doc, scrollTop: 0, viewportHeight: 800, overscan: 0 });
		const later = visibleWindow({
			layout: doc,
			scrollTop: 5000,
			viewportHeight: 800,
			overscan: 0,
		});
		expect(later.firstRow).toBeGreaterThan(first.firstRow);
		const numbers = pageOf(later);
		expect(numbers).toEqual([...numbers].sort((a, b) => a - b));
		expect(later.pages.every((page) => page.row >= later.firstRow)).toBe(true);
	});
});

describe("prefetch", () => {
	it("names the rows immediately after the window", () => {
		const doc = layout(50);
		const win = visibleWindow({ layout: doc, scrollTop: 0, viewportHeight: 800, overscan: 0 });
		const ahead = prefetchRows(doc, win, 2);
		expect(ahead).toHaveLength(2);
		expect(ahead[0]?.row).toBe(win.lastRow + 1);
		expect(ahead[1]?.row).toBe(win.lastRow + 2);
	});

	it("returns nothing at the end of the document", () => {
		const doc = layout(4);
		const win = visibleWindow({
			layout: doc,
			scrollTop: doc.contentHeight,
			viewportHeight: 800,
			overscan: 0,
		});
		expect(win.lastRow).toBe(doc.rows.length - 1);
		expect(prefetchRows(doc, win, 2)).toEqual([]);
		expect(prefetchRows(doc, win, 0)).toEqual([]);
	});
});

describe("scroll targets", () => {
	const doc = layout(20);

	it("puts a page's top at the top of the viewport", () => {
		const target = scrollTopForPage(doc, 5, 800);
		const page = doc.pages[5];
		expect(target).toBeCloseTo(page?.y ?? -1, 6);
	});

	it("centres a page on request", () => {
		const page = doc.pages[7];
		const target = scrollTopForPage(doc, 7, 800, "centre");
		expect(target).toBeCloseTo((page?.y ?? 0) - (800 - (page?.height ?? 0)) / 2, 6);
	});

	it("clamps to the document, leaving the leading padding visible", () => {
		// Page 0's top edge sits one padding step down, so "page 1 at the top of
		// the viewport" is 16, not 0 — the reader still sees it centred in the
		// document area.
		expect(scrollTopForPage(doc, 0, 800)).toBe(16);
		expect(scrollTopForPage(doc, 19, 800)).toBeCloseTo(doc.contentHeight - 800, 6);
		// An unknown page is a no-op, not a crash.
		expect(scrollTopForPage(doc, 999, 800)).toBe(0);
		expect(clampScrollTop(doc, -50, 800)).toBe(0);
		expect(clampScrollTop(doc, 1e9, 800)).toBeCloseTo(doc.contentHeight - 800, 6);
		expect(clampScrollTop(doc, Number.NaN, 800)).toBe(0);
	});
});
