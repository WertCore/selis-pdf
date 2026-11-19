/**
 * The compare card's presentation helpers, tested without a browser.
 *
 * These are checks on the copy and on the rows built from the engine's own
 * `selis-compare/1` report — not string matches against source files.
 */

import { describe, expect, it } from "vitest";
import {
	COMPARE_NOTE,
	COMPARE_SCHEMA,
	type ComparisonReportJson,
	compareReportRows,
} from "./compare.js";

const identical: ComparisonReportJson = {
	schema: COMPARE_SCHEMA,
	status: "ok",
	pages_a: 1,
	pages_b: 1,
	identical: true,
	text_identical: true,
	pages: [{ page: 1, present_in: "both", pixels_identical: true, diff_fraction: 0 }],
};

describe("the compare option note", () => {
	it("names the deterministic render, the text half and the local-only guarantee", () => {
		expect(COMPARE_NOTE).toContain("same resolution");
		expect(COMPARE_NOTE).toContain("sample for sample");
		expect(COMPARE_NOTE).toContain("deterministic");
		expect(COMPARE_NOTE).toContain("extracted text");
		expect(COMPARE_NOTE).toContain("never uploaded");
	});
});

describe("compareReportRows", () => {
	it("reports an identical verdict from the engine's own booleans", () => {
		const rows = compareReportRows(identical);
		expect(rows[0]?.label).toBe("Result");
		expect(rows[0]?.value).toContain("identical");
		expect(rows[1]?.value).toContain("matches");
		expect(rows[2]?.value).toBe("1 vs 1");
		expect(rows).toHaveLength(3);
	});

	it("reports a pixel difference with the engine's fraction", () => {
		const rows = compareReportRows({
			...identical,
			identical: false,
			pages: [{ page: 1, present_in: "both", pixels_identical: false, diff_fraction: 0.42 }],
		});
		expect(rows[0]?.value).toBe("different");
		expect(rows[3]?.label).toBe("Page 1");
		expect(rows[3]?.value).toContain("42%");
	});

	it("names a page present in only one document", () => {
		const rows = compareReportRows({
			...identical,
			pages_b: 2,
			pages: [
				{ page: 1, present_in: "both", pixels_identical: true, diff_fraction: 0 },
				{ page: 2, present_in: "b_only" },
			],
		});
		expect(rows[3]?.label).toBe("Page 2");
		expect(rows[3]?.value).toContain("only in the second document");
	});

	it("caps the per-page rows and points to the downloadable report", () => {
		const pages = Array.from({ length: 12 }, (_, i) => ({
			page: i + 1,
			present_in: "both",
			pixels_identical: false,
			diff_fraction: 0.1,
		}));
		const rows = compareReportRows({ ...identical, identical: false, pages });
		expect(rows.filter((row) => row.label.startsWith("Page "))).toHaveLength(8);
		expect(rows.at(-1)?.value).toContain("4 further page(s)");
	});

	it("refuses a report with the wrong schema", () => {
		const rows = compareReportRows({ ...identical, schema: "selis-other/1" });
		expect(rows).toHaveLength(1);
		expect(rows[0]?.value).toContain("not a selis-compare report");
	});
});
