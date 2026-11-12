/**
 * The `/edit-pdf/` workspace: a PDF editor, in the browser.
 *
 * ## What this page is
 *
 * The tools site is a catalogue of one-shot operations: drop a file, set some
 * options, press Run, download the result. That shape cannot be a document
 * editor — an editor holds a document open across many changes, and needs a
 * way to take the result away. So this is a **workspace**: the document is on
 * screen, the changes apply to it, and there is an undo.
 *
 * ## What it does, and with what
 *
 * - **Rendering and text** come from the core engine (`render`, `textLayer`),
 *   which is the same engine the reader uses.
 * - **Every change** is a request to the tools chunk — the engine's own
 *   operations, the same ones the one-shot cards run. The editor has no
 *   mutation primitives of its own, so it cannot disagree with the card that
 *   does the same job.
 * - **Undo** is a bounded stack of the document's bytes, restored verbatim.
 *   The mutation journal `23-EDIT-MODEL-SPEC.md` describes is not built; this
 *   is a stand-in for it, and it is exact rather than a re-derivation.
 *
 * ## What it will not pretend to do
 *
 * Changing text replaces the text **where it is drawn**. The line does not
 * re-flow: a longer replacement runs over what follows it. The page says so,
 * in the copy, because a document editor that silently reflowed would be
 * promising something this engine cannot do (`SL-5.TEXTEDIT.02`).
 *
 * The other limit is honest too: the engine replaces the first run of text on
 * the page that carries the words you clicked. When the page repeats those
 * words earlier, the editor notices that the click did not land where you
 * aimed and says so, rather than showing you a document that changed
 * somewhere else.
 */

import { type RequestRecorder, installRequestRecorder } from "../no-upload.js";
import { registerServiceWorker, takeUpdateWhenSafe } from "../sw-register.js";
import { type ThemeName, startTheme } from "../theme.js";
import {
	EXPORT_FORMATS,
	type ExportFormat,
	type ExportFormatInfo,
	exportDocument,
	exportFormat,
} from "../tools/export.js";
import { hasPdfHeader } from "../tools/inputs.js";
import { type ToolsWasm, loadTools, runTool } from "../tools/protocol.js";
import {
	type SecurityWasm,
	loadSecurity,
	randomKeyMaterial,
	runSecurity,
} from "../tools/security-protocol.js";
import { type SignWasm, loadSign, runSign } from "../tools/sign-protocol.js";
import { type AccessibilityReport, reportStructure } from "./accessibility.js";
import { type EditorAnnotation, annotationAt } from "./annotations.js";
import { EngineClient, EngineError, type RenderedPage } from "./engine.js";
import { type LinePlacement, placeLine } from "./geometry.js";
import { DocumentHistory } from "./history.js";
import { type IconName, icon } from "./icons.js";
import {
	type AnnotateOptions,
	MARKUP_COLOURS,
	type MarkupKind,
	type PageAnnotation,
	markupLabel,
	readAnnotationsReport,
} from "./markup.js";
import { measureOnCanvas, measurementText } from "./measure.js";
import {
	type EditorOperation,
	PAGE_SIZES,
	batesOptions,
	compressOptions,
	cropOptions,
	deletePage,
	duplicatePage,
	editLandedOnLine,
	editText,
	fillFormsOptions,
	flipOptions,
	headerFooterOptions,
	imageOptions,
	insertBlankPages,
	metadataOptions,
	movePage,
	nupOptions,
	outlineOptions,
	pageLabelsOptions,
	pageNumbersOptions,
	redactOptions,
	resizeOptions,
	rotatePage,
	searchCandidates,
	stampTextWidth,
	textBoxOptions,
	watermarkOptions,
} from "./operations.js";
import {
	type CommandSource,
	type PaletteCommand,
	filterCommands,
	readCommands,
} from "./palette.js";
import { canvasRect } from "./placement.js";
import {
	type PageGeometry,
	type Rect,
	displayRect,
	fitImage,
	pageGeometry,
	userRect,
} from "./placement.js";

/** The raster's base resolution: one device pixel per CSS pixel at 96 dpi. */
const BASE_DPI = 96;

/** The most device pixels per CSS pixel the workspace renders at. */
const MAX_DEVICE_PIXEL_RATIO = 2;

/** How many document states the undo stack keeps. */
const HISTORY_DEPTH = 25;

/** The font size a new text box is drawn at, in points. */
const TEXT_BOX_FONT_SIZE = 12;

/** The colour a new text box is drawn in, unless the reader chooses one. */
const BOX_COLOR_BLACK: readonly [number, number, number] = [0.05, 0.05, 0.12];

/** That colour as the browser draws it, for the swatch and the editor. */
const BOX_COLOR_CSS = "#0d0d1f";

/**
 * The sizes a new text box can be drawn at, in points.
 *
 * Acrobat, Foxit and Word all offer a ladder rather than a free field,
 * because a free field invites a size the document's font cannot draw.
 */
const BOX_SIZES = [8, 10, 12, 14, 18, 24] as const;

/**
 * The colours a new text box can be drawn in, as the stamp wants them:
 * red, green and blue between 0 and 1.
 *
 * The palette is the classic one — black, blue, red, green — because a
 * reader reaching for a colour expects the colours every other editor
 * offers, not a spectrum picker.
 */
const BOX_COLORS: readonly {
	readonly name: string;
	readonly rgb: readonly [number, number, number];
	readonly css: string;
}[] = [
	{ name: "Black", rgb: BOX_COLOR_BLACK, css: "#0d0d1f" },
	{ name: "Blue", rgb: [0, 0, 1], css: "#0000ff" },
	{ name: "Red", rgb: [1, 0, 0], css: "#ff0000" },
	{ name: "Green", rgb: [0, 0.5, 0], css: "#008000" },
];

/** The stamp's padding inside its rectangle, in points, plus a margin. */
const BOX_PADDING = 8;

/** The zoom levels the zoom buttons step through, as factors of 100%. */
const ZOOM_STEPS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 2, 3, 4] as const;

/** The smallest and largest zoom the workspace allows, as factors of 100%. */
const ZOOM_MIN = 0.25;
const ZOOM_MAX = 4;

/** How wide a rail thumbnail is drawn, in CSS pixels. */
const THUMBNAIL_WIDTH = 132;

/**
 * How much room the canvas keeps around the page, in CSS pixels.
 *
 * The canvas's own padding, plus a gap on each side, so a fitted page does
 * not sit flush against the edge — a page flush against the edge reads as a
 * page that has been cut off rather than as a page that has been fitted.
 */
const SURFACE_GUTTER = 72;

/** What the workspace is armed to do on the page. */
export type EditorTool = "text" | "box" | "image" | "redact" | "measure" | "sign" | "markup";

/** The image the reader has picked, ready to place. */
interface PickedImage {
	readonly name: string;
	readonly bytes: Uint8Array;
	readonly width: number;
	readonly height: number;
}

/** One place the find box matched. */
interface SearchMatch {
	/** The zero-based page the match is on. */
	readonly page: number;
	/** The zero-based line the match is on. */
	readonly line: number;
	/** The text of the line the match is on. */
	readonly text: string;
}

interface EditorState {
	engine: EngineClient | null;
	tools: ToolsWasm | null;
	toolsPromise: Promise<ToolsWasm> | null;
	/** The signing chunk, which draws the text boxes. */
	sign: SignWasm | null;
	signPromise: Promise<SignWasm> | null;
	/** The engine's handle for the open document. */
	doc: number;
	/** How many pages it has. */
	pages: number;
	/** Each page's media box, in points. */
	pageSizes: readonly { readonly width: number; readonly height: number }[];
	/** The document's bytes — the state every operation works from. */
	bytes: Uint8Array | null;
	/** The document's name, for the download and the produced file names. */
	name: string;
	history: DocumentHistory;
	/** The page the page tools apply to, zero-based. */
	page: number;
	/** What the workspace is armed to do on the page. */
	tool: EditorTool;
	/** The image the reader has picked, if any. */
	image: PickedImage | null;
	/** The size a new text box is drawn at, in points. */
	boxSize: number;
	/** The colour a new text box is drawn in. */
	boxColor: readonly [number, number, number];
	/** The kind of markup the markup tool leaves. */
	markupKind: MarkupKind;
	/** The annotations on the page the editor is showing. */
	annotations: readonly EditorAnnotation[];
	/** Each page's geometry, for the placements. */
	geometry: Map<number, PageGeometry>;
	/** Whether a long operation is running. */
	busy: boolean;
	/** How big the document is drawn, as a factor of 100%. */
	zoom: number;
	/** Whether the document has changed since it was opened. */
	dirty: boolean;
	/** Whether the next draw should fit the page to the surface. */
	fitOnDraw: boolean;
	/** Where the find box matched, and which one is the current one. */
	matches: readonly SearchMatch[];
	matchIndex: number;
	/** Each page's rail thumbnail, as an object URL. */
	thumbnails: Map<number, string>;
}

const state: EditorState = {
	engine: null,
	tools: null,
	toolsPromise: null,
	sign: null,
	signPromise: null,
	doc: 0,
	pages: 0,
	pageSizes: [],
	bytes: null,
	name: "document.pdf",
	history: new DocumentHistory(HISTORY_DEPTH),
	page: 0,
	tool: "text",
	image: null,
	boxSize: TEXT_BOX_FONT_SIZE,
	boxColor: BOX_COLOR_BLACK,
	markupKind: "highlight",
	annotations: [],
	geometry: new Map(),
	busy: false,
	zoom: 1,
	dirty: false,
	fitOnDraw: false,
	matches: [],
	matchIndex: 0,
	thumbnails: new Map(),
};

let recorder: RequestRecorder | null = null;

/**
 * Find one of the page's elements.
 *
 * A missing element is a build mistake, not a runtime condition, so it
 * throws here rather than producing a null reference three frames later.
 */
function el<T extends HTMLElement>(id: string): T {
	const found = document.getElementById(id);
	if (found === null) {
		throw new Error(`the editor page is missing #${id}`);
	}
	return found as T;
}

const dom = {
	openButton: el<HTMLButtonElement>("selis-open-file-button"),
	fileInput: el<HTMLInputElement>("selis-open-file"),
	pageIndicator: el<HTMLSpanElement>("selis-page-indicator"),
	pageCount: el<HTMLSpanElement>("selis-editor-page-count"),
	filename: el<HTMLSpanElement>("selis-editor-filename"),
	dirty: el<HTMLSpanElement>("selis-editor-dirty"),
	undo: el<HTMLButtonElement>("selis-undo"),
	redo: el<HTMLButtonElement>("selis-redo"),
	download: el<HTMLButtonElement>("selis-download"),
	tools: el<HTMLDivElement>("selis-editor-tools"),
	toolHint: el<HTMLParagraphElement>("selis-editor-tool-hint"),
	imageInput: el<HTMLInputElement>("selis-editor-image-input"),
	fields: el<HTMLDivElement>("selis-editor-fields"),
	fieldsApply: el<HTMLButtonElement>("selis-editor-fields-apply"),
	fieldsRead: el<HTMLButtonElement>("selis-editor-fields-read"),
	outline: el<HTMLDivElement>("selis-editor-outline"),
	outlineApply: el<HTMLButtonElement>("selis-editor-outline-apply"),
	outlineRead: el<HTMLButtonElement>("selis-editor-outline-read"),
	outlineAdd: el<HTMLButtonElement>("selis-editor-outline-add"),
	cropApply: el<HTMLButtonElement>("selis-editor-crop-apply"),
	resizeApply: el<HTMLButtonElement>("selis-editor-resize-apply"),
	propertiesApply: el<HTMLButtonElement>("selis-editor-properties-apply"),
	failure: el<HTMLDivElement>("selis-editor-failure"),
	status: el<HTMLParagraphElement>("selis-editor-status"),
	doc: el<HTMLDivElement>("selis-editor-doc"),
	surface: el<HTMLElement>("selis-editor-surface"),
	empty: el<HTMLDivElement>("selis-editor-empty"),
	pageList: el<HTMLOListElement>("selis-page-list"),
	pageTools: el<HTMLDivElement>("selis-page-tools"),
	search: el<HTMLInputElement>("selis-editor-search"),
	searchCount: el<HTMLSpanElement>("selis-editor-search-count"),
	searchPrev: el<HTMLButtonElement>("selis-editor-search-prev"),
	searchNext: el<HTMLButtonElement>("selis-editor-search-next"),
	tabs: el<HTMLDivElement>("selis-editor-tabs"),
	ribbon: el<HTMLDivElement>("selis-editor-ribbon"),
	palette: el<HTMLDialogElement>("selis-editor-palette"),
	paletteButton: el<HTMLButtonElement>("selis-editor-palette-button"),
	paletteInput: el<HTMLInputElement>("selis-editor-palette-input"),
	paletteList: el<HTMLUListElement>("selis-editor-palette-list"),
	zoomBar: el<HTMLDivElement>("selis-editor-zoom-bar"),
	viewGroup: el<HTMLDivElement>("selis-editor-view-group"),
	zoomOut: el<HTMLButtonElement>("selis-editor-zoom-out"),
	zoomIn: el<HTMLButtonElement>("selis-editor-zoom-in"),
	zoomLevel: el<HTMLButtonElement>("selis-editor-zoom-level"),
	fitWidth: el<HTMLButtonElement>("selis-editor-fit-width"),
	fitPage: el<HTMLButtonElement>("selis-editor-fit-page"),
	inspector: el<HTMLElement>("selis-editor-inspector"),
	inspectorToggle: el<HTMLButtonElement>("selis-editor-inspector-toggle"),
	history: el<HTMLOListElement>("selis-editor-history"),
	historyClear: el<HTMLButtonElement>("selis-editor-history-clear"),
	labelsApply: el<HTMLButtonElement>("selis-editor-labels-apply"),
	numbersApply: el<HTMLButtonElement>("selis-editor-numbers-apply"),
	headerApply: el<HTMLButtonElement>("selis-editor-header-apply"),
	watermarkApply: el<HTMLButtonElement>("selis-editor-watermark-apply"),
	batesApply: el<HTMLButtonElement>("selis-editor-bates-apply"),
	compressApply: el<HTMLButtonElement>("selis-editor-compress-apply"),
	nupApply: el<HTMLButtonElement>("selis-editor-nup-apply"),
	exportGroup: el<HTMLDivElement>("selis-editor-export"),
	accessibility: el<HTMLDivElement>("selis-editor-accessibility"),
	comments: el<HTMLDivElement>("selis-editor-comments"),
	commentsRead: el<HTMLButtonElement>("selis-editor-comments-read"),
	accessibilityCheck: el<HTMLButtonElement>("selis-editor-accessibility-check"),
	attachments: el<HTMLDivElement>("selis-editor-attachments"),
	attachmentsRead: el<HTMLButtonElement>("selis-editor-attachments-read"),
	attachmentsAdd: el<HTMLButtonElement>("selis-editor-attachments-add"),
	attachmentsClear: el<HTMLButtonElement>("selis-editor-attachments-clear"),
	attachmentInput: el<HTMLInputElement>("selis-editor-attachment-input"),
	splitFrom: el<HTMLInputElement>("selis-editor-split-from"),
	splitTo: el<HTMLInputElement>("selis-editor-split-to"),
	splitApply: el<HTMLButtonElement>("selis-editor-split-apply"),
	password: el<HTMLInputElement>("selis-editor-password"),
	ownerPassword: el<HTMLInputElement>("selis-editor-owner-password"),
	protectApply: el<HTMLButtonElement>("selis-editor-protect-apply"),
	unlockApply: el<HTMLButtonElement>("selis-editor-unlock-apply"),
	theme: el<HTMLButtonElement>("selis-theme-toggle"),
};

/** The ribbon's tabs, and the panel each one shows. */
const TABS: readonly { readonly tab: string; readonly panel: string }[] = [
	{ tab: "selis-editor-tab-home", panel: "selis-editor-panel-home" },
	{ tab: "selis-editor-tab-edit", panel: "selis-editor-panel-edit" },
	{ tab: "selis-editor-tab-organize", panel: "selis-editor-panel-organize" },
	{ tab: "selis-editor-tab-document", panel: "selis-editor-panel-document" },
	{ tab: "selis-editor-tab-protect", panel: "selis-editor-panel-protect" },
	{ tab: "selis-editor-tab-export", panel: "selis-editor-panel-export" },
];

/** Say what happened, in the status line under the document. */
function setStatus(message: string, kind: "info" | "error" = "info"): void {
	dom.status.textContent = message;
	dom.status.dataset.kind = kind;
}

/** Show a failure that stops the workspace from working. */
function showFailure(message: string): void {
	dom.failure.textContent = message;
	dom.failure.hidden = false;
}

/** Hide the failure panel. */
function clearFailure(): void {
	dom.failure.textContent = "";
	dom.failure.hidden = true;
}

/** The message an error carries, whether it is ours or the engine's. */
function messageOf(error: unknown): string {
	if (error instanceof EngineError) {
		return error.detail === null ? error.message : `${error.message} (${error.detail})`;
	}
	return error instanceof Error ? error.message : String(error);
}

/** The engine, loaded on the first document so a visit stays cheap. */
async function engine(): Promise<EngineClient> {
	if (state.engine === null) {
		state.engine = await EngineClient.load();
	}
	return state.engine;
}

/** The tools chunk, loaded on the first change so a visit stays cheap. */
async function tools(): Promise<ToolsWasm> {
	if (state.tools !== null) {
		return state.tools;
	}
	state.toolsPromise ??= loadTools().then((instance) => {
		state.tools = instance;
		return instance;
	});
	return state.toolsPromise;
}

/**
 * The signing chunk, loaded on the first text box so a visit stays cheap.
 *
 * It is the chunk that draws the stamp, which is the only primitive that
 * writes NEW text at a chosen position.
 */
async function signChunk(): Promise<SignWasm> {
	if (state.sign !== null) {
		return state.sign;
	}
	state.signPromise ??= loadSign().then((instance) => {
		state.sign = instance;
		return instance;
	});
	return state.signPromise;
}

/** Assert that nothing the workspace did carried the document off the device. */
async function assertLocalOnly(): Promise<void> {
	if (recorder === null || state.bytes === null) {
		return;
	}
	await recorder.assertNoUpload([state.bytes]);
	// The marker the tools page uses, so the browser check can read the gate's
	// verdict from the DOM instead of trusting that nothing threw.
	document.documentElement.dataset.noUpload = "verified";
}

// ── rendering ───────────────────────────────────────────────────────────────

/** The device pixels per CSS pixel the workspace renders at. */
function devicePixelRatio(): number {
	return Math.min(MAX_DEVICE_PIXEL_RATIO, globalThis.devicePixelRatio || 1);
}

/**
 * Build one page: its raster, and the text layer the user clicks.
 *
 * The text layer's lines are positioned through the transform the render
 * reported, so a page that is turned on its side — or whose `/MediaBox` does
 * not start at (0, 0) — gets its text where the engine drew it.
 */
function buildPage(client: EngineClient, page: number): HTMLElement {
	const dpr = devicePixelRatio();
	// The zoom is a change of RESOLUTION: the page is rendered again at a
	// higher or lower DPI rather than being scaled by the browser, so the
	// glyphs stay sharp when the reader zooms in. The CSS box then grows with
	// the raster, which is what keeps a click's position and the engine's
	// geometry in step.
	const dpi = BASE_DPI * dpr * state.zoom;
	const wrap = document.createElement("div");
	wrap.className = "selis-editor-page";
	wrap.dataset.page = String(page);

	let raster: RenderedPage;
	try {
		raster = client.render(state.doc, page, dpi);
	} catch (error) {
		// One page the engine cannot draw must not blank the workspace: the
		// other pages are still readable and still editable, and the page says
		// which one failed and why.
		const notice = document.createElement("p");
		notice.className = "selis-editor-page__failed";
		notice.textContent = `Page ${page + 1} could not be drawn: ${messageOf(error)}`;
		wrap.appendChild(notice);
		return wrap;
	}
	const canvas = document.createElement("canvas");
	canvas.width = raster.width;
	canvas.height = raster.height;
	canvas.className = "selis-editor-page__canvas";
	canvas.setAttribute("role", "img");
	canvas.setAttribute("aria-label", `Page ${page + 1}`);
	const context = canvas.getContext("2d");
	if (context !== null) {
		context.putImageData(
			new ImageData(
				new Uint8ClampedArray(
					raster.pixels.buffer as ArrayBuffer,
					raster.pixels.byteOffset,
					raster.pixels.byteLength,
				),
				raster.width,
				raster.height,
			),
			0,
			0,
		);
	}
	// The raster is denser than the CSS box, so the text stays crisp on a
	// high-DPI display instead of being upscaled from a 96-DPI bitmap.
	const cssWidth = raster.width / dpr;
	const cssHeight = raster.height / dpr;
	canvas.style.width = `${cssWidth}px`;
	canvas.style.height = `${cssHeight}px`;
	wrap.style.width = `${cssWidth}px`;
	wrap.appendChild(canvas);

	// The rail's thumbnail of this page, drawn from the raster that was just
	// rendered. It is a copy at a fraction of the size, so it costs one
	// scaled blit rather than a second render.
	void makeThumbnail(page, canvas);

	// The page's geometry, kept for the placements: it is what turns a
	// rectangle on the canvas into the two spaces the engine's writers use.
	const size = state.pageSizes[page];
	state.geometry.set(
		page,
		pageGeometry(
			raster.ctm,
			raster.width,
			raster.height,
			cssWidth,
			cssHeight,
			size?.width ?? 0,
			size?.height ?? 0,
		),
	);

	let layer: PageTextLayer | null = null;
	try {
		layer = client.textLayer(state.doc, page);
	} catch {
		layer = null;
	}
	if (layer !== null) {
		wrap.appendChild(buildTextLayer(layer, raster.ctm, cssWidth / raster.width));
	}
	return wrap;
}

/**
 * Draw one page's rail thumbnail.
 *
 * The thumbnail is a copy of the page's raster at a fraction of the size, so
 * the rail shows the document rather than a list of numbers. It is produced
 * out of band — `toBlob` is asynchronous — so a document with many pages does
 * not block the workspace while they are drawn.
 *
 * The object URL is revoked when the document is drawn again, so a session
 * that edits the same document a hundred times does not hold a hundred
 * thumbnails.
 */
function makeThumbnail(page: number, canvas: HTMLCanvasElement): void {
	const width = THUMBNAIL_WIDTH;
	const height = Math.max(1, Math.round((canvas.height / canvas.width) * width));
	const small = document.createElement("canvas");
	small.width = width;
	small.height = height;
	const context = small.getContext("2d");
	if (context === null) {
		return;
	}
	context.drawImage(canvas, 0, 0, width, height);
	small.toBlob((blob) => {
		if (blob === null) {
			return;
		}
		const previous = state.thumbnails.get(page);
		if (previous !== undefined) {
			URL.revokeObjectURL(previous);
		}
		const url = URL.createObjectURL(blob);
		state.thumbnails.set(page, url);
		const image = dom.pageList.querySelector<HTMLImageElement>(`img[data-thumb="${page}"]`);
		if (image !== null) {
			image.src = url;
		}
	}, "image/png");
}

/** Release the thumbnails of a document that is being replaced. */
function releaseThumbnails(): void {
	for (const url of state.thumbnails.values()) {
		URL.revokeObjectURL(url);
	}
	state.thumbnails.clear();
}

/** The text layer's own shape, as the engine reported it. */
interface PageTextLayer {
	readonly page: number;
	readonly width: number;
	readonly height: number;
	readonly lowConfidence: boolean;
	readonly lines: readonly {
		readonly text: string;
		readonly rect: {
			readonly x: number;
			readonly y: number;
			readonly width: number;
			readonly height: number;
		};
	}[];
}

/** Build the clickable text layer over one rendered page. */
function buildTextLayer(
	layer: PageTextLayer,
	ctm: { a: number; b: number; c: number; d: number; e: number; f: number },
	cssScale: number,
): HTMLElement {
	const container = document.createElement("div");
	container.className = "selis-editor-text-layer";
	if (layer.lowConfidence) {
		container.dataset.lowConfidence = "true";
		container.title = "This page's text was not fully recovered; some lines may not be editable.";
	}
	const placed: {
		readonly box: HTMLElement;
		readonly text: HTMLElement;
		readonly runWidth: number;
	}[] = [];
	layer.lines.forEach((line, index) => {
		const placement = placeLine(ctm, line.rect, cssScale);
		const box = document.createElement("span");
		box.className = "selis-editor-line";
		box.dataset.line = String(index);
		box.dataset.page = String(layer.page);
		box.tabIndex = 0;
		box.setAttribute("role", "button");
		box.setAttribute("aria-label", `Edit this line: ${line.text}`);
		box.style.left = `${placement.box.x}px`;
		box.style.top = `${placement.box.y}px`;
		box.style.width = `${placement.box.width}px`;
		box.style.height = `${placement.box.height}px`;

		const text = document.createElement("span");
		text.className = "selis-editor-line__text";
		text.textContent = line.text;
		text.style.fontSize = `${placement.em}px`;
		// The baseline start, and the text turned with the page when the page is
		// turned: a page rotated a quarter turn runs its text down the canvas.
		text.style.left = `${placement.origin.x}px`;
		text.style.top = `${placement.origin.y - placement.em * 0.8}px`;
		text.style.transformOrigin = "0 0.8em";
		text.style.transform = `rotate(${placement.angle}deg)`;
		box.appendChild(text);
		container.appendChild(box);
		linePlacements.set(box, placement);
		placed.push({ box, text, runWidth: placement.runWidth });
	});
	// The overlay's own text is a different face from the document's, so it is
	// stretched to cover the glyphs it stands in for. The measurement is the
	// LAYOUT width, not the painted one: a turned page's painted box is the
	// wrong axis to measure.
	for (const entry of placed) {
		const natural = entry.text.offsetWidth;
		if (natural > 0 && entry.runWidth > 0) {
			entry.text.style.transform = `${entry.text.style.transform} scaleX(${entry.runWidth / natural})`;
		}
	}
	return container;
}

/** Draw every page of the open document. */
function renderDocument(client: EngineClient): void {
	dom.doc.replaceChildren();
	for (let page = 0; page < state.pages; page += 1) {
		dom.doc.appendChild(buildPage(client, page));
	}
	// The empty state is the canvas's whole content when there is no
	// document, so the surface centres it rather than pushing it below the
	// document's (empty) box. The reader must never have to scroll to find
	// out that they have not opened a file yet.
	dom.empty.hidden = state.pages > 0;
	dom.surface.classList.toggle("is-empty", state.pages === 0);
	renderDocumentState();
}

/**
 * Rebuild the page rail.
 *
 * The rail is a map of the document: every page drawn small, with its number
 * under it. A rail of page NUMBERS is a list; a rail of pages is something a
 * reader can navigate by.
 */
function renderPageList(): void {
	dom.pageList.replaceChildren();
	for (let page = 0; page < state.pages; page += 1) {
		const item = document.createElement("li");
		item.className = "selis-editor-pages__item";
		const button = document.createElement("button");
		button.type = "button";
		button.className = "selis-editor-pages__page";
		button.dataset.page = String(page);
		if (page === state.page) {
			button.setAttribute("aria-current", "page");
			button.classList.add("is-current");
		}
		const image = document.createElement("img");
		image.className = "selis-editor-pages__thumb";
		image.dataset.thumb = String(page);
		image.alt = "";
		// The thumbnail takes the page's own shape, so a landscape page is a
		// landscape thumbnail and the rail is a map of the document rather
		// than a row of letterboxes.
		const geometry = state.geometry.get(page);
		if (geometry !== undefined && geometry.cssHeight > 0) {
			image.style.aspectRatio = `${geometry.cssWidth} / ${geometry.cssHeight}`;
		}
		const source = state.thumbnails.get(page);
		if (source !== undefined) {
			image.src = source;
		}
		const number = document.createElement("span");
		number.className = "selis-editor-pages__number";
		number.textContent = String(page + 1);
		button.append(image, number);
		item.appendChild(button);
		dom.pageList.appendChild(item);
	}
	dom.pageIndicator.textContent = `Page ${state.page + 1} of ${state.pages}`;
	dom.pageIndicator.hidden = state.pages === 0;
	dom.pageCount.textContent = state.pages === 0 ? "" : `${state.pages}`;
}

/**
 * Reflect whether there is a document to work on.
 *
 * The zoom controls are about a document: there is nothing to zoom, nothing
 * to fit to the width and nothing to fit to the page while the workspace is
 * empty. Showing them anyway is noise, and a control that cannot do
 * anything is a control that reads as broken.
 */
function renderDocumentState(): void {
	const empty = state.pages === 0;
	dom.zoomBar.hidden = empty;
	dom.viewGroup.hidden = empty;
}

/** Reflect the history's state in the undo and redo buttons, and in the panel. */
function renderHistory(): void {
	dom.undo.disabled = !state.history.canUndo;
	dom.redo.disabled = !state.history.canRedo;
	const undoLabel = state.history.undoLabel;
	const redoLabel = state.history.redoLabel;
	dom.undo.title = undoLabel === null ? "Undo" : `Undo: ${undoLabel}`;
	dom.redo.title = redoLabel === null ? "Redo" : `Redo: ${redoLabel}`;
	dom.download.disabled = state.bytes === null;
	// The ribbon carries its own undo and redo, and they follow the same
	// state as the ones in the app bar.
	for (const button of dom.ribbon.querySelectorAll<HTMLButtonElement>(
		"button[data-action='undo'], button[data-action='redo']",
	)) {
		if (button.dataset.action === "undo") {
			button.disabled = !state.history.canUndo;
		} else {
			button.disabled = !state.history.canRedo;
		}
	}
	renderHistoryList();
	dom.historyClear.disabled = state.history.cursor <= 0;
}

/** Say whether the document has changed since it was opened. */
function renderDirty(): void {
	dom.dirty.hidden = !state.dirty;
}

// ── the shell: the icons, the tabs, the zoom, the find ───────────────────────

/**
 * Put the icons into the buttons that ask for them.
 *
 * The markup NAMES the icon (`data-icon="undo"`) rather than inlining the
 * SVG, so the paths live in one file and the page stays readable. A button
 * that also names a label gets it as text, so the accessible name is the
 * label and never the glyph.
 */
function hydrateIcons(): void {
	for (const host of document.querySelectorAll<HTMLElement>("[data-icon]")) {
		const name = host.dataset.icon;
		if (name === undefined) {
			continue;
		}
		host.removeAttribute("data-icon");
		host.prepend(icon(name as IconName));
		const label = host.dataset.label;
		if (label !== undefined) {
			const span = document.createElement("span");
			span.className = "selis-editor-btn__label";
			span.textContent = label;
			host.appendChild(span);
		}
	}
}

/**
 * Show one ribbon tab's panel and hide the others.
 *
 * The panels are all in the DOM: a ribbon command that opens a panel in the
 * inspector needs the panel to exist, and a screen reader that reads the
 * page should find the commands it can reach.
 */
function selectTab(tabId: string): void {
	for (const entry of TABS) {
		const tab = document.getElementById(entry.tab);
		const panel = document.getElementById(entry.panel);
		if (tab === null || panel === null) {
			continue;
		}
		const selected = entry.tab === tabId;
		tab.setAttribute("aria-selected", selected ? "true" : "false");
		panel.hidden = !selected;
	}
}

/** Open one of the inspector's panels and put the cursor in it. */
function focusSection(name: string): void {
	const section = document.getElementById(`selis-editor-section-${name}`);
	if (!(section instanceof HTMLDetailsElement)) {
		return;
	}
	section.open = true;
	section.scrollIntoView({ block: "nearest", behavior: "smooth" });
	const first = section.querySelector<HTMLElement>("input, select, button");
	first?.focus();
}

/**
 * Run one of the ribbon's commands.
 *
 * The ribbon carries its commands as `data-action`, so the HTML says what
 * each button does and this function is the only place that decides what
 * that means.
 */
async function runAction(action: string): Promise<void> {
	if (action.startsWith("section:")) {
		focusSection(action.slice("section:".length));
		return;
	}
	switch (action) {
		case "open":
			dom.fileInput.click();
			return;
		case "download":
			download();
			return;
		case "undo":
			await undo();
			return;
		case "redo":
			await redo();
			return;
		case "fit-width":
			await fitToWidth();
			return;
		case "fit-page":
			await fitToPage();
			return;
		case "zoom-reset":
			await setZoom(1);
			return;
		case "tool:redact":
			setTool("redact");
			selectTab("selis-editor-tab-edit");
			return;
		case "strip-metadata":
			await stripMetadata();
			return;
		case "report:privacy":
			await runReport("privacy");
			return;
		case "report:fingerprint":
			await runReport("fingerprint");
			return;
		default:
			setStatus("That command is not wired up yet.", "error");
	}
}

// ── the zoom ─────────────────────────────────────────────────────────────────

/** Show the zoom in the status bar, and say what the buttons can do. */
function renderZoom(): void {
	const percent = Math.round(state.zoom * 100);
	dom.zoomLevel.textContent = `${percent}%`;
	dom.zoomOut.disabled = state.zoom <= ZOOM_MIN;
	dom.zoomIn.disabled = state.zoom >= ZOOM_MAX;
}

/**
 * Draw the document at a new size.
 *
 * The zoom is a change of **resolution**, not a change of scale: the page is
 * rendered again at a higher or lower DPI, so the glyphs stay sharp when the
 * reader zooms in instead of being stretched. The placements follow, because
 * the geometry is derived from the transform the render reported.
 */
async function setZoom(zoom: number): Promise<void> {
	const next = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, zoom));
	if (Math.abs(next - state.zoom) < 0.001) {
		return;
	}
	state.zoom = next;
	renderZoom();
	await redraw();
}

/** Step the zoom up or down to the next size on the ladder. */
async function stepZoom(direction: "in" | "out"): Promise<void> {
	const current = state.zoom;
	const ladder = direction === "in" ? ZOOM_STEPS : [...ZOOM_STEPS].reverse();
	const next = ladder.find((step) => (direction === "in" ? step > current : step < current));
	await setZoom(next ?? current);
}

/** Draw the document again, at the zoom it is already at. */
async function redraw(): Promise<void> {
	// The guard is the PAGE COUNT, not the bytes: the fit that runs when a
	// document is opened happens before the bytes are stored, and a redraw
	// that refused to draw then would leave the page at the wrong size.
	if (state.pages === 0) {
		return;
	}
	const client = await engine();
	renderDocument(client);
	paintMatches();
}

/**
 * Fit the page to the width of the canvas.
 *
 * The factor is worked out from the page as it is drawn now, so the fit is
 * exact for a turned page and for a page whose media box does not start at
 * the origin.
 */
async function fitToWidth(): Promise<void> {
	const box = pageBox();
	if (box === null || box.width <= 0) {
		return;
	}
	const available = dom.surface.clientWidth - SURFACE_GUTTER;
	if (available <= 0) {
		return;
	}
	await setZoom(state.zoom * (available / box.width));
}

/** Fit the whole page to the canvas, in both directions. */
async function fitToPage(): Promise<void> {
	const box = pageBox();
	if (box === null || box.width <= 0 || box.height <= 0) {
		return;
	}
	const availableWidth = dom.surface.clientWidth - SURFACE_GUTTER;
	const availableHeight = dom.surface.clientHeight - SURFACE_GUTTER;
	if (availableWidth <= 0 || availableHeight <= 0) {
		return;
	}
	const factor = Math.min(availableWidth / box.width, availableHeight / box.height);
	await setZoom(state.zoom * factor);
}

/**
 * The size of the selected page as it is drawn, in CSS pixels.
 *
 * This is the page's own layout size, NOT the size of its box on screen: the
 * canvas scrolls, so a page wider than the canvas is not shrunk to fit, and a
 * fit that measured the visible box would measure the wrong thing.
 */
function pageBox(): { readonly width: number; readonly height: number } | null {
	const geometry = state.geometry.get(state.page);
	if (geometry !== undefined) {
		return { width: geometry.cssWidth, height: geometry.cssHeight };
	}
	const canvas = dom.doc.querySelector<HTMLCanvasElement>(".selis-editor-page__canvas");
	if (canvas === null) {
		return null;
	}
	const box = canvas.getBoundingClientRect();
	return { width: box.width, height: box.height };
}

// ── the find box ─────────────────────────────────────────────────────────────

/**
 * Find the lines that carry what the reader typed.
 *
 * The engine's own search primitive does the work, so a match is a match
 * against the document's real text and not against what the page happens to
 * be showing. The matches are then painted onto the text layer.
 */
async function runSearch(): Promise<void> {
	const query = dom.search.value.trim();
	clearMatches();
	if (query === "" || state.bytes === null) {
		dom.searchCount.textContent = "";
		dom.searchPrev.disabled = true;
		dom.searchNext.disabled = true;
		return;
	}
	if (state.busy) {
		return;
	}
	state.busy = true;
	try {
		const client = await engine();
		const found = client.search(state.doc, query);
		state.matches = found.matches;
		state.matchIndex = 0;
		paintMatches();
		const total = found.total;
		dom.searchCount.textContent =
			total === 0 ? "no matches" : `${state.matchIndex + 1} of ${total}`;
		dom.searchPrev.disabled = total === 0;
		dom.searchNext.disabled = total === 0;
		if (total === 0) {
			setStatus(`Nothing in the document carries "${query}".`);
		} else {
			setStatus(`${total} place${total === 1 ? "" : "s"} in the document carries "${query}".`);
			goToMatch(0);
		}
	} catch (error) {
		setStatus(`The document could not be searched: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
	}
}

/** Forget the matches and take the highlights off the page. */
function clearMatches(): void {
	state.matches = [];
	state.matchIndex = 0;
	for (const line of dom.doc.querySelectorAll(".selis-editor-line")) {
		line.removeAttribute("data-match");
		line.removeAttribute("data-current");
	}
}

/**
 * Paint the matches onto the text layer.
 *
 * The highlight sits UNDER the glyphs — the line's own text is transparent
 * and the raster below shows through — so a matched line is marked without
 * the words becoming unreadable.
 */
function paintMatches(): void {
	for (const line of dom.doc.querySelectorAll(".selis-editor-line")) {
		line.removeAttribute("data-match");
		line.removeAttribute("data-current");
	}
	const current = state.matches[state.matchIndex];
	for (const match of state.matches) {
		const line = dom.doc.querySelector(
			`.selis-editor-page[data-page="${match.page}"] .selis-editor-line[data-line="${match.line}"]`,
		);
		if (line instanceof HTMLElement) {
			line.setAttribute("data-match", "true");
		}
	}
	if (current === undefined) {
		return;
	}
	const line = dom.doc.querySelector(
		`.selis-editor-page[data-page="${current.page}"] .selis-editor-line[data-line="${current.line}"]`,
	);
	if (line instanceof HTMLElement) {
		line.setAttribute("data-current", "true");
	}
}

/** Show one of the matches, and take the page to it. */
function goToMatch(index: number): void {
	if (state.matches.length === 0) {
		return;
	}
	state.matchIndex = ((index % state.matches.length) + state.matches.length) % state.matches.length;
	paintMatches();
	const match = state.matches[state.matchIndex];
	if (match === undefined) {
		return;
	}
	dom.searchCount.textContent = `${state.matchIndex + 1} of ${state.matches.length}`;
	state.page = match.page;
	renderPageList();
	const line = dom.doc.querySelector(
		`.selis-editor-page[data-page="${match.page}"] .selis-editor-line[data-line="${match.line}"]`,
	);
	if (line instanceof HTMLElement) {
		line.scrollIntoView({ block: "center", behavior: "smooth" });
	}
}

// ── the document's own furniture ────────────────────────────────────────────

/** Label the pages, so a reader sees `i, ii, iii` or `A-1, A-2`. */
async function applyPageLabels(): Promise<void> {
	const style = selectValue("selis-editor-label-style");
	const start = Math.max(1, Math.round(numberInput("selis-editor-label-start")));
	const prefix = textInput("selis-editor-label-prefix");
	await runOperation({
		op: "page_labels",
		options: pageLabelsOptions(style, start, prefix),
		label: "Labelled the pages",
	});
}

/** Number the pages. */
async function applyPageNumbers(): Promise<void> {
	const position = selectValue("selis-editor-number-position");
	const start = Math.max(1, Math.round(numberInput("selis-editor-number-start")));
	const size = Math.min(72, Math.max(4, Math.round(numberInput("selis-editor-number-size"))));
	await runOperation({
		op: "page_numbers",
		options: pageNumbersOptions(position, start, size),
		label: "Numbered the pages",
	});
}

/** Write a header and a footer on every page. */
async function applyHeaderFooter(): Promise<void> {
	const header = {
		left: textInput("selis-editor-header-left"),
		center: textInput("selis-editor-header-center"),
		right: textInput("selis-editor-header-right"),
	};
	const footer = {
		left: textInput("selis-editor-footer-left"),
		center: textInput("selis-editor-footer-center"),
		right: textInput("selis-editor-footer-right"),
	};
	if (
		header.left === "" &&
		header.center === "" &&
		header.right === "" &&
		footer.left === "" &&
		footer.center === "" &&
		footer.right === ""
	) {
		setStatus("There is no header or footer to write.", "error");
		return;
	}
	await runOperation({
		op: "header_footer",
		options: headerFooterOptions(header, footer, 9),
		label: "Wrote the header and footer",
	});
}

/** Draw a watermark across every page. */
async function applyWatermark(): Promise<void> {
	const text = textInput("selis-editor-watermark-text").trim();
	if (text === "") {
		setStatus("The watermark needs some text.", "error");
		return;
	}
	const size = Math.min(200, Math.max(6, Math.round(numberInput("selis-editor-watermark-size"))));
	const opacity = Math.min(1, Math.max(0.05, numberInput("selis-editor-watermark-opacity")));
	const angle = selectValue("selis-editor-watermark-angle");
	await runOperation({
		op: "watermark",
		options: watermarkOptions(
			text,
			size,
			opacity,
			angle === "horizontal" ? "horizontal" : "diagonal",
		),
		label: "Applied the watermark",
	});
}

/**
 * Take the document's metadata out of it.
 *
 * The metadata is what a file manager shows in its details pane, and what a
 * document carries about where it came from. Removing it is a real edit: the
 * bytes come back without it, and the change can be undone.
 */
/** Stamp a Bates number on every page. */
async function applyBates(): Promise<void> {
	const prefix = textInput("selis-editor-bates-prefix");
	const start = Math.max(1, Math.round(numberInput("selis-editor-bates-start")));
	const digits = Math.min(20, Math.max(1, Math.round(numberInput("selis-editor-bates-digits"))));
	const position = selectValue("selis-editor-bates-position");
	await runOperation({
		op: "bates",
		options: batesOptions(prefix, start, digits, position),
		label: "Stamped the Bates numbers",
	});
}

/**
 * Make the document smaller.
 *
 * The size before and after is in the status line, because "smaller" is only
 * interesting next to how much smaller.
 */
async function applyCompress(): Promise<void> {
	const level = selectValue("selis-editor-compress-level");
	const before = state.bytes?.length ?? 0;
	const chosen = level === "less" || level === "extreme" ? level : "recommended";
	const done = await runOperation({
		op: "compress",
		options: compressOptions(chosen),
		label: "Made the document smaller",
	});
	if (!done) {
		return;
	}
	const after = state.bytes?.length ?? 0;
	setStatus(`Made the document smaller: ${formatBytes(before)} to ${formatBytes(after)}.`);
}

/** Put several pages on one sheet. */
async function applyNup(): Promise<void> {
	const count = Math.round(numberInput("selis-editor-nup-count"));
	if (![2, 4, 6, 9].includes(count)) {
		setStatus("Choose 2, 4, 6 or 9 pages per sheet.", "error");
		return;
	}
	await runOperation({
		op: "nup",
		options: nupOptions(count),
		label: `Put ${count} pages on each sheet`,
	});
}

/** A byte count in the reader's own units. */
function formatBytes(bytes: number): string {
	if (bytes < 1024) {
		return `${bytes} B`;
	}
	if (bytes < 1024 * 1024) {
		return `${(bytes / 1024).toFixed(1)} KB`;
	}
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

async function stripMetadata(): Promise<void> {
	await runOperation({
		op: "strip_metadata",
		options: {},
		label: "Removed the document's metadata",
	});
}

/**
 * Read one of the engine's reports about the document.
 *
 * The privacy report says what the document might leak; the fingerprint says
 * what is inside it. Neither rewrites anything, so neither is a change to
 * undo — the report is shown, not committed.
 */
async function runReport(kind: "privacy" | "fingerprint"): Promise<void> {
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	if (state.busy) {
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus(kind === "privacy" ? "Scanning the document…" : "Reading the document…");
	try {
		const chunk = await tools();
		const result = runTool(chunk, {
			op: kind,
			options: {},
			files: [{ name: state.name, bytes }],
		});
		const produced = result.outputs[0];
		if (produced === undefined) {
			throw new Error("the engine produced no report");
		}
		const text = new TextDecoder().decode(produced.bytes);
		showReport(kind, text);
		setStatus(
			kind === "privacy"
				? "The privacy report is in the document's panels."
				: "The fingerprint is in the document's panels.",
		);
	} catch (error) {
		setStatus(`The report could not be read: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

/** Show a report in the inspector, in the reader's own words. */
function showReport(kind: "privacy" | "fingerprint", json: string): void {
	let parsed: unknown;
	try {
		parsed = JSON.parse(json);
	} catch {
		parsed = null;
	}
	const panel = document.getElementById(
		kind === "privacy" ? "selis-editor-section-privacy" : "selis-editor-section-fingerprint",
	);
	if (!(panel instanceof HTMLElement)) {
		return;
	}
	const body = panel.querySelector(".selis-editor-report");
	if (body === null) {
		return;
	}
	body.replaceChildren();
	const lines = summariseReport(kind, parsed);
	if (kind === "privacy") {
		focusSection("privacy");
	} else {
		focusSection("fingerprint");
	}
	for (const line of lines) {
		const row = document.createElement("p");
		row.className = "selis-editor-report__row";
		row.textContent = line;
		body.appendChild(row);
	}
	panel.classList.add("is-open");
}

/**
 * Turn a report into the few lines a reader can act on.
 *
 * The reports are JSON with a great deal in them; a person opening the
 * privacy report wants to know what was found, not to read a schema.
 */
function summariseReport(kind: "privacy" | "fingerprint", parsed: unknown): string[] {
	if (typeof parsed !== "object" || parsed === null) {
		return ["The report could not be read."];
	}
	const record = parsed as Record<string, unknown>;
	const lines: string[] = [];
	if (kind === "privacy") {
		const status = typeof record.status === "string" ? record.status : "unknown";
		lines.push(`Status: ${status}.`);
		const risks = record.risks;
		if (Array.isArray(risks)) {
			for (const risk of risks.slice(0, 20)) {
				if (typeof risk === "string") {
					lines.push(risk);
				} else if (typeof risk === "object" && risk !== null) {
					const item = risk as Record<string, unknown>;
					const kind2 = typeof item.kind === "string" ? item.kind : "risk";
					const detail = typeof item.detail === "string" ? item.detail : JSON.stringify(item);
					lines.push(`${kind2}: ${detail}`);
				}
			}
		}
	} else {
		lines.push(`Status: ${typeof record.status === "string" ? record.status : "unknown"}.`);
		for (const key of ["pages", "identifiers", "fonts", "images"]) {
			const value = record[key];
			if (value !== undefined) {
				lines.push(`${key}: ${typeof value === "object" ? JSON.stringify(value) : String(value)}`);
			}
		}
	}
	if (lines.length === 0) {
		lines.push("The report carries nothing to show.");
	}
	return lines;
}

// ── the export ───────────────────────────────────────────────────────────────

/**
 * The export buttons, one per format the engine can write.
 *
 * The list comes from the shared export module, so the editor and the tools
 * page offer the SAME formats and the same names: a format added there
 * appears here without a second edit.
 */
function renderExportButtons(): void {
	const group = dom.exportGroup;
	group.replaceChildren();
	for (const format of EXPORT_FORMATS) {
		const button = document.createElement("button");
		button.type = "button";
		button.className = "selis-editor-tool";
		button.dataset.export = format.id;
		button.title = format.note;
		// Each format carries its own mark: a Word document is not a deck, and
		// the reader should be able to tell them apart without reading the
		// labels.
		button.prepend(icon(EXPORT_ICONS[format.id]));
		const label = document.createElement("span");
		label.className = "selis-editor-tool__label";
		label.textContent = format.label;
		button.appendChild(label);
		group.appendChild(button);
	}
}

/** The mark each export format is drawn with. */
const EXPORT_ICONS: Readonly<Record<ExportFormat, IconName>> = {
	word: "word",
	slides: "slides",
	workbook: "workbook",
	html: "html",
	epub: "epub",
	reflow: "reflow",
	tables: "tables",
	text: "text",
	// The image formats are all the same mark: they are the same writer
	// with a different suffix.
	png: "image",
	jpg: "image",
	tiff: "image",
	webp: "image",
};

/**
 * Render every page of the document as a PNG, base64 encoded.
 *
 * The deck is the one export that carries the page's own picture: a slide
 * that shows the page is worth having, and a slide that is a list of text
 * boxes is not the same thing. The pages are drawn by the editor's own
 * engine, at the resolution the reader sees.
 */
async function pageImages(): Promise<string[]> {
	const client = await engine();
	const dpr = devicePixelRatio();
	const images: string[] = [];
	for (let page = 0; page < state.pages; page += 1) {
		const raster = client.render(state.doc, page, BASE_DPI * dpr);
		const canvas = document.createElement("canvas");
		canvas.width = raster.width;
		canvas.height = raster.height;
		const context = canvas.getContext("2d");
		if (context === null) {
			continue;
		}
		context.putImageData(
			new ImageData(
				new Uint8ClampedArray(
					raster.pixels.buffer as ArrayBuffer,
					raster.pixels.byteOffset,
					raster.pixels.byteLength,
				),
				raster.width,
				raster.height,
			),
			0,
			0,
		);
		const url = canvas.toDataURL("image/png");
		const comma = url.indexOf(",");
		images.push(comma < 0 ? url : url.slice(comma + 1));
	}
	return images;
}

/** The media type each kind of exported file is served as. */
const EXPORT_MEDIA_TYPES: Readonly<Record<string, string>> = {
	html: "text/html",
	text: "text/plain",
	report: "application/json",
	tables: "text/csv",
};

/**
 * Hand a produced file to the reader, and record it.
 *
 * The file is offered twice on purpose: the download starts as soon as the
 * button is pressed, and the link in the inspector takes it again. A browser
 * that refuses an automatic download - or a reader who missed the first one -
 * still gets the file.
 */
function downloadFile(name: string, bytes: Uint8Array, kind: string): void {
	const type = EXPORT_MEDIA_TYPES[kind] ?? "application/octet-stream";
	// The automatic download gets its own object URL, released as soon as the
	// browser has certainly read it.
	downloadBytes(name, bytes, type);
	// The link the reader can take the file with gets its OWN, and lives
	// until the next export replaces it: a link that has quietly stopped
	// working is worse than no link at all.
	if (lastExport !== null) {
		URL.revokeObjectURL(lastExport.url);
	}
	lastExport = {
		name,
		url: URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type })),
		size: bytes.byteLength,
		kind,
		content: bytes.slice(),
	};
	renderExportResult();
}

/**
 * Write the document out in another format.
 *
 * The bytes exported are the document's bytes AS THEY ARE NOW — every change
 * the reader has made is in them — and the export is written here, on this
 * device. Nothing is uploaded, and the no-upload assertion runs afterwards
 * like it does after every other operation.
 */
/**
 * Show or hide the inspector.
 *
 * A collapsed panel keeps a narrow strip with the button in it, so the way
 * back is exactly where the reader last saw the panel - a control that
 * disappears with the thing it controls is a one-way door.
 */
function toggleInspector(): void {
	const hidden = dom.inspector.classList.toggle("is-hidden");
	const label = hidden ? "Show the panels" : "Hide the panels";
	dom.inspectorToggle.title = label;
	dom.inspectorToggle.setAttribute("aria-label", label);
	const glyph = dom.inspectorToggle.querySelector("svg");
	if (glyph !== null) {
		glyph.remove();
	}
	dom.inspectorToggle.prepend(icon(hidden ? "chevron-right" : "chevron-left"));
}

/**
 * Label the theme button for the theme it switches TO.
 *
 * The glyph is chosen by the STYLESHEET — both are in the markup, and the
 * one for the dark theme is hidden in the light one — so the switch is right
 * before any script has run. The label is set here, because a title that
 * says which theme the button will land on is worth more than a glyph.
 */
function renderThemeButton(theme: ThemeName): void {
	const next = theme === "dark" ? "light" : "dark";
	const label = `Switch to the ${next} theme`;
	dom.theme.title = label;
	dom.theme.setAttribute("aria-label", label);
}

async function runExport(format: ExportFormat): Promise<void> {
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	if (state.busy) {
		return;
	}
	const info: ExportFormatInfo | null = exportFormat(format);
	const label = info?.label ?? format;
	state.busy = true;
	dom.download.disabled = true;
	setStatus(`Writing the ${label.toLowerCase()}…`);
	try {
		const images = info?.needsPageImages === true ? await pageImages() : undefined;
		const files = await exportDocument({
			format,
			bytes,
			name: state.name,
			...(images === undefined ? {} : { slideImages: images }),
		});
		if (files.length === 0) {
			throw new Error("the writer produced no file");
		}
		for (const file of files) {
			downloadFile(file.name, file.bytes, file.kind);
		}
		await assertLocalOnly();
		const produced = files.map((file) => file.name).join(", ");
		const size = files.reduce((total, file) => total + file.bytes.byteLength, 0);
		// The file has been handed to the browser, and the browser has put it
		// somewhere the reader may not be looking. Say so, and say what else
		// they can do about it.
		setStatus(
			`Wrote ${produced} (${formatBytes(size)}). The panel on the right has it - your browser can save it wherever you want.`,
		);
	} catch (error) {
		setStatus(`The ${label.toLowerCase()} was not written: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

// ── the visible history ──────────────────────────────────────────────────────

/**
 * Show the undo stack.
 *
 * The stack has always been there; this is the reader's view of it. Each row
 * is a state the document has been in, and choosing one returns the document
 * to it — which is the same thing the undo button does, one step at a time.
 */
function renderHistoryList(): void {
	const list = dom.history;
	list.replaceChildren();
	const entries = state.history.entries;
	if (entries.length === 0) {
		const note = document.createElement("li");
		note.className = "selis-editor-hint";
		note.textContent = "Nothing has been changed yet.";
		list.appendChild(note);
		return;
	}
	entries.forEach((entry, index) => {
		const item = document.createElement("li");
		const button = document.createElement("button");
		button.type = "button";
		button.className = "selis-editor-history__item";
		button.dataset.state = String(index);
		if (index === state.history.cursor) {
			button.classList.add("is-current");
		}
		const dot = document.createElement("span");
		dot.className = "selis-editor-history__dot";
		const label = document.createElement("span");
		label.textContent = entry.label;
		button.append(dot, label);
		item.appendChild(button);
		list.appendChild(item);
	});
}

/** Return the document to a state in the history. */
async function goToState(index: number): Promise<void> {
	const entries = state.history.entries;
	const entry = entries[index];
	if (entry === undefined) {
		return;
	}
	if (index === state.history.cursor) {
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus(`${entry.label}…`);
	try {
		await openInEngine(entry.bytes);
		state.bytes = entry.bytes;
		state.history.setCursor(index);
		state.dirty = true;
		await assertLocalOnly();
		setStatus(`Back at: ${entry.label}`);
	} catch (error) {
		setStatus(`That state could not be restored: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

// ── the document ────────────────────────────────────────────────────────────

/** Open the document's bytes in the engine and draw it. */
async function openInEngine(bytes: Uint8Array): Promise<void> {
	const client = await engine();
	// The new document is opened BEFORE the old one is closed: a document that
	// does not open leaves the workspace exactly as it was, rather than
	// replacing a working document with nothing.
	const opened = client.open(bytes);
	const previous = state.doc;
	state.doc = opened.doc;
	state.pages = opened.pages;
	state.pageSizes = opened.pageSizes;
	state.page = Math.min(state.page, Math.max(0, opened.pages - 1));
	if (previous !== null) {
		client.close(previous);
	}
	releaseThumbnails();
	renderDocument(client);
	renderPageList();
	renderHistory();
	// The document is drawn once at the size the reader last chose, and once
	// more to fit the canvas — but only when a NEW document was opened. A
	// change to the open document must not resize it under the reader.
	if (state.fitOnDraw) {
		state.fitOnDraw = false;
		await fitToWidth();
	}
}

/**
 * Put new bytes in place, record the change, and draw the result.
 *
 * The history is recorded only once the new document has actually opened, so
 * the stack never describes a state the workspace is not in.
 */
async function commit(bytes: Uint8Array, label: string): Promise<void> {
	await openInEngine(bytes);
	state.bytes = bytes;
	state.history.push(label, bytes);
	state.dirty = true;
	renderDirty();
	await assertLocalOnly();
	setStatus(label);
}

/** Open a file the user picked. */
async function openFile(file: File): Promise<void> {
	if (state.busy) {
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus(`Reading ${file.name}…`);
	try {
		const buffer = await file.arrayBuffer();
		const bytes = new Uint8Array(buffer);
		if (!hasPdfHeader(bytes)) {
			setStatus("That file does not start with %PDF-, so it is not a PDF.", "error");
			return;
		}
		clearFailure();
		state.name = file.name === "" ? "document.pdf" : file.name;
		state.page = 0;
		state.fitOnDraw = true;
		state.dirty = false;
		dom.filename.textContent = state.name;
		await openInEngine(bytes);
		state.bytes = bytes;
		state.history.reset(`Opened ${state.name}`, bytes);
		renderDirty();
		await assertLocalOnly();
		setStatus(`Opened ${state.name} — ${state.pages} page${state.pages === 1 ? "" : "s"}.`);
	} catch (error) {
		showFailure(`The document could not be opened: ${messageOf(error)}`);
		setStatus("The document could not be opened.", "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

// ── running an operation ────────────────────────────────────────────────────

/**
 * Run one engine operation against the open document and commit the result.
 *
 * The operation's bytes go in as the chunk's single input and the produced
 * PDF comes back; the workspace then re-opens the document so what is on
 * screen is the new bytes.
 *
 * @returns whether the change was applied
 */
async function runOperation(operation: EditorOperation): Promise<boolean> {
	if (state.busy) {
		return false;
	}
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return false;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus(`${operation.label}…`);
	try {
		const chunk = await tools();
		const result = runTool(chunk, {
			op: operation.op,
			options: operation.options,
			files: [{ name: state.name, bytes }],
		});
		const produced = result.outputs[0];
		if (produced === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(produced.bytes, operation.label);
		return true;
	} catch (error) {
		setStatus(`${operation.label} did not work: ${messageOf(error)}`, "error");
		return false;
	} finally {
		state.busy = false;
		renderHistory();
	}
}

/**
 * The security chunk, loaded once.
 *
 * The chunk is only fetched when the reader asks for a password: it is a
 * lazy chunk, and a reader who never protects a document never downloads
 * it.
 */
let securityPromise: Promise<SecurityWasm> | null = null;

function securityChunk(): Promise<SecurityWasm> {
	securityPromise ??= loadSecurity().catch((error: unknown) => {
		securityPromise = null;
		throw error;
	});
	return securityPromise;
}

// ── the text editor ─────────────────────────────────────────────────────────

let inline: InlineEditor | null = null;

/**
 * Whether an edit is on its way to the engine.
 *
 * While it is, the workspace must not open another editor: the reader's
 * click that ended the last edit would otherwise land on a page that is
 * about to change underneath it.
 */
let inlineCommitting = false;

/** What the inline editor is doing. */
type InlineMode =
	| {
			/** Changing a run of text that is already on the page. */
			readonly kind: "edit-line";
			readonly page: number;
			readonly line: number;
			/** The line's text before the change. */
			readonly before: string;
	  }
	| {
			/** Putting new text in a box the reader drew. */
			readonly kind: "new-box";
			readonly page: number;
			/** The box, in the canvas's CSS pixels. */
			readonly rect: Rect;
	  }
	| {
			/** Writing the comment on a note. */
			readonly kind: "new-note";
			readonly page: number;
			/** Where the note goes, in the canvas's CSS pixels. */
			readonly rect: Rect;
	  };

/**
 * Where each line's text sits on the canvas.
 *
 * The editor needs the line's own metrics - the size the text is drawn at and
 * where its baseline runs - to be INVISIBLE: the reader must not be able to
 * tell an edited line from an untouched one until they type.
 */
const linePlacements = new WeakMap<HTMLElement, LinePlacement>();

/** The inline editor, when one is open. */
interface InlineEditor {
	readonly root: HTMLElement;
	readonly textarea: HTMLTextAreaElement;
	readonly mode: InlineMode;
	/** The line under the editor, when there is one. */
	readonly line: HTMLElement | null;
}

/**
 * What the editor needs to know to BE the thing it is editing.
 *
 * The competitors (Acrobat, Foxit, PDF.js) all put the editor exactly where
 * the text is: the same size, the same place, the same baseline. A box with a
 * border and a shadow floating near the line is not that, and it is why
 * editing here felt like filling in a form.
 */
interface InlineMetrics {
	/** The editor's font size, in CSS pixels. */
	readonly fontSize: number;
	/**
	 * Where the editor's text starts, measured down from the top of the
	 * box it is over.
	 *
	 * This is the text layer's own rule - the overlay's text starts here
	 * too - so the editor's text and the printed text share a baseline to
	 * the pixel, whatever the font's own metrics happen to be.
	 */
	readonly textTop: number;
	/** The line under the editor, when there is one. */
	readonly line: HTMLElement | null;
}

/** Close the inline editor, if one is open. */
function closeInline(): void {
	inline?.line?.classList.remove("is-editing");
	inline?.root.remove();
	inline = null;
}

/**
 * Open the inline editor over a rectangle on the page.
 *
 * The rectangle is in the canvas's CSS pixels, relative to the canvas's
 * top-left corner; the editor is placed over it.
 *
 * The editor is built to BE the line it is over: the same size, the same
 * place, the same baseline. It is transparent until it is focused, it is the
 * ink on the paper, and the only chrome is a hairline and the line's own
 * highlight.
 */
function openInlineAt(
	page: number,
	rect: Rect,
	text: string,
	mode: InlineMode,
	metrics: InlineMetrics,
): void {
	closeInline();
	const wrap = dom.doc.querySelector(`.selis-editor-page[data-page="${page}"]`);
	if (wrap === null) {
		return;
	}
	const canvas = wrap.querySelector("canvas");
	if (!(canvas instanceof HTMLElement)) {
		return;
	}
	const host = wrap.getBoundingClientRect();
	const canvasBox = canvas.getBoundingClientRect();

	const root = document.createElement("div");
	root.className = "selis-editor-inline";
	root.style.left = `${canvasBox.left - host.left + rect.x}px`;
	root.style.top = `${canvasBox.top - host.top + rect.y}px`;

	// The size the editor draws its text at. It can change while the editor
	// is open - a new text box can be drawn at another size - so it is a
	// variable and not a constant.
	let fontSize = metrics.fontSize;

	const textarea = document.createElement("textarea");
	textarea.className = "selis-editor-inline__text";
	textarea.value = text;
	textarea.rows = 1;
	textarea.spellcheck = false;
	textarea.setAttribute(
		"aria-label",
		mode.kind === "new-box"
			? "The text to put in this box"
			: mode.kind === "new-note"
				? "The comment on this note"
				: "The new text for this line",
	);
	textarea.style.fontSize = `${fontSize}px`;
	// The text starts where the line's own text starts, so the two share a
	// baseline. Everything else about the editor follows from this.
	textarea.style.top = `${metrics.textTop}px`;

	// A mirror of the text, hidden, that measures how wide the words are:
	// the editor has to be wide enough for what the reader typed, or the
	// caret ends up outside the box.
	const mirror = document.createElement("span");
	mirror.className = "selis-editor-inline__measure";
	mirror.style.fontSize = `${fontSize}px`;
	mirror.textContent = text;

	// The row under the editor: the formatting controls for a NEW text box,
	// and the hint. Editing text that is already on the page cannot change
	// its size or its colour - the engine splices the string and the size
	// stays - so in that case the row carries the hint alone, rather than
	// offering controls that would do nothing.
	const tools = document.createElement("div");
	tools.className = "selis-editor-inline__tools";

	const hint = document.createElement("span");
	hint.className = "selis-editor-inline__hint";
	hint.textContent =
		mode.kind === "new-box"
			? "Enter places the text · Escape cancels"
			: mode.kind === "new-note"
				? "Enter leaves the note · Escape cancels"
				: "Enter applies · Escape cancels";

	if (mode.kind === "new-box") {
		const select = document.createElement("select");
		select.className = "selis-editor-inline__size";
		select.setAttribute("aria-label", "The size of the new text");
		for (const size of BOX_SIZES) {
			const option = document.createElement("option");
			option.value = String(size);
			option.textContent = String(size);
			option.selected = size === state.boxSize;
			select.appendChild(option);
		}
		select.addEventListener("change", () => {
			const chosen = Number(select.value);
			if (!Number.isFinite(chosen)) {
				return;
			}
			state.boxSize = chosen;
			fontSize = (chosen * 96) / 72;
			textarea.style.fontSize = `${fontSize}px`;
			mirror.style.fontSize = `${fontSize}px`;
			fit();
		});
		tools.appendChild(select);

		for (const swatch of BOX_COLORS) {
			const button = document.createElement("button");
			button.type = "button";
			button.className = "selis-editor-inline__swatch";
			button.dataset.color = swatch.name;
			button.title = swatch.name;
			button.setAttribute("aria-label", `The text in ${swatch.name.toLowerCase()}`);
			button.setAttribute("aria-pressed", swatch.css === BOX_COLOR_CSS ? "true" : "false");
			button.style.background = swatch.css;
			button.addEventListener("click", () => {
				state.boxColor = swatch.rgb;
				textarea.style.color = swatch.css;
				for (const other of tools.querySelectorAll("button[data-color]")) {
					other.setAttribute(
						"aria-pressed",
						other.getAttribute("aria-label") === button.getAttribute("aria-label")
							? "true"
							: "false",
					);
				}
			});
			tools.appendChild(button);
		}
	}
	tools.appendChild(hint);

	root.appendChild(textarea);
	root.appendChild(mirror);
	root.appendChild(tools);
	wrap.appendChild(root);

	// The line under the editor is marked, so the reader can see where they
	// are without the editor being a box around the text.
	metrics.line?.classList.add("is-editing");

	inline = { root, textarea, mode, line: metrics.line };

	// A new box is at least a sentence wide; a line is as wide as the line.
	const floor = mode.kind === "new-box" ? 240 : rect.width;

	/** Fit the editor to what has been typed. */
	const fit = (): void => {
		textarea.style.height = "auto";
		textarea.style.height = `${Math.max(textarea.scrollHeight, fontSize)}px`;
		mirror.textContent = textarea.value;
		const width = Math.max(floor, mirror.offsetWidth + fontSize);
		root.style.width = `${width}px`;
		textarea.style.width = `${width}px`;
		root.style.height = `${Math.max(rect.height, metrics.textTop + textarea.offsetHeight)}px`;
	};
	fit();
	textarea.addEventListener("input", fit);

	textarea.focus();
	textarea.select();

	textarea.addEventListener("keydown", (event) => {
		if (event.key === "Escape") {
			event.preventDefault();
			closeInline();
			setStatus("Change cancelled.");
			return;
		}
		if (event.key === "Enter" && !event.shiftKey) {
			event.preventDefault();
			void applyInline();
		}
	});
}

/** Open the inline editor over one line of existing text. */
function openInline(page: number, line: number, text: string): void {
	const wrap = dom.doc.querySelector(`.selis-editor-page[data-page="${page}"]`);
	if (wrap === null) {
		return;
	}
	const box = wrap.querySelector(`.selis-editor-line[data-line="${line}"]`);
	if (!(box instanceof HTMLElement)) {
		return;
	}
	const canvas = wrap.querySelector("canvas");
	if (!(canvas instanceof HTMLElement)) {
		return;
	}
	const rect = box.getBoundingClientRect();
	const canvasBox = canvas.getBoundingClientRect();
	// The line's own size and baseline: the editor has to BE the line, not
	// sit near it.
	const placement = linePlacements.get(box);
	openInlineAt(
		page,
		{
			x: rect.left - canvasBox.left,
			y: rect.top - canvasBox.top,
			width: rect.width,
			height: rect.height,
		},
		text,
		{ kind: "edit-line", page, line, before: text },
		{
			fontSize: placement?.em ?? 14,
			// The text layer puts its own text here, and the editor's text
			// has to start in the same place to share the baseline.
			textTop: (placement?.origin.y ?? 14) - (placement?.em ?? 14) * 0.8,
			line: box,
		},
	);
}

/**
 * Open the inline editor over a place for a note.
 *
 * The note's comment is written where the note will go, so the reader sees
 * the comment where they put it.
 */
function openNote(page: number, rect: Rect): void {
	openInlineAt(
		page,
		{ x: rect.x - 12, y: rect.y - 12, width: 24, height: 24 },
		"",
		{ kind: "new-note", page, rect },
		{ fontSize: 13, textTop: 0, line: null },
	);
}

/** Open the inline editor over a new box, for text that is not on the page yet. */
function openBox(page: number, rect: Rect): void {
	// A new box is drawn at the size the reader chose, in the colour they
	// chose, and the editor shows them both while they type.
	const fontSize = (state.boxSize * 96) / 72;
	openInlineAt(
		page,
		rect,
		"",
		{ kind: "new-box", page, rect },
		{
			fontSize,
			textTop: 0,
			line: null,
		},
	);
}

/**
 * Apply the inline editor's change.
 *
 * The editor is closed only once the engine has answered, so the reader sees
 * the change land on the page rather than a box disappear and a promise
 * appear in the status bar.
 */
async function applyInline(): Promise<void> {
	const editor = inline;
	if (editor === null || state.busy) {
		return;
	}
	inlineCommitting = true;
	try {
		await applyInlineNow(editor);
	} finally {
		inlineCommitting = false;
	}
}

/**
 * Apply one editor's change.
 *
 * The engine replaces the first run of text on the page that carries the
 * words. A line is often drawn as several runs, so the editor tries the whole
 * line first and then its longest words — and it checks afterwards that the
 * line the user clicked is the line that changed, because the page may carry
 * the same words earlier.
 */
async function applyInlineNow(editor: InlineEditor): Promise<void> {
	if (editor.mode.kind === "new-box") {
		await applyTextBox(editor.mode.page, editor.mode.rect, editor.textarea.value);
		return;
	}
	if (editor.mode.kind === "new-note") {
		const geometry = state.geometry.get(editor.mode.page);
		if (geometry === undefined) {
			closeInline();
			setStatus("That page could not be marked.", "error");
			return;
		}
		const comment = editor.textarea.value;
		closeInline();
		await placeMarkup(editor.mode.page, editor.mode.rect, geometry, comment);
		return;
	}
	const replacement = editor.textarea.value;
	if (replacement === editor.mode.before) {
		closeInline();
		setStatus("Nothing changed.");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Changing the text…");
	try {
		const chunk = await tools();
		const bytes = state.bytes;
		if (bytes === null) {
			throw new Error("no document is open");
		}
		const page = editor.mode.page;
		const line = editor.mode.line;
		const before = editor.mode.before;
		const candidates = searchCandidates(before);
		let firstError: unknown = null;
		for (const candidate of candidates) {
			let produced: Uint8Array;
			try {
				const operation = editText(page, candidate, replacement);
				const result = runTool(chunk, {
					op: operation.op,
					options: operation.options,
					files: [{ name: state.name, bytes }],
				});
				const output = result.outputs[0];
				if (output === undefined) {
					throw new Error("the engine produced no document");
				}
				produced = output.bytes;
			} catch (error) {
				firstError ??= error;
				continue;
			}
			// Did the change land on the line that was clicked? The engine edits
			// the FIRST run on the page that carries the words, and a page that
			// repeats them higher up would be edited there instead.
			const landed = await lineChanged(page, line, before, candidate, replacement, produced);
			if (!landed) {
				firstError ??= new Error(
					"that text appears earlier on the page, so the engine changed a different line",
				);
				continue;
			}
			await commit(produced, `Changed the text on page ${page + 1}`);
			closeInline();
			return;
		}
		throw firstError ?? new Error("the engine could not find that text");
	} catch (error) {
		setStatus(`The text could not be changed: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

/**
 * Whether the edit landed on the line the user clicked.
 *
 * The workspace re-opens the produced document and reads the line back, so
 * the answer comes from the document rather than from an assumption about
 * where the engine would have written.
 */
async function lineChanged(
	page: number,
	line: number,
	before: string,
	candidate: string,
	replacement: string,
	produced: Uint8Array,
): Promise<boolean> {
	const client = await engine();
	let handle: number | null = null;
	try {
		const opened = client.open(produced);
		handle = opened.doc;
		const layer = client.textLayer(handle, page);
		const text = layer.lines[line]?.text ?? "";
		return editLandedOnLine(before, text, candidate, replacement);
	} catch {
		return false;
	} finally {
		if (handle !== null) {
			client.close(handle);
		}
	}
}

// ── placing new content ─────────────────────────────────────────────────────

/**
 * What each tool does, in the reader's words.
 *
 * The hints carry the tool's LIMIT as well as its promise: the page says
 * what it cannot do, so a reader is never surprised by a line that ran over
 * what followed it or by an image that was covered rather than removed.
 */
const TOOL_HINTS: Record<EditorTool, string> = {
	text: "Click a line of text to change it. The text is replaced where it is drawn — the line does not re-flow, so a much longer replacement runs over what follows it.",
	box: "Click where the text should start, then type it. The text is drawn on top of the page.",
	image:
		"Pick a PNG or JPEG, then drag a rectangle to place it. The image is drawn on top of the page.",
	redact:
		"Drag a rectangle over what should not be there: the text under it is removed from the file and covered with a black box. Anything else under it — an image, a line of art — is covered, not erased.",
	measure:
		"Drag between two points to measure the page. The measurement is drawn on the screen and is not put in the document.",
	sign: "Draw your signature on the page. It is stamped onto the page as part of the document's own drawing, not as a cryptographic signature.",
	markup:
		"Drag over the words to mark them up. A mark is a standard PDF annotation, so every other reader sees it too.",
};

/** The font size a new text box is drawn at, in points. */

/** The line height the stamp's font uses, as a multiple of its size. */
const BOX_LINE_LEADING = 1.25;

/** Arm the workspace for a tool. */
function setTool(tool: EditorTool): void {
	state.tool = tool;
	for (const button of dom.tools.querySelectorAll("button[data-tool]")) {
		button.setAttribute(
			"aria-pressed",
			button.getAttribute("data-tool") === tool ? "true" : "false",
		);
	}
	dom.toolHint.textContent = TOOL_HINTS[tool];
	dom.doc.dataset.tool = tool;
	// The measurement and the signature belong to the tool that drew them.
	if (tool !== "measure") {
		clearMeasurement();
	}
	if (tool !== "sign") {
		clearSignature();
	}
	closeInline();
}

/** A drag in progress. */
interface Drag {
	readonly page: number;
	readonly startX: number;
	readonly startY: number;
	x: number;
	y: number;
	readonly band: HTMLElement | null;
	/** The points the pointer has made, for the signature's own path. */
	readonly path: { x: number; y: number }[];
	/** The annotation the drag is moving, when it is moving one. */
	readonly moving: EditorAnnotation | null;
}

let drag: Drag | null = null;

/** The rectangle the drag covers, in the canvas's CSS pixels. */
function dragRect(current: Drag): Rect {
	return {
		x: Math.min(current.startX, current.x),
		y: Math.min(current.startY, current.y),
		width: Math.abs(current.x - current.startX),
		height: Math.abs(current.y - current.startY),
	};
}

/** The canvas a pointer event landed on, and where on it. */
function pointerOnCanvas(
	event: PointerEvent,
): { canvas: HTMLCanvasElement; page: number; x: number; y: number } | null {
	const target = event.target;
	if (!(target instanceof HTMLCanvasElement)) {
		return null;
	}
	const wrap = target.closest(".selis-editor-page");
	if (!(wrap instanceof HTMLElement)) {
		return null;
	}
	const page = Number(wrap.dataset.page);
	if (!Number.isInteger(page)) {
		return null;
	}
	const box = target.getBoundingClientRect();
	return { canvas: target, page, x: event.clientX - box.left, y: event.clientY - box.top };
}

/** Start a placement drag. */
function startDrag(event: PointerEvent): void {
	if (state.tool === "text") {
		return;
	}
	const hit = pointerOnCanvas(event);
	if (hit === null) {
		return;
	}
	const wrap = hit.canvas.closest(".selis-editor-page");
	if (!(wrap instanceof HTMLElement)) {
		return;
	}
	// The measure tool draws a LINE, so it has no rubber band; the other
	// placement tools do.
	const band =
		state.tool === "measure"
			? null
			: (() => {
					const made = document.createElement("div");
					made.className = "selis-editor-band";
					wrap.appendChild(made);
					return made;
				})();
	// A drag that starts ON an existing mark MOVES it, when the markup
	// tool is armed: the reader picks a mark up the way they pick up
	// anything else on the page.
	const moving =
		state.tool === "markup" && state.markupKind !== "note"
			? annotationAt(state.annotations, hit.x, hit.y)
			: null;
	drag = {
		page: hit.page,
		startX: hit.x,
		startY: hit.y,
		x: hit.x,
		y: hit.y,
		band,
		path: [{ x: hit.x, y: hit.y }],
		moving,
	};
	try {
		hit.canvas.setPointerCapture(event.pointerId);
	} catch {
		// Capture is a convenience: it keeps the drag alive when the pointer
		// leaves the canvas. A browser that refuses it (or a synthetic event)
		// still delivers the events to the canvas.
	}
	event.preventDefault();
}

/** Follow the drag. */
function moveDrag(event: PointerEvent): void {
	if (drag === null) {
		return;
	}
	const hit = pointerOnCanvas(event);
	if (hit === null) {
		return;
	}
	drag.x = hit.x;
	drag.y = hit.y;
	if (state.tool === "sign") {
		// The signature's path: every point the pointer made.
		drag.path.push({ x: hit.x, y: hit.y });
		drawSignaturePath(drag.page, drag.path);
		return;
	}
	if (drag.band === null) {
		// The measure tool's own line, drawn as the pointer moves.
		drawMeasurement(drag.page, { x: drag.startX, y: drag.startY }, { x: drag.x, y: drag.y });
		return;
	}
	const rect = dragRect(drag);
	drag.band.style.left = `${rect.x}px`;
	drag.band.style.top = `${rect.y}px`;
	drag.band.style.width = `${rect.width}px`;
	drag.band.style.height = `${rect.height}px`;
}

/** Finish a placement drag and act on it. */
function endDrag(event: PointerEvent): void {
	const current = drag;
	if (current === null) {
		return;
	}
	drag = null;
	current.band?.remove();
	const hit = pointerOnCanvas(event);
	const rect = dragRect(current);
	const geometry = state.geometry.get(current.page);
	if (geometry === undefined) {
		setStatus("That page could not be drawn, so nothing was placed on it.", "error");
		return;
	}
	if (state.tool === "markup" && current.moving !== null) {
		// The mark goes where the pointer let go of it.
		const dx = hit === null ? 0 : hit.x - current.startX;
		const dy = hit === null ? 0 : hit.y - current.startY;
		const from = current.moving.rect;
		void moveAnnotation(
			current.page,
			current.moving.index,
			{ x: from.x + dx, y: from.y + dy, width: from.width, height: from.height },
			geometry,
		);
		return;
	}
	if (state.tool === "markup") {
		const kind = state.markupKind;
		if (kind === "note") {
			// A note is a CLICK: the reader writes the comment, and the
			// editor asks for it where the note will go.
			const click =
				hit === null ? { x: current.startX, y: current.startY } : { x: hit.x, y: hit.y };
			openNote(current.page, { x: click.x, y: click.y, width: 0, height: 0 });
			return;
		}
		void placeMarkup(current.page, rect, geometry, "");
		return;
	}
	if (state.tool === "sign") {
		// The signature is stamped where it was drawn.
		const path = current.path;
		clearSignature();
		void stampSignature(current.page, path);
		return;
	}
	if (state.tool === "measure") {
		// The measurement is a reading, not a change: it is drawn on the
		// screen and the document is left exactly as it was.
		const to = hit === null ? { x: current.startX, y: current.startY } : { x: hit.x, y: hit.y };
		const text = drawMeasurement(current.page, { x: current.startX, y: current.startY }, to);
		setStatus(text === "" ? "That page could not be measured." : `Measured: ${text}`);
		return;
	}
	if (state.tool === "box") {
		const click = hit === null ? { x: current.startX, y: current.startY } : { x: hit.x, y: hit.y };
		openBox(current.page, { x: click.x, y: click.y, width: 0, height: 0 });
		return;
	}
	if (state.tool === "image") {
		void placeImage(current.page, rect, geometry);
		return;
	}
	if (state.tool === "redact") {
		void redactRegion(current.page, rect, geometry);
	}
}

/**
 * Put a text box on a page.
 *
 * The engine draws it with its visual stamp: typed lines inside a rectangle,
 * appended to the page's content stream. The rectangle is placed at the point
 * the reader clicked, and sized to the text — the stamp does not wrap, so the
 * box is the text's own extent rather than a frame around it.
 */
async function applyTextBox(page: number, click: Rect, text: string): Promise<void> {
	if (text.trim() === "") {
		closeInline();
		setStatus("Nothing to put in the box.");
		return;
	}
	const geometry = state.geometry.get(page);
	const bytes = state.bytes;
	if (geometry === undefined || bytes === null) {
		closeInline();
		setStatus("That page could not be drawn.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Adding the text…");
	try {
		const chunk = await signChunk();
		const anchor = displayRect(geometry, click);
		const lines = text.split("\n");
		const size = state.boxSize;
		const width = Math.max(
			40,
			...lines.map((line) => stampTextWidth(line, size) + BOX_PADDING * 2),
		);
		const height = lines.length * size * BOX_LINE_LEADING + BOX_PADDING;
		const options = textBoxOptions(
			page,
			{
				x0: anchor.x0,
				y0: anchor.y0 - height,
				x1: anchor.x0 + width,
				y1: anchor.y0,
			},
			text,
			size,
			state.boxColor,
		);
		const response = runSign(chunk, {
			op: "stamp",
			options,
			file: { name: state.name, bytes },
		});
		if (response.kind !== "document") {
			throw new Error("the signing chunk returned no document");
		}
		await commit(response.output.bytes, `Added a text box on page ${page + 1}`);
		closeInline();
	} catch (error) {
		setStatus(`The text box could not be added: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

/**
 * Put the picked image on a page.
 *
 * The image keeps its own aspect ratio and is centred in the rectangle the
 * reader drew, because the writer scales an image to the rectangle it is
 * given and would otherwise stretch it.
 */
async function placeImage(page: number, rect: Rect, geometry: PageGeometry): Promise<void> {
	const image = state.image;
	const bytes = state.bytes;
	if (image === null) {
		setStatus("Pick an image first.", "error");
		return;
	}
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	if (rect.width < 6 || rect.height < 6) {
		setStatus("Drag a rectangle to put the image in.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Adding the image…");
	try {
		const chunk = await tools();
		const fitted = fitImage(displayRect(geometry, rect), image.width, image.height);
		const result = runTool(chunk, {
			op: "image_watermark",
			options: imageOptions(page, fitted.x, fitted.y, fitted.scale),
			files: [
				{ name: state.name, bytes },
				{ name: image.name, bytes: image.bytes },
			],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(output.bytes, `Added an image on page ${page + 1}`);
	} catch (error) {
		setStatus(`The image could not be added: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

/**
 * Redact a region of a page.
 *
 * The engine removes the **text** inside the region from the content stream
 * and draws an opaque black box over it. Anything else under the box — an
 * image, a line of vector art — is covered, not erased, and the page says so.
 */
async function redactRegion(page: number, rect: Rect, geometry: PageGeometry): Promise<void> {
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	if (rect.width < 6 || rect.height < 6) {
		setStatus("Drag a rectangle over what to redact.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Redacting…");
	try {
		const chunk = await tools();
		const result = runTool(chunk, {
			op: "redact",
			options: redactOptions(page, userRect(geometry, rect)),
			files: [{ name: state.name, bytes }],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(output.bytes, `Redacted a region on page ${page + 1}`);
	} catch (error) {
		setStatus(`The region could not be redacted: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

/** Read an image the reader picked, and remember its size. */
async function pickImage(file: File): Promise<void> {
	const buffer = await file.arrayBuffer();
	const bytes = new Uint8Array(buffer);
	const head = Array.from(bytes.slice(0, 4));
	const png = head[0] === 0x89 && head[1] === 0x50 && head[2] === 0x4e && head[3] === 0x47;
	const jpeg = head[0] === 0xff && head[1] === 0xd8 && head[2] === 0xff;
	if (!png && !jpeg) {
		setStatus("That is not a PNG or a JPEG, so the engine cannot place it.", "error");
		return;
	}
	let width = 0;
	let height = 0;
	try {
		const bitmap = await createImageBitmap(new Blob([bytes], { type: file.type }));
		width = bitmap.width;
		height = bitmap.height;
		bitmap.close();
	} catch {
		setStatus("That image could not be read on this device.", "error");
		return;
	}
	state.image = { name: file.name === "" ? "image.png" : file.name, bytes, width, height };
	setTool("image");
	setStatus(`${file.name} picked — drag a rectangle on the page to place it.`);
}

/**
 * The command palette.
 *
 * The commands are read out of the ribbon when the palette opens, so the
 * two cannot drift: a command that is in the ribbon is in the palette, and
 * running a command from the palette runs the ribbon's own button.
 */
interface PaletteState {
	/** Every command the ribbon carries. */
	readonly commands: readonly PaletteCommand[];
	/** The commands that match what has been typed. */
	shown: readonly PaletteCommand[];
	/** Which of them is selected. */
	selected: number;
}

let palette: PaletteState | null = null;

/** The ribbon's buttons, as the palette sees them. */
function paletteSources(): CommandSource[] {
	const sources: CommandSource[] = [];
	for (const button of dom.ribbon.querySelectorAll<HTMLButtonElement>("button[data-action]")) {
		const action = button.dataset.action ?? "";
		const group = button.closest(".selis-editor-group");
		sources.push({
			label: button.getAttribute("aria-label") ?? button.textContent ?? "",
			where: group?.querySelector(".selis-editor-group__label")?.textContent ?? "",
			icon: button.dataset.icon ?? null,
			action,
		});
	}
	return sources;
}

/** The ribbon's button a command stands for. */
function paletteButtonFor(action: string): HTMLButtonElement | null {
	for (const button of dom.ribbon.querySelectorAll<HTMLButtonElement>("button[data-action]")) {
		if (button.dataset.action === action) {
			return button;
		}
	}
	return null;
}

/** Draw the palette's list. */
function renderPalette(): void {
	if (palette === null) {
		return;
	}
	dom.paletteList.replaceChildren();
	if (palette.shown.length === 0) {
		const empty = document.createElement("li");
		empty.className = "selis-editor-palette__empty";
		empty.textContent = "No command matches that.";
		dom.paletteList.appendChild(empty);
		return;
	}
	palette.shown.forEach((command, index) => {
		const item = document.createElement("li");
		const button = document.createElement("button");
		button.type = "button";
		button.className = "selis-editor-palette__item";
		button.setAttribute("aria-selected", index === palette?.selected ? "true" : "false");
		if (command.icon !== null) {
			const mark = document.createElement("span");
			mark.dataset.icon = command.icon;
			mark.setAttribute("aria-hidden", "true");
			button.appendChild(mark);
		}
		const label = document.createElement("span");
		label.textContent = command.label;
		button.appendChild(label);
		if (command.where !== "") {
			const where = document.createElement("span");
			where.className = "selis-editor-palette__where";
			where.textContent = command.where;
			button.appendChild(where);
		}
		button.addEventListener("click", () => {
			runPaletteCommand(command);
		});
		item.appendChild(button);
		dom.paletteList.appendChild(item);
	});
	hydrateIcons();
}

/** Run a command from the palette: the ribbon's own button, pressed. */
function runPaletteCommand(command: PaletteCommand): void {
	const button = paletteButtonFor(command.action);
	closePalette();
	if (button === null) {
		setStatus("That command is not there any more.");
		return;
	}
	button.click();
}

/** Open the palette. */
function openPalette(): void {
	const commands = readCommands(paletteSources());
	palette = { commands, shown: commands, selected: 0 };
	renderPalette();
	if (!dom.palette.open) {
		dom.palette.showModal();
	}
	dom.paletteInput.value = "";
	dom.paletteInput.focus();
}

/** Close the palette. */
function closePalette(): void {
	palette = null;
	if (dom.palette.open) {
		dom.palette.close();
	}
}

/** Filter the palette's list by what has been typed. */
function filterPalette(): void {
	if (palette === null) {
		return;
	}
	palette.shown = filterCommands(palette.commands, dom.paletteInput.value);
	palette.selected = 0;
	renderPalette();
}

/** Move the palette's selection. */
function movePalette(delta: number): void {
	if (palette === null || palette.shown.length === 0) {
		return;
	}
	const count = palette.shown.length;
	palette.selected = (palette.selected + delta + count) % count;
	renderPalette();
	const items = dom.paletteList.querySelectorAll<HTMLButtonElement>(".selis-editor-palette__item");
	items[palette.selected]?.scrollIntoView({ block: "nearest" });
}

/**
 * The accessibility report.
 *
 * A READ-ONLY report: the engine can read a document's tag structure but it
 * cannot write one, so this says what the document carries rather than
 * changing anything. The engine's own rule is kept: an untagged document
 * is untagged, not "a document with no headings".
 */
async function checkAccessibility(): Promise<void> {
	const client = await engine();
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Reading the document's structure…");
	let handle: number | null = null;
	try {
		const opened = client.open(bytes);
		handle = opened.doc;
		const report = reportStructure(client.structure(handle));
		renderAccessibility(report);
		setStatus(
			report.tagged
				? "Read the document's structure."
				: "This document is not tagged: a screen reader will read it as one long flow.",
		);
	} catch (error) {
		setStatus(`The structure could not be read: ${messageOf(error)}`, "error");
	} finally {
		if (handle !== null) {
			client.close(handle);
		}
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/** Draw the accessibility report. */
function renderAccessibility(report: AccessibilityReport): void {
	dom.accessibility.replaceChildren();
	if (!report.tagged) {
		const line = document.createElement("p");
		line.className = "selis-editor-hint";
		line.textContent =
			"This document is not tagged. A screen reader reads it as one long flow of text, with no headings, no lists and no descriptions for its images.";
		dom.accessibility.appendChild(line);
		return;
	}
	const summary = document.createElement("p");
	summary.className = "selis-editor-hint";
	summary.textContent = `This document is tagged: ${report.headings} heading(s), ${report.images} image(s) and ${report.links} link(s).`;
	dom.accessibility.appendChild(summary);
	if (report.imagesWithoutAlt > 0) {
		const missing = document.createElement("p");
		missing.className = "selis-editor-hint";
		missing.textContent = `${report.imagesWithoutAlt} image(s) carry no alternative text${report.undescribed.length > 0 ? `: ${report.undescribed.join(", ")}` : ""}.`;
		dom.accessibility.appendChild(missing);
	}
}

/**
 * The document's attachments.
 *
 * The engine's `attachments` operation EXTRACTS the files, so reading the
 * list costs the extraction; the names are what the panel shows, and the
 * bytes are never written anywhere.
 */
async function readAttachments(): Promise<void> {
	const chunk = await tools();
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Reading the attachments…");
	try {
		const result = runTool(chunk, {
			op: "attachments",
			files: [{ name: state.name, bytes }],
		});
		const names = result.outputs.map((output) => output.name);
		renderAttachments(names);
		dom.attachmentsClear.disabled = names.length === 0;
		setStatus(
			names.length === 0
				? "This document carries no files."
				: `This document carries ${names.length} file(s).`,
		);
	} catch (error) {
		renderAttachments([]);
		dom.attachmentsClear.disabled = true;
		setStatus(`This document carries no files: ${messageOf(error)}`);
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/** Draw the attachments list. */
function renderAttachments(names: readonly string[]): void {
	dom.attachments.replaceChildren();
	if (names.length === 0) {
		const line = document.createElement("p");
		line.className = "selis-editor-hint";
		line.textContent = "This document carries no files.";
		dom.attachments.appendChild(line);
		return;
	}
	const list = document.createElement("ul");
	list.className = "selis-editor-list";
	for (const name of names) {
		const item = document.createElement("li");
		item.textContent = name;
		list.appendChild(item);
	}
	dom.attachments.appendChild(list);
}

/** Carry a file inside the document. */
async function attachFile(file: File): Promise<void> {
	const chunk = await tools();
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Attaching the file…");
	try {
		const carried = new Uint8Array(await file.arrayBuffer());
		const result = runTool(chunk, {
			op: "attach",
			files: [
				{ name: state.name, bytes },
				{ name: file.name === "" ? "attachment.bin" : file.name, bytes: carried },
			],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(output.bytes, `Attached ${file.name}`);
		setStatus(`${file.name} now travels with the document.`);
	} catch (error) {
		setStatus(`The file could not be attached: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/** Take every file out of the document. */
async function clearAttachments(): Promise<void> {
	const chunk = await tools();
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Removing the attachments…");
	try {
		const result = runTool(chunk, {
			op: "detach",
			files: [{ name: state.name, bytes }],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(output.bytes, "Removed the attachments");
		renderAttachments([]);
		dom.attachmentsClear.disabled = true;
	} catch (error) {
		setStatus(`The attachments could not be removed: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/**
 * Keep a run of pages and drop the rest.
 *
 * The engine's `split` operation takes a page range and produces the
 * document that carries only those pages, so "keep these pages" is that
 * operation with the range the reader typed.
 */
async function keepPages(): Promise<void> {
	const chunk = await tools();
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	const from = Number(dom.splitFrom.value);
	const to = Number(dom.splitTo.value);
	if (!Number.isInteger(from) || !Number.isInteger(to) || from < 1 || to < from) {
		setStatus("Choose the pages to keep: a first page and a last page.", "error");
		return;
	}
	if (to > state.pages) {
		setStatus(`This document has ${state.pages} page(s).`, "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Keeping those pages…");
	try {
		const result = runTool(chunk, {
			op: "split",
			// The engine's pages are zero-based; the editor's are one-based.
			options: { mode: "range", from: from - 1, to: to - 1 },
			files: [{ name: state.name, bytes }],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(output.bytes, `Kept pages ${from} to ${to}`);
	} catch (error) {
		setStatus(`Those pages could not be kept: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/**
 * The document's password.
 *
 * The password is used here, on this device, and it goes nowhere: the
 * security chunk holds it while the request is in flight and zeroes it
 * afterwards. The browser keeps the protected document's bytes in memory
 * while this tab is open, which is the honest limit of a document you have
 * just encrypted in a tab - the page says so.
 */
async function protectDocument(): Promise<void> {
	const userPassword = dom.password.value;
	const ownerPassword = dom.ownerPassword.value;
	if (userPassword === "" && ownerPassword === "") {
		setStatus("Enter an open password, an owner password, or both.", "error");
		return;
	}
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Protecting the document…");
	try {
		const wasm = await securityChunk();
		const result = runSecurity(wasm, {
			op: "protect",
			options: {
				userPassword,
				ownerPassword,
				// The editor grants everything: it is a workspace, not a
				// publishing desk, and a reader who restricts the document
				// here would only restrict their own copy of it.
				permissions: { print: true, modify: true, copy: true, annotate: true },
				encryptMetadata: false,
				keyMaterial: randomKeyMaterial(),
			},
			file: { name: state.name, bytes },
		});
		await commit(result.output.bytes, "Protected the document");
		dom.unlockApply.disabled = false;
		setStatus(
			`The document is protected with ${result.outcome.algorithm ?? "a password"}. The password is held in this tab's memory only.`,
		);
	} catch (error) {
		setStatus(`The document could not be protected: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/** Take the password off the document. */
async function unlockDocument(): Promise<void> {
	const password = dom.password.value;
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Taking the password off…");
	try {
		const wasm = await securityChunk();
		const result = runSecurity(wasm, {
			op: "unlock",
			options: { password },
			file: { name: state.name, bytes },
		});
		await commit(result.output.bytes, "Removed the password");
		setStatus(
			result.outcome.encrypted === true
				? "The password is off and the result was verified on this device."
				: "This document had no password, so its own bytes were copied.",
		);
	} catch (error) {
		setStatus(`The password could not be taken off: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/**
 * The measurement on the page.
 *
 * A reading, not an edit: the measurement is drawn on the screen and it is
 * NOT put in the document, exactly as Acrobat and Foxit draw theirs. The
 * reader can measure a page without changing it.
 */
function measureOverlay(page: number): SVGSVGElement | null {
	const wrap = dom.doc.querySelector(`.selis-editor-page[data-page="${page}"]`);
	if (wrap === null) {
		return null;
	}
	let overlay = wrap.querySelector<SVGSVGElement>(".selis-editor-measure");
	if (overlay === null) {
		overlay = document.createElementNS("http://www.w3.org/2000/svg", "svg");
		overlay.classList.add("selis-editor-measure");
		const line = document.createElementNS("http://www.w3.org/2000/svg", "line");
		line.classList.add("selis-editor-measure__line");
		const label = document.createElementNS("http://www.w3.org/2000/svg", "text");
		label.classList.add("selis-editor-measure__label");
		overlay.append(line, label);
		wrap.appendChild(overlay);
	}
	return overlay;
}

/** Take the measurement off the page. */
function clearMeasurement(): void {
	for (const overlay of dom.doc.querySelectorAll(".selis-editor-measure")) {
		overlay.remove();
	}
}

/**
 * Draw the measurement between two points on the canvas.
 *
 * The distance is the page's own: the canvas is drawn at a known size and
 * the page's geometry knows how many CSS pixels a point is, so the number
 * is the document's and not the screen's.
 */
function drawMeasurement(
	page: number,
	from: { readonly x: number; readonly y: number },
	to: { readonly x: number; readonly y: number },
): string {
	const geometry = state.geometry.get(page);
	if (geometry === undefined) {
		return "";
	}
	// The page's own scale: device pixels per point, carried into CSS
	// pixels. The length of the transform's linear part is the scale, so
	// this is right for a turned page too.
	const cssPerDevice = geometry.cssWidth / geometry.rasterWidth;
	const cssPerPoint = geometry.scale * cssPerDevice;
	const measurement = measureOnCanvas(from, to, cssPerPoint);
	const overlay = measureOverlay(page);
	if (overlay === null) {
		return "";
	}
	overlay.setAttribute("viewBox", `0 0 ${geometry.cssWidth} ${geometry.cssHeight}`);
	const line = overlay.querySelector(".selis-editor-measure__line");
	const label = overlay.querySelector(".selis-editor-measure__label");
	if (line instanceof SVGLineElement && label instanceof SVGTextElement) {
		line.setAttribute("x1", String(from.x));
		line.setAttribute("y1", String(from.y));
		line.setAttribute("x2", String(to.x));
		line.setAttribute("y2", String(to.y));
		label.setAttribute("x", String((from.x + to.x) / 2));
		label.setAttribute("y", String((from.y + to.y) / 2 - 6));
		label.textContent = measurementText(measurement);
	}
	return measurementText(measurement);
}

/**
 * A signature, drawn on the page.
 *
 * Acrobat and Foxit both let the reader draw their signature straight onto
 * the page, and that is what this does: the pointer's path is captured on
 * the canvas, drawn live while the finger or the mouse is down, and stamped
 * onto the page when the pointer is lifted.
 *
 * The honest part: this is a **visual stamp**, so it is page content and not
 * an annotation. Another reader sees the signature as part of the page's
 * drawing, and it is not a cryptographic signature. The panel says so.
 */

/** The signature's ink. */
const SIGN_COLOR: readonly [number, number, number] = [0.05, 0.05, 0.12];

/** The signature's stroke width, in points. */
const SIGN_WIDTH = 1.5;

/**
 * The live path, drawn over the page while the pointer is down.
 *
 * The path is an SVG polyline in the page's own CSS pixels, so it is drawn
 * exactly where the pointer went and it costs nothing until the pointer
 * moves.
 */
function signOverlay(page: number): SVGSVGElement | null {
	const wrap = dom.doc.querySelector(`.selis-editor-page[data-page="${page}"]`);
	if (wrap === null) {
		return null;
	}
	let overlay = wrap.querySelector<SVGSVGElement>(".selis-editor-sign");
	if (overlay === null) {
		overlay = document.createElementNS("http://www.w3.org/2000/svg", "svg");
		overlay.classList.add("selis-editor-sign");
		const path = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
		path.classList.add("selis-editor-sign__path");
		overlay.appendChild(path);
		wrap.appendChild(overlay);
	}
	return overlay;
}

/** Take the live path off the page. */
function clearSignature(): void {
	for (const overlay of dom.doc.querySelectorAll(".selis-editor-sign")) {
		overlay.remove();
	}
}

/** Draw the path the pointer has made so far. */
function drawSignaturePath(page: number, points: readonly { x: number; y: number }[]): void {
	const geometry = state.geometry.get(page);
	const overlay = signOverlay(page);
	if (geometry === undefined || overlay === null) {
		return;
	}
	overlay.setAttribute("viewBox", `0 0 ${geometry.cssWidth} ${geometry.cssHeight}`);
	const path = overlay.querySelector(".selis-editor-sign__path");
	if (path instanceof SVGPolylineElement) {
		path.setAttribute("points", points.map((point) => `${point.x},${point.y}`).join(" "));
	}
}

/**
 * Stamp the signature onto the page.
 *
 * The stamp takes normalised points inside its rectangle: `(0,0)` is the
 * rectangle's bottom-left and `(1,1)` its top-right. The canvas is y-down
 * and the stamp is y-up, so the vertical is flipped on the way in.
 */
async function stampSignature(
	page: number,
	points: readonly { x: number; y: number }[],
): Promise<void> {
	const bytes = state.bytes;
	const geometry = state.geometry.get(page);
	if (bytes === null || geometry === undefined) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	if (points.length < 2) {
		setStatus("Draw your signature on the page.");
		return;
	}
	const first = points[0];
	if (first === undefined) {
		setStatus("Draw your signature on the page.");
		return;
	}
	let left = first.x;
	let right = first.x;
	let top = first.y;
	let bottom = first.y;
	for (const point of points) {
		left = Math.min(left, point.x);
		right = Math.max(right, point.x);
		top = Math.min(top, point.y);
		bottom = Math.max(bottom, point.y);
	}
	const width = Math.max(right - left, 1);
	const height = Math.max(bottom - top, 1);
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Signing…");
	try {
		const chunk = await signChunk();
		const anchor = displayRect(geometry, { x: left, y: top, width, height });
		const strokes = [
			{
				points: points.map((point) => [(point.x - left) / width, (bottom - point.y) / height]),
			},
		];
		const options = {
			page: page + 1,
			rect: [anchor.x0, anchor.y0, anchor.x1, anchor.y1],
			lines: [],
			strokes,
			color: [SIGN_COLOR[0], SIGN_COLOR[1], SIGN_COLOR[2]],
			strokeWidth: SIGN_WIDTH,
			fontSize: 11,
			border: false,
		};
		const response = runSign(chunk, {
			op: "stamp",
			options,
			file: { name: state.name, bytes },
		});
		if (response.kind !== "document") {
			throw new Error("the signing chunk returned no document");
		}
		await commit(response.output.bytes, `Signed page ${page + 1}`);
	} catch (error) {
		setStatus(`The signature could not be placed: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/**
 * The markup: the marks a reader leaves on a document.
 *
 * The mark is a **standard PDF annotation** with a generated appearance
 * stream, written by the engine's annotation crate, so every other reader
 * sees it too (ADR-P0026). The editor chooses where it goes and what colour
 * it is; the engine draws it.
 */

/**
 * Put a mark on the page.
 *
 * The rectangle is the reader's own drag, in the canvas's CSS pixels, and it
 * is turned into the page's own space by the editor's geometry - the same
 * conversion every other placement uses.
 */
async function placeMarkup(
	page: number,
	rect: Rect,
	geometry: PageGeometry,
	contents: string,
): Promise<void> {
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	const kind = state.markupKind;
	const user = userRect(geometry, rect);
	const options: AnnotateOptions = {
		// The engine's annotate operation takes a 0-based page, like every
		// other operation the tools chunk carries.
		page,
		kind,
		rect: [user.x, user.y, user.x + user.width, user.y + user.height],
		quads: [[user.x, user.y, user.x + user.width, user.y + user.height]],
		colour: MARKUP_COLOURS[kind],
		...(contents === "" ? {} : { contents }),
	};
	state.busy = true;
	dom.download.disabled = true;
	setStatus(kind === "note" ? "Adding the note…" : "Adding the mark…");
	try {
		const chunk = await tools();
		const result = runTool(chunk, {
			op: "annotate",
			options: options as unknown as Record<string, unknown>,
			files: [{ name: state.name, bytes }],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(
			output.bytes,
			kind === "note" ? `Added a note on page ${page + 1}` : `Added a mark on page ${page + 1}`,
		);
	} catch (error) {
		setStatus(`The mark could not be added: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/**
 * Read the annotations the document carries.
 *
 * The engine's report is a read-only view: it changes nothing and writes
 * nothing.
 */
async function readComments(): Promise<void> {
	const chunk = await tools();
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Reading the comments…");
	try {
		const result = runTool(chunk, {
			op: "annotations",
			options: { page: state.page },
			files: [{ name: state.name, bytes }],
		});
		const report = result.outputs[0];
		if (report === undefined) {
			throw new Error("the engine produced no report");
		}
		const parsed = readAnnotationsReport(new TextDecoder().decode(report.bytes));
		renderComments(parsed.annotations);
		state.annotations = editorAnnotations(parsed.annotations, state.page);
		setStatus(
			parsed.annotations.length === 0
				? "This page carries no comments."
				: `This page carries ${parsed.annotations.length} comment(s).`,
		);
	} catch (error) {
		setStatus(`The comments could not be read: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/** Draw the comments the document carries. */
function renderComments(annotations: readonly PageAnnotation[]): void {
	dom.comments.replaceChildren();
	if (annotations.length === 0) {
		const line = document.createElement("p");
		line.className = "selis-editor-hint";
		line.textContent = "This page carries no comments.";
		dom.comments.appendChild(line);
		return;
	}
	const list = document.createElement("ul");
	list.className = "selis-editor-list";
	annotations.forEach((annotation, index) => {
		const item = document.createElement("li");
		const kind = document.createElement("strong");
		kind.textContent = markupLabel(annotation.subtype);
		item.appendChild(kind);
		if (annotation.contents !== "") {
			const text = document.createElement("span");
			text.className = "selis-editor-comment";
			text.textContent = ` — ${annotation.contents}`;
			item.appendChild(text);
		}
		if (annotation.author !== "") {
			const who = document.createElement("span");
			who.className = "selis-editor-comment__who";
			who.textContent = ` (${annotation.author})`;
			item.appendChild(who);
		}
		// The mark can be taken off. A comment the reader cannot remove is
		// a comment they have to live with.
		const remove = document.createElement("button");
		remove.type = "button";
		remove.className = "selis-editor-iconbtn selis-editor-iconbtn--tiny";
		remove.title = "Remove this mark";
		remove.setAttribute("aria-label", "Remove this mark");
		remove.dataset.icon = "close";
		remove.addEventListener("click", () => {
			void removeAnnotation(state.page, index);
		});
		item.appendChild(remove);
		list.appendChild(item);
	});
	dom.comments.appendChild(list);
}

/**
 * The annotations of a page, in the editor's own terms.
 *
 * The engine reports them in the page's own points; the editor needs to know
 * where they are ON THE SCREEN, so that a reader can pick one up.
 */
function editorAnnotations(
	annotations: readonly PageAnnotation[],
	page: number,
): readonly EditorAnnotation[] {
	const geometry = state.geometry.get(page);
	if (geometry === undefined) {
		return [];
	}
	return annotations.map((annotation, index) => ({
		index,
		rect: canvasRect(geometry, {
			x: annotation.rect[0],
			y: annotation.rect[1],
			width: annotation.rect[2] - annotation.rect[0],
			height: annotation.rect[3] - annotation.rect[1],
		}),
		contents: annotation.contents,
		subtype: annotation.subtype,
	}));
}

/**
 * Moving and removing an annotation.
 *
 * The reader picks a mark up by clicking it and dragging it, with the markup
 * tool armed: the mark follows the pointer and is written back where it was
 * let go. A mark can also be taken off from the comments panel.
 */

/**
 * Move an annotation to a new rectangle.
 *
 * The rectangle is in the canvas's CSS pixels, and it is turned into the
 * page's own space by the editor's geometry - the same conversion every other
 * placement uses.
 */
async function moveAnnotation(
	page: number,
	index: number,
	rect: Rect,
	geometry: PageGeometry,
): Promise<void> {
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	const user = userRect(geometry, rect);
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Moving the mark…");
	try {
		const chunk = await tools();
		const result = runTool(chunk, {
			op: "annotate_update",
			options: {
				page,
				index,
				rect: [user.x, user.y, user.x + user.width, user.y + user.height],
			},
			files: [{ name: state.name, bytes }],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(output.bytes, "Moved the mark");
	} catch (error) {
		setStatus(`The mark could not be moved: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

/** Take an annotation off the page. */
async function removeAnnotation(page: number, index: number): Promise<void> {
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	state.busy = true;
	dom.download.disabled = true;
	setStatus("Removing the mark…");
	try {
		const chunk = await tools();
		const result = runTool(chunk, {
			op: "annotate_update",
			options: { page, index, remove: true },
			files: [{ name: state.name, bytes }],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no document");
		}
		await commit(output.bytes, "Removed the mark");
	} catch (error) {
		setStatus(`The mark could not be removed: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		dom.download.disabled = state.bytes === null;
	}
}

// ── the document's own properties ────────────────────────────────────────────

/** One field of the document's form, as the engine reported it. */
interface FormField {
	readonly name: string;
	readonly kind: string;
	readonly value: string;
	readonly readOnly: boolean;
}

/**
 * Read the document's form fields and show them.
 *
 * The fields are read on this device, from the document the reader opened —
 * nothing is uploaded, and the values the reader types go back into the
 * document the same way.
 */
async function readFormFields(): Promise<void> {
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	if (state.busy) {
		return;
	}
	state.busy = true;
	setStatus("Reading the form fields…");
	try {
		const chunk = await tools();
		const result = runTool(chunk, {
			op: "form_fields",
			files: [{ name: state.name, bytes }],
		});
		const output = result.outputs[0];
		if (output === undefined) {
			throw new Error("the engine produced no report");
		}
		const fields = parseFormFields(output.bytes);
		if (fields === null) {
			throw new Error("the fields report could not be read");
		}
		renderFormFields(fields);
		const count = fields.length;
		setStatus(`${count} field${count === 1 ? "" : "s"} found.`);
	} catch (error) {
		setStatus(`The fields could not be read: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
	}
}

/** The form fields the engine reported, read out of its JSON report. */
function parseFormFields(bytes: Uint8Array): FormField[] | null {
	let parsed: unknown;
	try {
		parsed = JSON.parse(new TextDecoder().decode(bytes));
	} catch {
		return null;
	}
	if (typeof parsed !== "object" || parsed === null) {
		return null;
	}
	const report = parsed as { fields?: unknown };
	if (!Array.isArray(report.fields)) {
		return null;
	}
	const fields: FormField[] = [];
	for (const raw of report.fields) {
		if (typeof raw !== "object" || raw === null) {
			continue;
		}
		const field = raw as {
			name?: unknown;
			kind?: unknown;
			value?: unknown;
			read_only?: unknown;
		};
		if (typeof field.name !== "string" || field.name === "") {
			continue;
		}
		fields.push({
			name: field.name,
			kind: typeof field.kind === "string" ? field.kind : "unknown",
			value: typeof field.value === "string" ? field.value : "",
			readOnly: field.read_only === true,
		});
	}
	return fields;
}

/** Show the form's fields, one input each. */
function renderFormFields(fields: readonly FormField[]): void {
	const list = dom.fields;
	list.replaceChildren();
	if (fields.length === 0) {
		const note = document.createElement("p");
		note.className = "selis-editor-hint";
		note.textContent = "This document has no form fields.";
		list.appendChild(note);
		dom.fieldsApply.disabled = true;
		return;
	}
	for (const field of fields) {
		const row = document.createElement("label");
		row.className = "selis-editor-field";
		const name = document.createElement("span");
		name.className = "selis-editor-field__name";
		name.textContent = field.kind === "checkbox" ? `${field.name} (checkbox)` : field.name;
		const input = document.createElement("input");
		input.type = field.kind === "checkbox" ? "checkbox" : "text";
		input.className = "selis-editor-field__input";
		input.dataset.field = field.name;
		input.value = field.value;
		input.checked = field.value === "Yes" || field.value === "On";
		// A field the engine will not fill is shown, but not editable: a
		// read-only field, a push button and a signature field are all things
		// the writer refuses to write.
		const fillable = !field.readOnly && field.kind !== "push_button" && field.kind !== "signature";
		input.disabled = !fillable;
		if (!fillable) {
			name.textContent = `${name.textContent} (not fillable)`;
		}
		row.append(name, input);
		list.appendChild(row);
	}
	dom.fieldsApply.disabled = false;
}

/**
 * The values the reader put in the form's fields.
 *
 * The fields the engine will not write are left out entirely: sending them
 * would make the engine refuse the whole operation, because it refuses to
 * override a read-only flag rather than skipping it.
 */
function formValues(): { name: string; value: string }[] {
	const values: { name: string; value: string }[] = [];
	for (const input of dom.fields.querySelectorAll("input[data-field]")) {
		if (!(input instanceof HTMLInputElement) || input.disabled) {
			continue;
		}
		const name = input.dataset.field;
		if (name === undefined) {
			continue;
		}
		const value = input.type === "checkbox" ? (input.checked ? "Yes" : "Off") : input.value;
		values.push({ name, value });
	}
	return values;
}

/** Write the reader's values into the document's form fields. */
async function fillFormFields(): Promise<void> {
	const values = formValues();
	if (values.length === 0) {
		setStatus("Read the fields first.", "error");
		return;
	}
	await runOperation({
		op: "fill_forms",
		options: fillFormsOptions(values),
		label: "Filled the form fields",
	});
}

/** Crop the selected page by the margins the reader entered. */
async function cropPage(): Promise<void> {
	const margins = {
		left: numberInput("selis-editor-crop-left"),
		top: numberInput("selis-editor-crop-top"),
		right: numberInput("selis-editor-crop-right"),
		bottom: numberInput("selis-editor-crop-bottom"),
	};
	if (margins.left === 0 && margins.top === 0 && margins.right === 0 && margins.bottom === 0) {
		setStatus("Enter at least one margin to crop.", "error");
		return;
	}
	await runOperation({
		op: "crop",
		options: cropOptions(margins, state.page),
		label: `Cropped page ${state.page + 1}`,
	});
}

/** Write the document's properties. */
async function saveProperties(): Promise<void> {
	const fields: Record<string, string> = {};
	for (const [id, key] of [
		["selis-editor-title", "Title"],
		["selis-editor-author", "Author"],
		["selis-editor-subject", "Subject"],
		["selis-editor-keywords", "Keywords"],
	] as const) {
		const value = textInput(id).trim();
		if (value !== "") {
			fields[key] = value;
		}
	}
	if (Object.keys(fields).length === 0) {
		setStatus("Fill in at least one property.", "error");
		return;
	}
	await runOperation({
		op: "metadata",
		options: metadataOptions(fields),
		label: "Saved the document's properties",
	});
}

/** A number the reader entered, or zero. */
function numberInput(id: string): number {
	const value = Number(textInput(id));
	return Number.isFinite(value) && value > 0 ? value : 0;
}

/** A text the reader entered. */
function textInput(id: string): string {
	const input = document.getElementById(id);
	return input instanceof HTMLInputElement ? input.value : "";
}

// ── the document's bookmarks ─────────────────────────────────────────────────

/** One bookmark, as the editor holds it. */
interface Bookmark {
	readonly title: string;
	/** The **one-based** page the bookmark points at, or 0 when it does not. */
	readonly page: number;
	/** The nesting depth: 0 is a top-level bookmark. */
	readonly level: number;
	/** Whether the bookmark can be edited here. */
	readonly editable: boolean;
}

/**
 * Read the document's bookmarks and show them.
 *
 * The core engine reads the outline as a tree; the writer takes a flat list
 * with a nesting depth, so the tree is flattened on the way in and the depth
 * is carried along.
 */
async function readOutline(): Promise<void> {
	const bytes = state.bytes;
	if (bytes === null) {
		setStatus("Open a PDF first.", "error");
		return;
	}
	if (state.busy) {
		return;
	}
	state.busy = true;
	setStatus("Reading the bookmarks…");
	try {
		const client = await engine();
		const reply = client.dispatch({ op: "outline", doc: state.doc });
		const value = reply.response.ok === true ? reply.response.value : null;
		const items =
			value !== null && typeof value === "object" ? (value as { items?: unknown }).items : null;
		if (!Array.isArray(items)) {
			throw new Error("the outline could not be read");
		}
		const bookmarks: Bookmark[] = [];
		const flatten = (nodes: readonly unknown[], level: number): void => {
			for (const node of nodes) {
				if (typeof node !== "object" || node === null) {
					continue;
				}
				const entry = node as {
					title?: unknown;
					target?: { kind?: unknown; page?: unknown } | null;
					children?: unknown;
				};
				const title = typeof entry.title === "string" ? entry.title : "";
				const kind = entry.target?.kind;
				const page =
					kind === "page" && typeof entry.target?.page === "number" ? entry.target.page + 1 : 0;
				bookmarks.push({ title, page, level, editable: page > 0 });
				if (Array.isArray(entry.children)) {
					flatten(entry.children, level + 1);
				}
			}
		};
		flatten(items, 0);
		renderBookmarks(bookmarks);
		setStatus(`${bookmarks.length} bookmark${bookmarks.length === 1 ? "" : "s"} read.`);
	} catch (error) {
		setStatus(`The bookmarks could not be read: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
	}
}

/** Show the document's bookmarks, one row each. */
function renderBookmarks(bookmarks: readonly Bookmark[]): void {
	const list = dom.outline;
	list.replaceChildren();
	if (bookmarks.length === 0) {
		const note = document.createElement("p");
		note.className = "selis-editor-hint";
		note.textContent = "This document has no bookmarks.";
		list.appendChild(note);
		dom.outlineApply.disabled = false;
		dom.outlineAdd.disabled = false;
		return;
	}
	bookmarks.forEach((bookmark, index) => {
		const row = document.createElement("div");
		row.className = "selis-editor-bookmark";
		row.dataset.level = String(bookmark.level);
		row.style.paddingLeft = `${bookmark.level * 12}px`;
		const title = document.createElement("input");
		title.type = "text";
		title.className = "selis-editor-bookmark__title";
		title.dataset.title = String(index);
		title.value = bookmark.title;
		title.disabled = !bookmark.editable;
		const page = document.createElement("input");
		page.type = "number";
		page.min = "1";
		page.step = "1";
		page.className = "selis-editor-bookmark__page";
		page.dataset.page = String(index);
		page.value = bookmark.page > 0 ? String(bookmark.page) : "";
		page.disabled = !bookmark.editable;
		page.setAttribute("aria-label", `The page bookmark ${index + 1} points at`);
		const remove = document.createElement("button");
		remove.type = "button";
		remove.className = "selis-editor-bookmark__remove";
		remove.dataset.remove = String(index);
		remove.textContent = "×";
		remove.setAttribute("aria-label", `Remove bookmark ${index + 1}`);
		row.append(title, page, remove);
		list.appendChild(row);
	});
	dom.outlineApply.disabled = false;
	dom.outlineAdd.disabled = false;
}

/**
 * The bookmarks the editor holds, read back out of the list.
 *
 * A bookmark that points at a named destination cannot be written back: the
 * writer takes a page and nothing else. Rather than quietly repointing it at
 * page 1, the save is refused and the reader is told which bookmark to remove.
 */
function bookmarkValues(): {
	readonly entries: { title: string; page: number; level: number }[];
	readonly refusal: string | null;
} {
	const entries: { title: string; page: number; level: number }[] = [];
	let refusal: string | null = null;
	const rows = [...dom.outline.querySelectorAll(".selis-editor-bookmark")];
	for (const row of rows) {
		const title = row.querySelector("input[data-title]");
		const page = row.querySelector("input[data-page]");
		if (!(title instanceof HTMLInputElement) || !(page instanceof HTMLInputElement)) {
			continue;
		}
		const level = Number(row.getAttribute("data-level") ?? "0");
		if (title.disabled) {
			refusal ??= `the bookmark "${title.value}" points at a named destination, which the \
				writer cannot express; remove it to save the others`;
			continue;
		}
		const number = Number(page.value);
		entries.push({
			title: title.value,
			page: Number.isFinite(number) && number >= 1 ? number : 1,
			level,
		});
	}
	return { entries, refusal };
}

/** Write the document's bookmarks. */
async function saveOutline(): Promise<void> {
	const { entries, refusal } = bookmarkValues();
	if (refusal !== null) {
		setStatus(`The bookmarks were not saved: ${refusal}.`, "error");
		return;
	}
	await runOperation({
		op: "outline",
		options: outlineOptions(entries),
		label: "Saved the bookmarks",
	});
}

/** Add a bookmark pointing at the selected page. */
function addBookmark(): void {
	const index = dom.outline.querySelectorAll(".selis-editor-bookmark").length;
	const row = document.createElement("div");
	row.className = "selis-editor-bookmark";
	row.dataset.level = "0";
	const title = document.createElement("input");
	title.type = "text";
	title.className = "selis-editor-bookmark__title";
	title.dataset.title = String(index);
	title.value = `Bookmark ${index + 1}`;
	const page = document.createElement("input");
	page.type = "number";
	page.min = "1";
	page.step = "1";
	page.className = "selis-editor-bookmark__page";
	page.dataset.page = String(index);
	page.value = String(state.page + 1);
	page.setAttribute("aria-label", "The page the new bookmark points at");
	const remove = document.createElement("button");
	remove.type = "button";
	remove.className = "selis-editor-bookmark__remove";
	remove.dataset.remove = String(index);
	remove.textContent = "×";
	remove.setAttribute("aria-label", "Remove the new bookmark");
	row.append(title, page, remove);
	dom.outline.appendChild(row);
}

/** Resize the selected page to a named size. */
async function resizePage(): Promise<void> {
	const size = PAGE_SIZES[selectValue("selis-editor-size")];
	if (size === undefined) {
		setStatus("Choose a page size.", "error");
		return;
	}
	await runOperation({
		op: "resize",
		options: resizeOptions(size[0], size[1], true, state.page),
		label: `Resized page ${state.page + 1}`,
	});
}

/** The value of a select the reader chose. */
function selectValue(id: string): string {
	const select = document.getElementById(id);
	return select instanceof HTMLSelectElement ? select.value : "";
}

// ── undo and redo ───────────────────────────────────────────────────────────

/** Step back one change. */
async function undo(): Promise<void> {
	if (state.busy) {
		return;
	}
	const target = state.history.undo();
	if (target === null) {
		return;
	}
	state.busy = true;
	try {
		await openInEngine(target.bytes);
		state.bytes = target.bytes;
		await assertLocalOnly();
		setStatus(`Undone: ${target.label}`);
	} catch (error) {
		// The state did not open, so the document is still the one the history
		// says it is: put the cursor back where it was.
		state.history.redo();
		setStatus(`The undo did not work: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

/** Step forward one change. */
async function redo(): Promise<void> {
	if (state.busy) {
		return;
	}
	const target = state.history.redo();
	if (target === null) {
		return;
	}
	state.busy = true;
	try {
		await openInEngine(target.bytes);
		state.bytes = target.bytes;
		await assertLocalOnly();
		setStatus(`Redone: ${target.label}`);
	} catch (error) {
		state.history.undo();
		setStatus(`The redo did not work: ${messageOf(error)}`, "error");
	} finally {
		state.busy = false;
		renderHistory();
	}
}

// ── download ────────────────────────────────────────────────────────────────

/**
 * How long a download's object URL is kept before it is released.
 *
 * Chrome resolves the blob on a background thread, so a URL revoked
 * immediately after the click can lose the file: the reader sees a status
 * that says "Downloaded" and no file at all. Thirty seconds is far longer
 * than the read takes, and short enough not to hold anything.
 */
const DOWNLOAD_GRACE_MS = 30_000;

/** The object URLs of the downloads this page has started. */
const pendingDownloads: string[] = [];

/**
 * Hand a file to the reader, as a download in this tab.
 *
 * The object URL is NOT revoked in the same turn as the click - that is the
 * bug this function exists to fix - and the link stays in the document until
 * the next turn, because a link removed in the same turn as its click can be
 * missed by the browser.
 *
 * @returns the object URL, so the file can be offered again
 */
function downloadBytes(name: string, bytes: Uint8Array, type: string): string {
	// The copy matters: a `Uint8Array` over a shared buffer is not a
	// `BlobPart`, and the blob is what the browser reads.
	const blob = new Blob([new Uint8Array(bytes)], { type });
	const url = URL.createObjectURL(blob);
	pendingDownloads.push(url);
	const link = document.createElement("a");
	link.href = url;
	link.download = name;
	link.rel = "noopener";
	document.body.appendChild(link);
	link.click();
	window.setTimeout(() => {
		link.remove();
	}, 0);
	window.setTimeout(() => {
		releaseDownload(url);
	}, DOWNLOAD_GRACE_MS);
	return url;
}

/** Release one download's object URL. */
function releaseDownload(url: string): void {
	const index = pendingDownloads.indexOf(url);
	if (index >= 0) {
		pendingDownloads.splice(index, 1);
	}
	URL.revokeObjectURL(url);
}

/**
 * The file the last export produced, and the URL it can be taken from.
 *
 * The export is written to a file the reader can see: the button starts the
 * download, and this is the link that takes it again - because a browser
 * that refuses an automatic download still gives the reader the file.
 */
interface ExportedFile {
	readonly name: string;
	readonly url: string;
	/** How many bytes the file is. NOT the bytes: `content` is those. */
	readonly size: number;
	readonly kind: string;
	/** The file's own bytes, for the "Save as..." that writes them. */
	readonly content: Uint8Array;
}

/** The file the last export produced, if there was one. */
let lastExport: ExportedFile | null = null;

/** Show the file the last export produced, and the ways to take it. */
function renderExportResult(): void {
	const panel = document.getElementById("selis-editor-export-result");
	if (!(panel instanceof HTMLElement)) {
		return;
	}
	panel.replaceChildren();
	if (lastExport === null) {
		const note = document.createElement("p");
		note.className = "selis-editor-hint";
		note.textContent = "Nothing exported yet.";
		panel.appendChild(note);
		return;
	}
	const row = document.createElement("div");
	row.className = "selis-editor-exported";
	const name = document.createElement("span");
	name.className = "selis-editor-exported__name";
	name.textContent = lastExport.name;
	const size = document.createElement("span");
	size.className = "selis-editor-exported__size";
	size.textContent = formatBytes(lastExport.size);
	const link = document.createElement("a");
	link.className = "selis-editor-btn";
	link.href = lastExport.url;
	link.download = lastExport.name;
	link.textContent = "Download again";
	row.append(name, size, link);
	// The browser's OWN save route, which is the one that always reaches the
	// disk: right-click the link, or open it and save it. A download goes
	// through the browser's download manager, and a browser that swallows
	// those (an automated one, a policy, a kiosk) never gives the reader the
	// file at all - while the link is right there.
	const open = document.createElement("a");
	open.className = "selis-editor-btn";
	open.href = lastExport.url;
	open.target = "_blank";
	open.rel = "noopener";
	open.textContent = "Open";
	row.appendChild(open);
	panel.appendChild(row);
}

/** Hand the edited document to the user, as a Blob in this tab. */
function download(): void {
	const bytes = state.bytes;
	if (bytes === null) {
		return;
	}
	const name = state.name === "" ? "edited.pdf" : state.name;
	downloadBytes(name, bytes, "application/pdf");
	setStatus(`Downloaded ${name}.`);
}

// ── wiring ──────────────────────────────────────────────────────────────────

/** The page's operations, and the page they apply to. */
async function runPageTool(operation: string): Promise<void> {
	const page = state.page;
	switch (operation) {
		case "rotate-90":
			await runOperation(rotatePage(page, 90));
			return;
		case "rotate-180":
			await runOperation(rotatePage(page, 180));
			return;
		case "rotate-270":
			await runOperation(rotatePage(page, 270));
			return;
		case "flip-horizontal":
			await runOperation({
				op: "flip",
				options: flipOptions("horizontal", page),
				label: `Flipped page ${page + 1} across`,
			});
			return;
		case "flip-vertical":
			await runOperation({
				op: "flip",
				options: flipOptions("vertical", page),
				label: `Flipped page ${page + 1} down`,
			});
			return;
		case "duplicate":
			await runOperation(duplicatePage(page));
			return;
		case "delete": {
			const done = await runOperation(deletePage(page));
			if (!done) {
				return;
			}
			// The page that was selected is gone; the selection follows the
			// page that took its place, or the one before it.
			state.page = Math.min(page, Math.max(0, state.pages - 1));
			renderPageList();
			return;
		}
		case "blank-after":
			await runOperation(insertBlankPages(page + 1));
			return;
		case "move-up":
		case "move-down": {
			const order = Array.from({ length: state.pages }, (_, index) => index);
			const up = operation === "move-up";
			try {
				const done = await runOperation(movePage(order, page, up ? "up" : "down"));
				if (!done) {
					return;
				}
				// The page kept its identity; only its position changed, so the
				// selection follows it rather than staying on the number.
				state.page = up ? page - 1 : page + 1;
				renderPageList();
			} catch (error) {
				setStatus(messageOf(error), "error");
			}
			return;
		}
		default:
			return;
	}
}

/** Attach the page's events. */
function wire(): void {
	dom.openButton.addEventListener("click", () => {
		dom.fileInput.click();
	});
	dom.fileInput.addEventListener("change", () => {
		const file = dom.fileInput.files?.[0];
		if (file !== undefined) {
			void openFile(file);
		}
		dom.fileInput.value = "";
	});

	dom.undo.addEventListener("click", () => {
		void undo();
	});
	dom.redo.addEventListener("click", () => {
		void redo();
	});
	dom.download.addEventListener("click", download);

	// The panels that read and change the document.
	dom.accessibilityCheck.addEventListener("click", () => {
		void checkAccessibility();
	});
	dom.commentsRead.addEventListener("click", () => {
		void readComments();
	});
	dom.attachmentsRead.addEventListener("click", () => {
		void readAttachments();
	});
	dom.attachmentsAdd.addEventListener("click", () => {
		dom.attachmentInput.click();
	});
	dom.attachmentInput.addEventListener("change", () => {
		const file = dom.attachmentInput.files?.[0];
		if (file !== undefined) {
			void attachFile(file);
		}
		dom.attachmentInput.value = "";
	});
	dom.attachmentsClear.addEventListener("click", () => {
		void clearAttachments();
	});
	dom.splitApply.addEventListener("click", () => {
		void keepPages();
	});

	dom.protectApply.addEventListener("click", () => {
		void protectDocument();
	});
	dom.unlockApply.addEventListener("click", () => {
		void unlockDocument();
	});

	// The command palette: the button, the shortcut, and the keys.
	dom.paletteButton.addEventListener("click", () => {
		openPalette();
	});
	dom.paletteInput.addEventListener("input", () => {
		filterPalette();
	});
	dom.paletteInput.addEventListener("keydown", (event) => {
		if (event.key === "ArrowDown") {
			event.preventDefault();
			movePalette(1);
			return;
		}
		if (event.key === "ArrowUp") {
			event.preventDefault();
			movePalette(-1);
			return;
		}
		if (event.key === "Enter") {
			event.preventDefault();
			const command = palette?.shown[palette?.selected ?? 0];
			if (command !== undefined) {
				runPaletteCommand(command);
			}
		}
	});
	dom.palette.addEventListener("close", () => {
		palette = null;
	});
	// The shortcut is the one every other tool uses: Ctrl+K (Cmd+K on a
	// Mac). The palette is a VIEW over the ribbon, so it must never be a
	// dead key - the shortcut opens it whatever the reader is doing.
	document.addEventListener("keydown", (event) => {
		if (event.key.toLowerCase() !== "k" || !(event.ctrlKey || event.metaKey)) {
			return;
		}
		const target = event.target;
		if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
			return;
		}
		event.preventDefault();
		if (dom.palette.open) {
			closePalette();
		} else {
			openPalette();
		}
	});

	// The tool row: what the workspace is armed to do on the page.
	//
	// The click may land on the icon or the label rather than on the button
	// itself, so the target is walked UP to the button that carries the tool.
	// The markup's kinds are their own buttons: a reader reaching for a
	// highlight wants a highlight, not a tool with a dropdown.
	for (const button of document.querySelectorAll<HTMLButtonElement>("button[data-markup]")) {
		button.addEventListener("click", () => {
			const kind = button.dataset.markup;
			if (kind === undefined) {
				return;
			}
			state.markupKind = kind as MarkupKind;
			setTool("markup");
			for (const other of document.querySelectorAll<HTMLButtonElement>("button[data-markup]")) {
				other.setAttribute("aria-pressed", other.dataset.markup === kind ? "true" : "false");
			}
		});
	}
	dom.tools.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		const button = target.closest<HTMLElement>("button[data-tool]");
		const tool = button?.dataset.tool;
		if (tool === "image") {
			// The image tool needs an image, so it opens the picker first and
			// arms itself once the reader has chosen one.
			dom.imageInput.click();
			return;
		}
		if (
			tool === "text" ||
			tool === "box" ||
			tool === "redact" ||
			tool === "measure" ||
			tool === "sign"
		) {
			setTool(tool);
		}
	});
	dom.imageInput.addEventListener("change", () => {
		const file = dom.imageInput.files?.[0];
		if (file !== undefined) {
			void pickImage(file);
		}
		dom.imageInput.value = "";
	});

	// The document's own properties: its form fields, its page size, and the
	// properties a reader sees in their file manager.
	dom.fieldsRead.addEventListener("click", () => {
		void readFormFields();
	});
	dom.fieldsApply.addEventListener("click", () => {
		void fillFormFields();
	});
	dom.cropApply.addEventListener("click", () => {
		void cropPage();
	});
	dom.resizeApply.addEventListener("click", () => {
		void resizePage();
	});
	dom.propertiesApply.addEventListener("click", () => {
		void saveProperties();
	});

	// The document's bookmarks.
	dom.outlineRead.addEventListener("click", () => {
		void readOutline();
	});
	dom.outlineAdd.addEventListener("click", () => {
		addBookmark();
	});
	dom.outlineApply.addEventListener("click", () => {
		void saveOutline();
	});
	dom.outline.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof HTMLElement)) {
			return;
		}
		if (target.dataset.remove === undefined) {
			return;
		}
		const row = target.closest(".selis-editor-bookmark");
		if (row instanceof HTMLElement) {
			row.remove();
		}
	});

	// The placement drag. The pointer is captured so the drag survives the
	// pointer leaving the canvas.
	dom.doc.addEventListener("pointerdown", (event) => {
		if (event.button !== 0) {
			return;
		}
		startDrag(event);
	});
	dom.doc.addEventListener("pointermove", (event) => {
		moveDrag(event);
	});
	dom.doc.addEventListener("pointerup", (event) => {
		endDrag(event);
	});
	dom.doc.addEventListener("pointercancel", () => {
		drag?.band?.remove();
		drag = null;
	});

	dom.pageTools.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		const operation = target.closest<HTMLElement>("button[data-op]")?.dataset.op;
		if (operation !== undefined) {
			void runPageTool(operation);
		}
	});

	dom.pageList.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		const page = Number(target.closest<HTMLElement>("button[data-page]")?.dataset.page);
		if (!Number.isInteger(page)) {
			return;
		}
		state.page = page;
		renderPageList();
		// The page tools act on the selected page, so the selected page has to
		// be the one on screen. A document with fifty pages and a rail in the
		// sidebar is otherwise a guessing game.
		const wrap = dom.doc.querySelector(`.selis-editor-page[data-page="${page}"]`);
		if (wrap instanceof HTMLElement) {
			wrap.scrollIntoView({ block: "start", behavior: "smooth" });
		}
	});

	/*
	 * Clicking away from the editor commits the change.
	 *
	 * This is what Acrobat does, and it is what a reader expects: an edit
	 * that is thrown away because the reader looked at another part of the
	 * page is a silent loss, and a silent loss is worse than a change the
	 * reader did not mean to make - because the change is visible and can be
	 * undone, and the loss is not.
	 *
	 * The handler is in the CAPTURE phase, so it runs before the click
	 * handlers below: the click that ends the edit cannot also open another
	 * editor on a page that is about to change underneath it.
	 */
	document.addEventListener(
		"pointerdown",
		(event) => {
			const editor = inline;
			if (editor === null || inlineCommitting) {
				return;
			}
			const target = event.target;
			if (target instanceof Node && editor.root.contains(target)) {
				return;
			}
			// An editor nobody typed in just goes away. One somebody DID
			// change is committed, because a change that vanishes when the
			// reader looks elsewhere is a silent loss.
			const value = editor.textarea.value;
			const changed =
				editor.mode.kind === "edit-line" ? value !== editor.mode.before : value.trim() !== "";
			if (!changed) {
				closeInline();
				return;
			}
			void applyInline();
		},
		true,
	);

	// Clicking a line opens the editor over it. The handler is on the layer,
	// not on each line, so a document with ten thousand lines has one
	// listener rather than ten thousand.
	dom.doc.addEventListener("click", (event) => {
		if (state.tool !== "text") {
			return;
		}
		const target = event.target;
		if (!(target instanceof HTMLElement)) {
			return;
		}
		const box = target.closest(".selis-editor-line");
		if (!(box instanceof HTMLElement)) {
			return;
		}
		const line = Number(box.dataset.line);
		const page = Number(box.dataset.page);
		if (!Number.isInteger(line) || !Number.isInteger(page)) {
			return;
		}
		// An editor is already open, or one is on its way to the engine:
		// this click belongs to the edit that is finishing, not to a new one.
		if (inline !== null || inlineCommitting) {
			return;
		}
		openInline(page, line, box.textContent ?? "");
	});
	dom.doc.addEventListener("keydown", (event) => {
		if (event.key !== "Enter" || state.tool !== "text") {
			return;
		}
		const target = event.target;
		if (!(target instanceof HTMLElement)) {
			return;
		}
		const box = target.closest(".selis-editor-line");
		if (!(box instanceof HTMLElement)) {
			return;
		}
		event.preventDefault();
		const line = Number(box.dataset.line);
		const page = Number(box.dataset.page);
		if (Number.isInteger(line) && Number.isInteger(page) && inline === null && !inlineCommitting) {
			openInline(page, line, box.textContent ?? "");
		}
	});

	// The ribbon: the tabs, and the commands under them.
	dom.tabs.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		const tab = target.closest<HTMLElement>("button[role='tab']");
		const id = tab?.id;
		if (id !== undefined) {
			selectTab(id);
		}
	});
	/*
	 * The `data-action` dispatcher is on the DOCUMENT, not on the ribbon:
	 * the empty state's own "Open a PDF" button carries an action too, and it
	 * lives on the canvas, not in the ribbon. A command that only works in
	 * one corner of the page is a command that looks broken everywhere else.
	 */
	document.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		const action = target.closest<HTMLElement>("button[data-action]")?.dataset.action;
		if (action !== undefined) {
			void runAction(action);
		}
	});

	// The find box.
	let searchTimer: number | undefined;
	dom.search.addEventListener("input", () => {
		if (searchTimer !== undefined) {
			window.clearTimeout(searchTimer);
		}
		searchTimer = window.setTimeout(() => {
			void runSearch();
		}, 250);
	});
	dom.search.addEventListener("keydown", (event) => {
		if (event.key === "Enter") {
			event.preventDefault();
			void runSearch();
		}
	});
	dom.searchNext.addEventListener("click", () => {
		goToMatch(state.matchIndex + 1);
	});
	dom.searchPrev.addEventListener("click", () => {
		goToMatch(state.matchIndex - 1);
	});

	// The zoom.
	dom.zoomIn.addEventListener("click", () => {
		void stepZoom("in");
	});
	dom.zoomOut.addEventListener("click", () => {
		void stepZoom("out");
	});
	dom.zoomLevel.addEventListener("click", () => {
		void setZoom(1);
	});
	dom.fitWidth.addEventListener("click", () => {
		void fitToWidth();
	});
	dom.fitPage.addEventListener("click", () => {
		void fitToPage();
	});

	// The document's own furniture.
	dom.labelsApply.addEventListener("click", () => {
		void applyPageLabels();
	});
	dom.numbersApply.addEventListener("click", () => {
		void applyPageNumbers();
	});
	dom.headerApply.addEventListener("click", () => {
		void applyHeaderFooter();
	});
	dom.watermarkApply.addEventListener("click", () => {
		void applyWatermark();
	});
	dom.batesApply.addEventListener("click", () => {
		void applyBates();
	});
	dom.compressApply.addEventListener("click", () => {
		void applyCompress();
	});
	dom.nupApply.addEventListener("click", () => {
		void applyNup();
	});

	// The visible history: choosing a state returns the document to it.
	dom.historyClear.addEventListener("click", () => {
		void goToState(0);
	});
	dom.history.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		const index = Number(target.closest<HTMLElement>("button[data-state]")?.dataset.state);
		if (Number.isInteger(index)) {
			void goToState(index);
		}
	});

	// The export: one button per format, from the shared list.
	//
	// The produced file's object URL is deliberately NOT released when the
	// page goes away: the "Download again" link points at it, and a link
	// whose URL has been revoked downloads an EMPTY file rather than
	// failing. The browser drops the blob with the document anyway.
	dom.exportGroup.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		const format = target.closest<HTMLElement>("button[data-export]")?.dataset.export;
		if (format !== undefined) {
			void runExport(format as ExportFormat);
		}
	});

	// The theme: the reader's own choice, remembered across the site. The
	// switch itself is wired by the shared theme module, so it behaves the
	// same on every surface.
	renderThemeButton(startTheme());

	// The inspector can be put away when the reader wants the whole canvas.
	dom.inspectorToggle.addEventListener("click", () => {
		toggleInspector();
	});

	// Dragging a file onto the page opens it, the way the reader does.
	for (const eventName of ["dragover", "drop"]) {
		document.addEventListener(eventName, (event) => {
			event.preventDefault();
		});
	}
	document.addEventListener("drop", (event) => {
		const file = event.dataTransfer?.files?.[0];
		if (file !== undefined) {
			void openFile(file);
		}
	});
}

/** Start the workspace. */
async function boot(): Promise<void> {
	recorder = installRequestRecorder();
	document.documentElement.dataset.noUpload = "watching";
	hydrateIcons();
	wire();
	setTool("text");
	selectTab("selis-editor-tab-home");
	renderExportButtons();
	renderZoom();
	renderHistory();
	renderDirty();
	setStatus("Open a PDF to start editing. It stays on this device.");
	const registration = await registerServiceWorker();
	if (registration.ok) {
		// A new worker carries the new cache namespace, and taking it is what
		// drops the OLD modules from the cache. It costs a reload, so it is
		// only taken when the reader has nothing open to lose.
		takeUpdateWhenSafe(registration.registration, () => state.bytes === null);
	}
}

void boot().catch((error: unknown) => {
	showFailure(`The editor could not start: ${messageOf(error)}`);
});
