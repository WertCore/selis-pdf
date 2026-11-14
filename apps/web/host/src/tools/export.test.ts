/**
 * The shared export layer (pdf-plan/41, slice 8).
 *
 * The writers are the engine's; this module is the wire and the names, and
 * that is what is tested here: which chunk answers which format, what each
 * format is called, and which formats carry the page's own picture.
 *
 * The writers themselves are tested in Rust, where they live
 * (`crates/selis-pdf-text`), and the chunks are tested at their own
 * boundary. What is left for this layer is the mapping, and that is what
 * would silently break if a format were renamed on one side of it.
 */
import { describe, expect, it } from "vitest";
import { EXPORT_FORMATS, exportFormat, rasterFormatFor, textOpFor } from "./export.js";

describe("the export formats", () => {
	it("covers the formats the engine can write", () => {
		const ids = EXPORT_FORMATS.map((format) => format.id);
		expect(ids).toContain("word");
		expect(ids).toContain("slides");
		expect(ids).toContain("workbook");
		expect(ids).toContain("html");
		expect(ids).toContain("epub");
		expect(ids).toContain("text");
		expect(ids).toContain("tables");
		expect(ids).toContain("reflow");
		expect(ids).toContain("png");
		expect(ids).toContain("jpg");
		expect(ids).toContain("tiff");
		expect(ids).toContain("webp");
	});

	it("names every format and says what it is", () => {
		for (const format of EXPORT_FORMATS) {
			expect(format.label.length).toBeGreaterThan(0);
			expect(format.note.length).toBeGreaterThan(0);
			expect(format.extension.length).toBeGreaterThan(0);
		}
	});

	it("knows which format needs the page's own picture", () => {
		// The deck and the Word document are the two formats that carry a
		// page as a picture: the deck shows the page, and the document keeps
		// the photographs and the rules a text-only facsimile would drop.
		expect(exportFormat("slides")?.needsPageImages).toBe(true);
		expect(exportFormat("word")?.needsPageImages).toBe(true);
		expect(exportFormat("nonsense")).toBeNull();
	});

	it("reads the page's picture into the chunk that asked for it", async () => {
		const { pageImageOptions } = await import("./export");
		expect(pageImageOptions("word", ["aGk="])).toEqual({ pageImages: ["aGk="] });
		expect(pageImageOptions("slides", ["aGk="])).toEqual({ slideImages: ["aGk="] });
		expect(pageImageOptions("word", undefined)).toEqual({});
	});

	it("sends every format but the web page to the text chunk", () => {
		// The writers live in `crates/selis-pdf-text`, and the text chunk is
		// how a page reaches them. The web page is the HTML chunk's.
		expect(textOpFor("word")).toBe("word");
		expect(textOpFor("slides")).toBe("slides");
		expect(textOpFor("workbook")).toBe("workbook");
		expect(textOpFor("epub")).toBe("epub");
		expect(textOpFor("reflow")).toBe("reflow");
		expect(textOpFor("tables")).toBe("tables");
		expect(textOpFor("text")).toBe("text");
		expect(textOpFor("html")).toBeNull();
	});

	it("gives every format a writer", () => {
		for (const format of EXPORT_FORMATS) {
			const op = textOpFor(format.id);
			expect(
				op !== null || format.id === "html" || rasterFormatFor(format.id) !== null,
				format.id,
			).toBe(true);
		}
	});

	it("sends the image formats to the raster chunk", () => {
		expect(rasterFormatFor("png")).toBe("png");
		expect(rasterFormatFor("jpg")).toBe("jpg");
		expect(rasterFormatFor("tiff")).toBe("tiff");
		expect(rasterFormatFor("webp")).toBe("webp");
		expect(rasterFormatFor("word")).toBeNull();
	});
});
