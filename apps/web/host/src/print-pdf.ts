/**
 * SL-4.UI.08 - assembling a print-ready PDF.
 *
 * The Do: allows "a print-specific canvas or a generated print-ready PDF". This
 * is the second, and it exists because of a constraint recorded in `print.ts`:
 * a Letter page at 300 DPI is 2550x3300, which is 33 MB of RGBA for ONE page. A
 * document does not fit in a tab, so the only viable shape is page-at-a-time
 * into a writer.
 *
 * Two things are where PDF writers actually go wrong, and both are measured
 * rather than predicted here:
 *
 *  - **The xref table.** Every entry is a BYTE OFFSET from the start of the
 *    file. This writer records each object's position as it writes it, so the
 *    table cannot drift; a reader that tolerates a wrong table will hide the
 *    error until a strict one rejects the file, which is the worst time to find
 *    out.
 *  - **Units.** A MediaBox is in POINTS (1/72 inch). An image XObject is in
 *    PIXELS. Mixing them yields a file that opens and prints at a nonsensical
 *    size, which reads as a printing bug and is not one.
 *
 * Image data is written UNCOMPRESSED. That is valid PDF and keeps this module
 * synchronous, pure and testable, but at 300 DPI it is a large cost. A
 * `/FlateDecode` stream is the obvious next step and the writer is shaped for
 * it: image bytes pass through exactly one place, so a filter can be added
 * without reshaping the object graph.
 */

/** One rasterised page, ready to be embedded. */
export interface RasterPage {
	readonly widthPx: number;
	readonly heightPx: number;
	/** Packed RGB, 3 bytes per pixel, row-major, no padding. */
	readonly rgb: Uint8Array;
}

/** One page box, in points, as the plan decided it. */
export interface PageBox {
	readonly widthPt: number;
	readonly heightPt: number;
}

/** What the writer needs to know about the job. */
export interface PrintPdfOptions {
	/** Device pixels per inch the pages were rasterised at. */
	readonly dpi: number;
	/** One box per page, in document order. */
	readonly pageSizesPt: readonly PageBox[];
}

/**
 * First page object's id. Object 1 is the catalog and 2 the page tree, so page
 * objects start at 3 and step by THREE: each page is a page object, an image
 * XObject and a content stream.
 *
 * Stepping by two is the trap here. It makes page N's content stream and page
 * N+1's page object share an id, and the result is a quietly corrupt file
 * rather than a loudly broken one.
 */
const PAGE_ID_BASE = 3;

/** Objects contributed per page. */
const OBJECTS_PER_PAGE = 3;

/**
 * PDF syntax that is binary data and must never travel as text.
 *
 * The four marker bytes are above 0x7E. As UTF-8 each would become TWO bytes,
 * shifting every offset after them - which the xref table, written last, would
 * then get wrong for the whole file.
 */
const BINARY_MARKER = "%PDF-1.7\n%\xE2\xE3\xCF\xD3\n";

/** Growable byte buffer that reports how long the file is. */
class ByteWriter {
	#bytes: number[] = [];

	/** Total bytes written; a valid byte offset into the finished file. */
	get length(): number {
		return this.#bytes.length;
	}

	/** Append a Latin-1 string. Correct only for bytes 0x00-0xFF. */
	latin1(text: string): this {
		for (let i = 0; i < text.length; i++) this.#bytes.push(text.charCodeAt(i) & 0xff);
		return this;
	}

	/** Append ASCII text, mapping anything else to '?' rather than passing it. */
	ascii(text: string): this {
		for (let i = 0; i < text.length; i++) {
			const code = text.charCodeAt(i);
			this.#bytes.push(code >= 0x20 && code < 0x7f ? code : 0x3f);
		}
		return this;
	}

	/** Append raw bytes. The single funnel image data passes through. */
	raw(bytes: Uint8Array): this {
		for (let i = 0; i < bytes.length; i++) this.#bytes.push(bytes[i] as number);
		return this;
	}

	/** The finished file. */
	toUint8Array(): Uint8Array {
		return Uint8Array.from(this.#bytes);
	}
}
/**
 * Assemble a print-ready PDF from pages already rasterised to print size.
 *
 * Every object offset is MEASURED as the object is written, never predicted.
 * Prediction is where xref bugs come from: one wrong guess produces a file some
 * readers repair silently and others reject outright.
 *
 * @param pages rasterised pages, in document order
 * @param options DPI the pages were rendered at, and each page's box in points
 * @returns the finished file
 */
export function buildPrintPdf(
	pages: readonly RasterPage[],
	options: PrintPdfOptions,
): Uint8Array {
	// Two objects fixed (catalog, page tree) plus three per page, plus the
	// entry 0 the xref always reserves for its free list.
	const size = 2 + pages.length * OBJECTS_PER_PAGE + 1;

	if (options.pageSizesPt.length !== pages.length) {
		throw new RangeError(
			`${pages.length} raster page(s) but ${options.pageSizesPt.length} page box(es): every \
page needs exactly one, or the MediaBoxes drift onto the wrong pages`,
		);
	}
	if (!(options.dpi > 0) || !Number.isFinite(options.dpi)) {
		throw new RangeError(`cannot build a PDF at ${options.dpi} DPI`);
	}

	const w = new ByteWriter();
	const offsets: number[] = [];
	const begin = (): void => {
		offsets.push(w.length);
	};

	w.raw(Uint8Array.from(BINARY_MARKER, (c) => c.charCodeAt(0) & 0xff));

	// 1: catalog, pointing at the page tree in object 2.
	begin();
	w.latin1("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

	// 2: page tree.
	const kids: string[] = [];
	for (let i = 0; i < pages.length; i++) kids.push(`${PAGE_ID_BASE + i * OBJECTS_PER_PAGE} 0 R`);
	begin();
	w.latin1(`2 0 obj\n<< /Type /Pages /Count ${pages.length} /Kids [${kids.join(" ")}] >>\nendobj\n`);

	for (let i = 0; i < pages.length; i++) {
		const page = pages[i] as RasterPage;
		const box = options.pageSizesPt[i] as PageBox;
		const pageId = PAGE_ID_BASE + i * OBJECTS_PER_PAGE;
		const imageId = pageId + 1;
		const contentId = pageId + 2;

		const expected = page.widthPx * page.heightPx * 3;
		if (page.rgb.length !== expected) {
			// A short buffer written with a /Length that disagrees with the
			// stream is a truncated image: it opens, and prints wrong.
			throw new RangeError(
				`page ${i}: ${page.widthPx}x${page.heightPx} RGB needs ${expected} bytes, got \
${page.rgb.length}`,
			);
		}

		// The page object. MediaBox is POINTS; widthPx is PIXELS. At the page's
		// DPI those are the same physical size, which is exactly what puts a
		// 300 DPI raster back onto a page of its original physical size.
		begin();
		w.latin1(
			`${pageId} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${box.widthPt} ${box.heightPt}] \
/Resources << /XObject << /Im0 ${imageId} 0 R >> >> /Contents ${contentId} 0 R >>\nendobj\n`,
		);

		// The image: DeviceRGB, 8 bits per component, no filter.
		begin();
		w.latin1(
			`${imageId} 0 obj\n<< /Type /XObject /Subtype /Image /Width ${page.widthPx} \
/Height ${page.heightPx} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length ${page.rgb.length} \
>>\nstream\n`,
		);
		w.raw(page.rgb);
		w.latin1("\nendstream\nendobj\n");

		// The content stream: draw the image to fill the page.
		const content = `q ${page.widthPx} 0 0 ${page.heightPx} 0 0 cm /Im0 Do Q`;
		begin();
		w.latin1(
			`${contentId} 0 obj\n<< /Length ${content.length} >>\nstream\n${content}\nendstream\nendobj\n`,
		);
	}

	// The cross-reference table, from measured offsets, written last so that
	// everything it points at already exists.
	const xrefAt = w.length;
	w.latin1(`xref\n0 ${size}\n`);
	// Entry 0 is always free; readers treat it as the head of the free list.
	w.latin1("0000000000 65535 f \n");
	for (const offset of offsets) {
		// Fixed width, zero padded. Readers index into this rather than parse it.
		w.latin1(`${String(offset).padStart(10, "0")} 00000 n \n`);
	}
	w.latin1(`trailer\n<< /Size ${size} /Root 1 0 R >>\nstartxref\n${xrefAt}\n%%EOF\n`);

	return w.toUint8Array();
}