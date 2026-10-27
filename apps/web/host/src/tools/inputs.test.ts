import { describe, expect, it } from "vitest";
import {
	hasPdfHeader,
	pageCount,
	pageIndex,
	shiftPageSelection,
	ToolInputError,
} from "./inputs.js";

describe("shiftPageSelection", () => {
	it("converts 1-based pages and ranges to the engine's 0-based syntax", () => {
		expect(shiftPageSelection("1")).toBe("0");
		expect(shiftPageSelection("1-3, 7")).toBe("0-2,6");
		expect(shiftPageSelection(" 2 , 4-5 ")).toBe("1,3-4");
	});

	it("rejects zero, backwards ranges, empty items, and prose", () => {
		expect(() => shiftPageSelection("")).toThrow(ToolInputError);
		expect(() => shiftPageSelection("0")).toThrow(ToolInputError);
		expect(() => shiftPageSelection("3-1")).toThrow(/backwards/);
		expect(() => shiftPageSelection("1,,2")).toThrow(/empty item/);
		expect(() => shiftPageSelection("first")).toThrow(/not a page number/);
		expect(() => shiftPageSelection("1.5")).toThrow(ToolInputError);
	});

	it("keeps every endpoint in a multi-range selection", () => {
		expect(shiftPageSelection("1-2,4-5,9")).toBe("0-1,3-4,8");
	});
});

describe("pageIndex", () => {
	it("converts one 1-based page to a 0-based index", () => {
		expect(pageIndex("1", "From page")).toBe(0);
		expect(pageIndex("12", "To page")).toBe(11);
	});

	it("names the field in the error", () => {
		expect(() => pageIndex("0", "From page")).toThrow(/From page/);
	});
});

describe("pageCount", () => {
	it("keeps a count as-is rather than shifting it", () => {
		expect(pageCount("1", "Pages per file")).toBe(1);
		expect(pageCount("12", "Pages per file")).toBe(12);
	});

	it("rejects zero, fractions, and prose", () => {
		expect(() => pageCount("0", "Pages per file")).toThrow(/Pages per file/);
		expect(() => pageCount("1.5", "Pages per file")).toThrow(ToolInputError);
		expect(() => pageCount("two", "Pages per file")).toThrow(ToolInputError);
	});
});

describe("hasPdfHeader", () => {
	it("accepts a PDF header and rejects everything else", () => {
		expect(hasPdfHeader(new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d]))).toBe(true);
		expect(hasPdfHeader(new Uint8Array([0x25, 0x50, 0x44, 0x46]))).toBe(false);
		expect(hasPdfHeader(new Uint8Array([0x50, 0x4b, 0x03, 0x04]))).toBe(false);
	});
});
