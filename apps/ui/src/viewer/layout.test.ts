/**
 * Layout tests (SL-4.UI.02). Pure geometry, no DOM, no adapter.
 *
 * The properties the DoD leans on are asserted here rather than only in prose:
 * fit/width/spread produce sane, non-overlapping boxes, and the layout is a
 * pure function of its inputs — which is what makes "no layout shift when a
 * tile resolves" true by construction.
 */

import { describe, expect, it } from "vitest";
import type { Size } from "../platform/types.js";
import type { LayoutRequest, PageFitMode } from "./layout.js";
import {
	MAX_SCALE,
	MIN_SCALE,
	columnsFor,
	layoutPages,
	pageAt,
	pageAtScrollTop,
	planRows,
	rowIndexAt,
} from "./layout.js";

const LETTER: Size = { width: 612, height: 792 };
const A4: Size = { width: 595.28, height: 841.89 };

function sizes(count: number, size: Size = LETTER): Size[] {
	return Array.from({ length: count }, () => size);
}

function request(overrides: Partial<LayoutRequest> = {}): LayoutRequest {
	return {
		pageSizes: sizes(3),
		viewportWidth: 1000,
		viewportHeight: 800,
		mode: "page",
		zoom: 1,
		gap: 16,
		padding: 16,
		...overrides,
	};
}

describe("fit modes", () => {
	it("fit page fills the viewport height and centres the page", () => {
		const layout = layoutPages(request({ pageSizes: sizes(1) }));
		const page = layout.pages[0];
		// 800 − 2×16 padding = 768 px of height to fit into.
		expect(page?.height).toBeCloseTo(768, 6);
		expect(layout.scale).toBeCloseTo(768 / 792, 6);
		expect(layout.contentHeight).toBeCloseTo(800, 6);
		// The leftover width is split evenly either side of the page.
		expect(page?.x).toBeCloseTo(16 + (968 - (page?.width ?? 0)) / 2, 6);
	});

	it("fit width fills the viewport width and scrolls vertically", () => {
		const layout = layoutPages(request({ mode: "width", pageSizes: sizes(3) }));
		const page = layout.pages[0];
		expect(page?.width).toBeCloseTo(968, 6);
		expect(layout.contentHeight).toBeGreaterThan(800);
		// No horizontal scrollbar in fit width: content is exactly the viewport.
		expect(layout.contentWidth).toBeCloseTo(1000, 6);
	});

	it("zoom multiplies the fitted scale and is clamped at both ends", () => {
		const base = layoutPages(request({ pageSizes: sizes(1) }));
		const zoomed = layoutPages(request({ pageSizes: sizes(1), zoom: 2 }));
		expect(zoomed.scale).toBeCloseTo(base.scale * 2, 6);
		expect(layoutPages(request({ zoom: 1e6 })).scale).toBe(MAX_SCALE);
		expect(layoutPages(request({ zoom: 1e-6 })).scale).toBe(MIN_SCALE);
		// A nonsensical zoom is treated as "no zoom", never as a NaN scale.
		expect(layoutPages(request({ zoom: 0 })).scale).toBeCloseTo(base.scale, 6);
		expect(layoutPages(request({ zoom: Number.NaN })).scale).toBeCloseTo(base.scale, 6);
	});

	it("columns follow the mode", () => {
		expect(columnsFor("page")).toBe(1);
		expect(columnsFor("width")).toBe(1);
		expect(columnsFor("spread")).toBe(2);
	});
});

describe("spread rows", () => {
	it("pairs pages two to a row and gaps the columns", () => {
		const layout = layoutPages(request({ mode: "spread", pageSizes: sizes(4) }));
		expect(layout.rows).toHaveLength(2);
		const [left, right] = layout.pages;
		expect(left?.page).toBe(0);
		expect(right?.page).toBe(1);
		expect(right?.x).toBeCloseTo((left?.x ?? 0) + (left?.width ?? 0) + 16, 6);
		// Both halves share the row: same top, and the row fits the viewport.
		expect(right?.y).toBe(left?.y);
		expect(layout.contentWidth).toBeCloseTo(1000, 6);
	});

	it("gives an odd page count a lone cover page", () => {
		expect(planRows(5, "spread")).toEqual([[0], [1, 2], [3, 4]]);
		expect(planRows(4, "spread")).toEqual([
			[0, 1],
			[2, 3],
		]);
		expect(planRows(1, "spread")).toEqual([[0]]);
		expect(planRows(0, "spread")).toEqual([]);
		expect(planRows(3, "page")).toEqual([[0], [1], [2]]);
	});

	it("fits the pair as a unit rather than each page independently", () => {
		const layout = layoutPages(request({ mode: "spread", pageSizes: sizes(2) }));
		const first = layout.pages[0];
		const second = layout.pages[1];
		// (968 − 16 gap) / 2 per column.
		expect(first?.width).toBeCloseTo((968 - 16) / 2, 6);
		expect(second?.width).toBeCloseTo((968 - 16) / 2, 6);
	});
});

describe("geometry invariants", () => {
	const modes: PageFitMode[] = ["page", "width", "spread"];

	for (const mode of modes) {
		it(`${mode}: rows never overlap and are separated by exactly the gap`, () => {
			const layout = layoutPages(
				request({ mode, pageSizes: sizes(12, mode === "width" ? A4 : LETTER) }),
			);
			for (const [index, row] of layout.rows.entries()) {
				if (index === 0) {
					expect(row.y).toBe(16);
					continue;
				}
				const previous = layout.rows[index - 1];
				expect(row.y - (previous?.y ?? 0) - (previous?.height ?? 0)).toBeCloseTo(16, 6);
			}
		});

		it(`${mode}: every page box is positive and index-aligned with pageSizes`, () => {
			const mixed = [...sizes(3), A4, LETTER];
			const layout = layoutPages(request({ mode, pageSizes: mixed }));
			expect(layout.pages).toHaveLength(mixed.length);
			for (const [index, page] of layout.pages.entries()) {
				expect(page.page).toBe(index);
				expect(page.width).toBeGreaterThan(0);
				expect(page.height).toBeGreaterThan(0);
				expect(page.y).toBeGreaterThanOrEqual(16);
			}
		});

		it(`${mode}: content height covers the last row plus padding`, () => {
			const layout = layoutPages(request({ mode, pageSizes: sizes(7) }));
			const last = layout.rows[layout.rows.length - 1];
			expect(layout.contentHeight).toBeCloseTo((last?.y ?? 0) + (last?.height ?? 0) + 16, 6);
		});
	}

	it("a landscape page widens the fit and the others centre within it", () => {
		const landscape = { width: 792, height: 612 };
		const layout = layoutPages(request({ mode: "width", pageSizes: [LETTER, landscape, LETTER] }));
		// The widest row drives fit width: 792 pt into 968 px.
		expect(layout.scale).toBeCloseTo(968 / 792, 6);
		const wide = layout.pages[1];
		const narrow = layout.pages[0];
		expect(wide?.width).toBeGreaterThan(narrow?.width ?? 0);
		expect(wide?.x).toBeCloseTo(16, 6);
		expect(narrow?.x).toBeCloseTo(16 + ((wide?.width ?? 0) - (narrow?.width ?? 0)) / 2, 6);
	});

	it("is a pure function of its inputs — the same request always gives the same layout", () => {
		const first = layoutPages(request({ mode: "spread", pageSizes: sizes(9) }));
		const second = layoutPages(request({ mode: "spread", pageSizes: sizes(9) }));
		expect(second).toEqual(first);
	});

	it("survives degenerate inputs instead of throwing", () => {
		const layout = layoutPages(
			request({ pageSizes: [], viewportWidth: 0, viewportHeight: 0, gap: -4, padding: -8 }),
		);
		expect(layout.pages).toEqual([]);
		expect(layout.rows).toEqual([]);
		expect(layout.contentHeight).toBe(0);
		expect(rowIndexAt(layout, 100)).toBe(-1);
		expect(pageAtScrollTop(layout, 0)).toBeNull();
		expect(pageAt(layout, 0, 0)).toBeNull();
	});
});

describe("hit testing", () => {
	const layout = layoutPages(request({ mode: "spread", pageSizes: sizes(6) }));

	it("finds the row containing an offset and the nearest one in a gap", () => {
		expect(rowIndexAt(layout, 0)).toBe(0);
		const firstRow = layout.rows[0];
		expect(rowIndexAt(layout, (firstRow?.y ?? 0) + 1)).toBe(0);
		// In the inter-row gap: the row above still owns the reader.
		const gapTop = (firstRow?.y ?? 0) + (firstRow?.height ?? 0) + 1;
		expect(rowIndexAt(layout, gapTop)).toBe(0);
		// Past the end: the last row.
		expect(rowIndexAt(layout, layout.contentHeight + 500)).toBe(layout.rows.length - 1);
	});

	it("picks the column nearest a horizontal point", () => {
		const left = layout.pages[0];
		const right = layout.pages[1];
		expect(pageAt(layout, (left?.x ?? 0) + 5, left?.y ?? 0)?.page).toBe(0);
		expect(pageAt(layout, (right?.x ?? 0) + 5, right?.y ?? 0)?.page).toBe(1);
	});

	it("reports the topmost page in view as the current page", () => {
		const secondRow = layout.rows[1];
		expect(pageAtScrollTop(layout, secondRow?.y ?? 0)?.page).toBe(2);
		// A caller asking for a page past the end still gets the last one.
		expect(pageAtScrollTop(layout, 1e9)?.page).toBe(4);
	});
});
