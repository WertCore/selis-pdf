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
import { registerServiceWorker } from "../sw-register.js";
import { hasPdfHeader } from "../tools/inputs.js";
import { type ToolsWasm, loadTools, runTool } from "../tools/protocol.js";
import { EngineClient, EngineError, type RenderedPage } from "./engine.js";
import { placeLine } from "./geometry.js";
import { DocumentHistory } from "./history.js";
import {
	type EditorOperation,
	deletePage,
	duplicatePage,
	editLandedOnLine,
	editText,
	insertBlankPages,
	movePage,
	rotatePage,
	searchCandidates,
} from "./operations.js";

/** The raster's base resolution: one device pixel per CSS pixel at 96 dpi. */
const BASE_DPI = 96;

/** The most device pixels per CSS pixel the workspace renders at. */
const MAX_DEVICE_PIXEL_RATIO = 2;

/** How many document states the undo stack keeps. */
const HISTORY_DEPTH = 25;

interface EditorState {
	engine: EngineClient | null;
	tools: ToolsWasm | null;
	toolsPromise: Promise<ToolsWasm> | null;
	/** The engine's handle for the open document. */
	doc: number;
	/** How many pages it has. */
	pages: number;
	/** The document's bytes — the state every operation works from. */
	bytes: Uint8Array | null;
	/** The document's name, for the download and the produced file names. */
	name: string;
	history: DocumentHistory;
	/** The page the page tools apply to, zero-based. */
	page: number;
	/** Whether clicking a line opens the text editor. */
	editText: boolean;
	/** Whether a long operation is running. */
	busy: boolean;
}

const state: EditorState = {
	engine: null,
	tools: null,
	toolsPromise: null,
	doc: 0,
	pages: 0,
	bytes: null,
	name: "document.pdf",
	history: new DocumentHistory(HISTORY_DEPTH),
	page: 0,
	editText: true,
	busy: false,
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
	undo: el<HTMLButtonElement>("selis-undo"),
	redo: el<HTMLButtonElement>("selis-redo"),
	download: el<HTMLButtonElement>("selis-download"),
	editToggle: el<HTMLButtonElement>("selis-edit-toggle"),
	failure: el<HTMLDivElement>("selis-editor-failure"),
	status: el<HTMLParagraphElement>("selis-editor-status"),
	doc: el<HTMLDivElement>("selis-editor-doc"),
	empty: el<HTMLDivElement>("selis-editor-empty"),
	pageList: el<HTMLOListElement>("selis-page-list"),
	pageTools: el<HTMLDivElement>("selis-page-tools"),
};

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
	const dpi = BASE_DPI * dpr;
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
	canvas.style.width = `${cssWidth}px`;
	canvas.style.height = `${raster.height / dpr}px`;
	wrap.style.width = `${cssWidth}px`;
	wrap.appendChild(canvas);

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
	dom.empty.hidden = state.pages > 0;
}

/** Rebuild the page list in the sidebar. */
function renderPageList(): void {
	dom.pageList.replaceChildren();
	for (let page = 0; page < state.pages; page += 1) {
		const item = document.createElement("li");
		const button = document.createElement("button");
		button.type = "button";
		button.className = "selis-editor-pages__page";
		button.dataset.page = String(page);
		button.textContent = `Page ${page + 1}`;
		if (page === state.page) {
			button.setAttribute("aria-current", "page");
			button.classList.add("is-current");
		}
		item.appendChild(button);
		dom.pageList.appendChild(item);
	}
	dom.pageIndicator.textContent = `Page ${state.page + 1} of ${state.pages}`;
	dom.pageIndicator.hidden = state.pages === 0;
}

/** Reflect the history's state in the undo and redo buttons. */
function renderHistory(): void {
	dom.undo.disabled = !state.history.canUndo;
	dom.redo.disabled = !state.history.canRedo;
	const undoLabel = state.history.undoLabel;
	const redoLabel = state.history.redoLabel;
	dom.undo.title = undoLabel === null ? "" : `Undo: ${undoLabel}`;
	dom.redo.title = redoLabel === null ? "" : `Redo: ${redoLabel}`;
	dom.download.disabled = state.bytes === null;
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
	state.page = Math.min(state.page, Math.max(0, opened.pages - 1));
	if (previous !== null) {
		client.close(previous);
	}
	renderDocument(client);
	renderPageList();
	renderHistory();
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
		await openInEngine(bytes);
		state.bytes = bytes;
		state.history.reset(`Opened ${state.name}`, bytes);
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

// ── the text editor ─────────────────────────────────────────────────────────

/** The inline editor, when one is open. */
interface InlineEditor {
	readonly root: HTMLElement;
	readonly textarea: HTMLTextAreaElement;
	readonly page: number;
	readonly line: number;
	readonly before: string;
}

let inline: InlineEditor | null = null;

/** Close the inline editor, if one is open. */
function closeInline(): void {
	inline?.root.remove();
	inline = null;
}

/** Open the inline editor over one line. */
function openInline(page: number, line: number, text: string): void {
	closeInline();
	const wrap = dom.doc.querySelector(`.selis-editor-page[data-page="${page}"]`);
	if (wrap === null) {
		return;
	}
	const box = wrap.querySelector(`.selis-editor-line[data-line="${line}"]`);
	if (!(box instanceof HTMLElement)) {
		return;
	}
	const rect = box.getBoundingClientRect();
	const host = wrap.getBoundingClientRect();

	const root = document.createElement("div");
	root.className = "selis-editor-inline";

	const textarea = document.createElement("textarea");
	textarea.className = "selis-editor-inline__text";
	textarea.value = text;
	textarea.rows = 2;
	textarea.setAttribute("aria-label", "The new text for this line");
	textarea.style.left = `${rect.left - host.left}px`;
	textarea.style.top = `${rect.top - host.top}px`;
	textarea.style.width = `${Math.max(rect.width, 220)}px`;
	textarea.style.minHeight = `${Math.max(rect.height, 34)}px`;

	const hint = document.createElement("p");
	hint.className = "selis-editor-inline__hint";
	hint.textContent = "Enter applies the change · Escape cancels · the line does not re-flow";

	root.appendChild(textarea);
	root.appendChild(hint);
	wrap.appendChild(root);

	inline = { root, textarea, page, line, before: text };
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

/**
 * Apply the inline editor's change.
 *
 * The engine replaces the first run of text on the page that carries the
 * words. A line is often drawn as several runs, so the editor tries the whole
 * line first and then its longest words — and it checks afterwards that the
 * line the user clicked is the line that changed, because the page may carry
 * the same words earlier.
 */
async function applyInline(): Promise<void> {
	const editor = inline;
	if (editor === null || state.busy) {
		return;
	}
	const replacement = editor.textarea.value;
	if (replacement === editor.before) {
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
		const candidates = searchCandidates(editor.before);
		let firstError: unknown = null;
		for (const candidate of candidates) {
			let produced: Uint8Array;
			try {
				const operation = editText(editor.page, candidate, replacement);
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
			const landed = await lineChanged(
				editor.page,
				editor.line,
				editor.before,
				candidate,
				replacement,
				produced,
			);
			if (!landed) {
				firstError ??= new Error(
					"that text appears earlier on the page, so the engine changed a different line",
				);
				continue;
			}
			await commit(produced, `Changed the text on page ${editor.page + 1}`);
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

/** Hand the edited document to the user, as a Blob in this tab. */
function download(): void {
	const bytes = state.bytes;
	if (bytes === null) {
		return;
	}
	const blob = new Blob([new Uint8Array(bytes)], { type: "application/pdf" });
	const url = URL.createObjectURL(blob);
	const link = document.createElement("a");
	link.href = url;
	link.download = state.name === "" ? "edited.pdf" : state.name;
	document.body.appendChild(link);
	link.click();
	link.remove();
	// The object URL is released on the next turn: the download has already
	// been handed to the browser by then.
	setTimeout(() => {
		URL.revokeObjectURL(url);
	}, 0);
	setStatus(`Downloaded ${link.download}.`);
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

	dom.editToggle.addEventListener("click", () => {
		state.editText = !state.editText;
		dom.editToggle.setAttribute("aria-pressed", state.editText ? "true" : "false");
		dom.editToggle.textContent = state.editText ? "Edit text: on" : "Edit text: off";
		dom.doc.dataset.editText = state.editText ? "on" : "off";
	});

	dom.pageTools.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof HTMLElement)) {
			return;
		}
		const operation = target.dataset.op;
		if (operation !== undefined) {
			void runPageTool(operation);
		}
	});

	dom.pageList.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof HTMLElement)) {
			return;
		}
		const page = Number(target.dataset.page);
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

	// Clicking a line opens the editor over it. The handler is on the layer,
	// not on each line, so a document with ten thousand lines has one
	// listener rather than ten thousand.
	dom.doc.addEventListener("click", (event) => {
		if (!state.editText) {
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
		openInline(page, line, box.textContent ?? "");
	});
	dom.doc.addEventListener("keydown", (event) => {
		if (event.key !== "Enter" || !state.editText) {
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
		if (Number.isInteger(line) && Number.isInteger(page)) {
			openInline(page, line, box.textContent ?? "");
		}
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
	wire();
	renderHistory();
	setStatus("Open a PDF to start editing. It stays on this device.");
	await registerServiceWorker();
}

void boot().catch((error: unknown) => {
	showFailure(`The editor could not start: ${messageOf(error)}`);
});
