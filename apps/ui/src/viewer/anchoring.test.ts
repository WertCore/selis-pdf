/**
 * Scroll-anchoring tests (SL-4.UI.02, "correct scroll anchoring on zoom").
 *
 * The scenario is always the same one a reader would recognise: get deep into
 * a long document, change the zoom or the fit mode, and stay exactly where you
 * were looking.
 */

import { describe, expect, it } from "vitest";
import { anchorScrollTop, captureAnchor, proportionalScrollTop } from "./anchoring.js";
import type { LayoutRequest } from "./layout.js";
import { layoutPages, rowIndexAt } from "./layout.js";

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

describe("capture", () => {
	it("names the topmost page and how far into it the reader is", () => {
		const doc = layout(100);
		const page = doc.pages[10];
		const scrollTop = (page?.y ?? 0) + (page?.height ?? 0) / 4;
		const anchor = captureAnchor(doc, scrollTop);
		expect(anchor?.page).toBe(10);
		expect(anchor?.fraction).toBeCloseTo(0.25, 6);
		expect(anchor?.offsetPx).toBeCloseTo((page?.height ?? 0) / 4, 6);
	});

	it("clamps the fraction to the page and tolerates a hostile offset", () => {
		const doc = layout(10);
		// Above the first page: the very top of page 0.
		expect(captureAnchor(doc, -500)?.fraction).toBe(0);
		// Past the end: the last page, fully past.
		expect(captureAnchor(doc, 1e9)?.page).toBe(doc.pages.length - 1);
		expect(captureAnchor(doc, 1e9)?.fraction).toBe(1);
		expect(captureAnchor(doc, Number.NaN)).not.toBeNull();
	});

	it("has nothing to anchor to in an empty document", () => {
		expect(captureAnchor(layout(0), 0)).toBeNull();
		expect(anchorScrollTop(null, layout(10), 800)).toBeNull();
	});
});

describe("restore", () => {
	it("keeps the reader's place in the same page when zooming in", () => {
		const before = layout(500);
		const page = before.pages[300];
		const scrollTop = (page?.y ?? 0) + (page?.height ?? 0) / 2;
		const anchor = captureAnchor(before, scrollTop);
		const after = layout(500, { zoom: 2.5 });
		const restored = anchorScrollTop(anchor, after, 800);
		expect(restored).not.toBeNull();
		// The same half of the same page is at the top of the viewport.
		const newPage = after.pages[300];
		expect((restored ?? 0) - (newPage?.y ?? 0)).toBeCloseTo((newPage?.height ?? 0) / 2, 6);
	});

	it("keeps the reader's page across a fit-mode change that re-pairs every row", () => {
		const before = layout(200, { mode: "width" });
		const page = before.pages[120];
		const scrollTop = (page?.y ?? 0) + 10;
		const anchor = captureAnchor(before, scrollTop);
		const after = layout(200, { mode: "spread" });
		const restored = anchorScrollTop(anchor, after, 800);
		const newPage = after.pages[120];
		expect(restored).toBeGreaterThanOrEqual(newPage?.y ?? 0);
		expect(restored).toBeLessThan((newPage?.y ?? 0) + (newPage?.height ?? 0));
		// Spread re-pairs the pages: 120 now shares a row with 121, and the row
		// that owns the restored offset starts at 120 — so the current page, and
		// with it `aria-current`, has not moved.
		const row = after.rows[rowIndexAt(after, restored ?? 0)];
		expect(row?.firstPage).toBe(120);
	});

	it("clamps to the document when the anchor lands past the end", () => {
		const before = layout(20);
		const anchor = captureAnchor(before, (before.pages[19]?.y ?? 0) + 5);
		// Zoom out hard: the anchor page is now much closer to the end.
		const after = layout(20, { zoom: 0.25 });
		const restored = anchorScrollTop(anchor, after, 800) ?? -1;
		expect(restored).toBeGreaterThanOrEqual(0);
		expect(restored).toBeLessThanOrEqual(after.contentHeight);
	});

	it("returns null when the anchor page is gone, so the caller can fall back", () => {
		const before = layout(20);
		const anchor = captureAnchor(before, before.pages[10]?.y ?? 0);
		expect(anchor?.page).toBe(10);
		expect(anchorScrollTop(anchor, layout(5), 800)).toBeNull();
	});
});

describe("proportional fallback", () => {
	it("preserves the distance into the document when there is no anchor", () => {
		const before = layout(100);
		const after = layout(100, { zoom: 2 });
		const mid = before.contentHeight / 2;
		const restored = proportionalScrollTop(before, after, mid, 800);
		expect(restored / after.contentHeight).toBeCloseTo(mid / before.contentHeight, 6);
	});

	it("is a no-op for two empty documents", () => {
		expect(proportionalScrollTop(layout(0), layout(0), 0, 800)).toBe(0);
	});
});
