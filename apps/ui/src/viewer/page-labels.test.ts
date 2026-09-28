/**
 * Page labels (SL-4.UI.06).
 *
 * Every number here is checkable against PDF 32000-2:2020 §7.7.3.4 by hand, and
 * that is the point: the interesting bugs in page labels are arithmetic bugs in a
 * corner of the spec (alphabetic labels past Z, a roman above 3999, a range that
 * starts after page 0), and a test that asserted "the output looks right" would
 * pass straight through all of them. So the expectations spell out the
 * arithmetic — 27 is `AA`, not `BA` — rather than the shape of the answer.
 *
 * Headless honesty: this is a pure function of data, so Node can assert all of
 * it. What cannot be asserted here is that a reader *understands* the label in
 * their locale's numerals, which is a rendering question, not a formatting one.
 */

import { describe, expect, it } from "vitest";
import type { PageLabelRange } from "../platform/types.js";
import {
	DEFAULT_LABEL_STYLE,
	MAX_ROMAN,
	formatSequenceValue,
	hasMeaningfulLabels,
	normaliseLabelRanges,
	pageLabelFor,
	pageLabelsFor,
	rangeForPage,
	toAlphabetic,
	toRoman,
} from "./page-labels.js";

describe("normaliseLabelRanges", () => {
	it("sorts, defaults and copies", () => {
		const ranges = normaliseLabelRanges([
			{ firstPage: 10, style: "D" },
			{ firstPage: 0 },
			{ firstPage: 4, style: "r", prefix: "p", firstValue: 1 },
		]);
		expect(ranges.map((range) => range.firstPage)).toEqual([0, 4, 10]);
		expect(ranges[0]).toEqual({ firstPage: 0, style: "D", prefix: "", firstValue: 1 });
	});

	it("drops ranges that cannot apply to any page", () => {
		const ranges = normaliseLabelRanges([
			{ firstPage: -1 },
			{ firstPage: 1.5 },
			{ firstPage: 0, firstValue: 2.5 },
			{ firstPage: 0, style: "D" },
		]);
		expect(ranges).toHaveLength(1);
		expect(ranges[0]?.firstPage).toBe(0);
	});

	it("drops an unknown /S rather than silently labelling decimal", () => {
		// Five styles exist. A sixth is a producer bug, and guessing `D` would
		// mislabel a document without saying so.
		const ranges = normaliseLabelRanges([
			{ firstPage: 0, style: "Z" as PageLabelRange["style"] },
			{ firstPage: 0, style: "A" },
		]);
		expect(ranges).toHaveLength(1);
		expect(ranges[0]?.style).toBe("A");
	});

	it("lets a later range win when two start at the same page", () => {
		const ranges = normaliseLabelRanges([
			{ firstPage: 0, style: "D" },
			{ firstPage: 0, style: "r" },
		]);
		expect(rangeForPage(ranges, 0)?.style).toBe("r");
	});

	it("defaults the style to D, per Table 165", () => {
		expect(DEFAULT_LABEL_STYLE).toBe("D");
		expect(normaliseLabelRanges([{ firstPage: 0 }])[0]?.style).toBe("D");
	});
});

describe("the sequence renderers", () => {
	it("renders roman numerals the way the spec means them", () => {
		expect(toRoman(1)).toBe("I");
		expect(toRoman(4)).toBe("IV");
		expect(toRoman(9)).toBe("IX");
		expect(toRoman(40)).toBe("XL");
		expect(toRoman(1994)).toBe("MCMXCIV");
		expect(toRoman(MAX_ROMAN)).toBe("MMMCMXCIX");
	});

	it("refuses roman outside 1..3999 rather than inventing a numeral", () => {
		// §7.7.3.4 leaves this to the viewer; Acrobat writes decimal, and so do
		// we, by way of the fallback in `pageLabelFor`.
		expect(toRoman(0)).toBeNull();
		expect(toRoman(-1)).toBeNull();
		expect(toRoman(MAX_ROMAN + 1)).toBeNull();
		expect(formatSequenceValue(MAX_ROMAN + 1, "R")).toBeNull();
	});

	it("renders lower-case roman by lower-casing the same numeral", () => {
		expect(formatSequenceValue(14, "r")).toBe("xiv");
		expect(formatSequenceValue(14, "R")).toBe("XIV");
	});

	it("renders alphabetic labels bijectively: Z then AA, not BA", () => {
		expect(toAlphabetic(1)).toBe("A");
		expect(toAlphabetic(26)).toBe("Z");
		expect(toAlphabetic(27)).toBe("AA");
		expect(toAlphabetic(28)).toBe("AB");
		expect(toAlphabetic(52)).toBe("AZ");
		expect(toAlphabetic(53)).toBe("BA");
		expect(toAlphabetic(702)).toBe("ZZ");
		expect(toAlphabetic(703)).toBe("AAA");
		expect(formatSequenceValue(27, "a")).toBe("aa");
	});

	it("refuses a sequence value below one in every style but decimal", () => {
		// Decimal zero is a perfectly good label; roman and alphabetic zero is
		// not a numeral at all, and inventing one would be a lie.
		expect(formatSequenceValue(0, "D")).toBe("0");
		for (const style of ["R", "r", "A", "a"] as const) {
			expect(formatSequenceValue(0, style), style).toBeNull();
		}
	});

describe("pageLabelFor", () => {
	it("labels an unlabelled document with its own number", () => {
		expect(pageLabelFor(0, undefined)).toBe("1");
		expect(pageLabelFor(9, undefined)).toBe("10");
		expect(pageLabelFor(0, [])).toBe("1");
	});

	it("numbers a page before the first range with the page number", () => {
		const ranges: PageLabelRange[] = [{ firstPage: 4, style: "r" }];
		expect(pageLabelFor(0, ranges)).toBe("1");
		expect(pageLabelFor(3, ranges)).toBe("4");
		expect(pageLabelFor(4, ranges)).toBe("i");
	});

	it("counts from /St within a range", () => {
		const ranges: PageLabelRange[] = [{ firstPage: 2, style: "D", firstValue: 7 }];
		expect(pageLabelFor(0, ranges)).toBe("1");
		expect(pageLabelFor(2, ranges)).toBe("7");
		expect(pageLabelFor(3, ranges)).toBe("8");
	});

	it("uses the range with the largest start at or before the page", () => {
		const ranges: PageLabelRange[] = [
			{ firstPage: 0, style: "r" },
			{ firstPage: 3, style: "D" },
			{ firstPage: 5, style: "A" },
		];
		expect(pageLabelFor(2, ranges)).toBe("iii");
		expect(pageLabelFor(3, ranges)).toBe("1");
		expect(pageLabelFor(4, ranges)).toBe("2");
		expect(pageLabelFor(5, ranges)).toBe("A");
		expect(pageLabelFor(6, ranges)).toBe("B");
	});

	it("applies the prefix verbatim", () => {
		const ranges: PageLabelRange[] = [{ firstPage: 0, style: "D", prefix: "A-" }];
		expect(pageLabelFor(0, ranges)).toBe("A-1");
		expect(pageLabelFor(41, ranges)).toBe("A-42");
	});

	it("falls back to the page number when the style cannot express the value", () => {
		// A 4 000-page document in roman. Page 3 998 is the 3 999th page, so its
		// label is 3999 — the last roman numeral there is; page 3 999 would be
		// 4000, which no roman numeral can say, so the label degrades to the
		// page number rather than to nothing.
		const ranges: PageLabelRange[] = [{ firstPage: 0, style: "R" }];
		expect(pageLabelFor(3997, ranges)).toBe("MMMCMXCVIII");
		expect(pageLabelFor(3998, ranges)).toBe("MMMCMXCIX");
		expect(pageLabelFor(3999, ranges)).toBe("4000");
	});

	it("is empty for a page that is not a page", () => {
		expect(pageLabelFor(-1, undefined)).toBe("");
		expect(pageLabelFor(1.5, undefined)).toBe("");
		expect(pageLabelFor(Number.NaN, undefined)).toBe("");
	});

	it("accepts a pre-normalised table without re-sorting it", () => {
		const table = normaliseLabelRanges([{ firstPage: 0, style: "r" }]);
		expect(pageLabelFor(3, undefined, table)).toBe("iv");
	});
});

describe("hasMeaningfulLabels", () => {
	it("is false when the document says nothing a page number would not", () => {
		expect(hasMeaningfulLabels(undefined, 10)).toBe(false);
		expect(hasMeaningfulLabels([], 10)).toBe(false);
		// Decimal from page 0, no prefix: identical to the page list's own box.
		expect(hasMeaningfulLabels([{ firstPage: 0, style: "D" }], 10)).toBe(false);
	});

	it("is true for anything that differs from the page number", () => {
		expect(hasMeaningfulLabels([{ firstPage: 0, style: "r" }], 10)).toBe(true);
		expect(hasMeaningfulLabels([{ firstPage: 0, prefix: "A-" }], 10)).toBe(true);
		expect(hasMeaningfulLabels([{ firstPage: 0, firstValue: 5 }], 10)).toBe(true);
		// Front matter labelled, body not: the gap is itself a labelling decision.
		expect(hasMeaningfulLabels([{ firstPage: 0, style: "r" }, { firstPage: 4 }], 10)).toBe(true);
	});

	it("treats a range that starts after page 0 as meaningful", () => {
		// The range runs to the end of the document, so the only unlabelled pages
		// are the ones *before* it — and that is already a difference.
		expect(hasMeaningfulLabels([{ firstPage: 0, style: "D" }], 10)).toBe(false);
		expect(hasMeaningfulLabels([{ firstPage: 2, style: "D" }], 10)).toBe(true);
		// A document with no pages has nothing to differ from.
		expect(hasMeaningfulLabels([{ firstPage: 0, style: "r" }], 0)).toBe(false);
	});
});

describe("pageLabelsFor", () => {
	it("labels a list of pages in one pass, in order", () => {
		const ranges: PageLabelRange[] = [{ firstPage: 0, style: "r" }];
		expect(pageLabelsFor([0, 1, 2, 3], ranges)).toEqual(["i", "ii", "iii", "iv"]);
		expect(pageLabelsFor([], ranges)).toEqual([]);
	});
});

});
