import { HtmlProtocolError, type HtmlWasm, loadHtmlWasm, writeHtml } from "./html-protocol.js";
/**
 * Exporting a PDF to another format — the shared layer.
 *
 * ## What this is
 *
 * The engine has always been able to write a PDF's text out as a Word
 * document, a PowerPoint deck, an Excel workbook, an EPUB book, a CSV of its
 * tables, or a positioned HTML page: `crates/selis-pdf-text` holds the writers
 * and the lazy text chunk puts them on the wire. The tools page has had a
 * card for them since then.
 *
 * The editor has always held a **document**, and an export of a document is
 * not the same thing as an export of a file: the bytes to export are the
 * bytes the reader has been editing. So the editor needs the same writers,
 * driven with its own bytes.
 *
 * This module is that shared layer: one place that knows which chunk answers
 * which format, what each format is called, and what the caller has to
 * supply. The tools page and the editor both call it, so a format is added
 * once.
 *
 * ## What belongs to the core, and what belongs here
 *
 * The WRITERS are the core: Rust, in `crates/selis-pdf-text`, tested there,
 * and reachable from the web app, the desktop app and the SDK through the same
 * chunk. This module is the wire and the names — the part that cannot be
 * shared, because a desktop app has no `fetch` of a chunk URL and an SDK has
 * no DOM.
 */
import {
	loadRasterWasm,
	type RasterFormat,
	type RasterWasm,
	renderPages,
} from "./raster-protocol.js";
import {
	type TextOp,
	type TextOutput,
	type TextWasm,
	loadTextChunk,
	runTextOp,
} from "./text-protocol.js";

/** The formats a PDF can be exported to. */
export type ExportFormat =
	| "word"
	| "slides"
	| "workbook"
	| "html"
	| "epub"
	| "text"
	| "tables"
	| "reflow"
	| "png"
	| "jpg"
	| "tiff"
	| "webp";

/** What the reader is told about a format. */
export interface ExportFormatInfo {
	/** The format's own name. */
	readonly id: ExportFormat;
	/** What the button says. */
	readonly label: string;
	/** What the file is, in a sentence. */
	readonly note: string;
	/** The file's extension, without the dot. */
	readonly extension: string;
	/**
	 * Whether the format needs the page's own image.
	 *
	 * A deck that carries each page as a picture is a deck that looks like
	 * the document; one that carries only the text is a list of text boxes.
	 * The caller supplies the images.
	 */
	readonly needsPageImages: boolean;
}

/** The formats, in the order the panel lists them. */
export const EXPORT_FORMATS: readonly ExportFormatInfo[] = [
	{
		id: "word",
		label: "Word document",
		note: "A .docx with the document's text in reading order.",
		extension: "docx",
		needsPageImages: false,
	},
	{
		id: "slides",
		label: "PowerPoint deck",
		note: "A .pptx with one slide per page, carrying the page's own picture.",
		extension: "pptx",
		needsPageImages: true,
	},
	{
		id: "workbook",
		label: "Excel workbook",
		note: "A .xlsx with one sheet per table the document holds.",
		extension: "xlsx",
		needsPageImages: false,
	},
	{
		id: "html",
		label: "Web page",
		note: "One .html file with the pages as positioned sections.",
		extension: "html",
		needsPageImages: false,
	},
	{
		id: "epub",
		label: "EPUB book",
		note: "An .epub with one chapter per page.",
		extension: "epub",
		needsPageImages: false,
	},
	{
		id: "reflow",
		label: "Reflowed article",
		note: "One .html file that reads as an article rather than as a page.",
		extension: "html",
		needsPageImages: false,
	},
	{
		id: "tables",
		label: "Tables as CSV",
		note: "A .csv of the tables the document holds.",
		extension: "csv",
		needsPageImages: false,
	},
	{
		id: "text",
		label: "Plain text",
		note: "A .txt of the document's text, one separator per page.",
		extension: "txt",
		needsPageImages: false,
	},
	{
		id: "png",
		label: "PNG image",
		note: "A .png of every page, as the page is drawn.",
		extension: "png",
		needsPageImages: false,
	},
	{
		id: "jpg",
		label: "JPEG image",
		note: "A .jpg of every page, as the page is drawn.",
		extension: "jpg",
		needsPageImages: false,
	},
	{
		id: "tiff",
		label: "TIFF image",
		note: "A .tif of every page, as the page is drawn.",
		extension: "tif",
		needsPageImages: false,
	},
	{
		id: "webp",
		label: "WebP image",
		note: "A .webp of every page, as the page is drawn.",
		extension: "webp",
		needsPageImages: false,
	},
];

/**
 * The image formats, and the raster format each one names.
 *
 * The four are the chunk's own: the raster chunk writes PNG, JPEG, TIFF and
 * WebP, and the tools page has a card for each.
 */
const RASTER_FORMAT: Readonly<Partial<Record<ExportFormat, RasterFormat>>> = {
	png: "png",
	jpg: "jpg",
	tiff: "tiff",
	webp: "webp",
};

/**
 * The raster format a format names, when it names one.
 *
 * The image formats are the raster chunk's; every other format belongs to
 * the text chunk or the HTML one.
 */
export function rasterFormatFor(format: ExportFormat): RasterFormat | null {
	return RASTER_FORMAT[format] ?? null;
}

/** The text chunk's operation for a format, when it has one. */
const TEXT_OP: Readonly<Record<ExportFormat, TextOp | null>> = {
	word: "word",
	slides: "slides",
	workbook: "workbook",
	html: null,
	epub: "epub",
	text: "text",
	tables: "tables",
	reflow: "reflow",
	// The image formats are the raster chunk's, not the text chunk's.
	png: null,
	jpg: null,
	tiff: null,
	webp: null,
};

/**
 * The text chunk's operation for a format, or `null` when the format is not
 * the text chunk's.
 *
 * Every format but the web page is the text chunk's: the writers live in
 * `crates/selis-pdf-text`, and the chunk is how a page reaches them.
 */
export function textOpFor(format: ExportFormat): TextOp | null {
	return TEXT_OP[format] ?? null;
}

/** One exported file. */
export interface ExportFile {
	/** The file's name. */
	readonly name: string;
	/** What the file is: a package, a text file, a web page, a table. */
	readonly kind: "file" | "text" | "report" | "zip" | "html" | "tables";
	/** The file's bytes. */
	readonly bytes: Uint8Array;
}

/** What an export needs. */
export interface ExportRequest {
	/** Which format to write. */
	readonly format: ExportFormat;
	/** The document's bytes — the edited ones, when the caller has them. */
	readonly bytes: Uint8Array;
	/** The document's name, with or without its extension. */
	readonly name: string;
	/** An optional page selection, in the engine's own syntax. */
	readonly pages?: string;
	/** The page's own images, base64 PNGs, when the format needs them. */
	readonly slideImages?: readonly string[];
}

/** The chunk loaders, injected so a test can supply its own. */
export interface ExportDeps {
	/** Load the text chunk. */
	loadText?: () => Promise<TextWasm>;
	/** Load the HTML chunk. */
	loadHtml?: () => Promise<HtmlWasm>;
	/** Load the raster chunk. */
	loadRaster?: () => Promise<RasterWasm>;
}

/** The default loaders: the chunks the page serves. */
const DEFAULT_DEPS: Required<ExportDeps> = {
	loadText: () => loadTextChunk(),
	loadHtml: () => loadHtmlWasm(),
	loadRaster: () => loadRasterWasm(),
};

/**
 * Write the document out in another format.
 *
 * The document's bytes are read on this device and the result is returned as
 * bytes; nothing is uploaded, and the caller decides what to do with the
 * file.
 */
export async function exportDocument(
	request: ExportRequest,
	deps: Partial<ExportDeps> = {},
): Promise<readonly ExportFile[]> {
	const loaders = { ...DEFAULT_DEPS, ...deps };
	const stem = request.name.replace(/\.[^.]+$/, "");
	const op = TEXT_OP[request.format];

	if (op !== null) {
		const wasm = await loaders.loadText();
		const options: Record<string, string | string[]> = { name: request.name, title: stem };
		if (request.pages !== undefined && request.pages !== "") {
			options.pages = request.pages;
		}
		if (request.slideImages !== undefined) {
			options.slideImages = [...request.slideImages];
		}
		const outputs: readonly TextOutput[] = runTextOp(wasm, request.bytes, op, options);
		return outputs.map((output) => ({
			name: output.name,
			kind: output.kind,
			bytes: output.bytes,
		}));
	}

	const raster = RASTER_FORMAT[request.format];
	if (raster !== undefined) {
		const wasm = await loaders.loadRaster();
		const rendered = renderPages(wasm, {
			bytes: request.bytes,
			name: request.name,
			format: raster,
			...(request.pages === undefined ? {} : { pages: request.pages }),
		});
		return [{ name: rendered.name, kind: "file", bytes: rendered.bytes }];
	}

	if (request.format === "html") {
		const wasm = await loaders.loadHtml();
		const written = writeHtml(wasm, request.bytes, stem, request.pages, stem);
		return [{ name: written.name, kind: "html", bytes: written.bytes }];
	}

	throw new HtmlProtocolError(`the format ${request.format} has no writer`);
}

/** The format with this id, or `null` when there is none. */
export function exportFormat(id: string): ExportFormatInfo | null {
	return EXPORT_FORMATS.find((format) => format.id === id) ?? null;
}
