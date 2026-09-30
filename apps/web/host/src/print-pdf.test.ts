import { describe, expect, it } from "vitest";
import { type RasterPage, buildPrintPdf } from "./print-pdf.js";

/** A solid-colour page, so the embedded bytes can be found exactly. */
function page(widthPx: number, heightPx: number, fill = 7): RasterPage {
	return {
		widthPx,
		heightPx,
		rgb: new Uint8Array(widthPx * heightPx * 3).fill(fill),
	};
}

const LETTER = { widthPt: 612, heightPt: 792 };

/** Decode as latin-1 so byte offsets can be searched directly. */
function text(bytes: Uint8Array): string {
	return Buffer.from(bytes).toString("latin1");
}

/**
 * Parse the xref table into its recorded offsets.
 *
 * A real parse rather than a spot regex, so a malformed table fails the test
 * instead of quietly matching nothing.
 */
function xrefOffsets(body: string): number[] {
	return [...body.matchAll(/(\d{10}) 00000 n /g)].map((m) => Number(m[1]));
}

describe("buildPrintPdf", () => {
	it("writes a well-formed file envelope", () => {
		const body = text(buildPrintPdf([page(4, 4)], { dpi: 300, pageSizesPt: [LETTER] }));
		expect(body.startsWith("%PDF-1.7")).toBe(true);
		expect(body.endsWith("%%EOF\n")).toBe(true);
		expect(body).toContain("/Type /Catalog");
	});

	it("keeps the binary marker above 0x7E as single bytes", () => {
		const pdf = buildPrintPdf([page(4, 4)], { dpi: 300, pageSizesPt: [LETTER] });
		// The header `%PDF-1.7\n%` is ten bytes, so the marker sits at 10..13.
		// Encoded as UTF-8 those four bytes would each become TWO and shift
		// every xref offset after them - which the offsets test below catches.
		expect([...pdf.slice(10, 14)]).toEqual([0xe2, 0xe3, 0xcf, 0xd3]);
	});

	it("records every object offset so each lands on that object", () => {
		// THE classic PDF bug. Offsets are measured, so this passes; a writer
		// that predicted them would drift as soon as anything changed size.
		const pdf = buildPrintPdf([page(4, 4), page(6, 8)], {
			dpi: 300,
			pageSizesPt: [LETTER, { widthPt: 300, heightPt: 400 }],
		});
		const body = text(pdf);
		for (const offset of xrefOffsets(body)) {
			expect(body.slice(offset, offset + 12)).toMatch(/^\d+ 0 obj/);
		}
	});

	it("gives every object a distinct id, even across many pages", () => {
		// A page contributes three objects. Stepping by two collides page N's
		// content stream with page N+1's page object, and the file is then
		// quietly corrupt rather than loudly broken.
		const pdf = buildPrintPdf([page(4, 4), page(4, 4), page(4, 4)], {
			dpi: 300,
			pageSizesPt: [LETTER, LETTER, LETTER],
		});
		const ids = [...text(pdf).matchAll(/^(\d+) 0 obj/gm)].map((m) => Number(m[1]));
		expect(ids).toHaveLength(2 + 3 * 3);
		expect(new Set(ids).size).toBe(ids.length);
		expect([...ids].sort((a, b) => a - b)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
	});

	it("declares the page count and one page object per page", () => {
		const body = text(
			buildPrintPdf([page(4, 4), page(4, 4)], { dpi: 300, pageSizesPt: [LETTER, LETTER] }),
		);
		expect(body).toContain("/Type /Pages /Count 2");
		expect([...body.matchAll(/\/Type \/Page[^s]/g)]).toHaveLength(2);
	});

	it("writes MediaBox in POINTS while the image is in PIXELS", () => {
		// A 300 DPI Letter raster is 2550x3300. The MediaBox must still say
		// 612x792; writing the pixel count there yields a file that opens fine
		// and prints at a nonsensical physical size.
		const body = text(buildPrintPdf([page(2550, 3300)], { dpi: 300, pageSizesPt: [LETTER] }));
		expect(body).toContain("/MediaBox [0 0 612 792]");
		expect(body).toContain("/Width 2550 /Height 3300");
	});

	it("sizes each MediaBox from its OWN page, not the first one", () => {
		// Boxes drifting onto the wrong pages is invisible to a single-page test.
		const body = text(
			buildPrintPdf([page(4, 4), page(4, 4)], {
				dpi: 300,
				pageSizesPt: [LETTER, { widthPt: 300, heightPt: 400 }],
			}),
		);
		expect(body).toContain("/MediaBox [0 0 612 792]");
		expect(body).toContain("/MediaBox [0 0 300 400]");
	});

	it("embeds the exact pixel bytes, and declares that length", () => {
		const p = page(2, 2, 200);
		const pdf = buildPrintPdf([p], { dpi: 300, pageSizesPt: [LETTER] });
		const start = text(pdf).indexOf("stream\n") + "stream\n".length;
		expect([...pdf.slice(start, start + p.rgb.length)]).toEqual([...p.rgb]);
		expect(text(pdf)).toContain(`/Length ${p.rgb.length}`);
	});

	it("declares DeviceRGB at 8 bits, which is what the RGB data is", () => {
		const body = text(buildPrintPdf([page(2, 2)], { dpi: 300, pageSizesPt: [LETTER] }));
		expect(body).toContain("/ColorSpace /DeviceRGB /BitsPerComponent 8");
	});

	it("refuses a page whose pixel count contradicts its declared size", () => {
		const bad = { widthPx: 2, heightPx: 2, rgb: new Uint8Array(5) };
		expect(() => buildPrintPdf([bad], { dpi: 300, pageSizesPt: [LETTER] })).toThrow(
			RangeError,
		);
	});

	it("refuses a page count that disagrees with the box count", () => {
		expect(() =>
			buildPrintPdf([page(2, 2), page(2, 2)], { dpi: 300, pageSizesPt: [LETTER] }),
		).toThrow(RangeError);
	});

	it("refuses a nonsensical DPI", () => {
		for (const dpi of [0, -1, Number.NaN]) {
			expect(() => buildPrintPdf([page(2, 2)], { dpi, pageSizesPt: [LETTER] })).toThrow(
				RangeError,
			);
		}
	});

	it("leaks nothing between calls", () => {
		// A reused writer carrying ids or offsets would show up as a stale page.
		const one = text(buildPrintPdf([page(2, 2)], { dpi: 300, pageSizesPt: [LETTER] }));
		expect(one).toContain("/Count 1");
		expect(one).not.toContain("/Count 2");
		expect([...text(buildPrintPdf([page(2, 2)], { dpi: 300, pageSizesPt: [LETTER] })).matchAll(/^(\d+) 0 obj/gm)]).toHaveLength(
			5,
		);
	});
});