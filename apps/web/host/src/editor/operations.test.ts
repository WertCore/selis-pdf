/**
 * The editor's operation builders (pdf-plan/39).
 *
 * The engine numbers pages from zero and the editor's chrome numbers them
 * from one. Every builder here takes a zero-based index and writes it into
 * the request, so the shift happens in exactly one place — and these tests
 * are what keep it there.
 */

import { describe, expect, it } from "vitest";
import {
	cropOptions,
	deletePage,
	duplicatePage,
	editLandedOnLine,
	editText,
	fillFormsOptions,
	imageOptions,
	insertBlankPages,
	metadataOptions,
	movePage,
	pageSelection,
	redactOptions,
	reorderPages,
	rotatePage,
	searchCandidates,
	stampTextWidth,
	textBoxOptions,
} from "./operations.js";

describe("the text the stamp will draw", () => {
	it("is measured the way the engine measures it", () => {
		// The engine charges 667 for a capital, 833 for a wide lower case, 278
		// for a narrow one and 556 for everything else. "Hello from the
		// editor" is 10 396 units, which is 124.75 points at 12.
		expect(stampTextWidth("Hello from the editor", 12)).toBeCloseTo(124.752, 3);
		expect(stampTextWidth("i", 12)).toBeCloseTo(3.336, 3);
		expect(stampTextWidth("m", 12)).toBeCloseTo(9.996, 3);
		expect(stampTextWidth("A", 12)).toBeCloseTo(8.004, 3);
		expect(stampTextWidth("", 12)).toBe(0);
	});
});

describe("the options for placing content", () => {
	it("places a text box in the engine's stamp shape", () => {
		const options = textBoxOptions(2, { x0: 10, y0: 20, x1: 110, y1: 40 }, "Hi", 12);
		expect(options.page).toBe(3);
		expect(options.rect).toEqual([10, 20, 110, 40]);
		expect(options.lines).toEqual([{ text: "Hi", size: 12 }]);
		expect(options.border).toBe(false);
	});

	it("places an image at a point, opaque and not tiled", () => {
		const options = imageOptions(1, 30, 40, 0.5);
		expect(options.opacity).toBe(1);
		expect(options.scale).toBe(0.5);
		expect(options.x).toBe(30);
		expect(options.y).toBe(40);
		expect(options.tiled).toBe(false);
		expect(options.pages).toBe("2");
	});

	it("redacts a region in the page's own user space", () => {
		const options = redactOptions(0, { x: 5, y: 6, width: 7, height: 8 });
		expect(options.page).toBe(0);
		expect(options.rects).toEqual([[5, 6, 7, 8]]);
	});

	it("crops a page by its margins, zero-based", () => {
		const options = cropOptions({ left: 1, top: 2, right: 3, bottom: 4 }, 2);
		expect(options).toEqual({ left: 1, top: 2, right: 3, bottom: 4, pages: "2" });
	});

	it("writes the document's properties under the names the engine uses", () => {
		const options = metadataOptions({ Title: "A title", Author: "Someone" });
		expect(options.fields).toEqual({ Title: "A title", Author: "Someone" });
	});

	it("fills the form's fields by name, in order", () => {
		const options = fillFormsOptions([
			{ name: "Name", value: "Selis" },
			{ name: "Agree", value: "Yes" },
		]);
		expect(options.fields).toEqual([
			{ name: "Name", value: "Selis" },
			{ name: "Agree", value: "Yes" },
		]);
	});
});

describe("the page selection the engine is sent", () => {
	it("is the engine's own zero-based syntax", () => {
		expect(pageSelection(0)).toBe("0");
		expect(pageSelection(11)).toBe("11");
	});

	it("refuses a page that does not exist", () => {
		expect(() => pageSelection(-1)).toThrow(RangeError);
		expect(() => pageSelection(1.5)).toThrow(RangeError);
	});
});

describe("the page operations", () => {
	it("rotates by a quarter, a half or three quarters", () => {
		expect(rotatePage(2, 90)).toEqual({
			op: "rotate",
			options: { angle: 90, pages: "2" },
			label: "Rotated page 3 by 90°",
		});
		expect(rotatePage(0, 180).options.angle).toBe(180);
	});

	it("deletes, duplicates and reorders by the same zero-based index", () => {
		expect(deletePage(4).options).toEqual({ pages: "4" });
		expect(deletePage(4).label).toBe("Deleted page 5");
		expect(duplicatePage(4).options).toEqual({ mode: "duplicate", pages: "4" });
		expect(reorderPages([2, 0, 1]).options).toEqual({ mode: "reorder", order: "2,0,1" });
	});

	it("inserts a blank page at the engine's one-based boundary", () => {
		// `insert_blank_pages` documents `after` as the ONE-based page the
		// blanks follow, where 0 puts them before the first page. The editor's
		// page index is zero-based, so "after page 3" is the boundary 3.
		expect(insertBlankPages(3).options).toEqual({ mode: "blank", after: 3, count: 1 });
		expect(insertBlankPages(0).options).toEqual({ mode: "blank", after: 0, count: 1 });
		expect(insertBlankPages(2, 3).options).toEqual({ mode: "blank", after: 2, count: 3 });
		expect(insertBlankPages(2, 3).label).toBe("Inserted 3 blank pages");
	});

	it("refuses a boundary or a count that mean nothing", () => {
		expect(() => insertBlankPages(-1)).toThrow(RangeError);
		expect(() => insertBlankPages(1, 0)).toThrow(RangeError);
		expect(() => reorderPages([])).toThrow(RangeError);
	});

	it("moves a page one place and refuses the ends", () => {
		const order = [0, 1, 2];
		expect(movePage(order, 2, "up").options).toEqual({ mode: "reorder", order: "0,2,1" });
		expect(movePage(order, 0, "down").options).toEqual({ mode: "reorder", order: "1,0,2" });
		expect(() => movePage(order, 0, "up")).toThrow(RangeError);
		expect(() => movePage(order, 2, "down")).toThrow(RangeError);
		expect(() => movePage(order, 7, "up")).toThrow(RangeError);
	});
});

describe("the text edit", () => {
	it("sends the engine's own option names", () => {
		expect(editText(0, "Annual Report", "Quarterly Report")).toEqual({
			op: "edit_text",
			options: { page: 0, find: "Annual Report", replace: "Quarterly Report" },
			label: "Edited the text on page 1",
		});
	});

	it("refuses an empty search string and a page that does not exist", () => {
		expect(() => editText(0, "", "x")).toThrow(RangeError);
		expect(() => editText(-1, "x", "y")).toThrow(RangeError);
	});
});

describe("the search strings tried for a clicked line", () => {
	it("tries the whole line first", () => {
		// The whole line is the user's literal intent, and it is the right
		// answer whenever the line is drawn as one run.
		expect(searchCandidates("Annual Report")[0]).toBe("Annual Report");
	});

	it("falls back to the line's longest words", () => {
		// A justified paragraph draws each word separately, so the whole line
		// is not inside any one operand. A word usually is.
		const candidates = searchCandidates("The Annual Report 2024");
		expect(candidates[0]).toBe("The Annual Report 2024");
		expect(candidates.slice(1)).toEqual(["Annual", "Report", "2024"]);
	});

	it("skips a word it has already offered", () => {
		const candidates = searchCandidates("Report Report");
		expect(candidates).toEqual(["Report Report", "Report"]);
	});

	it("has nothing to try for an empty line", () => {
		expect(searchCandidates("")).toEqual([]);
		expect(searchCandidates("   ")).toEqual([]);
	});

	it("does not turn one line into dozens of engine runs", () => {
		const words = Array.from({ length: 30 }, (_, index) => `word${index}`).join(" ");
		expect(searchCandidates(words).length).toBeLessThanOrEqual(4);
	});
});

describe("whether the edit landed where the user clicked", () => {
	it("accepts the line the user clicked", () => {
		expect(
			editLandedOnLine("Annual Report", "Quarterly Report", "Annual Report", "Quarterly Report"),
		).toBe(true);
	});

	it("accepts a line where only one word was replaced", () => {
		// The line is drawn as several runs, so the engine replaced one of them.
		expect(
			editLandedOnLine("The Annual Report", "The Quarterly Report", "Annual", "Quarterly"),
		).toBe(true);
	});

	it("rejects an edit that left the line alone", () => {
		// The engine found the words EARLIER on the page and changed those.
		// The clicked line is untouched, and the editor says so.
		expect(
			editLandedOnLine("Annual Report", "Annual Report", "Annual Report", "Quarterly Report"),
		).toBe(false);
	});

	it("accepts an empty replacement, which deletes the run", () => {
		expect(editLandedOnLine("Annual Report", "Report", "Annual ", "")).toBe(true);
	});
});
