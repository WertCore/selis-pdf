/**
 * Local-only document handoff (SL-4.WEB.03).
 *
 * The one thing this app must never do is upload a document. Every entry
 * point — drag-drop, file picker, paste, and `?src=` URL opening — resolves
 * to a {@link HandoffSource} that the Worker opens *locally* (bytes copied
 * into guest memory, or a Blob/OPFS/FSA handle the Worker reads itself, or
 * an http-range fetch whose bytes stream into the local engine). Nothing
 * here performs a POST/PUT of document bytes, and `no-upload.ts` asserts
 * that in CI.
 *
 * This module is DOM-free and pure: it takes `File`-like records
 * (`{ name, type, size }`) and URL strings, never `window`/`document`/
 * `fetch` directly, so Vitest runs it in Node. The thin DOM adapters
 * (event listeners) live in the shell entry point, outside this package's
 * test surface.
 */

/** Maximum single document accepted through handoff (mirrors the WASM Worker's 64 MiB bound). */
export const MAX_HANDOFF_BYTES = 64 * 1024 * 1024;

/** Maximum files accepted from one drop/paste/pick (a hostile drag carries thousands). */
export const MAX_HANDOFF_FILES = 32;

/** Where a handed-off document comes from. Mirrors the UI's `DocumentSourceDescriptor` names. */
export type HandoffSource =
	| { readonly kind: "bytes"; readonly name: string; readonly size: number }
	| { readonly kind: "blob"; readonly name: string; readonly size: number }
	| { readonly kind: "opfs"; readonly path: string }
	| { readonly kind: "fsa"; readonly handleId: string; readonly name: string }
	| { readonly kind: "http-range"; readonly url: string; readonly size?: number };

/** Minimal file shape the handoff needs (a real `File` satisfies it). */
export interface HandoffFile {
	readonly name: string;
	readonly type: string;
	readonly size: number;
}

/** Minimal DataTransfer shape (a real `DataTransfer` satisfies it). */
export interface HandoffDataTransfer {
	readonly files: ArrayLike<HandoffFile>;
}

/** Minimal clipboard shape (a real `ClipboardEvent.clipboardData` satisfies it). */
export interface HandoffClipboard {
	readonly files: ArrayLike<HandoffFile>;
}

/** Result of filtering one handoff event. */
export interface HandoffResult {
	/** Accepted files, in event order, capped at {@link MAX_HANDOFF_FILES}. */
	readonly accepted: readonly HandoffFile[];
	/** Human-readable reasons for every rejection (same order as encountered). */
	readonly rejected: readonly string[];
}

/** Whether a filename looks like a PDF (extension check only — the engine validates bytes). */
export function isPdfName(name: string): boolean {
	const trimmed = name.trim();
	if (trimmed.length === 0) {
		return false;
	}
	return trimmed.toLowerCase().endsWith(".pdf");
}

/** Whether a MIME type is a PDF type (empty type is common from OS drags — allowed, name decides). */
export function isPdfMime(type: string): boolean {
	if (type === "") {
		return true;
	}
	return type === "application/pdf";
}

function filterFiles(files: ArrayLike<HandoffFile>): HandoffResult {
	const accepted: HandoffFile[] = [];
	const rejected: string[] = [];
	const n = files.length;
	for (let i = 0; i < n; i += 1) {
		const file = files[i] as HandoffFile | undefined;
		if (file === undefined) {
			continue;
		}
		if (accepted.length >= MAX_HANDOFF_FILES) {
			rejected.push(`"${file.name}": too many files (max ${MAX_HANDOFF_FILES})`);
			continue;
		}
		if (file.size <= 0) {
			rejected.push(`"${file.name}": empty file`);
			continue;
		}
		if (file.size > MAX_HANDOFF_BYTES) {
			rejected.push(`"${file.name}": exceeds the 64 MiB handoff bound`);
			continue;
		}
		if (!isPdfName(file.name)) {
			rejected.push(`"${file.name}": not a .pdf filename`);
			continue;
		}
		if (!isPdfMime(file.type)) {
			rejected.push(`"${file.name}": unexpected type "${file.type}"`);
			continue;
		}
		accepted.push(file);
	}
	return { accepted, rejected };
}

/**
 * Files from a drag-drop event's `dataTransfer`. Pure: no DOM access beyond
 * the passed record.
 */
export function filesFromDrop(dataTransfer: HandoffDataTransfer): HandoffResult {
	return filterFiles(dataTransfer.files);
}

/**
 * Files from a paste event's `clipboardData`. Pure: no DOM access beyond
 * the passed record.
 */
export function filesFromPaste(clipboardData: HandoffClipboard): HandoffResult {
	return filterFiles(clipboardData.files);
}

/**
 * Files from `<input type="file">` / `showOpenFilePicker()`. Same filter as
 * drop/paste so every entry point enforces the same bound.
 */
export function filesFromPicker(files: ArrayLike<HandoffFile>): HandoffResult {
	return filterFiles(files);
}

/** A `?src=` opening resolved locally (never an upload). */
export type SrcOpening =
	| { readonly ok: true; readonly source: HandoffSource }
	| { readonly ok: false; readonly reason: string };

/**
 * Parse `?src=` from a full URL string (e.g. `location.href`).
 *
 * Rules (all local):
 * - Missing/empty `src` → `{ ok: false }` (nothing to open, not an error).
 * - `blob:` / `opfs:` pseudo-URLs are rejected — those handles arrive via
 *   picker/drop, never via a pasted URL.
 * - `javascript:`, `data:`, `file:` are rejected (active-content / exfil
 *   vectors, ADR-P0020).
 * - `http:`/`https:` are accepted as `http-range` sources: the Worker's
 *   range fetcher streams bytes into the *local* engine. Cross-origin URLs
 *   are accepted here and gated on CORS at fetch time (the fetch driver
 *   documents the fallback when the origin refuses ranges or omits CORS
 *   headers — WASM.06); acceptance here never implies an upload.
 * - Same-origin is preferred but not required for parsing; the caller may
 *   compare against `location.origin` for UI hints via {@link isSameOrigin}.
 */
export function parseSrcParam(href: string): SrcOpening {
	let url: URL;
	try {
		url = new URL(href);
	} catch {
		return { ok: false, reason: "not a URL" };
	}
	const src = url.searchParams.get("src");
	if (src === null || src.trim() === "") {
		return { ok: false, reason: "no ?src= parameter" };
	}
	const trimmed = src.trim();
	const lower = trimmed.toLowerCase();
	if (
		lower.startsWith("javascript:") ||
		lower.startsWith("data:") ||
		lower.startsWith("file:") ||
		lower.startsWith("blob:") ||
		lower.startsWith("opfs:")
	) {
		return { ok: false, reason: `rejected ?src= scheme in "${trimmed.slice(0, 32)}"` };
	}
	let parsed: URL;
	try {
		parsed = new URL(trimmed, url);
	} catch {
		return { ok: false, reason: "unparseable ?src= URL" };
	}
	if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
		return { ok: false, reason: `rejected ?src= scheme "${parsed.protocol}"` };
	}
	return { ok: true, source: { kind: "http-range", url: parsed.toString() } };
}

/** Whether a `?src=` URL is same-origin with the app (for UI hints only — never a security gate). */
export function isSameOrigin(srcUrl: string, appHref: string): boolean {
	try {
		return new URL(srcUrl).origin === new URL(appHref).origin;
	} catch {
		return false;
	}
}

/**
 * Map an accepted picker/drop file to the handoff source the Worker opens.
 * The file itself is never read here (the main thread never parses); the
 * Worker glue registers the `Blob` and opens `{ kind: "blob" }`.
 */
export function sourceForFile(file: HandoffFile): HandoffSource {
	return { kind: "blob", name: file.name, size: file.size };
}
