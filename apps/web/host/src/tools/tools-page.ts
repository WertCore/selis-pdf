/**
 * The tool-first page (SL-4.TOOLS.02).
 *
 * Six browser-local operations, one page. The only fetch this module makes is
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
	hasPdfHeader,
	pageBoundary,
	pageCount,
	pageIndex,
	shiftPageSelection,
	sizeBytes,
} from "./inputs.js";
import {
	type ToolOp,
	ToolProtocolError,
	type ToolRequest,
	type ToolsWasm,
	type WireFile,
	formatBytes,
	loadTools,
	runTool,
	verificationRows,
} from "./protocol.js";

interface ToolDef {
	readonly id: ToolOp;
	readonly mark: string;
	readonly title: string;
	readonly blurb: string;
	readonly verb: string;
	readonly multiple: boolean;
}

const TOOLS: readonly ToolDef[] = [
	{
		id: "merge",
		mark: "⇄",
		title: "Merge PDF",
		blurb: "Combine several PDFs into one, in the order you choose.",
		verb: "Merge PDFs",
		multiple: true,
	},
	{
		id: "split",
		mark: "✂",
		title: "Split PDF",
		blurb: "Split by range, every N pages, in half, by size, by bookmarks, or extract pages.",
		verb: "Split PDF",
		multiple: false,
	},
	{
		id: "rotate",
		mark: "↻",
		title: "Rotate PDF",
		blurb: "Turn all pages or a selection by 90, 180, or 270 degrees.",
		verb: "Rotate PDF",
		multiple: false,
	},
	{
		id: "delete",
		mark: "⌫",
		title: "Delete pages",
		blurb: "Remove the pages you no longer need.",
		verb: "Delete pages",
		multiple: false,
	},
	{
		id: "reorder",
		mark: "⇅",
		title: "Organize pages",
		blurb: "Reorder pages, duplicate pages, or insert blank pages.",
		verb: "Organize pages",
		multiple: false,
	},
	{
		id: "metadata",
		mark: "Aa",
		title: "Edit metadata",
		blurb: "Set the title, author, subject, and keywords.",
		verb: "Save metadata",
		multiple: false,
	},
];

const TOOL_BY_ID = new Map<ToolOp, ToolDef>(TOOLS.map((tool) => [tool.id, tool]));

/** Browser memory is finite; this keeps one run comfortably copyable. */
const MAX_TOTAL_BYTES = 256 * 1024 * 1024;

type Control = HTMLInputElement | HTMLSelectElement;
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

	let selectedTool: ToolOp | null = null;
	let files: File[] = [];
	let busy = false;
	let wasmPromise: Promise<ToolsWasm> | null = null;
	let resultUrls: string[] = [];
	const optionControls = new Map<string, Control>();
	const optionFields = new Map<string, HTMLElement>();

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
		const needed = tool.multiple ? 2 : 1;
		runButton.disabled = busy || files.length < needed;
		runButton.textContent = busy ? "Working locally…" : tool.verb;
	}

	function renderToolGrid(): void {
		const cards: HTMLElement[] = [];
		for (const tool of TOOLS) {
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
			cards.push(card);
		}
		toolGrid.replaceChildren(...cards);
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

	function textInput(key: string, type: "text" | "number", placeholder: string): HTMLInputElement {
		const input = make("input", "control");
		input.type = type;
		input.name = key;
		input.placeholder = placeholder;
		input.autocomplete = "off";
		input.spellcheck = false;
		if (type === "number") {
			input.min = "1";
			input.step = "1";
		}
		return input;
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
			note.textContent =
				"Each group becomes its own PDF. Multiple outputs also come as one ZIP.";
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

	function renderOptions(tool: ToolDef): void {
		options.replaceChildren();
		optionControls.clear();
		optionFields.clear();
		if (tool.id === "merge") {
			const note = make("p", "option-note");
			note.textContent = "Files are merged top to bottom. Use ↑ and ↓ to change the order.";
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
			addField(
				"mode",
				"Split mode",
				mode,
				"Choose what each downloaded file should contain.",
			);
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

	function selectTool(id: ToolOp): void {
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
		fileInput.value = "";
		toolsSection.hidden = true;
		workspace.hidden = false;
		history.replaceState(null, "", `#${id}`);
		setStatus(
			tool.multiple
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
	}

	function optionValue(key: string): string {
		return optionControls.get(key)?.value ?? "";
	}

	function buildRequest(tool: ToolDef, wireFiles: readonly WireFile[]): ToolRequest {
		if (tool.id === "merge") {
			return { op: "merge", files: wireFiles };
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
						throw new ToolInputError(
							"The last page must not come before the first page.",
						);
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
		const wireFiles: WireFile[] = [];
		for (const file of files) {
			const bytes = new Uint8Array(await file.arrayBuffer());
			if (!hasPdfHeader(bytes)) {
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

	function showResult(result: ReturnType<typeof runTool>): void {
		clearResult();
		const totalBytes = result.outputs.reduce(
			(total, output) => total + output.bytes.byteLength,
			0,
		);
		const pdfCount = result.reports.filter((report) => report.kind === "pdf").length;
		const hasZip = result.reports.some((report) => report.kind === "zip");
		const single = result.outputs.length === 1 ? result.outputs[0] : undefined;
		resultTitle.textContent =
			single !== undefined
				? single.name
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
			const blob = new Blob([copy.buffer], {
				type: report.kind === "zip" ? "application/zip" : "application/pdf",
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
			info.append(name, meta);

			const actions = make("div", "output-actions");
			if (report.kind === "pdf") {
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
			download.textContent =
				report.kind === "zip" ? "Download all (.zip)" : "Download";
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
		for (const row of verificationRows(result.verification)) {
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
		if (files.length < (tool.multiple ? 2 : 1)) {
			setStatus(tool.multiple ? "Choose at least two PDFs first." : "Choose a PDF first.", "error");
			return;
		}
		setBusy(true);
		clearResult();
		setStatus("Reading your PDFs on this device…", "info");
		try {
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
		renderFiles();
		for (const control of optionControls.values()) {
			if (control instanceof HTMLInputElement && control.type === "number") {
				control.value =
					control.name === "from"
						? "1"
						: control.name === "every"
							? "2"
							: control.name === "target"
								? "1"
								: control.name === "count"
									? "1"
									: "";
			} else if (control instanceof HTMLInputElement) {
				control.value = "";
			} else if (control instanceof HTMLSelectElement) {
				control.selectedIndex = 0;
			}
		}
		if (selectedTool === "split") {
			syncSplitFields();
		} else if (selectedTool === "reorder") {
			syncOrganizeFields();
		}
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
	if (TOOL_BY_ID.has(hash as ToolOp)) {
		selectTool(hash as ToolOp);
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
