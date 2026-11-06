/**
 * The tool-first page (SL-4.TOOLS.02).
 *
 * Every browser-local operation, one page. The only fetch this module makes is
 * the same-origin lazy WASM chunk it was built to drive; every document byte is
 * read with `File.arrayBuffer()` and written back through a Blob URL. There is
 * no form action, no telemetry, and no remote endpoint to configure.
 *
 * The page is deliberately bundle-free: this is `tsc` output loaded directly by
 * `tools.html`, exactly like `boot.js` and the viewer modules. Dynamic DOM is
 * built with `createElement`/`textContent`; no input value is ever interpreted
 * as HTML.
 */

import { installRequestRecorder } from "../no-upload.js";
import { registerServiceWorker } from "../sw-register.js";
import {
	ToolInputError,
	hasImageHeader,
	hasPdfHeader,
	pageBoundary,
	pageCount,
	pageIndex,
	shiftPageSelection,
	sizeBytes,
} from "./inputs.js";
import {
	type BatchSummary,
	type ToolOp,
	type ToolOutput,
	type ToolOutputReport,
	ToolProtocolError,
	type ToolRequest,
	type ToolsWasm,
	type Verification,
	type VerificationRow,
	type WireFile,
	formatBytes,
	loadTools,
	runTool,
	verificationRows,
} from "./protocol.js";
import {
	SECURITY_WASM_URL,
	type SecurityRequest,
	type SecurityResult,
	type SecurityWasm,
	instantiateSecurity,
	randomKeyMaterial,
	runSecurity,
	securityRows,
} from "./security-protocol.js";
import {
	SIGN_WASM_URL,
	type SignRequest,
	type SignWasm,
	instantiateSign,
	runSign,
	signRows,
	signatureRows,
	signingStamp,
} from "./sign-protocol.js";
import {
	type TextOp,
	type TextOutput,
	TextProtocolError,
	type TextWasm,
	loadTextChunk,
	runTextOp,
} from "./text-protocol.js";

interface ConvertWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_convert_alloc(len: number): number;
		selis_convert_free(ptr: number, len: number): void;
		selis_convert_run(
			format: number,
			inputPtr: number,
			inputLen: number,
			pageSize: number,
			titlePtr: number,
			titleLen: number,
			outLen: number,
			outPages: number,
		): number;
		selis_convert_last_error(outLen: number): number;
		readonly selis_convert_capabilities: () => number;
	};
}

interface RasterWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_raster_alloc(len: number): number;
		selis_raster_free(ptr: number, len: number): void;
		selis_raster_run(
			inputPtr: number,
			inputLen: number,
			pagesPtr: number,
			pagesLen: number,
			dpi: number,
			format: number,
			quality: number,
			outLen: number,
			outFiles: number,
		): number;
		selis_raster_last_error(outLen: number): number;
		readonly selis_raster_available: () => number;
	};
}

interface HtmlWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_html_alloc(len: number): number;
		selis_html_free(ptr: number, len: number): void;
		selis_html_run(
			inputPtr: number,
			inputLen: number,
			pagesPtr: number,
			pagesLen: number,
			titlePtr: number,
			titleLen: number,
			outLen: number,
			outPages: number,
		): number;
		selis_html_last_error(outLen: number): number;
		readonly selis_html_available: () => number;
	};
}

/** Every card the page can open: a tools-chunk op or a lazy-chunk tool. */
type ToolId =
	| ToolOp
	| "convert"
	| "png"
	| "jpg"
	| "tiff"
	| "webp"
	| "html"
	| "text"
	| "unlock"
	| "protect"
	| "clear_permissions"
	| "permissions"
	| "sign"
	| "certificate"
	| "workflow";

/** One AcroForm field as the read op reports it. */
interface FormFieldJson {
	readonly name: string;
	readonly kind: "text" | "checkbox" | "radio" | "choice" | "signature" | "push_button" | "unknown";
	readonly value?: string;
	readonly options?: readonly string[];
	readonly labels?: readonly string[];
	readonly editable?: boolean;
	readonly multi_select?: boolean;
	readonly max_len?: number;
	readonly states?: readonly string[];
	readonly read_only?: boolean;
	readonly required?: boolean;
	readonly page?: number;
}

/** The read op's report: what the page needs to render the form. */
interface FormFieldsJson {
	readonly status: string;
	readonly pages?: number;
	readonly has_signature: boolean;
	readonly fields: readonly FormFieldJson[];
}

/** The cards that hand their document to the lazy security chunk. */
type SecurityToolId = "unlock" | "protect" | "clear_permissions" | "permissions";

/** The cards that hand their pages to the lazy raster chunk. */
type RasterToolId = "png" | "jpg" | "tiff" | "webp";

/**
 * The sections the card grid is organised into. Forty-two cards in one flat
 * list is not a menu, it is a wall: the groups are named after what a reader
 * has come to *do*, in the order the operations build on each other — arrange
 * the pages, change what they are, mark them up, then decide who may open
 * them. `TOOL_GROUPS` is the display order; a card with no group is a bug, so
 * the type makes the field required rather than optional.
 */
type ToolGroup =
	| "organise"
	| "convert"
	| "stamp"
	| "metadata"
	| "security"
	| "attach"
	| "repair"
	| "bundle";

const TOOL_GROUPS: readonly {
	readonly id: ToolGroup;
	readonly label: string;
	readonly note: string;
}[] = [
	{ id: "organise", label: "Organise pages", note: "Put pages in the order you want them." },
	{
		id: "convert",
		label: "Convert",
		note: "Change a PDF into another format, or another format into a PDF.",
	},
	{
		id: "stamp",
		label: "Stamp & annotate",
		note: "Write numbers, marks and bookmarks onto the pages.",
	},
	{
		id: "metadata",
		label: "Metadata",
		note: "Read the document’s own description, edit it, or remove it.",
	},
	{
		id: "security",
		label: "Security & signatures",
		note: "Passwords, permissions, forms and signatures.",
	},
	{ id: "attach", label: "Attachments", note: "Files carried inside the document." },
	{ id: "repair", label: "Optimise & repair", note: "Make it smaller, or make it open again." },
	{
		id: "bundle",
		label: "Batch & archive",
		note: "Run one tool over many files, or collect them into a ZIP.",
	},
];

interface ToolDef {
	readonly id: ToolId;
	readonly group: ToolGroup;
	readonly mark: string;
	readonly title: string;
	readonly blurb: string;
	readonly verb: string;
	readonly multiple: boolean;
	/** The fewest files a run needs; defaults to 2 for multi-file tools. */
	readonly minimum?: number;
	/**
	 * The input containers this tool accepts; defaults to PDF. `pdf-image`
	 * takes exactly one PDF plus one PNG/JPEG watermark image.
	 */
	readonly accepts?: "pdf" | "image" | "text" | "pdf-image";
}

const TOOLS: readonly ToolDef[] = [
	{
		id: "merge",
		group: "organise",
		mark: "⇄",
		title: "Merge PDF",
		blurb: "Combine several PDFs into one, in the order you choose.",
		verb: "Merge PDFs",
		multiple: true,
	},
	{
		id: "mix",
		group: "organise",
		mark: "⇆",
		title: "Mix pages",
		blurb: "Interleave pages from two or more PDFs, round-robin, into one document.",
		verb: "Mix pages",
		multiple: true,
	},
	{
		id: "split",
		group: "organise",
		mark: "✂",
		title: "Split PDF",
		blurb: "Split by range, every N pages, in half, by size, by bookmarks, or extract pages.",
		verb: "Split PDF",
		multiple: false,
	},
	{
		id: "rotate",
		group: "organise",
		mark: "↻",
		title: "Rotate PDF",
		blurb: "Turn all pages or a selection by 90, 180, or 270 degrees.",
		verb: "Rotate PDF",
		multiple: false,
	},
	{
		id: "crop",
		group: "organise",
		mark: "⌗",
		title: "Crop PDF",
		blurb: "Trim page edges by points, measured in the page's displayed orientation.",
		verb: "Crop PDF",
		multiple: false,
	},
	{
		id: "flip",
		group: "organise",
		mark: "⇋",
		title: "Flip PDF",
		blurb: "Mirror page content horizontally or vertically.",
		verb: "Flip PDF",
		multiple: false,
	},
	{
		id: "page_labels",
		group: "organise",
		mark: "L",
		title: "Page labels",
		blurb: "Set the numbering readers show: decimal, roman, letters, or a prefix.",
		verb: "Apply page labels",
		multiple: false,
	},
	{
		id: "resize",
		group: "organise",
		mark: "⤢",
		title: "Resize pages",
		blurb: "Scale pages to A4, Letter, or a custom size in points.",
		verb: "Resize pages",
		multiple: false,
	},
	{
		id: "nup",
		group: "organise",
		mark: "▦",
		title: "N-up pages",
		blurb: "Place 2, 4, 6, or 9 pages on each sheet.",
		verb: "Place pages",
		multiple: false,
	},
	{
		id: "delete",
		group: "organise",
		mark: "⌫",
		title: "Delete pages",
		blurb: "Remove the pages you no longer need.",
		verb: "Delete pages",
		multiple: false,
	},
	{
		id: "reorder",
		group: "organise",
		mark: "⇅",
		title: "Organize pages",
		blurb: "Reorder pages, duplicate pages, or insert blank pages.",
		verb: "Organize pages",
		multiple: false,
	},
	{
		id: "metadata_report",
		group: "metadata",
		mark: "ℹ",
		title: "Read metadata",
		blurb: "Export the Info dictionary, XMP packet, and structure as JSON.",
		verb: "Read metadata",
		multiple: false,
	},
	{
		id: "metadata",
		group: "metadata",
		mark: "Aa",
		title: "Edit metadata",
		blurb: "Set the title, author, subject, and keywords.",
		verb: "Save metadata",
		multiple: false,
	},
	{
		id: "strip_metadata",
		group: "metadata",
		mark: "×",
		title: "Remove metadata",
		blurb: "Strip the Info dictionary and the XMP packet from the document.",
		verb: "Remove metadata",
		multiple: false,
	},
	{
		id: "page_numbers",
		group: "stamp",
		mark: "1",
		title: "Add page numbers",
		blurb: "Stamp page numbers on every page or a selection.",
		verb: "Add page numbers",
		multiple: false,
	},
	{
		id: "watermark",
		group: "stamp",
		mark: "W",
		title: "Watermark PDF",
		blurb: "Add a translucent diagonal or horizontal text watermark.",
		verb: "Add watermark",
		multiple: false,
	},
	{
		id: "image_watermark",
		group: "stamp",
		mark: "IW",
		title: "Image watermark",
		blurb: "Stamp a PNG or JPEG logo or seal across the pages you choose.",
		verb: "Add image watermark",
		multiple: true,
		minimum: 2,
		accepts: "pdf-image",
	},
	{
		id: "outline",
		group: "stamp",
		mark: "BM",
		title: "Edit bookmarks",
		blurb: "Add, rename, retarget, or remove the document's bookmarks.",
		verb: "Save bookmarks",
		multiple: false,
	},
	{
		id: "header_footer",
		group: "stamp",
		mark: "HF",
		title: "Header & footer",
		blurb: "Add header and footer lines with page placeholders.",
		verb: "Add header & footer",
		multiple: false,
	},
	{
		id: "bates",
		group: "stamp",
		mark: "B",
		title: "Bates numbering",
		blurb: "Apply sequential legal-style numbers with a prefix and padding.",
		verb: "Add Bates numbers",
		multiple: false,
	},
	{
		id: "compress",
		group: "repair",
		mark: "Z",
		title: "Compress PDF",
		blurb: "Lossless cleanup: drop unreachable objects and re-encode streams.",
		verb: "Compress PDF",
		multiple: false,
	},
	{
		id: "attachments",
		group: "attach",
		mark: "A",
		title: "Extract attachments",
		blurb: "Download the files embedded inside a PDF, individually or as a ZIP.",
		verb: "Extract attachments",
		multiple: false,
	},
	{
		id: "attach",
		group: "attach",
		mark: "＋",
		title: "Embed files",
		blurb: "Attach files inside a PDF so they travel with it.",
		verb: "Embed files",
		multiple: true,
	},
	{
		id: "detach",
		group: "attach",
		mark: "－",
		title: "Remove attachments",
		blurb: "Delete every file embedded inside a PDF.",
		verb: "Remove attachments",
		multiple: false,
	},
	{
		id: "repair",
		group: "repair",
		mark: "R",
		title: "Repair PDF",
		blurb: "Recover the pages of a damaged PDF into a clean, consistent file.",
		verb: "Repair PDF",
		multiple: false,
	},
	{
		id: "batch",
		group: "bundle",
		mark: "≡",
		title: "Batch queue",
		blurb: "Run one tool over several PDFs; each file gets its own verified result.",
		verb: "Run the queue",
		multiple: true,
		minimum: 2,
		accepts: "pdf",
	},
	{
		id: "image_to_pdf",
		group: "convert",
		mark: "▣",
		title: "Image to PDF",
		blurb: "Turn PNG and JPEG images into a PDF, one page per image.",
		verb: "Create PDF",
		multiple: true,
		minimum: 1,
		accepts: "image",
	},
	{
		id: "convert",
		group: "convert",
		mark: "▾",
		title: "Convert to PDF",
		blurb: "Turn Markdown, HTML, CSV, LaTeX, SVG or EPUB into a PDF.",
		verb: "Convert to PDF",
		multiple: false,
		accepts: "text",
	},
	{
		id: "png",
		group: "convert",
		mark: "▧",
		title: "PDF to PNG",
		blurb: "Render every page, or a selection, to lossless PNG images.",
		verb: "Render PNGs",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "jpg",
		group: "convert",
		mark: "▨",
		title: "PDF to JPG",
		blurb: "Render every page, or a selection, to compact JPEG images.",
		verb: "Render JPGs",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "tiff",
		group: "convert",
		mark: "▤",
		title: "PDF to TIFF",
		blurb: "Render every page, or a selection, into one lossless multi-page TIFF.",
		verb: "Render TIFF",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "webp",
		group: "convert",
		mark: "▥",
		title: "PDF to WebP",
		blurb: "Render every page, or a selection, to lossless WebP images.",
		verb: "Render WebPs",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "html",
		group: "convert",
		mark: "◩",
		title: "PDF to HTML",
		blurb: "Write every page, or a selection, as one positioned HTML document.",
		verb: "Export HTML",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "text",
		group: "convert",
		mark: "¶",
		title: "Extract text",
		blurb:
			"Recover the document's text, its tables, a reading-order article, a Word document, a PowerPoint deck, or an Excel workbook.",
		verb: "Extract text",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "zip",
		group: "bundle",
		mark: "▣",
		title: "ZIP files",
		blurb: "Bundle several files into one deterministic ZIP.",
		verb: "Create ZIP",
		multiple: true,
		minimum: 2,
		accepts: "pdf",
	},
	{
		id: "fill_forms",
		group: "security",
		mark: "☑",
		title: "Fill forms",
		blurb: "Read an AcroForm, fill its fields, and save the filled PDF.",
		verb: "Fill form",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "unlock",
		group: "security",
		mark: "◇",
		title: "Remove password",
		blurb: "Open a protected PDF with its password and save it without one.",
		verb: "Remove password",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "protect",
		group: "security",
		mark: "◆",
		title: "Protect with password",
		blurb: "Encrypt a PDF with AES-256 and choose what a reader may do with it.",
		verb: "Protect PDF",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "clear_permissions",
		group: "security",
		mark: "◈",
		title: "Clear permissions",
		blurb: "Lift the print, copy, modify and annotate restrictions, with the owner password.",
		verb: "Clear permissions",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "permissions",
		group: "security",
		mark: "◉",
		title: "Set permissions",
		blurb: "Write the permission bits a document should carry, guarded by an owner password.",
		verb: "Set permissions",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "sign",
		group: "security",
		mark: "✍",
		title: "Sign PDF",
		blurb: "Draw a typed or freehand stamp, or sign with a key that never leaves this device.",
		verb: "Sign PDF",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "certificate",
		group: "security",
		mark: "◊",
		title: "Certificate validation",
		blurb: "Read every signature and say whether its chain ends at a certificate you trust.",
		verb: "Validate signatures",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "workflow",
		group: "security",
		mark: "⤳",
		title: "Approval workflow",
		blurb: "Record an approval decision on the document itself, with no server in the loop.",
		verb: "Record approval",
		multiple: false,
		accepts: "pdf",
	},
];

const TOOL_BY_ID = new Map<ToolId, ToolDef>(TOOLS.map((tool) => [tool.id, tool]));

/** Browser memory is finite; this keeps one run comfortably copyable. */
const MAX_TOTAL_BYTES = 256 * 1024 * 1024;

/**
 * Where a signature block lands, in display points from the bottom-left. The
 * default sits at the bottom-left of the first page, the position the CLI's
 * `--rect` default uses, so the two surfaces put the same mark in the same
 * place.
 */
const SIGNATURE_RECT: readonly [number, number, number, number] = [36, 36, 256, 106];

/** How the page hands one raster format to the browser. */
interface RasterInfo {
	readonly mime: string;
	readonly label: string;
	readonly preview: boolean;
}

/**
 * A raster output's own suffix decides its MIME, the word its row uses and
 * whether a preview is offered at all: Chromium does not display TIFF, so a
 * `.tif` says download and nothing more. A suffix this page never writes gets
 * an opaque type and no preview rather than a claim about a format it cannot
 * name.
 */
const RASTERS: ReadonlyMap<string, RasterInfo> = new Map([
	["png", { mime: "image/png", label: "PNG image", preview: true }],
	["jpg", { mime: "image/jpeg", label: "JPEG image", preview: true }],
	["tif", { mime: "image/tiff", label: "TIFF image", preview: false }],
	["webp", { mime: "image/webp", label: "WebP image", preview: true }],
]);

/**
 * What each raster card asks the chunk for: the ABI's format number and the
 * suffix its files get. The chunk numbers its writers 0..3 and the page must
 * agree with it, so the table is the one place the two are tied together.
 */
const RASTER_FORMATS: Record<RasterToolId, { code: number; extension: string }> = {
	png: { code: 0, extension: "png" },
	jpg: { code: 1, extension: "jpg" },
	tiff: { code: 2, extension: "tif" },
	webp: { code: 3, extension: "webp" },
};

const UNNAMED_RASTER: RasterInfo = {
	mime: "application/octet-stream",
	label: "image",
	preview: false,
};

function rasterInfo(name: string): RasterInfo {
	const dot = name.lastIndexOf(".");
	const suffix = dot < 0 ? "" : name.slice(dot + 1).toLowerCase();
	return RASTERS.get(suffix) ?? UNNAMED_RASTER;
}

type Control = HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement;
type StatusKind = "info" | "error" | "ok";
type SplitMode = "range" | "every" | "half" | "size" | "bookmarks" | "extract";
type OrganizeMode = "reorder" | "duplicate" | "blank";

function byId<T extends HTMLElement>(id: string): T {
	const element = document.getElementById(id);
	if (element === null) {
		throw new Error(`tools page is missing #${id}`);
	}
	return element as T;
}

function make<K extends keyof HTMLElementTagNameMap>(
	tag: K,
	className?: string,
): HTMLElementTagNameMap[K] {
	const element = document.createElement(tag);
	if (className !== undefined) {
		element.className = className;
	}
	return element;
}

function button(label: string, className: string): HTMLButtonElement {
	const element = make("button", className);
	element.type = "button";
	element.textContent = label;
	return element;
}

function nextPaint(): Promise<void> {
	return new Promise((resolve) => {
		requestAnimationFrame(() => resolve());
	});
}

function openBlobUrl(url: string): void {
	const anchor = document.createElement("a");
	anchor.href = url;
	anchor.target = "_blank";
	anchor.rel = "noopener";
	anchor.textContent = "Open PDF";
	document.body.append(anchor);
	anchor.click();
	anchor.remove();
}

/** How many `/Info` fields the card lists before pointing at the JSON file. */
const MAX_REPORT_FIELDS = 10;
/** How long one reported value may get before the card shortens it. */
const MAX_REPORT_VALUE = 120;

/**
 * The metadata report the engine exports. Only what the card shows is typed;
 * the downloaded JSON carries every field.
 */
interface MetadataReportJson {
	readonly status?: string;
	readonly pdf_version?: string;
	readonly revisions?: number;
	readonly page_size?: { readonly width?: number; readonly height?: number };
	readonly info?: readonly { readonly key?: string; readonly value?: string }[];
	readonly xmp?: {
		readonly present?: boolean;
		readonly decoded?: boolean;
		readonly bytes?: number;
		readonly truncated?: boolean;
		readonly filter?: string;
	};
}

/** One XMP packet, said the way it was found. */
function xmpText(packet: NonNullable<MetadataReportJson["xmp"]>): string {
	if (packet.present !== true) {
		return "none";
	}
	const held = typeof packet.bytes === "number" ? formatBytes(packet.bytes) : "size unknown";
	if (packet.decoded === true) {
		return packet.truncated === true
			? `decoded, capped in the export | ${held}`
			: `decoded | ${held}`;
	}
	return `held, not decoded (${packet.filter ?? "unknown filter"}) | ${held}`;
}

/**
 * The rows a metadata report adds to the result card. The downloadable JSON
 * holds everything; these are the facts a reader acts on, with the packet
 * described rather than printed and a bounded number of fields so a hostile
 * dictionary cannot grow the page.
 */
function metadataRows(report: MetadataReportJson): VerificationRow[] {
	if (report.status === "encrypted") {
		return [
			{
				label: "Metadata",
				value: "encrypted — every dictionary is still ciphertext, so nothing is shown",
			},
		];
	}
	const rows: VerificationRow[] = [];
	if (report.pdf_version !== undefined && report.pdf_version !== "") {
		rows.push({ label: "PDF version", value: report.pdf_version });
	}
	if (report.revisions !== undefined) {
		rows.push({ label: "Revisions", value: String(report.revisions) });
	}
	const box = report.page_size;
	if (box?.width !== undefined && box?.height !== undefined) {
		rows.push({ label: "First page", value: `${box.width} × ${box.height} pt` });
	}
	const fields = report.info ?? [];
	for (const field of fields.slice(0, MAX_REPORT_FIELDS)) {
		if (field.key !== undefined && field.value !== undefined) {
			const value = field.value;
			rows.push({
				label: field.key,
				value: value.length > MAX_REPORT_VALUE ? `${value.slice(0, MAX_REPORT_VALUE)}…` : value,
			});
		}
	}
	if (fields.length > MAX_REPORT_FIELDS) {
		rows.push({
			label: "More fields",
			value: `${fields.length - MAX_REPORT_FIELDS} in the JSON file`,
		});
	}
	if (report.xmp !== undefined) {
		rows.push({ label: "XMP packet", value: xmpText(report.xmp) });
	}
	return rows;
}

/** The report rows of one run, when the run produced a report at all. */
function reportRows(
	reports: readonly ToolOutputReport[],
	outputs: readonly ToolOutput[],
): VerificationRow[] {
	const report = reports.find((item) => item.kind === "report");
	if (report === undefined) {
		return [];
	}
	const output = outputs.find((item) => item.name === report.name);
	if (output === undefined) {
		return [];
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(new TextDecoder().decode(output.bytes));
	} catch {
		return [{ label: "Metadata", value: "the report could not be read" }];
	}
	if (typeof parsed !== "object" || parsed === null) {
		return [{ label: "Metadata", value: "the report held no object" }];
	}
	// The Phase 3 reports are not metadata reports: each names its own schema,
	// and the card shows the facts a reader acts on rather than dumping JSON.
	const schema = (parsed as { schema?: unknown }).schema;
	if (schema === "selis-extract-quality/1") {
		return textQualityRows(parsed as TextQualityJson);
	}
	if (schema === "selis-tables/1") {
		const report = parsed as TablesReportJson;
		return [
			{ label: "Tables found", value: String(report.tables ?? 0) },
			{ label: "Rows", value: String(report.rows ?? 0) },
			{
				label: "Lowest confidence",
				value: typeof report.min_confidence === "number" ? report.min_confidence.toFixed(2) : "—",
			},
		];
	}
	if (schema === "selis-split-text/1") {
		const report = parsed as SplitTextJson;
		const pages = (report.matched_pages ?? []).join(", ");
		return [
			{ label: "Rule", value: report.rule ?? "a new part starts at each matching page" },
			{ label: "Matched pages", value: pages === "" ? "none" : pages },
			{ label: "Parts", value: String((report.parts ?? []).length) },
		];
	}
	return metadataRows(parsed as MetadataReportJson);
}

/** The text export's quality report, as the chunk writes it. */
interface TextQualityJson {
	readonly verdict?: string;
	readonly pages?: number;
	readonly characters?: number;
	readonly lines?: number;
	readonly blank_pages?: number;
	readonly pages_with_text?: number;
	readonly low_confidence_pages?: number;
}

/** The table report's shape. */
interface TablesReportJson {
	readonly tables?: number;
	readonly rows?: number;
	readonly min_confidence?: number;
}

/** The split report's shape. */
interface SplitTextJson {
	readonly rule?: string;
	readonly matched_pages?: readonly number[];
	readonly parts?: readonly unknown[];
}

/**
 * The rows a text export shows: what was recovered, and the two facts that make
 * the verdict honest — pages that recovered nothing, and pages that drew text
 * the extractor could not read.
 */
function textQualityRows(report: TextQualityJson): VerificationRow[] {
	return [
		{ label: "Verdict", value: report.verdict ?? "unknown" },
		{ label: "Pages", value: String(report.pages ?? 0) },
		{ label: "Characters", value: String(report.characters ?? 0) },
		{ label: "Lines", value: String(report.lines ?? 0) },
		{ label: "Pages with text", value: String(report.pages_with_text ?? 0) },
		{
			label: "Low confidence",
			value:
				(report.low_confidence_pages ?? 0) === 0
					? "no page lost its text"
					: `${report.low_confidence_pages ?? 0} page(s) drew text nothing could read`,
		},
	];
}

/** The longest failure the queue prints in a row; the full text is in the log. */
const MAX_BATCH_VALUE = 120;

/**
 * The rows a batch queue adds to the result card: one per file the user
 * picked, in the order they picked them. A file that failed is shown as a
 * file that failed, with its reason, because a queue that only lists its
 * successes would be reporting a run the user did not ask for.
 */
function batchRows(batch: BatchSummary | undefined): VerificationRow[] {
	if (batch === undefined) {
		return [];
	}
	const rows: VerificationRow[] = [
		{
			label: "Queue",
			value: `${batch.tool} · ${batch.ok} of ${batch.total} file${batch.total === 1 ? "" : "s"}${
				batch.failed === 0 ? "" : ` · ${batch.failed} not processed`
			}`,
		},
	];
	for (const file of batch.files) {
		if (file.status === "ok") {
			rows.push({
				label: file.input,
				value: `${file.name ?? "a result"} · ${file.pages ?? 0} page${
					file.pages === 1 ? "" : "s"
				} · ${file.verdict ?? "ok"}`,
			});
			continue;
		}
		const reason = file.error ?? "this file could not be processed";
		rows.push({
			label: file.input,
			value: `not processed — ${reason.length > MAX_BATCH_VALUE ? `${reason.slice(0, MAX_BATCH_VALUE)}…` : reason}`,
		});
	}
	return rows;
}

function init(): void {
	const toolsSection = byId<HTMLElement>("tools");
	const toolGrid = byId<HTMLDivElement>("tool-grid");
	const workspace = byId<HTMLElement>("workspace");
	const backButton = byId<HTMLButtonElement>("back-to-tools");
	const workspaceTitle = byId<HTMLHeadingElement>("workspace-title");
	const workspaceBlurb = byId<HTMLParagraphElement>("workspace-blurb");
	const dropzone = byId<HTMLDivElement>("dropzone");
	const fileInput = byId<HTMLInputElement>("file-input");
	const fileList = byId<HTMLUListElement>("file-list");
	const options = byId<HTMLDivElement>("options");
	const runButton = byId<HTMLButtonElement>("run-tool");
	const status = byId<HTMLParagraphElement>("status");
	const resultCard = byId<HTMLElement>("result");
	const resultTitle = byId<HTMLHeadingElement>("result-title");
	const resultMeta = byId<HTMLParagraphElement>("result-meta");
	const resultOutputs = byId<HTMLUListElement>("result-outputs");
	const resultRows = byId<HTMLDListElement>("result-rows");
	const againButton = byId<HTMLButtonElement>("result-again");
	// The page is local-only by construction (CSP connect-src 'self', no form
	// action, no remote literals). This recorder is the mechanical check behind
	// the claim: after every operation it asserts that no request carried any of
	// the input bytes, and only then is the result shown. `data-no-upload`
	// exposes the outcome to the browser check without exposing document data.
	const requestRecorder = installRequestRecorder();
	document.documentElement.dataset.noUpload = "watching";

	let selectedTool: ToolId | null = null;
	let files: File[] = [];
	let draggedFileIndex: number | null = null;
	let busy = false;
	let wasmPromise: Promise<ToolsWasm> | null = null;
	let resultUrls: string[] = [];
	const optionControls = new Map<string, Control>();
	const optionFields = new Map<string, HTMLElement>();
	/**
	 * The field whose value travels in the request, with the getter for the
	 * control the user edits. Rebuilt every time the fields list is read;
	 * empty until a form PDF has been read.
	 */
	let formFieldControls: Array<{ readonly name: string; readonly value: () => string }> = [];

	function currentTool(): ToolDef | null {
		return selectedTool === null ? null : (TOOL_BY_ID.get(selectedTool) ?? null);
	}

	function setStatus(message: string, kind: StatusKind = "info"): void {
		status.textContent = message;
		status.dataset.kind = kind;
		status.hidden = message === "";
	}

	function setBusy(value: boolean): void {
		busy = value;
		dropzone.classList.toggle("is-busy", value);
		for (const element of optionControls.values()) {
			element.disabled = value;
		}
		updateRunState();
	}

	function clearResult(): void {
		for (const url of resultUrls) {
			URL.revokeObjectURL(url);
		}
		resultUrls = [];
		resultCard.hidden = true;
		resultOutputs.replaceChildren();
		resultRows.replaceChildren();
	}

	function updateRunState(): void {
		const tool = currentTool();
		if (tool === null) {
			runButton.disabled = true;
			return;
		}
		const needed = tool.minimum ?? (tool.multiple ? 2 : 1);
		runButton.disabled = busy || files.length < needed;
		runButton.textContent = busy ? "Working locally…" : tool.verb;
	}

	function toolCard(tool: ToolDef): HTMLElement {
		{
			const card = button("", "tool-card");
			card.dataset.tool = tool.id;
			card.append(
				(() => {
					const mark = make("span", "tool-mark");
					mark.textContent = tool.mark;
					mark.setAttribute("aria-hidden", "true");
					return mark;
				})(),
				(() => {
					const title = make("span", "tool-title");
					title.textContent = tool.title;
					return title;
				})(),
				(() => {
					const blurb = make("span", "tool-blurb");
					blurb.textContent = tool.blurb;
					return blurb;
				})(),
				(() => {
					const meta = make("span", "tool-meta");
					meta.textContent = "Runs on this device →";
					return meta;
				})(),
			);
			card.addEventListener("click", () => {
				selectTool(tool.id);
			});
			return card;
		}
	}

	/**
	 * The grid is one section per group, in `TOOL_GROUPS` order. A tool whose
	 * group is missing from that list would silently disappear, so the count is
	 * checked before anything is rendered.
	 */
	function renderToolGrid(): void {
		const sections = TOOL_GROUPS.map((group) => {
			const section = make("section", "tool-group");
			section.id = `tool-group-${group.id}`;
			const head = make("div", "tool-group-head");
			const title = make("h2", "tool-group-title");
			title.textContent = group.label;
			const note = make("p", "tool-group-note");
			note.textContent = group.note;
			head.append(title, note);
			const grid = make("div", "tool-grid");
			grid.append(...TOOLS.filter((tool) => tool.group === group.id).map(toolCard));
			section.append(head, grid);
			return section;
		});
		toolGrid.replaceChildren(...sections);
		const dark = toolGrid.querySelectorAll(".tool-card").length;
		if (dark !== TOOLS.length) {
			throw new Error(`tool grid rendered ${dark} of ${TOOLS.length} cards`);
		}
	}

	function addField(key: string, labelText: string, control: Control, hint?: string): Control {
		control.id = `option-${key}`;
		optionControls.set(key, control);
		const field = make("div", "field");
		field.dataset.option = key;
		optionFields.set(key, field);
		const label = make("label", "field-label");
		label.htmlFor = control.id;
		label.textContent = labelText;
		field.append(label, control);
		if (hint !== undefined) {
			const note = make("p", "field-hint");
			note.textContent = hint;
			field.append(note);
		}
		options.append(field);
		control.addEventListener("input", () => {
			clearResult();
		});
		control.addEventListener("change", () => {
			clearResult();
		});
		return control;
	}

	function textInput(
		key: string,
		type: "text" | "number" | "password",
		placeholder: string,
	): HTMLInputElement {
		const input = make("input", "control");
		input.type = type;
		input.name = key;
		input.placeholder = placeholder;
		// "off" everywhere, and "new-password" for a password: this page has no
		// account and no server, so a browser that offered to remember, autofill,
		// or generate a value here would be proposing to keep a document's key
		// somewhere the user never chose to keep it.
		input.autocomplete = type === "password" ? "new-password" : "off";
		input.spellcheck = false;
		if (type === "number") {
			input.min = "1";
			input.step = "1";
		}
		return input;
	}

	function checkInput(key: string): HTMLInputElement {
		const input = make("input", "control");
		input.type = "checkbox";
		input.name = key;
		return input;
	}

	/**
	 * A local file the card reads in the tab (a key, a certificate, or the
	 * anchors a report trusts). The bytes are read with `File.text()` and travel
	 * only into the signing chunk's own memory.
	 */
	function fileChooser(key: string, accept: string, multiple: boolean): HTMLInputElement {
		const input = make("input", "control");
		input.type = "file";
		input.name = key;
		input.accept = accept;
		input.multiple = multiple;
		return input;
	}

	function numberInRange(
		key: string,
		label: string,
		minimum: number,
		maximum: number,
		fallback: number,
		integer = false,
	): number {
		const raw = optionValue(key).trim();
		if (raw === "") {
			return fallback;
		}
		const value = Number(raw);
		if (
			!Number.isFinite(value) ||
			value < minimum ||
			value > maximum ||
			(integer && !Number.isInteger(value))
		) {
			throw new ToolInputError(
				`${label} must be ${integer ? "a whole number" : "a number"} between ${minimum} and ${maximum}.`,
			);
		}
		return value;
	}

	/** Overlay selections are 1-based; the Rust option parser validates them. */
	function overlayPages(): string | undefined {
		const text = optionValue("pages").trim();
		return text === "" ? undefined : text;
	}

	function setOptionFieldVisible(key: string, visible: boolean): void {
		const field = optionFields.get(key);
		if (field !== undefined) {
			field.hidden = !visible;
		}
	}

	function splitModeValue(): SplitMode {
		const mode = optionValue("mode");
		if (mode === "every" || mode === "half" || mode === "size" || mode === "bookmarks") {
			return mode;
		}
		if (mode === "extract") {
			return mode;
		}
		return "range";
	}

	function syncSplitFields(): void {
		const mode = splitModeValue();
		setOptionFieldVisible("from", mode === "range");
		setOptionFieldVisible("to", mode === "range");
		setOptionFieldVisible("every", mode === "every");
		setOptionFieldVisible("target", mode === "size");
		setOptionFieldVisible("pages", mode === "extract");
		const note = document.getElementById("split-note");
		if (note === null) {
			return;
		}
		if (mode === "range") {
			note.textContent = "The selected range becomes one PDF.";
		} else if (mode === "every") {
			note.textContent = "Each group becomes its own PDF. Multiple outputs also come as one ZIP.";
		} else if (mode === "half") {
			note.textContent = "The document splits into two PDFs at the middle page.";
		} else if (mode === "size") {
			note.textContent =
				"Each PDF stays at or below the target. A single page that cannot fit is kept on its own and flagged.";
		} else if (mode === "bookmarks") {
			note.textContent =
				"Top-level bookmarks become boundaries. Pages before the first bookmark become their own PDF.";
		} else {
			note.textContent = "The selected pages become one PDF in document order.";
		}
	}

	function organizeModeValue(): OrganizeMode {
		const mode = optionValue("mode");
		if (mode === "duplicate" || mode === "blank") {
			return mode;
		}
		return "reorder";
	}

	function syncOrganizeFields(): void {
		const mode = organizeModeValue();
		setOptionFieldVisible("order", mode === "reorder");
		setOptionFieldVisible("pages", mode === "duplicate");
		setOptionFieldVisible("after", mode === "blank");
		setOptionFieldVisible("count", mode === "blank");
		const note = document.getElementById("organize-note");
		if (note === null) {
			return;
		}
		if (mode === "duplicate") {
			note.textContent =
				"Each selected page is copied immediately after its original. Content and annotations come with it.";
		} else if (mode === "blank") {
			note.textContent =
				"Blank pages use the neighboring page's size and rotation. Enter 0 to insert before page 1; leave the page empty to append.";
		} else {
			note.textContent = "List every page once, in the order you want.";
		}
	}

	/** Build the per-field rows for the fields list, one call per field. */
	type FormFieldRow = { readonly name: string; readonly value: () => string };

	/** The label for a choice option: the parallel `/Opt` label when the file carries one, the export value otherwise. */
	function choiceLabel(
		labels: readonly string[] | undefined,
		value: string,
		index: number,
	): string {
		const label = labels?.[index];
		return label === undefined || label === "" ? value : label;
	}

	function buildFormFieldRow(rows: FormFieldRow[], field: FormFieldJson): HTMLElement {
		const row = make("div", "form-field");
		row.dataset.field = field.name;
		const id = `field-${rows.length}`;
		const label = make("label", "field-label");
		label.htmlFor = id;
		label.textContent = field.name;
		if (field.required === true) {
			const badge = make("span", "field-badge");
			badge.textContent = "required";
			label.append(badge);
		}
		if (field.read_only === true) {
			const badge = make("span", "field-badge");
			badge.textContent = "read-only";
			label.append(badge);
		}
		const control = buildFormFieldControl(rows, field, id);
		row.append(label, control);
		return row;
	}

	function disabledFormControl(id: string, text: string): HTMLElement {
		const input = make("input", "control");
		input.id = id;
		input.type = "text";
		input.disabled = true;
		input.value = text;
		return input;
	}

	function buildFormFieldControl(
		rows: FormFieldRow[],
		field: FormFieldJson,
		id: string,
	): HTMLElement {
		if (field.read_only === true) {
			// A read-only field is shown for completeness and never submitted; the
			// engine refuses it too, and hiding it would make a signed or
			// locked form look filled.
			return disabledFormControl(id, field.value ?? "read-only");
		}
		switch (field.kind) {
			case "text": {
				const input = make("input", "control");
				input.id = id;
				input.type = "text";
				input.value = field.value ?? "";
				input.spellcheck = false;
				input.autocomplete = "off";
				if (typeof field.max_len === "number" && field.max_len > 0) {
					input.maxLength = field.max_len;
				}
				rows.push({ name: field.name, value: () => input.value });
				return input;
			}
			case "checkbox":
			case "radio": {
				const states = field.states ?? [];
				if (states.length === 0) {
					return disabledFormControl(id, "No appearance states — not fillable");
				}
				const select = make("select", "control");
				select.id = id;
				for (const state of ["Off", ...states]) {
					const option = document.createElement("option");
					option.value = state;
					option.textContent = state;
					if (state === (field.value ?? "Off")) {
						option.selected = true;
					}
					select.append(option);
				}
				rows.push({ name: field.name, value: () => select.value });
				return select;
			}
			case "choice": {
				if (field.multi_select === true) {
					return disabledFormControl(id, "Multiple values — not supported by this tool");
				}
				const options = field.options ?? [];
				if (options.length > 0 && field.editable !== true) {
					const select = make("select", "control");
					select.id = id;
					const labels = field.labels;
					for (const [index, value] of options.entries()) {
						const option = document.createElement("option");
						option.value = value;
						option.textContent = choiceLabel(labels, value, index);
						if (value === field.value) {
							option.selected = true;
						}
						select.append(option);
					}
					// A stored value that is no longer an option stays selectable
					// — clearing it would silently erase something this tool
					// cannot bring back.
					if (field.value !== undefined && !options.includes(field.value)) {
						const option = document.createElement("option");
						option.value = field.value;
						option.textContent = `${field.value} (current)`;
						option.selected = true;
						select.append(option);
					}
					rows.push({ name: field.name, value: () => select.value });
					return select;
				}
				const input = make("input", "control");
				input.id = id;
				input.type = "text";
				input.value = field.value ?? "";
				input.spellcheck = false;
				input.autocomplete = "off";
				rows.push({ name: field.name, value: () => input.value });
				return input;
			}
			case "signature":
				return disabledFormControl(id, "Signature — filling is a signing operation");
			case "push_button":
				return disabledFormControl(id, "Button — no value to fill");
			default:
				return disabledFormControl(id, "Field type not supported");
		}
	}

	/** Render the fields list: one labelled control per field. */
	function renderFormFields(list: HTMLElement, report: FormFieldsJson): void {
		const rows: FormFieldRow[] = [];
		const signature = byId<HTMLElement>("form-fields-signature");
		if (signature !== null) {
			signature.hidden = report.has_signature !== true;
		}
		if (report.fields.length === 0) {
			const empty = make("p", "field-hint");
			empty.textContent = "This PDF carries no fillable AcroForm fields.";
			formFieldControls = [];
			list.replaceChildren(empty);
			return;
		}
		const elements = report.fields.map((field) => buildFormFieldRow(rows, field));
		formFieldControls = rows;
		list.replaceChildren(...elements);
	}

	/** Parse the fields JSON the tools chunk returns. */
	function parseFieldsJson(bytes: Uint8Array): FormFieldsJson | null {
		try {
			const parsed: unknown = JSON.parse(new TextDecoder().decode(bytes));
			if (typeof parsed !== "object" || parsed === null) {
				return null;
			}
			const report = parsed as { fields?: unknown };
			if (!Array.isArray(report.fields)) {
				return null;
			}
			return parsed as FormFieldsJson;
		} catch {
			return null;
		}
	}

	/** Read the fields of the currently picked PDF and render their inputs. */
	async function loadFormFields(): Promise<void> {
		const file = files[0];
		const list = byId<HTMLElement>("form-fields");
		if (file === undefined || list === null) {
			return;
		}
		const bytes = new Uint8Array(await file.arrayBuffer());
		const wasm = await getWasm().catch(() => null);
		if (wasm === null) {
			const note = make("p", "field-hint");
			note.textContent = "The local engine could not load. Reload the page and try again.";
			list.replaceChildren(note);
			return;
		}
		setBusy(true);
		setStatus("Reading the form fields on this device…", "info");
		try {
			const request: ToolRequest = {
				op: "form_fields",
				files: [{ name: file.name, bytes }],
			};
			const result = runTool(wasm, request);
			try {
				await requestRecorder.assertNoUpload([bytes]);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the field list was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			const output = result.outputs[0];
			const report = output === undefined ? null : parseFieldsJson(output.bytes);
			if (report === null || report.status !== "ok") {
				throw new ToolProtocolError("the fields report could not be read.");
			}
			renderFormFields(list, report);
			const count = report.fields.length;
			setStatus(`${count} field${count === 1 ? "" : "s"} found.`, "ok");
		} catch (error) {
			const message =
				error instanceof ToolProtocolError
					? error.message
					: "The fields could not be read. Your file was not uploaded.";
			const note = make("p", "field-hint");
			note.textContent = message;
			list.replaceChildren(note);
			setStatus(message, "error");
		} finally {
			setBusy(false);
		}
	}

	/** The edit payload every visible field contributes to, in field order. */
	function fillFormsPayload(): Array<{ name: string; value: string }> {
		return formFieldControls.map((row) => ({
			name: row.name,
			value: row.value(),
		}));
	}

	function renderFillFormsOptions(options: HTMLElement): void {
		const note = make("p", "option-note");
		note.textContent =
			"Nothing leaves this device: the fields are read and the filled file is written on this machine. Values are stored in the document as written, and viewers rebuild the appearances on open; flattening is a separate step.";
		const list = make("div", "form-fields");
		list.id = "form-fields";
		const empty = make("p", "field-hint");
		empty.textContent = "Choose a form PDF to see its fields.";
		list.append(empty);
		const warning = make("p", "field-hint field-warning");
		warning.id = "form-fields-signature";
		warning.hidden = true;
		warning.textContent =
			"This document is signed. Filling it here invalidates the signature — re-sign the filled file if it must stay valid.";
		options.append(note, list, warning);
	}

	function renderOptions(tool: ToolDef): void {
		options.replaceChildren();
		optionControls.clear();
		optionFields.clear();
		if (tool.id === "fill_forms") {
			renderFillFormsOptions(options);
			return;
		}
		if (tool.id === "merge" || tool.id === "mix") {
			const note = make("p", "option-note");
			note.textContent =
				tool.id === "merge"
					? "Files are merged top to bottom. Use ↑ and ↓ to change the order."
					: "Pages alternate round-robin: page 1 of each file, then page 2, and so on. Use ↑ and ↓ to change the rotation order.";
			options.append(note);
			return;
		}
		if (tool.id === "split") {
			const mode = make("select", "control");
			for (const [value, label] of [
				["range", "Page range"],
				["every", "Every N pages"],
				["half", "Split in half"],
				["size", "By target size"],
				["bookmarks", "By bookmarks"],
				["extract", "Extract pages"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				mode.append(choice);
			}
			addField("mode", "Split mode", mode, "Choose what each downloaded file should contain.");
			const from = addField(
				"from",
				"From page",
				textInput("from", "number", "1"),
				"1-based, inclusive.",
			);
			(from as HTMLInputElement).value = "1";
			addField(
				"to",
				"To page",
				textInput("to", "number", "Last page"),
				"Leave empty to keep through the final page.",
			);
			const every = addField(
				"every",
				"Pages per file",
				textInput("every", "number", "2"),
				"Each output gets this many pages, except the last.",
			);
			(every as HTMLInputElement).value = "2";
			const target = addField(
				"target",
				"Target size (MB)",
				textInput("target", "number", "1"),
				"Fractions of an MB are allowed. Output groups are measured after writing.",
			);
			(target as HTMLInputElement).min = "0.001";
			(target as HTMLInputElement).step = "0.001";
			target.value = "1";
			addField(
				"pages",
				"Pages to extract",
				textInput("pages", "text", "e.g. 1-3, 5"),
				"1-based pages or ranges; they stay in document order.",
			);
			const note = make("p", "option-note");
			note.id = "split-note";
			options.append(note);
			syncSplitFields();
			mode.addEventListener("change", syncSplitFields);
			return;
		}
		if (tool.id === "text") {
			const mode = make("select", "control");
			for (const [value, label] of [
				["text", "Plain text, one separator per page"],
				["tables", "Tables as CSV"],
				["reflow", "Reading-order HTML article"],
				["epub", "EPUB 3 book, one chapter per page"],
				["word", "Word document (.docx), reading order"],
				["slides", "PowerPoint deck (.pptx), one slide per page"],
				["workbook", "Excel workbook (.xlsx), one sheet per table"],
				["split", "Split at a phrase"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				mode.append(choice);
			}
			const modeField = addField(
				"textMode",
				"What to recover",
				mode,
				"A table is inferred from where words start, not from drawn rules, and reports the share of cells that landed in their column.",
			);
			const pagesField = addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges; they stay in document order.",
			);
			const findField = addField(
				"find",
				"Split at this text",
				textInput("find", "text", "Chapter 2"),
				"A new part starts at each page whose text contains the phrase, except the first. The page names the rule before anything is written.",
			);
			const syncTextFields = (): void => {
				const split = optionValue("textMode") === "split";
				pagesField.disabled = split;
				findField.disabled = !split;
			};
			modeField.addEventListener("change", syncTextFields);
			syncTextFields();
			const note = make("p", "option-note");
			note.textContent =
				"The text comes from the document's own fonts and its own mapping tables; a page that drew text the extractor cannot read is marked low-confidence in the quality report rather than shown as a blank page. Nothing is uploaded, and nothing is sent to a model.";
			options.append(note);
			return;
		}
		if (tool.id === "rotate") {
			const angle = make("select", "control");
			for (const value of ["90", "180", "270"]) {
				const option = document.createElement("option");
				option.value = value;
				option.textContent = `${value}° clockwise`;
				angle.append(option);
			}
			addField("angle", "Rotation", angle);
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"Use 1-based numbers. Leave blank to rotate every page.",
			);
			return;
		}
		if (tool.id === "crop") {
			for (const [key, label] of [
				["left", "Left margin (pt)"],
				["top", "Top margin (pt)"],
				["right", "Right margin (pt)"],
				["bottom", "Bottom margin (pt)"],
			] as const) {
				const input = addField(
					key,
					label,
					textInput(key, "number", "0"),
					key === "left" ? "1 point = 1/72 inch. At least one margin must be non-zero." : undefined,
				) as HTMLInputElement;
				input.min = "0";
				input.step = "0.5";
				input.value = "0";
			}
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges. Leave blank to crop every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Margins follow the page as displayed, so rotated pages crop the edge you see. The new box becomes the visible page; print boxes outside it are dropped.";
			options.append(note);
			return;
		}
		if (tool.id === "flip") {
			const axis = make("select", "control");
			for (const [value, label] of [
				["horizontal", "Horizontal (left ↔ right)"],
				["vertical", "Vertical (top ↕ bottom)"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				axis.append(choice);
			}
			addField("axis", "Flip axis", axis);
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges. Leave blank to flip every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Mirrors painted page content. Annotations and form fields are carried unchanged, so they keep their original coordinates.";
			options.append(note);
			return;
		}
		if (tool.id === "page_labels") {
			const style = make("select", "control");
			for (const [value, label] of [
				["D", "Decimal (1, 2, 3)"],
				["r", "Lower-case roman (i, ii, iii)"],
				["R", "Upper-case roman (I, II, III)"],
				["a", "Lower-case letters (a, b, c)"],
				["A", "Upper-case letters (A, B, C)"],
				["None", "No number (prefix only)"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				style.append(choice);
			}
			addField("style", "Numbering style", style);
			const start = addField(
				"start",
				"First number",
				textInput("start", "number", "1"),
				"The number shown on the first selected page.",
			) as HTMLInputElement;
			start.min = "1";
			start.value = "1";
			addField(
				"prefix",
				"Prefix (optional)",
				textInput("prefix", "text", "e.g. A-"),
				"Up to 64 printable ASCII characters, placed before every number.",
			);
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges. Leave blank to relabel every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Selected pages are renumbered in document order. Other pages keep the label they already show. Every output page gets an explicit label.";
			options.append(note);
			return;
		}
		if (tool.id === "resize") {
			const preset = make("select", "control");
			for (const [value, label] of [
				["a4", "A4 (210 × 297 mm)"],
				["a5", "A5 (148 × 210 mm)"],
				["letter", "US Letter (8.5 × 11 in)"],
				["legal", "US Legal (8.5 × 14 in)"],
				["a3", "A3 (297 × 420 mm)"],
				["custom", "Custom size in points"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				preset.append(choice);
			}
			addField("preset", "Page size", preset);
			const width = addField(
				"width",
				"Width (pt, custom only)",
				textInput("width", "number", "595.28"),
				"1 to 14400 points. Used when the page size is Custom.",
			) as HTMLInputElement;
			width.min = "1";
			width.max = "14400";
			width.step = "0.01";
			width.value = "595.28";
			const height = addField(
				"height",
				"Height (pt, custom only)",
				textInput("height", "number", "841.89"),
				"1 to 14400 points. Used when the page size is Custom.",
			) as HTMLInputElement;
			height.min = "1";
			height.max = "14400";
			height.step = "0.01";
			height.value = "841.89";
			const mode = make("select", "control");
			for (const [value, label] of [
				["fit", "Fit (keep the aspect ratio)"],
				["stretch", "Stretch (fill both dimensions)"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				mode.append(choice);
			}
			addField("size_mode", "Scaling", mode);
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges. Leave blank to resize every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Selected pages get the new size in their displayed orientation, with rotation normalized. Other pages are copied unchanged; annotations keep their coordinates.";
			options.append(note);
			return;
		}
		if (tool.id === "nup") {
			const perSheet = make("select", "control");
			for (const [value, label] of [
				["2", "2 pages per sheet (1 × 2)"],
				["4", "4 pages per sheet (2 × 2)"],
				["6", "6 pages per sheet (2 × 3)"],
				["9", "9 pages per sheet (3 × 3)"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				perSheet.append(choice);
			}
			addField("per_sheet", "Pages per sheet", perSheet);
			const note = make("p", "option-note");
			note.textContent =
				"Every sheet takes the first page's size and orientation. Each page keeps its aspect ratio, and annotations, links, and form fields are not painted on the sheet.";
			options.append(note);
			return;
		}
		if (tool.id === "attach") {
			const note = make("p", "option-note");
			note.textContent =
				"Choose the PDF first, then the files to embed. Extra files are stored inside the PDF and travel with it; use the arrows to keep the PDF first.";
			options.append(note);
			return;
		}
		if (tool.id === "detach") {
			const note = make("p", "option-note");
			note.textContent =
				"Every embedded file is removed. Pages, annotations, forms, and bookmarks are kept unchanged.";
			options.append(note);
			return;
		}
		if (tool.id === "repair") {
			const note = make("p", "option-note");
			note.textContent =
				"Tries the ordinary parser first, then the last intact revision, then a rebuilt object index. Every recovered page is rewritten and counted, so a repair cannot silently drop pages. Encrypted PDFs are refused.";
			options.append(note);
			return;
		}
		if (tool.id === "delete") {
			addField(
				"pages",
				"Pages to delete",
				textInput("pages", "text", "e.g. 2, 5-6"),
				"1-based page numbers or ranges, separated by commas.",
			);
			return;
		}
		if (tool.id === "reorder") {
			const mode = make("select", "control");
			for (const [value, label] of [
				["reorder", "Reorder pages"],
				["duplicate", "Duplicate pages"],
				["blank", "Insert blank pages"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				mode.append(choice);
			}
			addField("mode", "Page action", mode);
			addField(
				"order",
				"New page order",
				textInput("order", "text", "e.g. 2, 1, 4, 3"),
				"1-based pages. List every page once.",
			);
			addField(
				"pages",
				"Pages to duplicate",
				textInput("pages", "text", "e.g. 1, 3-4"),
				"1-based pages or ranges. Each copy follows its original.",
			);
			const after = addField(
				"after",
				"Insert after page",
				textInput("after", "number", "Blank = end"),
				"Use 0 to insert before page 1; leave empty to append.",
			) as HTMLInputElement;
			after.min = "0";
			const count = addField(
				"count",
				"Number of blank pages",
				textInput("count", "number", "1"),
				"Up to 100 blank pages per run.",
			) as HTMLInputElement;
			count.value = "1";
			count.max = "100";
			const note = make("p", "option-note");
			note.id = "organize-note";
			options.append(note);
			syncOrganizeFields();
			mode.addEventListener("change", syncOrganizeFields);
			return;
		}
		if (tool.id === "page_numbers") {
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges. Leave blank to number every page.",
			);
			const position = make("select", "control");
			for (const [value, label] of [
				["bottom_center", "Bottom center"],
				["bottom_left", "Bottom left"],
				["bottom_right", "Bottom right"],
				["top_center", "Top center"],
				["top_left", "Top left"],
				["top_right", "Top right"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				position.append(choice);
			}
			addField("position", "Position", position);
			const format = make("select", "control");
			for (const [value, label] of [
				["plain", "1"],
				["fraction", "1 / total"],
				["page_of_pages", "Page 1 of total"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				format.append(choice);
			}
			addField("format", "Number format", format);
			const start = addField(
				"start",
				"First number",
				textInput("start", "number", "1"),
			) as HTMLInputElement;
			start.value = "1";
			const fontSize = addField(
				"font-size",
				"Font size (pt)",
				textInput("font-size", "number", "10"),
			) as HTMLInputElement;
			fontSize.value = "10";
			fontSize.step = "0.5";
			fontSize.max = "400";
			return;
		}
		if (tool.id === "watermark") {
			addField(
				"text",
				"Watermark text",
				textInput("text", "text", "CONFIDENTIAL"),
				"WinAnsi text. Unknown glyphs are refused rather than replaced.",
			);
			const orientation = make("select", "control");
			for (const [value, label] of [
				["diagonal", "Diagonal"],
				["horizontal", "Horizontal"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				orientation.append(choice);
			}
			addField("orientation", "Orientation", orientation);
			const opacity = addField(
				"opacity",
				"Opacity",
				textInput("opacity", "number", "0.2"),
				"Between 0 and 1. Values below 1 add a local graphics state.",
			) as HTMLInputElement;
			opacity.value = "0.2";
			opacity.min = "0";
			opacity.max = "1";
			opacity.step = "0.05";
			const fontSize = addField(
				"font-size",
				"Font size (pt)",
				textInput("font-size", "number", "48"),
			) as HTMLInputElement;
			fontSize.value = "48";
			fontSize.step = "0.5";
			fontSize.max = "400";
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges.",
			);
			addField(
				"tiled",
				"Tile across the page",
				checkInput("tiled"),
				"Repeats the line on a staggered grid instead of stamping it once in the center.",
			);
			return;
		}
		if (tool.id === "image_watermark") {
			const opacity = addField(
				"opacity",
				"Opacity",
				textInput("opacity", "number", "0.3"),
				"Between 0 and 1. Values below 1 add a local graphics state.",
			) as HTMLInputElement;
			opacity.value = "0.3";
			opacity.min = "0";
			opacity.max = "1";
			opacity.step = "0.05";
			const scale = addField(
				"scale",
				"Scale",
				textInput("scale", "number", "1"),
				"1 draws one point per image pixel; 0.5 halves the mark.",
			) as HTMLInputElement;
			scale.value = "1";
			scale.min = "0.01";
			scale.step = "0.05";
			addField(
				"x",
				"Horizontal offset (pt, optional)",
				textInput("x", "number", "Centered"),
				"Distance from the page's left edge. Empty centers the mark.",
			);
			addField(
				"y",
				"Vertical offset (pt, optional)",
				textInput("y", "number", "Centered"),
				"Distance from the page's bottom edge. Empty centers the mark.",
			);
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges.",
			);
			addField(
				"tiled",
				"Tile across the page",
				checkInput("tiled"),
				"Repeats the image on a staggered grid instead of stamping it once in the center.",
			);
			return;
		}
		if (tool.id === "outline") {
			const entries = make("textarea", "control");
			entries.rows = 10;
			entries.spellcheck = false;
			entries.placeholder = "Chapter 1 | 1\nSection 1.1 | 2 | 1\nChapter 2 | 3";
			addField(
				"entries",
				"Bookmarks",
				entries,
				"One per line: Title | page | level. Level is optional and 0-based; indent a child with level 1. Leave empty to remove every bookmark.",
			);
			return;
		}
		if (tool.id === "header_footer") {
			addField(
				"header-left",
				"Header left",
				textInput("header-left", "text", "Page {page} of {pages}"),
				"Supports {page} and {pages}.",
			);
			addField(
				"header-center",
				"Header center",
				textInput("header-center", "text", "Optional"),
				"Left empty when unused.",
			);
			addField(
				"header-right",
				"Header right",
				textInput("header-right", "text", "Optional"),
				"Left empty when unused.",
			);
			addField(
				"footer-left",
				"Footer left",
				textInput("footer-left", "text", "Optional"),
				"Supports {page} and {pages}.",
			);
			addField(
				"footer-center",
				"Footer center",
				textInput("footer-center", "text", "Optional"),
				"Left empty when unused.",
			);
			addField(
				"footer-right",
				"Footer right",
				textInput("footer-right", "text", "Optional"),
				"At least one header or footer slot must be filled.",
			);
			const fontSize = addField(
				"font-size",
				"Font size (pt)",
				textInput("font-size", "number", "9"),
			) as HTMLInputElement;
			fontSize.value = "9";
			fontSize.step = "0.5";
			fontSize.max = "400";
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges.",
			);
			return;
		}
		if (tool.id === "bates") {
			addField("prefix", "Prefix", textInput("prefix", "text", "e.g. ABC-"));
			addField("suffix", "Suffix", textInput("suffix", "text", "Optional"));
			const start = addField(
				"start",
				"First number",
				textInput("start", "number", "1"),
			) as HTMLInputElement;
			start.value = "1";
			const digits = addField(
				"digits",
				"Minimum digits",
				textInput("digits", "number", "6"),
				"1 to 20. A wider number is printed in full.",
			) as HTMLInputElement;
			digits.value = "6";
			digits.max = "20";
			addField(
				"pages",
				"Pages (optional)",
				textInput("pages", "text", "All pages, or 1-3, 7"),
				"1-based page numbers or ranges.",
			);
			return;
		}
		if (tool.id === "compress") {
			const note = make("p", "option-note");
			note.textContent =
				"Lossless only: unreachable objects are removed and streams are re-encoded. Images and fonts are not downsampled. Encrypted files are refused.";
			options.append(note);
			return;
		}
		if (tool.id === "attachments") {
			const note = make("p", "option-note");
			note.textContent =
				"Reads /Names /EmbeddedFiles and decodes each file locally. Unsafe names are flattened, and encrypted PDFs are refused.";
			options.append(note);
			return;
		}
		if (tool.id === "metadata_report") {
			const note = make("p", "option-note");
			note.textContent =
				"Reads the /Info dictionary, the catalog's XMP packet, and the structural counts, then writes them to a JSON file. The PDF itself is not modified. An encrypted PDF reports only that it is encrypted, because its dictionaries are still ciphertext.";
			options.append(note);
			return;
		}
		if (tool.id === "strip_metadata") {
			const note = make("p", "option-note");
			note.textContent =
				"Removes the Info dictionary and the XMP packet. Pages, annotations, forms, outlines, page labels, attachments, and tags are kept. PDF/A conformance and digital signatures may be invalidated.";
			options.append(note);
			return;
		}
		if (tool.id === "batch") {
			const queueTool = make("select", "control");
			for (const [value, label] of [
				["compress", "Compress every PDF"],
				["strip-metadata", "Strip metadata from every PDF"],
				["repair", "Repair every PDF"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				queueTool.append(choice);
			}
			addField("tool", "Tool to run", queueTool);
			const note = make("p", "option-note");
			note.textContent =
				"Every file is processed on its own: one that fails is reported and skipped, and the rest still get their results. Each result is named after the file it came from, and several results arrive together in a ZIP.";
			options.append(note);
			return;
		}
		if (tool.id === "image_to_pdf") {
			const pageSize = make("select", "control");
			for (const [value, label] of [
				["fit", "Fit to image"],
				["a4", "A4"],
				["letter", "Letter"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				pageSize.append(choice);
			}
			addField(
				"page_size",
				"Page size",
				pageSize,
				"Fit makes the page the image size plus the margin.",
			);
			const margin = addField(
				"margin",
				"Margin (pt)",
				textInput("margin", "number", "36"),
				"Points on every side. 36 pt is half an inch.",
			) as HTMLInputElement;
			margin.min = "0";
			margin.step = "1";
			margin.value = "36";
			const note = make("p", "option-note");
			note.textContent =
				"PNG and JPEG are decoded on this device. Each image becomes one page; a fixed page size scales the image down and centres it.";
			options.append(note);
			return;
		}
		if (tool.id === "png") {
			const dpi = textInput("dpi", "number", "150");
			dpi.min = "1";
			dpi.max = "600";
			dpi.step = "1";
			dpi.value = "150";
			addField(
				"dpi",
				"Resolution (DPI)",
				dpi,
				"1 to 600. 72 renders one pixel per point; 150 is a good screen/print default.",
			);
			addField(
				"pages",
				"Pages",
				textInput("pages", "text", "All pages"),
				"1-based, like the page numbers readers show: 1-3,5. Leave empty for every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Every page is rendered on this device from the PDF's own content. One page is saved as a PNG; several pages are bundled into one ZIP of page-1.png, page-2.png entries, padded to the width of the page count.";
			options.append(note);
			return;
		}
		if (tool.id === "jpg") {
			const dpi = textInput("dpi", "number", "150");
			dpi.min = "1";
			dpi.max = "600";
			dpi.step = "1";
			dpi.value = "150";
			addField(
				"dpi",
				"Resolution (DPI)",
				dpi,
				"1 to 600. 72 renders one pixel per point; 150 is a good screen/print default.",
			);
			const quality = textInput("quality", "number", "90");
			quality.min = "1";
			quality.max = "100";
			quality.step = "1";
			quality.value = "90";
			addField(
				"quality",
				"Quality",
				quality,
				"1 to 100. 90 is the balanced default; lower makes a smaller file with more visible softening.",
			);
			addField(
				"pages",
				"Pages",
				textInput("pages", "text", "All pages"),
				"1-based, like the page numbers readers show: 1-3,5. Leave empty for every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Every page is rendered on this device from the PDF's own content. Each JPEG is decoded back and compared with the page before it is offered, so a file that does not match comes back as a failure instead of a download. One page is saved as a JPG; several pages are bundled into one ZIP of page-1.jpg, page-2.jpg entries, padded to the width of the page count. A render wider or taller than 16384 px cannot be read back on this device, so lower the DPI for that page or use PDF to PNG.";
			options.append(note);
			return;
		}
		if (tool.id === "tiff") {
			const dpi = textInput("dpi", "number", "150");
			dpi.min = "1";
			dpi.max = "600";
			dpi.step = "1";
			dpi.value = "150";
			addField(
				"dpi",
				"Resolution (DPI)",
				dpi,
				"1 to 600. 72 renders one pixel per point; 150 is a good screen/print default.",
			);
			addField(
				"pages",
				"Pages",
				textInput("pages", "text", "All pages"),
				"1-based, like the page numbers readers show: 1-3,5. Leave empty for every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Every page is rendered on this device from the PDF's own content. The selected pages go into one multi-page TIFF rather than a file per page, and each page is read back out of the assembled file and matched sample for sample before it is offered, so a file that does not agree with the pages comes back as a failure instead of a download. There is no quality knob: a TIFF here is lossless. Browsers do not display TIFF inline, so the result offers a download rather than a preview.";
			options.append(note);
			return;
		}
		if (tool.id === "webp") {
			const dpi = textInput("dpi", "number", "150");
			dpi.min = "1";
			dpi.max = "600";
			dpi.step = "1";
			dpi.value = "150";
			addField(
				"dpi",
				"Resolution (DPI)",
				dpi,
				"1 to 600. 72 renders one pixel per point; 150 is a good screen/print default.",
			);
			addField(
				"pages",
				"Pages",
				textInput("pages", "text", "All pages"),
				"1-based, like the page numbers readers show: 1-3,5. Leave empty for every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Every page is rendered on this device from the PDF's own content. Each WebP is decoded back and compared with the page sample for sample, alpha included, before it is offered, so a file that does not match comes back as a failure instead of a download. There is no quality knob: the coder here is VP8L, which is lossless. One page is saved as a WebP; several pages are bundled into one ZIP of page-1.webp, page-2.webp entries, padded to the width of the page count. A render wider or taller than 16384 px is past what a VP8L stream can name, so lower the DPI for that page or use PDF to PNG.";
			options.append(note);
			return;
		}
		if (tool.id === "html") {
			addField(
				"pages",
				"Pages",
				textInput("pages", "text", "All pages"),
				"1-based, like the page numbers readers show: 1-3,5. Leave empty for every page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Every selected page becomes one <section> of a single HTML file, sized by that page's own box at 72 dpi — the same canvas PDF to PNG paints at 72 — and each word the engine recovers is placed where the page puts it. The file is read back and compared with the extraction before it is offered, so a document that does not carry its own pages comes back as a failure instead of a download. This is a positioned text layer, not a picture of the page: no colour, no images, no font faces and no spaces between words, so it reads as prose only once you style it. A page that drew text the engine could not recover says so in the file rather than going blank.";
			options.append(note);
			return;
		}
		if (tool.id === "unlock") {
			addField(
				"password",
				"Password",
				textInput("password", "password", "The password the document opens with"),
				"Leave it empty for a PDF that opens without a password but still restricts editing.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"The document is decrypted and rewritten on this device, then re-opened and checked page for page before it is offered as a download. A wrong password is refused and nothing is produced — you never get an unencrypted-looking file that is actually still locked. A PDF that was never protected has no password to remove, so its own bytes are copied through and the result says so.";
			options.append(note);
			return;
		}
		if (tool.id === "protect" || tool.id === "permissions") {
			if (tool.id === "protect") {
				addField(
					"userPassword",
					"Open password",
					textInput("userPassword", "password", "What a reader must type to see the pages"),
					"The document cannot be opened without this.",
				);
			}
			addField(
				"ownerPassword",
				"Owner password",
				textInput(
					"ownerPassword",
					"password",
					tool.id === "protect" ? "Optional — defaults to the open password" : "Required",
				),
				"Changes or removes the permissions below. Without it they are fixed as written.",
			);
			addPermissionFields();
			const metadata = checkInput("encryptMetadata");
			metadata.checked = true;
			addField(
				"encryptMetadata",
				"Encrypt document metadata too",
				metadata,
				"Leaving this unticked keeps the document's title, author and XMP packet readable to a search tool, while the pages stay encrypted.",
			);
			const note = make("p", "option-note");
			note.textContent =
				tool.id === "protect"
					? "Written with AES-256 (revision 6), the newest handler the PDF specification defines, using a 256-bit key this page generates from your browser's own random source and never sends anywhere. The passwords are used to write the file and then thrown away; only the derived keys go into it. Permission flags are a convention rather than a lock: a reader that respects them will not print, copy, modify or annotate against your wishes, and a reader that ignores them can still do those things — the page cannot force a program it has never met to behave. A PDF that already carries a password is refused: removing one is the Remove password tool."
					: "This card writes the permission flags onto a document that still opens without a password, which is what the flags are for. The owner password is required because changing permissions is the owner's decision, and it is what lets you change them again later. The flags are a convention, not enforcement: a reader that respects them will not print, copy, modify or annotate against your wishes, and a reader that ignores them can. To lift restrictions instead, use Clear permissions with the owner password; a document that already carries a password is refused here, so remove it first.";
			options.append(note);
			return;
		}
		if (tool.id === "clear_permissions") {
			addField(
				"password",
				"Owner password",
				textInput("password", "password", "The owner password of the document"),
				"The user password is not enough: changing permissions is the owner's decision.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Only the flags change. The page content is left exactly as it is — on a modern encrypted file the document is appended to rather than rewritten, so the bytes underneath are untouched — and the result is re-read and checked before it is offered. What this lifts is reported afterwards by name, read back out of the flag the tool actually wrote. A wrong owner password is refused with nothing produced, and a document that needs a password just to open keeps needing it: that is the Remove password tool.";
			options.append(note);
			return;
		}
		if (tool.id === "sign") {
			const mode = make("select", "control");
			for (const [value, label] of [
				["stamp", "Typed stamp (no key)"],
				["certificate", "Certificate signature (local key)"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				mode.append(choice);
			}
			addField(
				"signMode",
				"Signature mode",
				mode,
				"A stamp marks the page but proves nothing about who wrote it. A certificate signature covers the document's own bytes with a key you hold.",
			);
			addField(
				"stampText",
				"Stamp text",
				textInput("stampText", "text", "Approved"),
				"Everything is drawn inside the signature rectangle on the page you choose.",
			);
			addField(
				"page",
				"Page",
				textInput("page", "number", "1"),
				"1-based, like the page numbers readers show.",
			);
			addField(
				"signerName",
				"Signer name",
				textInput("signerName", "text", "Your name"),
				"Printed under the stamp, and written as the signature's name when a key is used.",
			);
			addField(
				"certKey",
				"Private key (PEM)",
				fileChooser("certKey", ".pem,.key,application/x-pem-file,application/octet-stream", false),
				"A PKCS#8 or PKCS#1 RSA key of at least 2048 bits. It is read in this tab, used once, and never written anywhere — not into the PDF, not into a download, not into a request.",
			);
			addField(
				"certChain",
				"Certificate chain (PEM)",
				fileChooser(
					"certChain",
					".pem,.crt,.cer,application/x-pem-file,application/octet-stream",
					true,
				),
				"Leaf first, then any intermediates. Select one file per certificate.",
			);
			addField(
				"reason",
				"Reason",
				textInput("reason", "text", "Why this document was signed"),
				"Optional. Recorded in the signature dictionary.",
			);
			addField(
				"location",
				"Location",
				textInput("location", "text", "Where it was signed"),
				"Optional. Recorded in the signature dictionary.",
			);
			const field = addField(
				"field",
				"Field name",
				textInput("field", "text", "Signature"),
				"The AcroForm field the signature lives in.",
			);
			(field as HTMLInputElement).value = "Signature";
			const note = make("p", "option-note");
			note.textContent =
				"Signing is an incremental revision: the bytes already in the document are kept, so an earlier signature stays verifiable. A certificate signature proves the file has not changed since it was signed; it does not by itself prove the signer is trusted — use Certificate validation with the certificates you trust for that. Signing a document that already carries a signature appends a revision, and the report shows which bytes each signature covers rather than hiding the difference.";
			options.append(note);
			const syncMode = (): void => {
				const certificate = mode.value === "certificate";
				for (const key of ["certKey", "certChain", "reason", "location", "field"]) {
					const element = optionFields.get(key);
					if (element !== undefined) {
						element.hidden = !certificate;
					}
				}
				const stamp = optionFields.get("stampText");
				if (stamp !== undefined) {
					stamp.hidden = certificate;
				}
			};
			mode.addEventListener("change", syncMode);
			syncMode();
			return;
		}
		if (tool.id === "certificate") {
			addField(
				"anchors",
				"Trusted certificates (PEM)",
				fileChooser(
					"anchors",
					".pem,.crt,.cer,application/x-pem-file,application/octet-stream",
					true,
				),
				"The certificates you already trust, one per file. A chain is trusted only when it ends at one of these; with none selected, every signature is reported untrusted rather than assumed good.",
			);
			const note = make("p", "option-note");
			note.textContent =
				'Every signature is checked against the exact bytes it names, on this device, and the report says what was checked and what was not. Revocation is never claimed: this build has no network, so no CRL or OCSP response is fetched and the report says "not checked" instead of implying a status it did not look up. A signature that does not verify is still listed, with its reason, because hiding it would be worse than reporting it.';
			options.append(note);
			return;
		}
		if (tool.id === "workflow") {
			const decision = make("select", "control");
			for (const [value, label] of [
				["approved", "Approved"],
				["changes", "Changes requested"],
				["rejected", "Rejected"],
				["reviewed", "Reviewed"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				decision.append(choice);
			}
			addField("decision", "Decision", decision, "Recorded in the document itself.");
			addField(
				"approver",
				"Approver",
				textInput("approver", "text", "Your name"),
				"Printed on the approval block.",
			);
			addField(
				"note",
				"Note",
				textInput("note", "text", "Optional comment"),
				"Optional. Kept short so it fits the block.",
			);
			addField(
				"page",
				"Page",
				textInput("page", "number", "1"),
				"1-based. The decision block is appended to this page.",
			);
			const note = make("p", "option-note");
			note.textContent =
				"Routing stays on this device: the decision is appended to the document as a visible approval block, and no queue, account or server holds a copy. Send the resulting PDF on by whatever means you already use. A later decision appends another block and leaves the earlier one readable, so the route history travels with the file.";
			options.append(note);
			return;
		}
		if (tool.id === "convert") {
			const format = make("select", "control");
			for (const [value, label] of [
				["auto", "Auto-detect"],
				["md", "Markdown"],
				["html", "HTML"],
				["csv", "CSV"],
				["latex", "LaTeX (subset)"],
				["svg", "SVG (documented subset)"],
				["epub", "EPUB 3"],
			] as const) {
				const choice = document.createElement("option");
				choice.value = value;
				choice.textContent = label;
				format.append(choice);
			}
			addField("format", "Format", format, "Auto-detects from the file extension.");
			addField("title", "Title", textInput("title", "text", "Document title"));
			const note = make("p", "option-note");
			note.textContent =
				"Markdown, HTML, CSV, LaTeX, SVG and EPUB are converted on this device through the owned engine. The output is a generated PDF.";
			options.append(note);
			return;
		}
		addField("title", "Title", textInput("title", "text", "Document title"));
		addField("author", "Author", textInput("author", "text", "Author name"));
		addField("subject", "Subject", textInput("subject", "text", "Short subject"));
		addField(
			"keywords",
			"Keywords",
			textInput("keywords", "text", "comma, separated, keywords"),
			"At least one field must be filled.",
		);
	}

	function selectTool(id: ToolId): void {
		const tool = TOOL_BY_ID.get(id);
		if (tool === undefined) {
			return;
		}
		selectedTool = id;
		files = [];
		clearResult();
		renderOptions(tool);
		renderFiles();
		updateRunState();
		workspaceTitle.textContent = tool.title;
		workspaceBlurb.textContent = tool.blurb;
		fileInput.multiple = tool.multiple;
		// Attach accepts opaque payloads after the first file, so the picker
		// must not restrict the whole selection to PDFs.
		fileInput.accept =
			tool.accepts === "image"
				? "image/png,image/jpeg,.png,.jpg,.jpeg"
				: tool.accepts === "pdf-image"
					? "application/pdf,.pdf,image/png,image/jpeg,.png,.jpg,.jpeg"
					: tool.accepts === "text"
						? ".md,.markdown,.html,.htm,.xhtml,.csv,.txt,.tex,.latex,.svg,.epub"
						: id === "attach"
							? ""
							: "application/pdf,.pdf";
		fileInput.value = "";
		toolsSection.hidden = true;
		workspace.hidden = false;
		history.replaceState(null, "", `#${id}`);
		setStatus(
			id === "attach"
				? "Choose the PDF first, then the files to embed. They are read here, not sent anywhere."
				: tool.accepts === "pdf-image"
					? "Choose the PDF, then the PNG or JPEG watermark. They are read here, not sent anywhere."
					: tool.accepts === "image"
						? "Choose one or more PNG or JPEG images. They are read here, not sent anywhere."
						: tool.accepts === "text"
							? "Choose a Markdown, HTML, CSV, LaTeX, SVG or EPUB file. It is read here, not sent anywhere."
							: tool.multiple
								? "Choose two or more PDFs. They are read here, not sent anywhere."
								: "Choose a PDF. It is read here, not sent anywhere.",
			"info",
		);
		dropzone.focus();
	}

	function clearTool(): void {
		selectedTool = null;
		files = [];
		clearResult();
		optionControls.clear();
		optionFields.clear();
		options.replaceChildren();
		renderFiles();
		workspace.hidden = true;
		toolsSection.hidden = false;
		history.replaceState(null, "", window.location.pathname + window.location.search);
		updateRunState();
	}

	function fileMeta(file: File, index: number): string {
		return `${index + 1}. ${formatBytes(file.size)}`;
	}

	function renderFiles(): void {
		const tool = currentTool();
		const rows: HTMLElement[] = [];
		files.forEach((file, index) => {
			const row = make("li", "file-row");
			const info = make("div", "file-info");
			const name = make("span", "file-name");
			name.textContent = file.name;
			const meta = make("span", "file-meta");
			meta.textContent = fileMeta(file, index);
			info.append(name, meta);

			const actions = make("div", "file-actions");
			const preview = button("Preview", "text-button");
			preview.setAttribute("aria-label", `Preview ${file.name} in the browser`);
			preview.addEventListener("click", () => {
				const url = URL.createObjectURL(file);
				openBlobUrl(url);
				window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
			});
			actions.append(preview);

			if (tool?.multiple === true) {
				const up = button("↑", "icon-button");
				up.setAttribute("aria-label", `Move ${file.name} up`);
				up.disabled = index === 0;
				up.addEventListener("click", () => {
					moveFile(index, -1);
				});
				const down = button("↓", "icon-button");
				down.setAttribute("aria-label", `Move ${file.name} down`);
				down.disabled = index === files.length - 1;
				down.addEventListener("click", () => {
					moveFile(index, 1);
				});
				actions.append(up, down);
			}

			const remove = button("Remove", "text-button danger");
			remove.setAttribute("aria-label", `Remove ${file.name}`);
			remove.addEventListener("click", () => {
				removeFile(index);
			});
			actions.append(remove);
			row.append(info, actions);
			if (tool?.multiple === true) {
				row.setAttribute("draggable", "true");
				row.addEventListener("dragstart", (event) => {
					draggedFileIndex = index;
					event.dataTransfer?.setData("text/plain", String(index));
					if (event.dataTransfer !== null) {
						event.dataTransfer.effectAllowed = "move";
					}
					row.classList.add("dragging");
				});
				row.addEventListener("dragend", () => {
					draggedFileIndex = null;
					row.classList.remove("dragging");
				});
				row.addEventListener("dragover", (event) => {
					event.preventDefault();
					if (event.dataTransfer !== null) {
						event.dataTransfer.dropEffect = "move";
					}
				});
				row.addEventListener("drop", (event) => {
					event.preventDefault();
					if (draggedFileIndex === null || draggedFileIndex === index) {
						return;
					}
					const from = draggedFileIndex;
					const to = index;
					const next = [...files];
					const [moved] = next.splice(from, 1);
					if (moved === undefined) {
						return;
					}
					next.splice(to, 0, moved);
					files = next;
					clearResult();
					renderFiles();
					updateRunState();
				});
			}
			rows.push(row);
		});
		fileList.replaceChildren(...rows);
	}

	function moveFile(index: number, delta: number): void {
		const target = index + delta;
		if (target < 0 || target >= files.length) {
			return;
		}
		const current = files[index];
		const other = files[target];
		if (current === undefined || other === undefined) {
			return;
		}
		files[index] = other;
		files[target] = current;
		clearResult();
		renderFiles();
		updateRunState();
	}

	function removeFile(index: number): void {
		files = files.filter((_, current) => current !== index);
		clearResult();
		renderFiles();
		updateRunState();
		// A removed PDF means the rendered fields are no longer its fields.
		if (currentTool()?.id === "fill_forms" && files.length === 0) {
			const list = byId<HTMLElement>("form-fields");
			if (list !== null) {
				const hint = make("p", "field-hint");
				hint.textContent = "Choose a form PDF to see its fields.";
				list.replaceChildren(hint);
			}
			formFieldControls = [];
		}
	}

	async function addFiles(incoming: readonly File[]): Promise<void> {
		const tool = currentTool();
		if (tool === null || busy || incoming.length === 0) {
			return;
		}
		let next: File[];
		if (tool.multiple) {
			next = [...files, ...incoming];
		} else {
			const first = incoming[0];
			if (first === undefined) {
				return;
			}
			next = [first];
			if (incoming.length > 1) {
				setStatus("This tool takes one PDF at a time; using the first file.", "info");
			}
		}
		const total = next.reduce((sum, file) => sum + file.size, 0);
		if (total > MAX_TOTAL_BYTES) {
			setStatus(
				`That selection is ${formatBytes(total)}. Keep a run under ${formatBytes(MAX_TOTAL_BYTES)} for browser memory.`,
				"error",
			);
			return;
		}
		files = next;
		clearResult();
		renderFiles();
		updateRunState();
		setStatus("Ready. Files stay on this device.", "info");
		if (tool.id === "fill_forms") {
			// The fields list drives the inputs, so read it as soon as a PDF
			// lands; the run button stays disabled until it succeeds.
			void loadFormFields();
		}
	}

	function optionValue(key: string): string {
		return optionControls.get(key)?.value ?? "";
	}

	/** The state of a checkbox field, or false when the card never showed one. */
	function optionChecked(key: string): boolean {
		const control = optionControls.get(key);
		return control instanceof HTMLInputElement && control.checked;
	}

	/** The four boxes every permission-writing card offers, all granted by default. */
	function addPermissionFields(): void {
		for (const [key, label] of [
			["print", "Allow printing"],
			["modify", "Allow modifying"],
			["copy", "Allow copying text and images"],
			["annotate", "Allow adding comments and form fills"],
		] as const) {
			const box = checkInput(key);
			box.checked = true;
			addField(key, label, box);
		}
	}

	/** The permission spec the boxes add up to: what is ticked is what is granted. */
	function permissionSpec(): string {
		const granted = ["print", "modify", "copy", "annotate"].filter(optionChecked);
		return granted.length === 0 ? "none" : granted.join(",");
	}

	function buildRequest(tool: ToolDef, wireFiles: readonly WireFile[]): ToolRequest {
		if (tool.id === "merge") {
			return { op: "merge", files: wireFiles };
		}
		if (tool.id === "fill_forms") {
			return {
				op: "fill_forms",
				options: { fields: fillFormsPayload() },
				files: wireFiles,
			};
		}
		if (tool.id === "mix") {
			return { op: "mix", files: wireFiles };
		}
		if (tool.id === "split") {
			const mode = splitModeValue();
			const splitOptions: Record<string, unknown> = { mode };
			if (mode === "range") {
				const fromText = optionValue("from").trim() === "" ? "1" : optionValue("from");
				const from = pageIndex(fromText, "From page");
				splitOptions.from = from;
				const toText = optionValue("to").trim();
				if (toText !== "") {
					const to = pageIndex(toText, "To page");
					if (to < from) {
						throw new ToolInputError("The last page must not come before the first page.");
					}
					splitOptions.to = to;
				}
			} else if (mode === "every") {
				splitOptions.every = pageCount(optionValue("every"), "Pages per file");
			} else if (mode === "size") {
				splitOptions.targetBytes = sizeBytes(optionValue("target"), "Target size");
			} else if (mode === "extract") {
				splitOptions.pages = shiftPageSelection(optionValue("pages"));
			}
			return { op: "split", options: splitOptions, files: wireFiles };
		}
		if (tool.id === "rotate") {
			const angle = Number(optionValue("angle"));
			if (angle !== 90 && angle !== 180 && angle !== 270) {
				throw new ToolInputError("Choose a 90, 180, or 270 degree rotation.");
			}
			const pagesText = optionValue("pages").trim();
			const options: Record<string, unknown> = { angle };
			if (pagesText !== "") {
				options.pages = shiftPageSelection(pagesText);
			}
			return { op: "rotate", options, files: wireFiles };
		}
		if (tool.id === "crop") {
			const left = numberInRange("left", "Left margin", 0, 10_000, 0);
			const top = numberInRange("top", "Top margin", 0, 10_000, 0);
			const right = numberInRange("right", "Right margin", 0, 10_000, 0);
			const bottom = numberInRange("bottom", "Bottom margin", 0, 10_000, 0);
			if (left === 0 && top === 0 && right === 0 && bottom === 0) {
				throw new ToolInputError("Enter at least one non-zero crop margin.");
			}
			const options: Record<string, unknown> = { left, top, right, bottom };
			const pagesText = optionValue("pages").trim();
			if (pagesText !== "") {
				options.pages = shiftPageSelection(pagesText);
			}
			return { op: "crop", options, files: wireFiles };
		}
		if (tool.id === "flip") {
			const options: Record<string, unknown> = { axis: optionValue("axis") };
			const pagesText = optionValue("pages").trim();
			if (pagesText !== "") {
				options.pages = shiftPageSelection(pagesText);
			}
			return { op: "flip", options, files: wireFiles };
		}
		if (tool.id === "page_labels") {
			const options: Record<string, unknown> = {
				style: optionValue("style"),
				start: numberInRange("start", "First number", 1, 1_000_000, 1, true),
				prefix: optionValue("prefix"),
			};
			const pagesText = optionValue("pages").trim();
			if (pagesText !== "") {
				options.pages = shiftPageSelection(pagesText);
			}
			return { op: "page_labels", options, files: wireFiles };
		}
		if (tool.id === "resize") {
			const sizes: Record<string, readonly [number, number]> = {
				a3: [841.89, 1190.55],
				a4: [595.28, 841.89],
				a5: [419.53, 595.28],
				letter: [612, 792],
				legal: [612, 1008],
			};
			const preset = optionValue("preset");
			const size = sizes[preset];
			let width: number;
			let height: number;
			if (size !== undefined) {
				[width, height] = size;
			} else {
				width = numberInRange("width", "Page width", 1, 14_400, 595.28);
				height = numberInRange("height", "Page height", 1, 14_400, 841.89);
			}
			const options: Record<string, unknown> = {
				width,
				height,
				fit: optionValue("size_mode") !== "stretch",
			};
			const pagesText = optionValue("pages").trim();
			if (pagesText !== "") {
				options.pages = shiftPageSelection(pagesText);
			}
			return { op: "resize", options, files: wireFiles };
		}
		if (tool.id === "nup") {
			const perSheet = Number(optionValue("per_sheet"));
			if (![2, 4, 6, 9].includes(perSheet)) {
				throw new ToolInputError("Pages per sheet must be 2, 4, 6, or 9.");
			}
			return {
				op: "nup",
				options: { per_sheet: perSheet },
				files: wireFiles,
			};
		}
		if (tool.id === "delete") {
			return {
				op: "delete",
				options: { pages: shiftPageSelection(optionValue("pages")) },
				files: wireFiles,
			};
		}
		if (tool.id === "reorder") {
			const mode = organizeModeValue();
			if (mode === "duplicate") {
				return {
					op: "reorder",
					options: {
						mode,
						pages: shiftPageSelection(optionValue("pages")),
					},
					files: wireFiles,
				};
			}
			if (mode === "blank") {
				const options: Record<string, unknown> = {
					mode,
					count: pageCount(optionValue("count"), "Number of blank pages"),
				};
				const afterText = optionValue("after").trim();
				if (afterText !== "") {
					options.after = pageBoundary(afterText, "Insert after page");
				}
				return { op: "reorder", options, files: wireFiles };
			}
			return {
				op: "reorder",
				options: {
					mode,
					order: shiftPageSelection(optionValue("order")),
				},
				files: wireFiles,
			};
		}
		if (tool.id === "page_numbers") {
			const options: Record<string, unknown> = {
				position: optionValue("position"),
				format: optionValue("format"),
				start: numberInRange("start", "First number", 1, 1_000_000_000, 1, true),
				font_size: numberInRange("font-size", "Font size", 0.1, 400, 10),
			};
			const pages = overlayPages();
			if (pages !== undefined) {
				options.pages = pages;
			}
			return { op: "page_numbers", options, files: wireFiles };
		}
		if (tool.id === "watermark") {
			const text = optionValue("text").trim();
			if (text === "") {
				throw new ToolInputError("Enter the watermark text.");
			}
			const options: Record<string, unknown> = {
				text,
				orientation: optionValue("orientation"),
				opacity: numberInRange("opacity", "Opacity", 0, 1, 0.2),
				font_size: numberInRange("font-size", "Font size", 0.1, 400, 48),
			};
			const pages = overlayPages();
			if (pages !== undefined) {
				options.pages = pages;
			}
			const tiled = optionControls.get("tiled");
			if (tiled instanceof HTMLInputElement && tiled.checked) {
				options.tiled = true;
			}
			return { op: "watermark", options, files: wireFiles };
		}
		if (tool.id === "image_watermark") {
			const options: Record<string, unknown> = {
				opacity: numberInRange("opacity", "Opacity", 0, 1, 0.3),
				scale: numberInRange("scale", "Scale", 0.01, 1000, 1),
			};
			for (const [key, label] of [
				["x", "Horizontal offset"],
				["y", "Vertical offset"],
			] as const) {
				const raw = optionValue(key).trim();
				if (raw !== "") {
					options[key] = numberInRange(key, label, -1_000_000, 1_000_000, 0);
				}
			}
			const pages = overlayPages();
			if (pages !== undefined) {
				options.pages = pages;
			}
			const tiled = optionControls.get("tiled");
			if (tiled instanceof HTMLInputElement && tiled.checked) {
				options.tiled = true;
			}
			return { op: "image_watermark", options, files: wireFiles };
		}
		if (tool.id === "outline") {
			const entries: Array<{ title: string; page: number; level: number }> = [];
			for (const [index, line] of optionValue("entries").split("\n").entries()) {
				const trimmed = line.trim();
				if (trimmed === "") {
					continue;
				}
				const parts = trimmed.split("|").map((part) => part.trim());
				if (parts.length < 2 || parts.length > 3) {
					throw new ToolInputError(`Bookmark line ${index + 1}: use "Title | page | level".`);
				}
				const title = parts[0] ?? "";
				if (title === "") {
					throw new ToolInputError(`Bookmark line ${index + 1}: the title is empty.`);
				}
				const page = Number(parts[1]);
				if (!Number.isInteger(page) || page < 1) {
					throw new ToolInputError(
						`Bookmark line ${index + 1}: the page must be a whole number of 1 or more.`,
					);
				}
				let level = 0;
				if (parts.length === 3) {
					level = Number(parts[2]);
					if (!Number.isInteger(level) || level < 0) {
						throw new ToolInputError(
							`Bookmark line ${index + 1}: the level must be a whole number of 0 or more.`,
						);
					}
				}
				entries.push({ title, page, level });
			}
			return { op: "outline", options: { entries }, files: wireFiles };
		}
		if (tool.id === "header_footer") {
			const header = {
				left: optionValue("header-left").trim(),
				center: optionValue("header-center").trim(),
				right: optionValue("header-right").trim(),
			};
			const footer = {
				left: optionValue("footer-left").trim(),
				center: optionValue("footer-center").trim(),
				right: optionValue("footer-right").trim(),
			};
			if (
				header.left === "" &&
				header.center === "" &&
				header.right === "" &&
				footer.left === "" &&
				footer.center === "" &&
				footer.right === ""
			) {
				throw new ToolInputError("Fill in at least one header or footer slot.");
			}
			const options: Record<string, unknown> = {
				header,
				footer,
				font_size: numberInRange("font-size", "Font size", 0.1, 400, 9),
			};
			const pages = overlayPages();
			if (pages !== undefined) {
				options.pages = pages;
			}
			return { op: "header_footer", options, files: wireFiles };
		}
		if (tool.id === "bates") {
			const options: Record<string, unknown> = {
				prefix: optionValue("prefix").trim(),
				suffix: optionValue("suffix").trim(),
				start: numberInRange("start", "First number", 1, 1_000_000_000, 1, true),
				digits: numberInRange("digits", "Minimum digits", 1, 20, 6, true),
			};
			const pages = overlayPages();
			if (pages !== undefined) {
				options.pages = pages;
			}
			return { op: "bates", options, files: wireFiles };
		}
		if (tool.id === "compress") {
			return { op: "compress", files: wireFiles };
		}
		if (tool.id === "zip") {
			return { op: "zip", files: wireFiles };
		}
		if (tool.id === "image_to_pdf") {
			return {
				op: "image_to_pdf",
				options: {
					page_size: optionValue("page_size"),
					margin: numberInRange("margin", "Page margin", 0, 10_000, 36),
				},
				files: wireFiles,
			};
		}
		if (tool.id === "attachments") {
			return { op: "attachments", files: wireFiles };
		}
		if (tool.id === "attach") {
			return { op: "attach", files: wireFiles };
		}
		if (tool.id === "detach") {
			return { op: "detach", files: wireFiles };
		}
		if (tool.id === "repair") {
			return { op: "repair", files: wireFiles };
		}
		if (tool.id === "strip_metadata") {
			return { op: "strip_metadata", files: wireFiles };
		}
		if (tool.id === "metadata_report") {
			return { op: "metadata_report", files: wireFiles };
		}
		if (tool.id === "batch") {
			return { op: "batch", options: { tool: optionValue("tool") }, files: wireFiles };
		}
		if (tool.id !== "metadata") {
			throw new ToolInputError("This tool is not wired into the local engine yet.");
		}
		const fields: Record<string, string> = {};
		for (const [key, label] of [
			["Title", "title"],
			["Author", "author"],
			["Subject", "subject"],
			["Keywords", "keywords"],
		] as const) {
			const value = optionValue(label).trim();
			if (value !== "") {
				fields[key] = value;
			}
		}
		if (Object.keys(fields).length === 0) {
			throw new ToolInputError("Fill in at least one metadata field.");
		}
		return { op: "metadata", options: { fields }, files: wireFiles };
	}

	async function readWireFiles(): Promise<WireFile[]> {
		const tool = currentTool();
		// Attach takes the PDF first and treats the remaining files as opaque
		// payloads, so only index 0 has to be a PDF. Image conversion accepts
		// the two containers `selis-image` decodes and nothing else. The image
		// watermark needs one of each, and the wire order is PDF then image, so
		// the pair is classified by header rather than by pick order.
		if (tool?.accepts === "pdf-image") {
			if (files.length !== 2) {
				throw new ToolInputError("Choose exactly one PDF and one PNG or JPEG watermark image.");
			}
			const read = await Promise.all(
				files.map(async (file) => ({
					name: file.name,
					bytes: new Uint8Array(await file.arrayBuffer()),
				})),
			);
			const document = read.find((file) => hasPdfHeader(file.bytes));
			const image = read.find((file) => hasImageHeader(file.bytes));
			if (document === undefined) {
				throw new ToolInputError("One of the two files must be a PDF.");
			}
			if (image === undefined) {
				throw new ToolInputError("One of the two files must be a PNG or JPEG.");
			}
			return [document, image];
		}
		const payloadsAfterFirst = tool?.id === "attach";
		const images = tool?.accepts === "image";
		const wireFiles: WireFile[] = [];
		for (const [index, file] of files.entries()) {
			const bytes = new Uint8Array(await file.arrayBuffer());
			if (images) {
				if (!hasImageHeader(bytes)) {
					throw new ToolInputError(`${file.name} does not look like a PNG or JPEG.`);
				}
			} else if (tool?.accepts === "text") {
				if (bytes.length === 0) {
					throw new ToolInputError(`${file.name} is empty.`);
				}
			} else if (!(payloadsAfterFirst && index > 0) && !hasPdfHeader(bytes)) {
				throw new ToolInputError(`${file.name} does not look like a PDF.`);
			}
			wireFiles.push({ name: file.name, bytes });
		}
		return wireFiles;
	}

	function getWasm(): Promise<ToolsWasm> {
		if (wasmPromise === null) {
			wasmPromise = loadTools().catch((error: unknown) => {
				wasmPromise = null;
				throw error;
			});
		}
		return wasmPromise;
	}

	let convertWasmPromise: Promise<ConvertWasm> | null = null;
	function getConvertWasm(): Promise<ConvertWasm> {
		if (convertWasmPromise === null) {
			convertWasmPromise = (async () => {
				const response = await fetch("/wasm/selis_pdf_wasm_convert.wasm", {
					credentials: "same-origin",
				});
				if (!response.ok) {
					throw new ToolProtocolError("the convert chunk is unavailable");
				}
				const bytes = await response.arrayBuffer();
				const module = await WebAssembly.compile(bytes);
				const instance = await WebAssembly.instantiate(module, {});
				return instance as unknown as ConvertWasm;
			})().catch((error: unknown) => {
				convertWasmPromise = null;
				throw error;
			});
		}
		return convertWasmPromise;
	}

	async function runConvert(): Promise<void> {
		setBusy(true);
		clearResult();
		setStatus("Reading your file on this device…", "info");
		try {
			const file = files[0];
			if (file === undefined) {
				throw new ToolInputError("Choose a file first.");
			}
			const bytes = new Uint8Array(await file.arrayBuffer());
			if (bytes.length === 0) {
				throw new ToolInputError(`${file.name} is empty.`);
			}
			const formatOption = optionValue("format");
			let format = 0;
			const lower = file.name.toLowerCase();
			if (formatOption === "md") {
				format = 0;
			} else if (formatOption === "html") {
				format = 1;
			} else if (formatOption === "csv") {
				format = 2;
			} else if (formatOption === "latex") {
				format = 3;
			} else if (formatOption === "epub") {
				format = 4;
			} else if (formatOption === "svg") {
				format = 5;
			} else {
				if (lower.endsWith(".html") || lower.endsWith(".htm") || lower.endsWith(".xhtml")) {
					format = 1;
				} else if (lower.endsWith(".csv")) {
					format = 2;
				} else if (lower.endsWith(".tex") || lower.endsWith(".latex")) {
					format = 3;
				} else if (lower.endsWith(".epub")) {
					format = 4;
				} else if (lower.endsWith(".svg")) {
					format = 5;
				} else {
					format = 0;
				}
			}
			const titleText = optionValue("title").trim();
			const titleBytes = new TextEncoder().encode(titleText);
			const wasm = await getConvertWasm();
			setStatus("Running locally…", "info");
			await nextPaint();
			const exports = wasm.exports;
			const memory = exports.memory;
			const inputPtr = exports.selis_convert_alloc(bytes.length);
			if (inputPtr === 0) {
				throw new ToolProtocolError("the convert chunk could not allocate input memory");
			}
			new Uint8Array(memory.buffer, inputPtr, bytes.length).set(bytes);
			let titlePtr = 0;
			if (titleBytes.length > 0) {
				titlePtr = exports.selis_convert_alloc(titleBytes.length);
				if (titlePtr === 0) {
					throw new ToolProtocolError("the convert chunk could not allocate title memory");
				}
				new Uint8Array(memory.buffer, titlePtr, titleBytes.length).set(titleBytes);
			}
			const outLenPtr = exports.selis_convert_alloc(4);
			const outPagesPtr = exports.selis_convert_alloc(4);
			const pageSize = 1;
			const resultPtr = exports.selis_convert_run(
				format,
				inputPtr,
				bytes.length,
				pageSize,
				titlePtr,
				titleBytes.length,
				outLenPtr,
				outPagesPtr,
			);
			exports.selis_convert_free(inputPtr, bytes.length);
			if (titlePtr !== 0) {
				exports.selis_convert_free(titlePtr, titleBytes.length);
			}
			const outLen = new DataView(memory.buffer).getUint32(outLenPtr, true);
			const outPages = new DataView(memory.buffer).getUint32(outPagesPtr, true);
			exports.selis_convert_free(outLenPtr, 4);
			exports.selis_convert_free(outPagesPtr, 4);
			if (resultPtr === 0) {
				const errLenPtr = exports.selis_convert_alloc(4);
				const errPtr = exports.selis_convert_last_error(errLenPtr);
				const errLen = new DataView(memory.buffer).getUint32(errLenPtr, true);
				let message = "The conversion failed.";
				if (errPtr !== 0 && errLen > 0) {
					message = new TextDecoder().decode(new Uint8Array(memory.buffer, errPtr, errLen));
					exports.selis_convert_free(errPtr, errLen);
				}
				exports.selis_convert_free(errLenPtr, 4);
				throw new ToolProtocolError(message);
			}
			const outBytes = new Uint8Array(memory.buffer, resultPtr, outLen).slice();
			exports.selis_convert_free(resultPtr, outLen);
			const name = `${file.name.replace(/\.[^.]+$/, "")}-converted.pdf`;
			const report: ToolOutputReport = {
				name,
				kind: "pdf",
				pages: outPages || 1,
				bytes: outBytes.length,
				verdict: "ok",
			};
			const output: ToolOutput = { name, bytes: outBytes };
			// The chunk generates a document rather than rewriting one, so there
			// is no input page count; it does assert the page count and the
			// structural gate, and saying so is what puts the Pages row on the
			// result instead of silently reporting nothing was checked.
			const verification: Verification = {
				pages_out: outPages || 1,
				annotations_out: 0,
				fields_out: 0,
				ocgs_out: 0,
				outline_entries_out: 0,
				embedded_files_out: 0,
				bytes_in: bytes.length,
				bytes_out: outBytes.length,
				verdict: "ok",
				checked: ["pages"],
			};
			const result = { verification, reports: [report], outputs: [output] };
			try {
				await requestRecorder.assertNoUpload([bytes]);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			showResult(result);
			setStatus("Done. The result was verified on this device.", "ok");
		} catch (error) {
			const message =
				error instanceof ToolProtocolError || error instanceof ToolInputError
					? error.message
					: "The tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	let rasterWasmPromise: Promise<RasterWasm> | null = null;
	function getRasterWasm(): Promise<RasterWasm> {
		if (rasterWasmPromise === null) {
			rasterWasmPromise = (async () => {
				const response = await fetch("/wasm/selis_pdf_wasm_raster.wasm", {
					credentials: "same-origin",
				});
				if (!response.ok) {
					throw new ToolProtocolError("the renderer is unavailable");
				}
				const bytes = await response.arrayBuffer();
				const module = await WebAssembly.compile(bytes);
				// No imports: the renderer cannot call out, so the document
				// bytes it is handed have nowhere to go but back to this page.
				const instance = await WebAssembly.instantiate(module, {});
				return instance as unknown as RasterWasm;
			})().catch((error: unknown) => {
				rasterWasmPromise = null;
				throw error;
			});
		}
		return rasterWasmPromise;
	}

	async function runRaster(toolId: RasterToolId): Promise<void> {
		setBusy(true);
		clearResult();
		setStatus("Reading your PDF on this device…", "info");
		try {
			const file = files[0];
			if (file === undefined) {
				throw new ToolInputError("Choose a PDF first.");
			}
			const bytes = new Uint8Array(await file.arrayBuffer());
			if (!hasPdfHeader(bytes)) {
				throw new ToolInputError(`${file.name} does not look like a PDF.`);
			}
			const dpi = numberInRange("dpi", "Resolution", 1, 600, 150, true);
			const format = RASTER_FORMATS[toolId].code;
			const extension = RASTER_FORMATS[toolId].extension;
			// A TIFF is a container: several pages live in one file, so unlike the
			// per-page formats its page count never turns the result into a ZIP.
			const container = toolId === "tiff";
			const quality = numberInRange("quality", "Quality", 1, 100, 90, true);
			const selection = optionValue("pages").trim();
			const selectionBytes = new TextEncoder().encode(selection);
			const wasm = await getRasterWasm();
			setStatus("Rendering locally…", "info");
			await nextPaint();
			const exports = wasm.exports;
			const memory = exports.memory;
			const inputPtr = exports.selis_raster_alloc(bytes.length);
			if (inputPtr === 0) {
				throw new ToolProtocolError("the renderer could not allocate input memory");
			}
			new Uint8Array(memory.buffer, inputPtr, bytes.length).set(bytes);
			let pagesPtr = 0;
			if (selectionBytes.length > 0) {
				pagesPtr = exports.selis_raster_alloc(selectionBytes.length);
				if (pagesPtr === 0) {
					exports.selis_raster_free(inputPtr, bytes.length);
					throw new ToolProtocolError("the renderer could not allocate page memory");
				}
				new Uint8Array(memory.buffer, pagesPtr, selectionBytes.length).set(selectionBytes);
			}
			const outLenPtr = exports.selis_raster_alloc(4);
			const outFilesPtr = exports.selis_raster_alloc(4);
			if (outLenPtr === 0 || outFilesPtr === 0) {
				exports.selis_raster_free(inputPtr, bytes.length);
				if (pagesPtr !== 0) {
					exports.selis_raster_free(pagesPtr, selectionBytes.length);
				}
				throw new ToolProtocolError("the renderer could not allocate result memory");
			}
			const resultPtr = exports.selis_raster_run(
				inputPtr,
				bytes.length,
				pagesPtr,
				selectionBytes.length,
				dpi,
				format,
				quality,
				outLenPtr,
				outFilesPtr,
			);
			exports.selis_raster_free(inputPtr, bytes.length);
			if (pagesPtr !== 0) {
				exports.selis_raster_free(pagesPtr, selectionBytes.length);
			}
			const view = new DataView(memory.buffer);
			const outLen = view.getUint32(outLenPtr, true);
			const outFiles = view.getUint32(outFilesPtr, true);
			exports.selis_raster_free(outLenPtr, 4);
			exports.selis_raster_free(outFilesPtr, 4);
			if (resultPtr === 0) {
				const errLenPtr = exports.selis_raster_alloc(4);
				const errPtr = exports.selis_raster_last_error(errLenPtr);
				const errLen = new DataView(memory.buffer).getUint32(errLenPtr, true);
				let message = "The pages could not be rendered.";
				if (errPtr !== 0 && errLen > 0) {
					message = new TextDecoder().decode(new Uint8Array(memory.buffer, errPtr, errLen));
					exports.selis_raster_free(errPtr, errLen);
				}
				exports.selis_raster_free(errLenPtr, 4);
				throw new ToolProtocolError(message);
			}
			const outBytes = new Uint8Array(memory.buffer, resultPtr, outLen).slice();
			exports.selis_raster_free(resultPtr, outLen);
			if (outFiles === 0) {
				throw new ToolProtocolError("the renderer returned no pages.");
			}
			const stem = file.name.replace(/\.[^.]+$/, "");
			const bundle = outFiles > 1 && !container;
			const name = bundle ? `${stem}-pages.zip` : `${stem}.${extension}`;
			const report: ToolOutputReport = {
				name,
				kind: bundle ? "zip" : "image",
				pages: bundle ? 0 : outFiles,
				bytes: outBytes.length,
				verdict: "ok",
			};
			const output: ToolOutput = { name, bytes: outBytes };
			// The chunk verifies itself before returning: a PNG is decoded back and
			// matched pixel for pixel, a TIFF's page chain is walked and every page's
			// samples compared, a JPEG is decoded back and compared with the
			// page inside the bounds its coder can honestly meet. Either way `ok` is
			// a claim this page has observed rather than a hope.
			const verification: Verification = {
				pages_out: outFiles,
				annotations_out: 0,
				fields_out: 0,
				ocgs_out: 0,
				outline_entries_out: 0,
				embedded_files_out: 0,
				bytes_in: bytes.length,
				bytes_out: outBytes.length,
				verdict: "ok",
				checked: ["pages"],
			};
			const result: ReturnType<typeof runTool> = {
				verification,
				reports: [report],
				outputs: [output],
			};
			try {
				await requestRecorder.assertNoUpload([bytes]);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			showResult(result);
			setStatus("Done. The result was verified on this device.", "ok");
		} catch (error) {
			const message =
				error instanceof ToolProtocolError || error instanceof ToolInputError
					? error.message
					: "The tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	let textWasmPromise: Promise<TextWasm> | null = null;
	function getTextWasm(): Promise<TextWasm> {
		if (textWasmPromise === null) {
			textWasmPromise = loadTextChunk().catch((error: unknown) => {
				textWasmPromise = null;
				throw error;
			});
		}
		return textWasmPromise;
	}

	/**
	 * The eight text surfaces in one card, because they all read the same
	 * document through the same chunk and differ only in what they return: the
	 * separated text and its quality report, the tables as CSV, the reflowed
	 * article, the same article as an EPUB book, a Word document, a PowerPoint
	 * deck, an Excel workbook, or a ZIP of parts split at a phrase. The mode
	 * select decides, and the card says which one it will run.
	 */
	async function runText(): Promise<void> {
		setBusy(true);
		clearResult();
		setStatus("Reading your PDF on this device…", "info");
		try {
			const file = files[0];
			if (file === undefined) {
				throw new ToolInputError("Choose a PDF first.");
			}
			const bytes = new Uint8Array(await file.arrayBuffer());
			if (!hasPdfHeader(bytes)) {
				throw new ToolInputError(`${file.name} does not look like a PDF.`);
			}
			const mode = optionValue("textMode");
			const options: Record<string, string> = {
				name: file.name,
				title: file.name.replace(/\.[^.]+$/, ""),
			};
			if (mode === "split") {
				const find = optionValue("find").trim();
				if (find === "") {
					throw new ToolInputError("Type the phrase to split at.");
				}
				options.find = find;
			} else {
				const pages = optionValue("pages").trim();
				if (pages !== "") {
					options.pages = pages;
				}
			}
			const wasm = await getTextWasm();
			setStatus("Recovering the text locally…", "info");
			await nextPaint();
			const op: TextOp =
				mode === "split"
					? "split"
					: mode === "tables"
						? "tables"
						: mode === "reflow"
							? "reflow"
							: mode === "epub"
								? "epub"
								: mode === "word"
									? "word"
									: mode === "slides"
										? "slides"
										: mode === "workbook"
											? "workbook"
											: "text";
			const outputs = runTextOp(wasm, bytes, op, options);
			const reports: ToolOutputReport[] = outputs.map((output: TextOutput) => ({
				name: output.name,
				kind: output.kind,
				pages: 0,
				bytes: output.bytes.byteLength,
				verdict: "read",
			}));
			const bytesOut = outputs.reduce((total, output) => total + output.bytes.byteLength, 0);
			const verification: Verification = {
				pages_out: 0,
				annotations_out: 0,
				fields_out: 0,
				ocgs_out: 0,
				outline_entries_out: 0,
				embedded_files_out: 0,
				bytes_in: bytes.byteLength,
				bytes_out: bytesOut,
				verdict: "read",
				checked: [],
			};
			const result = {
				verification,
				reports,
				outputs: outputs.map((output) => ({ name: output.name, bytes: output.bytes })),
			};
			try {
				await requestRecorder.assertNoUpload([bytes]);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			showResult(result);
			setStatus("Done. The export was read from the document on this device.", "ok");
		} catch (error) {
			const message =
				error instanceof ToolProtocolError ||
				error instanceof TextProtocolError ||
				error instanceof ToolInputError
					? error.message
					: "The text tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis text tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	let htmlWasmPromise: Promise<HtmlWasm> | null = null;
	function getHtmlWasm(): Promise<HtmlWasm> {
		if (htmlWasmPromise === null) {
			htmlWasmPromise = (async () => {
				const response = await fetch("/wasm/selis_pdf_wasm_html.wasm", {
					credentials: "same-origin",
				});
				if (!response.ok) {
					throw new ToolProtocolError("the HTML writer is unavailable");
				}
				const bytes = await response.arrayBuffer();
				const module = await WebAssembly.compile(bytes);
				// No imports: the writer cannot call out, so the document bytes it
				// is handed have nowhere to go but back to this page.
				const instance = await WebAssembly.instantiate(module, {});
				return instance as unknown as HtmlWasm;
			})().catch((error: unknown) => {
				htmlWasmPromise = null;
				throw error;
			});
		}
		return htmlWasmPromise;
	}

	/**
	 * One `.html` file for the whole selection, whatever its length: the pages are
	 * `<section>`s inside a single document, not one file per page. So there is no
	 * ZIP branch here — a three-page selection is one download.
	 */
	async function runHtml(): Promise<void> {
		setBusy(true);
		clearResult();
		setStatus("Reading your PDF on this device…", "info");
		try {
			const file = files[0];
			if (file === undefined) {
				throw new ToolInputError("Choose a PDF first.");
			}
			const bytes = new Uint8Array(await file.arrayBuffer());
			if (!hasPdfHeader(bytes)) {
				throw new ToolInputError(`${file.name} does not look like a PDF.`);
			}
			const encoder = new TextEncoder();
			const selection = optionValue("pages").trim();
			const selectionBytes = encoder.encode(selection);
			// The name the user picked is the only title worth putting in the
			// document: a tab that says "export" tells them nothing about which
			// PDF they exported.
			const titleBytes = encoder.encode(file.name.replace(/\.[^.]+$/, ""));
			const wasm = await getHtmlWasm();
			setStatus("Writing the HTML document locally…", "info");
			await nextPaint();
			const exports = wasm.exports;
			const memory = exports.memory;
			const inputPtr = exports.selis_html_alloc(bytes.length);
			if (inputPtr === 0) {
				throw new ToolProtocolError("the HTML writer could not allocate input memory");
			}
			new Uint8Array(memory.buffer, inputPtr, bytes.length).set(bytes);
			let pagesPtr = 0;
			if (selectionBytes.length > 0) {
				pagesPtr = exports.selis_html_alloc(selectionBytes.length);
				if (pagesPtr === 0) {
					exports.selis_html_free(inputPtr, bytes.length);
					throw new ToolProtocolError("the HTML writer could not allocate page memory");
				}
				new Uint8Array(memory.buffer, pagesPtr, selectionBytes.length).set(selectionBytes);
			}
			let titlePtr = 0;
			if (titleBytes.length > 0) {
				titlePtr = exports.selis_html_alloc(titleBytes.length);
				if (titlePtr === 0) {
					exports.selis_html_free(inputPtr, bytes.length);
					if (pagesPtr !== 0) {
						exports.selis_html_free(pagesPtr, selectionBytes.length);
					}
					throw new ToolProtocolError("the HTML writer could not allocate title memory");
				}
				new Uint8Array(memory.buffer, titlePtr, titleBytes.length).set(titleBytes);
			}
			const outLenPtr = exports.selis_html_alloc(4);
			const outPagesPtr = exports.selis_html_alloc(4);
			if (outLenPtr === 0 || outPagesPtr === 0) {
				exports.selis_html_free(inputPtr, bytes.length);
				if (pagesPtr !== 0) {
					exports.selis_html_free(pagesPtr, selectionBytes.length);
				}
				if (titlePtr !== 0) {
					exports.selis_html_free(titlePtr, titleBytes.length);
				}
				throw new ToolProtocolError("the HTML writer could not allocate result memory");
			}
			const resultPtr = exports.selis_html_run(
				inputPtr,
				bytes.length,
				pagesPtr,
				selectionBytes.length,
				titlePtr,
				titleBytes.length,
				outLenPtr,
				outPagesPtr,
			);
			exports.selis_html_free(inputPtr, bytes.length);
			if (pagesPtr !== 0) {
				exports.selis_html_free(pagesPtr, selectionBytes.length);
			}
			if (titlePtr !== 0) {
				exports.selis_html_free(titlePtr, titleBytes.length);
			}
			const view = new DataView(memory.buffer);
			const outLen = view.getUint32(outLenPtr, true);
			const outPages = view.getUint32(outPagesPtr, true);
			exports.selis_html_free(outLenPtr, 4);
			exports.selis_html_free(outPagesPtr, 4);
			if (resultPtr === 0) {
				const errLenPtr = exports.selis_html_alloc(4);
				const errPtr = exports.selis_html_last_error(errLenPtr);
				const errLen = new DataView(memory.buffer).getUint32(errLenPtr, true);
				let message = "The pages could not be written as HTML.";
				if (errPtr !== 0 && errLen > 0) {
					message = new TextDecoder().decode(new Uint8Array(memory.buffer, errPtr, errLen));
					exports.selis_html_free(errPtr, errLen);
				}
				exports.selis_html_free(errLenPtr, 4);
				throw new ToolProtocolError(message);
			}
			const outBytes = new Uint8Array(memory.buffer, resultPtr, outLen).slice();
			exports.selis_html_free(resultPtr, outLen);
			if (outPages === 0) {
				throw new ToolProtocolError("the HTML writer returned a document with no pages.");
			}
			const name = `${file.name.replace(/\.[^.]+$/, "")}.html`;
			const report: ToolOutputReport = {
				name,
				kind: "html",
				pages: outPages,
				bytes: outBytes.length,
				verdict: "ok",
			};
			const output: ToolOutput = { name, bytes: outBytes };
			// The chunk hands back a document it has already re-read with its own
			// strict HTML reader and compared, span for span, against the extraction
			// it came from — so `ok` is a claim about bytes this device produced,
			// not about a writer that hoped.
			const verification: Verification = {
				pages_out: outPages,
				annotations_out: 0,
				fields_out: 0,
				ocgs_out: 0,
				outline_entries_out: 0,
				embedded_files_out: 0,
				bytes_in: bytes.length,
				bytes_out: outBytes.length,
				verdict: "ok",
				checked: ["pages"],
			};
			const result: ReturnType<typeof runTool> = {
				verification,
				reports: [report],
				outputs: [output],
			};
			try {
				await requestRecorder.assertNoUpload([bytes]);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			showResult(result);
			setStatus("Done. The result was verified on this device.", "ok");
		} catch (error) {
			const message =
				error instanceof ToolProtocolError || error instanceof ToolInputError
					? error.message
					: "The tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	let securityWasmPromise: Promise<SecurityWasm> | null = null;
	function getSecurityWasm(): Promise<SecurityWasm> {
		if (securityWasmPromise === null) {
			securityWasmPromise = (async () => {
				const response = await fetch(SECURITY_WASM_URL, { credentials: "same-origin" });
				if (!response.ok) {
					throw new ToolProtocolError("the security chunk is unavailable");
				}
				const bytes = await response.arrayBuffer();
				// `instantiateSecurity` refuses a module that declares any import, so
				// the guest the passwords and document bytes reach has nowhere to
				// send them.
				return await instantiateSecurity(bytes);
			})().catch((error: unknown) => {
				securityWasmPromise = null;
				throw error;
			});
		}
		return securityWasmPromise;
	}

	let signWasmPromise: Promise<SignWasm> | null = null;
	function getSignWasm(): Promise<SignWasm> {
		if (signWasmPromise === null) {
			signWasmPromise = (async () => {
				const response = await fetch(SIGN_WASM_URL, { credentials: "same-origin" });
				if (!response.ok) {
					throw new ToolProtocolError("the signing chunk is unavailable");
				}
				const bytes = await response.arrayBuffer();
				// `instantiateSign` refuses a module that declares any import, so the
				// guest that sees the key and the document has nowhere to send them.
				return await instantiateSign(bytes);
			})().catch((error: unknown) => {
				signWasmPromise = null;
				throw error;
			});
		}
		return signWasmPromise;
	}

	/** The text of the one file a card's file input holds. */
	async function readOptionText(key: string): Promise<string> {
		const control = optionControls.get(key);
		if (!(control instanceof HTMLInputElement) || control.type !== "file") {
			return "";
		}
		const file = control.files?.[0];
		return file === undefined ? "" : await file.text();
	}

	/** The text of every file a card's file input holds, in selection order. */
	async function readOptionTexts(key: string): Promise<string[]> {
		const control = optionControls.get(key);
		if (!(control instanceof HTMLInputElement) || control.type !== "file") {
			return [];
		}
		const out: string[] = [];
		for (const file of control.files ?? []) {
			out.push(await file.text());
		}
		return out;
	}

	/** A date the approval block prints: `YYYY-MM-DD`, from the page's clock. */
	function today(): string {
		return new Date().toISOString().slice(0, 10);
	}

	/** The document bytes the grid already holds, refused before they reach a guest. */
	async function readSinglePdf(): Promise<WireFile> {
		const file = files[0];
		if (file === undefined) {
			throw new ToolInputError("Choose a PDF first.");
		}
		const bytes = new Uint8Array(await file.arrayBuffer());
		if (!hasPdfHeader(bytes)) {
			throw new ToolInputError(`${file.name} does not look like a PDF.`);
		}
		return { name: file.name, bytes };
	}

	/**
	 * The request a stamp or signature run makes. A stamp is nothing but typed
	 * lines; a certificate signature carries the key and chain the user picked,
	 * and the page supplies the signing time because the chunk has no clock.
	 */
	async function buildSignRequest(file: WireFile): Promise<SignRequest> {
		const page = numberInRange("page", "Page", 1, 100_000, 1, true);
		const signerName = optionValue("signerName").trim();
		if (optionValue("signMode") === "certificate") {
			const keyPem = await readOptionText("certKey");
			if (keyPem.trim() === "") {
				throw new ToolInputError("Choose the private key file first.");
			}
			const certificatesPem = await readOptionTexts("certChain");
			if (certificatesPem.length === 0) {
				throw new ToolInputError("Choose at least the leaf certificate (then any intermediates).");
			}
			const options: Record<string, unknown> = {
				page,
				rect: [...SIGNATURE_RECT],
				fieldName: optionValue("field").trim() === "" ? "Signature" : optionValue("field").trim(),
				signerName: signerName === "" ? "Selis signer" : signerName,
				signingTime: signingStamp(Math.floor(Date.now() / 1000)),
				keyPem,
				certificatesPem,
			};
			const reason = optionValue("reason").trim();
			if (reason !== "") {
				options.reason = reason;
			}
			const location = optionValue("location").trim();
			if (location !== "") {
				options.location = location;
			}
			return { op: "sign", options, file };
		}
		const text = optionValue("stampText").trim();
		if (text === "") {
			throw new ToolInputError("Enter the stamp text.");
		}
		const second = signerName === "" ? today() : `${signerName} · ${today()}`;
		return {
			op: "stamp",
			options: {
				page,
				rect: [...SIGNATURE_RECT],
				lines: [
					{ text, size: 12 },
					{ text: second, size: 9 },
				],
				strokes: [],
				color: [0, 0, 0],
				strokeWidth: 1,
				fontSize: 11,
				border: true,
			},
			file,
		};
	}

	/** The bytes a no-upload check must not find in a request: the document and any key. */
	async function signSecrets(file: WireFile): Promise<Uint8Array[]> {
		const secrets: Uint8Array[] = [file.bytes];
		if (optionValue("signMode") === "certificate") {
			const encoder = new TextEncoder();
			const key = await readOptionText("certKey");
			if (key !== "") {
				secrets.push(encoder.encode(key));
			}
		}
		return secrets;
	}

	async function runSignCard(): Promise<void> {
		setBusy(true);
		clearResult();
		setStatus("Reading your PDF on this device…", "info");
		try {
			const file = await readSinglePdf();
			const request = await buildSignRequest(file);
			const secrets = await signSecrets(file);
			const wasm = await getSignWasm();
			setStatus("Signing on this device…", "info");
			await nextPaint();
			const result = runSign(wasm, request);
			if (result.kind !== "document") {
				throw new ToolProtocolError("a signing run produced no document");
			}
			try {
				await requestRecorder.assertNoUpload(secrets);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			showResult(
				{
					verification: result.verification,
					reports: [result.report],
					outputs: [result.output],
				},
				signRows(result),
			);
			setStatus(
				result.outcome.operation === "stamp"
					? "Done. The stamp was appended and verified on this device."
					: "Done. The signature was written and verified on this device.",
				"ok",
			);
		} catch (error) {
			const message =
				error instanceof ToolProtocolError || error instanceof ToolInputError
					? error.message
					: "The tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	async function runCertificateCard(): Promise<void> {
		setBusy(true);
		clearResult();
		setStatus("Reading your PDF on this device…", "info");
		try {
			const file = await readSinglePdf();
			const anchors = await readOptionTexts("anchors");
			const options: Record<string, unknown> = {
				now: Math.floor(Date.now() / 1000),
			};
			if (anchors.length > 0) {
				options.anchors = anchors;
			}
			const wasm = await getSignWasm();
			setStatus("Checking every signature locally…", "info");
			await nextPaint();
			const result = runSign(wasm, { op: "report", options, file });
			if (result.kind !== "report") {
				throw new ToolProtocolError("a signature report produced a document");
			}
			try {
				await requestRecorder.assertNoUpload([file.bytes]);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			const report = result.report;
			const verified = report.signatures.filter((signature) => signature.verified).length;
			showReportCard(
				"Signature report",
				`${report.signatures.length} signature${report.signatures.length === 1 ? "" : "s"} · ${report.anchorCount} anchor${report.anchorCount === 1 ? "" : "s"} · checked locally`,
				signatureRows(report),
			);
			setStatus(
				report.signatures.length === 0
					? "Done. This document carries no signature."
					: `Done. ${verified} of ${report.signatures.length} signature${report.signatures.length === 1 ? "" : "s"} verified on this device.`,
				"ok",
			);
		} catch (error) {
			const message =
				error instanceof ToolProtocolError || error instanceof ToolInputError
					? error.message
					: "The tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	const DECISION_LABELS: Readonly<Record<string, string>> = {
		approved: "APPROVED",
		changes: "CHANGES REQUESTED",
		rejected: "REJECTED",
		reviewed: "REVIEWED",
	};

	async function runWorkflowCard(): Promise<void> {
		setBusy(true);
		clearResult();
		setStatus("Reading your PDF on this device…", "info");
		try {
			const file = await readSinglePdf();
			const approver = optionValue("approver").trim();
			if (approver === "") {
				throw new ToolInputError("Enter the approver's name.");
			}
			const decision = DECISION_LABELS[optionValue("decision")] ?? "REVIEWED";
			const note = optionValue("note").trim();
			const lines = [
				{ text: decision, size: 12 },
				{ text: `${approver} · ${today()}`, size: 9 },
			];
			if (note !== "") {
				lines.push({ text: note.slice(0, 96), size: 8 });
			}
			const request: SignRequest = {
				op: "stamp",
				options: {
					page: numberInRange("page", "Page", 1, 100_000, 1, true),
					rect: [...SIGNATURE_RECT],
					lines,
					strokes: [],
					color: [0, 0, 0],
					strokeWidth: 1,
					fontSize: 11,
					border: true,
				},
				file,
			};
			const wasm = await getSignWasm();
			setStatus("Recording the decision locally…", "info");
			await nextPaint();
			const result = runSign(wasm, request);
			if (result.kind !== "document") {
				throw new ToolProtocolError("the approval run produced no document");
			}
			try {
				await requestRecorder.assertNoUpload([file.bytes]);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			showResult(
				{
					verification: result.verification,
					reports: [result.report],
					outputs: [result.output],
				},
				[
					{ label: "Decision", value: decision },
					{ label: "Approver", value: approver },
					...(note === "" ? [] : [{ label: "Note", value: note }]),
					{
						label: "Workflow state",
						value: "recorded in the appended block; no server holds a copy",
					},
					...signRows(result),
				],
			);
			setStatus("Done. The decision was appended and verified on this device.", "ok");
		} catch (error) {
			const message =
				error instanceof ToolProtocolError || error instanceof ToolInputError
					? error.message
					: "The tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	/**
	 * The options for one security card. A protect run with no owner password
	 * simply omits the field — the chunk then writes the open password as the
	 * owner password, the way Acrobat does — and the 64 bytes of key material are
	 * drawn here, because the guest has no entropy of its own and must not
	 * invent a document key.
	 */
	function buildSecurityRequest(toolId: SecurityToolId, file: WireFile): SecurityRequest {
		if (toolId === "unlock" || toolId === "clear_permissions") {
			return {
				op: toolId,
				options: { password: optionValue("password") },
				file,
			};
		}
		const userPassword = toolId === "protect" ? optionValue("userPassword") : "";
		const ownerPassword = optionValue("ownerPassword");
		if (userPassword === "" && ownerPassword === "") {
			throw new ToolInputError(
				toolId === "protect"
					? "Enter an open password, an owner password, or both."
					: "Setting permissions needs an owner password.",
			);
		}
		const options: Record<string, unknown> = {
			userPassword,
			permissions: permissionSpec(),
			encryptMetadata: optionChecked("encryptMetadata"),
			keyMaterial: randomKeyMaterial(),
		};
		if (ownerPassword !== "") {
			options.ownerPassword = ownerPassword;
		}
		return { op: "protect", options, file };
	}

	/** One plain sentence about what the protection now is. */
	function securitySummary(result: SecurityResult): string {
		const { outcome } = result;
		if (outcome.encrypted === false) {
			return "Done. This PDF had no password, so its own bytes were copied.";
		}
		if (outcome.encrypted === true) {
			return "Done. The password is removed and the open document was verified on this device.";
		}
		if (outcome.granted !== undefined) {
			const list = outcome.granted.length === 0 ? "none" : outcome.granted.join(", ");
			return `Done. Protected with AES-256 and verified on this device. Granted: ${list}.`;
		}
		if (outcome.previouslyDenied !== undefined) {
			return outcome.previouslyDenied.length === 0
				? "Done. Nothing was restricted before, so the flags already granted everything."
				: `Done. Permissions cleared and verified on this device. Now granted: ${outcome.previouslyDenied.join(", ")}.`;
		}
		return "Done. The result was verified on this device.";
	}

	/**
	 * One password or permission operation over the lazy security chunk. Always
	 * one document in, one document out.
	 */
	async function runSecurityCard(toolId: SecurityToolId): Promise<void> {
		setBusy(true);
		clearResult();
		setStatus("Reading your PDF on this device…", "info");
		try {
			const file = files[0];
			if (file === undefined) {
				throw new ToolInputError("Choose a PDF first.");
			}
			const bytes = new Uint8Array(await file.arrayBuffer());
			if (!hasPdfHeader(bytes)) {
				throw new ToolInputError(`${file.name} does not look like a PDF.`);
			}
			const request = buildSecurityRequest(toolId, { name: file.name, bytes });
			const wasm = await getSecurityWasm();
			setStatus("Working on the document locally…", "info");
			await nextPaint();
			const result = runSecurity(wasm, request);
			const shown: ReturnType<typeof runTool> = {
				verification: result.verification,
				reports: [result.report],
				outputs: [result.output],
			};
			try {
				await requestRecorder.assertNoUpload([bytes]);
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			showResult(shown, securityRows(result));
			setStatus(securitySummary(result), "ok");
		} catch (error) {
			const message =
				error instanceof ToolProtocolError || error instanceof ToolInputError
					? error.message
					: "The tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	/**
	 * The result card for a read-only report: rows, no output file. The report
	 * card is the one result that cannot be downloaded by accident, because a
	 * signature report is a statement about the document, not a document.
	 */
	function showReportCard(title: string, meta: string, rows: readonly VerificationRow[]): void {
		clearResult();
		resultTitle.textContent = title;
		resultMeta.textContent = meta;
		resultOutputs.replaceChildren();
		const nodes: HTMLElement[] = [];
		for (const row of rows) {
			const wrapper = make("div", "result-row");
			const term = make("dt");
			term.textContent = row.label;
			const detail = make("dd");
			detail.textContent = row.value;
			wrapper.append(term, detail);
			nodes.push(wrapper);
		}
		resultRows.replaceChildren(...nodes);
		resultCard.hidden = false;
		resultCard.scrollIntoView({ behavior: "smooth", block: "nearest" });
	}

	function showResult(
		result: ReturnType<typeof runTool>,
		extraRows: readonly VerificationRow[] = [],
	): void {
		clearResult();
		const totalBytes = result.outputs.reduce((total, output) => total + output.bytes.byteLength, 0);
		const pdfCount = result.reports.filter((report) => report.kind === "pdf").length;
		const fileCount = result.reports.filter((report) => report.kind === "file").length;
		const hasZip = result.reports.some((report) => report.kind === "zip");
		const single = result.outputs.length === 1 ? result.outputs[0] : undefined;
		resultTitle.textContent =
			single !== undefined
				? single.name
				: fileCount > 0
					? `${fileCount} attachment${fileCount === 1 ? "" : "s"}${hasZip ? " and a ZIP" : ""}`
					: `${pdfCount} PDF${pdfCount === 1 ? "" : "s"}${hasZip ? " and a ZIP" : ""}`;
		resultMeta.textContent = `${result.outputs.length} file${result.outputs.length === 1 ? "" : "s"} · ${formatBytes(totalBytes)} · verified locally`;

		const outputRows: HTMLElement[] = [];
		result.outputs.forEach((output, index) => {
			const report = result.reports[index];
			if (report === undefined) {
				throw new ToolProtocolError("the tool chunk returned no metadata for an output");
			}
			const copy = new Uint8Array(output.bytes.byteLength);
			copy.set(output.bytes);
			const raster = rasterInfo(output.name);
			const blob = new Blob([copy.buffer], {
				type:
					report.kind === "zip"
						? "application/zip"
						: report.kind === "file"
							? "application/octet-stream"
							: report.kind === "report"
								? "application/json"
								: report.kind === "image"
									? raster.mime
									: report.kind === "html"
										? "text/html"
										: "application/pdf",
			});
			const url = URL.createObjectURL(blob);
			resultUrls.push(url);

			const row = make("li", "output-row");
			row.dataset.kind = report.kind;
			row.dataset.name = output.name;
			row.dataset.bytes = String(output.bytes.byteLength);
			row.dataset.pages = String(report.pages);
			row.dataset.overTarget = report.overTarget === true ? "true" : "false";
			const info = make("div", "output-info");
			const name = make("span", "output-name");
			name.textContent = output.name;
			const meta = make("span", "output-meta");
			const overTarget = report.overTarget === true ? " · over target" : "";
			meta.textContent =
				report.kind === "zip"
					? `ZIP bundle · ${formatBytes(output.bytes.byteLength)}`
					: `${report.pages} page${report.pages === 1 ? "" : "s"} · ${formatBytes(output.bytes.byteLength)} · ${report.verdict}${overTarget}`;
			if (report.kind === "file") {
				meta.textContent = `Attachment | ${formatBytes(output.bytes.byteLength)} | ${report.verdict}`;
			}
			if (report.kind === "report") {
				meta.textContent = `Metadata report | ${formatBytes(output.bytes.byteLength)} | ${report.verdict}`;
			}
			if (report.kind === "image") {
				meta.textContent = `${raster.label} | ${formatBytes(output.bytes.byteLength)} | ${report.verdict}`;
			}
			if (report.kind === "text") {
				meta.textContent = `Plain text | ${formatBytes(output.bytes.byteLength)}`;
			}
			if (report.kind === "tables") {
				meta.textContent = `Tables (CSV) | ${formatBytes(output.bytes.byteLength)}`;
			}
			if (report.kind === "html") {
				meta.textContent = `HTML article | ${formatBytes(output.bytes.byteLength)}`;
			}
			if (report.kind === "file") {
				meta.textContent = `Document | ${formatBytes(output.bytes.byteLength)}`;
			}
			info.append(name, meta);

			const actions = make("div", "output-actions");
			// An HTML result is deliberately not previewed: a blob: document opens
			// in this page's own origin, so the only honest action is a download.
			if (report.kind === "pdf" || (report.kind === "image" && raster.preview)) {
				const preview = make("a", "text-button");
				preview.href = url;
				preview.target = "_blank";
				preview.rel = "noopener";
				preview.textContent = "Preview";
				actions.append(preview);
			}
			const download = make("a", "primary-link");
			download.href = url;
			download.download = output.name;
			download.textContent = report.kind === "zip" ? "Download all (.zip)" : "Download";
			download.addEventListener("click", () => {
				// The href is the Blob URL; no network request is made by a download.
				setStatus("Saved to your download folder by the browser.", "ok");
			});
			actions.append(download);
			row.append(info, actions);
			outputRows.push(row);
		});
		resultOutputs.replaceChildren(...outputRows);

		const rows: HTMLElement[] = [];
		const shown = [
			...batchRows(result.batch),
			...reportRows(result.reports, result.outputs),
			...extraRows,
			...verificationRows(result.verification),
		];
		for (const row of shown) {
			const wrapper = make("div", "result-row");
			const term = make("dt");
			term.textContent = row.label;
			const detail = make("dd");
			detail.textContent = row.value;
			wrapper.append(term, detail);
			rows.push(wrapper);
		}
		resultRows.replaceChildren(...rows);
		resultCard.hidden = false;
		resultCard.scrollIntoView({ behavior: "smooth", block: "nearest" });
	}

	async function run(): Promise<void> {
		if (busy) {
			return;
		}
		const tool = currentTool();
		if (tool === null) {
			return;
		}
		const needed = tool.minimum ?? (tool.multiple ? 2 : 1);
		if (files.length < needed) {
			setStatus(
				tool.id === "attach"
					? "Choose a PDF and at least one file to embed first."
					: tool.accepts === "image"
						? "Choose at least one PNG or JPEG image first."
						: tool.multiple
							? "Choose at least two PDFs first."
							: "Choose a PDF first.",
				"error",
			);
			return;
		}
		setBusy(true);
		clearResult();
		setStatus("Reading your PDFs on this device…", "info");
		try {
			if (tool.id === "convert") {
				await runConvert();
				return;
			}
			if (tool.id === "png" || tool.id === "jpg" || tool.id === "tiff" || tool.id === "webp") {
				await runRaster(tool.id);
				return;
			}
			if (tool.id === "html") {
				await runHtml();
				return;
			}
			if (tool.id === "text") {
				await runText();
				return;
			}
			if (
				tool.id === "unlock" ||
				tool.id === "protect" ||
				tool.id === "clear_permissions" ||
				tool.id === "permissions"
			) {
				await runSecurityCard(tool.id);
				return;
			}
			if (tool.id === "sign") {
				await runSignCard();
				return;
			}
			if (tool.id === "certificate") {
				await runCertificateCard();
				return;
			}
			if (tool.id === "workflow") {
				await runWorkflowCard();
				return;
			}
			const wireFiles = await readWireFiles();
			const request = buildRequest(tool, wireFiles);
			setStatus("Loading the local tools engine…", "info");
			const wasm = await getWasm();
			setStatus("Running locally…", "info");
			await nextPaint();
			const result = runTool(wasm, request);
			try {
				await requestRecorder.assertNoUpload(wireFiles.map((file) => file.bytes));
			} catch (error) {
				console.error("The local no-upload check failed:", error);
				throw new ToolProtocolError(
					"The local no-upload check failed, so the result was discarded. Nothing was sent.",
				);
			}
			document.documentElement.dataset.noUpload = "verified";
			showResult(result);
			setStatus("Done. The result was verified on this device.", "ok");
		} catch (error) {
			const message =
				error instanceof ToolProtocolError || error instanceof ToolInputError
					? error.message
					: "The tool could not finish. Your files were not uploaded.";
			setStatus(message, "error");
			console.error("Selis tools failed:", error);
		} finally {
			setBusy(false);
		}
	}

	function resetForAnother(): void {
		files = [];
		clearResult();
		const tool = currentTool();
		if (tool !== null) {
			renderOptions(tool);
		}
		renderFiles();
		updateRunState();
		setStatus("Files stay on this device.", "info");
		dropzone.focus();
	}

	backButton.addEventListener("click", clearTool);
	runButton.addEventListener("click", () => {
		void run();
	});
	againButton.addEventListener("click", resetForAnother);

	dropzone.addEventListener("click", () => {
		if (!busy) {
			fileInput.click();
		}
	});
	dropzone.addEventListener("keydown", (event) => {
		if (event.key === "Enter" || event.key === " ") {
			event.preventDefault();
			if (!busy) {
				fileInput.click();
			}
		}
	});
	fileInput.addEventListener("change", () => {
		const chosen = Array.from(fileInput.files ?? []);
		fileInput.value = "";
		void addFiles(chosen);
	});
	dropzone.addEventListener("dragenter", (event) => {
		event.preventDefault();
		dropzone.classList.add("is-dragging");
	});
	dropzone.addEventListener("dragover", (event) => {
		event.preventDefault();
		dropzone.classList.add("is-dragging");
	});
	dropzone.addEventListener("dragleave", (event) => {
		if (!(event.relatedTarget instanceof Node) || !dropzone.contains(event.relatedTarget)) {
			dropzone.classList.remove("is-dragging");
		}
	});
	dropzone.addEventListener("drop", (event) => {
		event.preventDefault();
		dropzone.classList.remove("is-dragging");
		if (!busy) {
			void addFiles(Array.from(event.dataTransfer?.files ?? []));
		}
	});
	window.addEventListener("beforeunload", () => {
		for (const url of resultUrls) {
			URL.revokeObjectURL(url);
		}
	});

	renderToolGrid();
	const firstTool = TOOLS[0];
	if (firstTool !== undefined) {
		renderOptions(firstTool);
	}
	updateRunState();
	const hash = window.location.hash.slice(1);
	if (TOOL_BY_ID.has(hash as ToolId)) {
		selectTool(hash as ToolId);
	} else {
		workspace.hidden = true;
		toolsSection.hidden = false;
	}
}

/**
 * Register the same origin-wide worker the viewer uses.
 *
 * The tools page is often the first page a visitor opens, so it cannot rely on
 * a previous viewer visit to install the worker. Registration is deliberately
 * non-blocking and non-fatal: an operation must work online even when a browser
 * refuses service workers (private mode, `file://`, an insecure origin). The
 * browser check waits for `navigator.serviceWorker.ready`, then reloads with the
 * network cut; `data-sw` is the page's own record of which of those happened.
 */
async function registerOfflineSupport(): Promise<void> {
	const result = await registerServiceWorker();
	document.documentElement.dataset.sw = result.ok ? "registered" : result.reason;
	if (!result.ok) {
		console.warn("Selis tools: offline support is unavailable:", result.reason);
	}
}

if (typeof document !== "undefined") {
	init();
	void registerOfflineSupport();
}
