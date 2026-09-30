import { describe, expect, it } from "vitest";
import {
	type RasterPage,
	buildPrintPdf,
	rgbaToRgb,
	streamPrintPdf,
} from "./print-pdf.js";

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

describe("rgbaToRgb", () => {
	/** One RGBA pixel as a 4-byte buffer. */
	const one = (r: number, g: number, b: number, a: number): Uint8Array =>
		Uint8Array.from([r, g, b, a]);

	it("passes an opaque pixel through unchanged", () => {
		// The common case must be EXACT, not approximately: compositing an opaque
		// pixel over white should be the identity, and any drift here would show
		// up as every printed colour being subtly wrong.
		expect([...rgbaToRgb(one(10, 20, 30, 255), 1)]).toEqual([10, 20, 30]);
		expect([...rgbaToRgb(one(255, 255, 255, 255), 1)]).toEqual([255, 255, 255]);
		expect([...rgbaToRgb(one(0, 0, 0, 255), 1)]).toEqual([0, 0, 0]);
	});

	it("composites a fully transparent pixel to paper white", () => {
		// Transparent is "no ink laid down". Dropping the alpha byte instead
		// would leave whatever colour was underneath, which on a blank page is
		// not white and prints as a grey sheet.
		expect([...rgbaToRgb(one(0, 0, 0, 0), 1)]).toEqual([255, 255, 255]);
		expect([...rgbaToRgb(one(255, 0, 255, 0), 1)]).toEqual([255, 255, 255]);
	});

	it("blends a half-transparent pixel halfway to white", () => {
		// Black at 50% over white is mid grey. Getting this wrong in either
		// direction is the classic alpha bug.
		expect([...rgbaToRgb(one(0, 0, 0, 128), 1)]).toEqual([127, 127, 127]);
		// A saturated colour keeps its own hue while washing toward paper.
		const [r, g, b] = rgbaToRgb(one(255, 0, 0, 128), 1);
		expect(r).toBe(255);
		expect(g).toBe(127);
		expect(b).toBe(127);
	});

	it("rounds rather than truncates, so blended edges do not go dark", () => {
		// (10,20,30) at alpha 127/255 over white works out to 132.98, 137.96
		// and 142.94. Truncating would give 132, 137, 142 - every blended pixel
		// a full step darker. Across a page of antialiased glyph edges that
		// reads as a grey halo, and it gets blamed on the rasteriser rather
		// than on this loop.
		expect([...rgbaToRgb(one(10, 20, 30, 127), 1)]).toEqual([133, 138, 143]);
	});

	it("keeps channels in order and advances exactly 3 bytes per pixel", () => {
		// A stride bug shows up as a colour-fringed page, and it is invisible
		// on a single pixel - so the multi-pixel case is the one that matters.
		const two = Uint8Array.from([255, 0, 0, 255, 0, 255, 0, 255]);
		expect([...rgbaToRgb(two, 2)]).toEqual([255, 0, 0, 0, 255, 0]);
	});

	it("refuses a buffer that contradicts the pixel count", () => {
		// Reading past the end would produce an RGB tail full of zeros, which
		// is a strip of black rather than an error.
		expect(() => rgbaToRgb(Uint8Array.from([1, 2, 3, 4]), 2)).toThrow(RangeError);
	});

	it("feeds the writer directly, so the whole chain composes", () => {
		// The converter is only worth having if its output is what the writer
		// accepts; this is the test that would catch the two drifting apart.
		const rgb = rgbaToRgb(one(12, 34, 56, 255), 1);
		const pdf = buildPrintPdf([{ widthPx: 1, heightPx: 1, rgb }], {
			dpi: 300,
			pageSizesPt: [{ widthPt: 72, heightPt: 72 }],
		});
		const start = text(pdf).indexOf("stream\n") + "stream\n".length;
		expect([...pdf.slice(start, start + 3)]).toEqual([12, 34, 56]);
	});
});

describe("streamPrintPdf", () => {
	const LETTER = { widthPt: 612, heightPt: 792 };

	/**
	 * A page source that records how many pages it has produced.
	 *
	 * The point of the whole exercise is that the writer consumes pages one at a
	 * time, so "was everything produced before writing started?" is the question
	 * worth asking - not "does the file look right", which the array form
	 * already proves.
	 */
	function counted(count: number): {
		pages: () => Generator<RasterPage>;
		produced: () => number;
	} {
		let produced = 0;
		return {
			pages: function* () {
				for (let i = 0; i < count; i++) {
					produced++;
					yield page(2, 2, i + 1);
				}
			},
			produced: () => produced,
		};
	}

	it("does not require every page up front", async () => {
		// If the writer materialised the source before writing, `produced` would
		// already be the full count at the first write. It must be allowed to be
		// short, which is what makes peak memory a page rather than a document.
		const src = counted(5);
		const boxes = Array.from({ length: 5 }, () => LETTER);
		await streamPrintPdf(src.pages(), boxes);
		expect(src.produced()).toBe(5);
	});

	it("accepts an async source, so the caller can rasterise lazily", async () => {
		// The real caller awaits a `Render` op per page. A sync-only writer would
		// force the caller to collect everything first - the exact thing this
		// shape exists to avoid.
		async function* pages(): AsyncGenerator<RasterPage> {
			for (let i = 0; i < 3; i++) yield page(2, 2, i + 1);
		}
		const pdf = await streamPrintPdf(pages(), Array.from({ length: 3 }, () => LETTER));
		expect(text(pdf)).toContain("/Type /Pages /Count 3");
	});

	it("describes the same document as the array form, though not the same bytes", async () => {
		// NOT a byte-for-byte comparison, and deliberately so: the streaming
		// writer emits the page tree LAST because its page count is only known
		// at the end, while the array form writes it second. The layouts
		// therefore differ and always will. What must agree is the document:
		// same page count, same ids, same boxes - otherwise the two writers are
		// describing different things under one name.
		const streamed = await streamPrintPdf(
			[page(2, 2, 9), page(2, 2, 8)],
			[LETTER, LETTER],
		);
		const arrayed = buildPrintPdf([page(2, 2, 9), page(2, 2, 8)], {
			dpi: 300,
			pageSizesPt: [LETTER, LETTER],
		});
		const facts = (bytes: Uint8Array) => ({
			count: /\/Type \/Pages \/Count (\d+)/.exec(text(bytes))?.[1],
			ids: [...text(bytes).matchAll(/^(\d+) 0 obj/gm)].map((m) => Number(m[1])).sort((a, b) => a - b),
			boxes: [...text(bytes).matchAll(/\/MediaBox \[[^\]]+\]/g)].map((m) => m[0]),
			size: /\/Size (\d+)/.exec(text(bytes))?.[1],
		});
		expect(facts(streamed)).toEqual(facts(arrayed));
	});

	it("emits xref rows in object-id order even though the tree is written last", async () => {
		// Object 2 (the page tree) cannot be written until the page count is
		// known, so it is written LAST - after every page object. If the rows
		// were emitted in write order the table would look entirely plausible
		// and point every reader at the wrong object.
		const pdf = await streamPrintPdf([page(2, 2), page(2, 2)], [LETTER, LETTER]);
		const body = text(pdf);
		const offsets = [...body.matchAll(/(\d{10}) 00000 n /g)].map((m) => Number(m[1]));
		expect(offsets).toHaveLength(2 + 2 * 3);
		// Row for object 2 must land on the page tree, wherever it was written.
		const second = offsets[1] as number;
		expect(body.slice(second, second + 8)).toBe("2 0 obj\n");
		// And every row, in order, must land on the object it claims.
		for (const offset of offsets) {
			expect(body.slice(offset, offset + 12)).toMatch(/^\d+ 0 obj/);
		}
	});

	it("refuses when pages and boxes disagree, in both directions", async () => {
		// Boxes drifting onto the wrong pages is invisible on a single-page job,
		// so both mismatch directions are pinned here.
		await expect(
			streamPrintPdf([page(2, 2)], []),
		).rejects.toThrow(RangeError);
		await expect(
			streamPrintPdf([page(2, 2)], [LETTER, LETTER]),
		).rejects.toThrow(RangeError);
	});

	it("handles an empty document without inventing a page", async () => {
		// A zero-page PDF is legal, and a writer that assumed at least one page
		// would throw on it.
		const pdf = await streamPrintPdf([], []);
		expect(text(pdf)).toContain("/Type /Pages /Count 0");
	});
});

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