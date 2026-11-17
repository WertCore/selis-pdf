/**
 * The subset-fonts card's presentation helpers, tested without a browser.
 *
 * These are read-back/structural checks on the rows the card builds from the
 * engine's measured verification — not string matches against source files.
 */

import { describe, expect, it } from "vitest";
import type { Verification } from "./protocol.js";
import { SUBSET_FONTS_NOTE, subsetReportRows } from "./subset.js";

const verification: Verification = {
	pages_in: 1,
	pages_out: 1,
	annotations_in: 0,
	annotations_out: 0,
	fields_in: 0,
	fields_out: 0,
	ocgs_in: 0,
	ocgs_out: 0,
	outline_entries_in: 0,
	outline_entries_out: 0,
	embedded_files_in: 0,
	embedded_files_out: 0,
	bytes_in: 140_376,
	bytes_out: 4_386,
	verdict: "ok",
	checked: ["pages", "annotations", "fields", "ocgs", "outline_entries", "embedded_files"],
	fonts_subset: 1,
	glyphs_kept: 12,
};

describe("the subset-fonts option note", () => {
	it("states the supported shapes, the refusals and the local-only guarantee", () => {
		expect(SUBSET_FONTS_NOTE).toContain("WinAnsi");
		expect(SUBSET_FONTS_NOTE).toContain("Identity-H");
		expect(SUBSET_FONTS_NOTE).toContain("refused with a reason");
		expect(SUBSET_FONTS_NOTE).toContain("never uploaded");
	});
});

describe("subsetReportRows", () => {
	it("reports the measured fonts, glyphs and the before/after size", () => {
		const rows = subsetReportRows(verification);
		const find = (label: string) => rows.find((row) => row.label === label)?.value;
		expect(find("Fonts subset")).toBe("1");
		expect(find("Glyphs kept")).toBe("12");
		// The saving and both measured sizes are shown, formatted from bytes.
		expect(find("Size saved")).toBe("133 KiB (137 KiB → 4.3 KiB)");
	});

	it("derives every value from the verification, not fixed copy", () => {
		const other = subsetReportRows({
			...verification,
			fonts_subset: 3,
			glyphs_kept: 91,
			bytes_in: 2048,
			bytes_out: 1024,
		});
		const find = (label: string) => other.find((row) => row.label === label)?.value;
		expect(find("Fonts subset")).toBe("3");
		expect(find("Glyphs kept")).toBe("91");
		expect(find("Size saved")).toBe("1.0 KiB (2.0 KiB → 1.0 KiB)");
	});

	it("does not claim a subset when the engine reported none", () => {
		const plain: Verification = { ...verification };
		const mutable = plain as { fonts_subset?: number; glyphs_kept?: number };
		delete mutable.fonts_subset;
		delete mutable.glyphs_kept;
		const rows = subsetReportRows(plain);
		expect(rows).toHaveLength(1);
		expect(rows[0]?.value).toContain("no font was subset");
	});
});
