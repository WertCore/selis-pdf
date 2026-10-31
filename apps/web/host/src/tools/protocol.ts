/**
 * Wire protocol for the lazy tools WASM chunk (SL-4.TOOLS.01).
 *
 * The chunk is deliberately separate from the viewer: the tools page never
 * downloads it until the user runs an operation, and it has **no imports**.
 * A wasm-bindgen, filesystem, or network import in this module would make the
 * artifact non-instantiable under the page's closed import object, so the
 * loader rejects one before doing any work. That is a structural check, not a
 * promise in a comment.
 *
 * The byte layout is the one documented in
 * `crates/selis-pdf-wasm-tools/src/lib.rs`. All integers are little-endian.
 */

/** The chunk the tools page fetches on first use. */
export const TOOLS_WASM_URL = "/wasm/selis_pdf_wasm_tools.wasm";

/** Request magic, `SLTS`. */
export const REQUEST_MAGIC = 0x53544c53;
/** Response magic, `SLTR`. */
export const RESPONSE_MAGIC = 0x52544c53;
/** Protocol version. */
export const PROTOCOL_VERSION = 1;
/** The maximum number of inputs the chunk accepts. */
export const MAX_FILES = 64;
/** The maximum outputs (up to 64 PDF parts or attachments plus a ZIP bundle). */
export const MAX_OUTPUTS = MAX_FILES + 1;
/** The maximum request name length the chunk accepts. */
export const MAX_NAME_BYTES = 128;
/** The maximum options JSON length the chunk accepts. */
export const MAX_OPTIONS_BYTES = 64 * 1024;

/** Operations this chunk implements today. */
export type ToolOp =
	| "merge"
	| "mix"
	| "split"
	| "rotate"
	| "delete"
	| "reorder"
	| "metadata"
	| "page_numbers"
	| "watermark"
	| "header_footer"
	| "bates"
	| "compress"
	| "attachments"
	| "crop"
	| "flip"
	| "page_labels"
	| "strip_metadata"
	| "resize"
	| "nup"
	| "attach"
	| "detach"
	| "repair"
	| "image_to_pdf";

/** The wire operation number for each operation. */
const OP_CODE: Record<ToolOp, number> = {
	merge: 1,
	split: 2,
	rotate: 3,
	delete: 4,
	reorder: 5,
	metadata: 6,
	page_numbers: 7,
	watermark: 8,
	header_footer: 9,
	bates: 10,
	compress: 11,
	attachments: 12,
	mix: 13,
	crop: 14,
	flip: 15,
	page_labels: 16,
	strip_metadata: 17,
	resize: 18,
	nup: 19,
	attach: 20,
	detach: 21,
	repair: 22,
	image_to_pdf: 23,
};

/** One input file on the wire. */
export interface WireFile {
	readonly name: string;
	readonly bytes: Uint8Array;
}

/** One requested operation. */
export interface ToolRequest {
	readonly op: ToolOp;
	readonly options?: Record<string, unknown>;
	readonly files: readonly WireFile[];
}

/** The verification object emitted by `selis-pdf-tools`. */
export interface Verification {
	readonly pages_in?: number;
	readonly pages_out: number;
	readonly annotations_in?: number;
	readonly annotations_out: number;
	readonly fields_in?: number;
	readonly fields_out: number;
	readonly ocgs_in?: number;
	readonly ocgs_out: number;
	readonly outline_entries_in?: number;
	readonly outline_entries_out: number;
	readonly embedded_files_in?: number;
	readonly embedded_files_out: number;
	readonly bytes_in: number;
	readonly bytes_out: number;
	readonly verdict: string;
	readonly checked: readonly string[];
	/** How a damaged input was recovered, when the operation is repair. */
	readonly repair_mode?: string;
}

/** One produced file. */
export interface ToolOutput {
	readonly name: string;
	readonly bytes: Uint8Array;
}

/** Per-file metadata emitted alongside the aggregate verification. */
export interface ToolOutputReport {
	readonly name: string;
	readonly kind: "pdf" | "zip" | "file";
	readonly pages: number;
	readonly bytes: number;
	readonly verdict: string;
	/** A size-split part that is larger than the requested target. */
	readonly overTarget?: boolean;
}

/** A successful operation. */
export interface ToolResult {
	readonly verification: Verification;
	readonly reports: readonly ToolOutputReport[];
	readonly outputs: readonly ToolOutput[];
}

/** The guest exports this page uses. */
export interface ToolsWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_tools_alloc(len: number): number;
		selis_tools_free(ptr: number, len: number): void;
		selis_tools_run(ptr: number, len: number, outLen: number): number;
		readonly selis_tools_capabilities: () => number;
		readonly selis_tools_chunk_id: (outLen: number) => number;
	};
}

/** A protocol or operation failure that is safe to show to the user. */
export class ToolProtocolError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "ToolProtocolError";
	}
}

function writeU32(target: Uint8Array, offset: number, value: number): number {
	target[offset] = value & 0xff;
	target[offset + 1] = (value >>> 8) & 0xff;
	target[offset + 2] = (value >>> 16) & 0xff;
	target[offset + 3] = (value >>> 24) & 0xff;
	return offset + 4;
}

function byteLength(value: number): number {
	return 4 + value;
}

/**
 * Encode a request for `selis_tools_run`.
 *
 * Throws {@link ToolProtocolError} for a malformed request before the guest
 * sees it: an empty file list, too many files, an unknown operation, an
 * oversized name, or oversized options.
 */
export function encodeToolRequest(request: ToolRequest): Uint8Array {
	const opCode = OP_CODE[request.op];
	if (opCode === undefined) {
		throw new ToolProtocolError(`unknown tool operation: ${String(request.op)}`);
	}
	if (request.files.length === 0) {
		throw new ToolProtocolError("choose at least one PDF first");
	}
	if (request.files.length > MAX_FILES) {
		throw new ToolProtocolError(`at most ${MAX_FILES} files can be processed at once`);
	}
	const encoder = new TextEncoder();
	const options =
		request.options === undefined
			? new Uint8Array(0)
			: encoder.encode(JSON.stringify(request.options));
	if (options.length > MAX_OPTIONS_BYTES) {
		throw new ToolProtocolError("tool options are too large");
	}

	const names = request.files.map((file) => encoder.encode(file.name));
	for (const name of names) {
		if (name.length > MAX_NAME_BYTES) {
			throw new ToolProtocolError("a file name is too long");
		}
	}

	// magic + version + op + options length are the first 16 bytes; the options
	// block counts its payload once, not its own length field again.
	let total = 16 + options.length + 4;
	for (let i = 0; i < request.files.length; i += 1) {
		const name = names[i];
		const file = request.files[i];
		if (name === undefined || file === undefined) {
			throw new ToolProtocolError("file list changed while encoding the request");
		}
		total += byteLength(name.length) + byteLength(file.bytes.length);
	}
	if (total > 0xffff_ffff) {
		throw new ToolProtocolError("the request exceeds the 4 GiB protocol limit");
	}

	const out = new Uint8Array(total);
	let offset = 0;
	offset = writeU32(out, offset, REQUEST_MAGIC);
	offset = writeU32(out, offset, PROTOCOL_VERSION);
	offset = writeU32(out, offset, opCode);
	offset = writeU32(out, offset, options.length);
	out.set(options, offset);
	offset += options.length;
	offset = writeU32(out, offset, request.files.length);
	for (let i = 0; i < request.files.length; i += 1) {
		const name = names[i];
		const file = request.files[i];
		if (name === undefined || file === undefined) {
			throw new ToolProtocolError("file list changed while encoding the request");
		}
		offset = writeU32(out, offset, name.length);
		out.set(name, offset);
		offset += name.length;
		offset = writeU32(out, offset, file.bytes.length);
		out.set(file.bytes, offset);
		offset += file.bytes.length;
	}
	return out;
}

interface Cursor {
	readonly view: DataView;
	pos: number;
}

function readU32(cursor: Cursor): number {
	if (cursor.pos + 4 > cursor.view.byteLength) {
		throw new ToolProtocolError("the tool response ended before its header");
	}
	const value = cursor.view.getUint32(cursor.pos, true);
	cursor.pos += 4;
	return value;
}

function readBytes(cursor: Cursor): Uint8Array {
	const length = readU32(cursor);
	const start = cursor.pos;
	const end = start + length;
	if (end > cursor.view.byteLength) {
		throw new ToolProtocolError("the tool response ended inside a byte block");
	}
	const bytes = new Uint8Array(
		cursor.view.buffer,
		cursor.view.byteOffset + start,
		length,
	).slice();
	cursor.pos = end;
	return bytes;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null;
}

function decodeReports(
	raw: unknown,
	outputs: readonly ToolOutput[],
	verification: Verification,
): ToolOutputReport[] {
	if (raw === undefined) {
		if (outputs.length !== 1) {
			throw new ToolProtocolError(
				"the tool chunk returned multiple outputs without per-file metadata",
			);
		}
		const output = outputs[0];
		if (output === undefined) {
			throw new ToolProtocolError("the tool chunk returned no output");
		}
		const zip = output.name.toLowerCase().endsWith(".zip");
		return [
			{
				name: output.name,
				kind: zip ? "zip" : "pdf",
				pages: zip ? 0 : verification.pages_out,
				bytes: output.bytes.length,
				verdict: verification.verdict,
				overTarget: false,
			},
		];
	}
	if (!Array.isArray(raw) || raw.length !== outputs.length) {
		throw new ToolProtocolError("the tool chunk returned mismatched output metadata");
	}
	return raw.map((item, index) => {
		if (!isRecord(item)) {
			throw new ToolProtocolError("the tool chunk returned malformed output metadata");
		}
		const output = outputs[index];
		const { name, kind, pages, bytes, verdict, overTarget } = item;
		if (output === undefined || typeof name !== "string" || name !== output.name) {
			throw new ToolProtocolError("the tool chunk returned mismatched output metadata");
		}
		if (kind !== "pdf" && kind !== "zip" && kind !== "file") {
			throw new ToolProtocolError("the tool chunk returned an unknown output kind");
		}
		if (
			typeof pages !== "number" ||
			!Number.isInteger(pages) ||
			pages < 0 ||
			typeof bytes !== "number" ||
			!Number.isInteger(bytes) ||
			bytes < 0 ||
			bytes !== output.bytes.length ||
			typeof verdict !== "string" ||
			(overTarget !== undefined && typeof overTarget !== "boolean")
		) {
			throw new ToolProtocolError("the tool chunk returned malformed output metadata");
		}
		return { name, kind, pages, bytes, verdict, overTarget: overTarget === true };
	});
}

/**
 * Decode one response. The caller has already copied the bytes out of guest
 * memory, so the returned arrays are safe to retain after the chunk grows.
 */
export function decodeToolResponse(response: Uint8Array): ToolResult {
	const cursor: Cursor = {
		view: new DataView(response.buffer, response.byteOffset, response.byteLength),
		pos: 0,
	};
	const magic = readU32(cursor);
	if (magic !== RESPONSE_MAGIC) {
		throw new ToolProtocolError("the tool chunk returned an unexpected response");
	}
	const status = readU32(cursor);
	const message = new TextDecoder().decode(readBytes(cursor));
	const outputCount = readU32(cursor);
	if (outputCount > MAX_OUTPUTS) {
		throw new ToolProtocolError("the tool chunk returned too many outputs");
	}
	const outputs: ToolOutput[] = [];
	for (let i = 0; i < outputCount; i += 1) {
		const name = new TextDecoder().decode(readBytes(cursor));
		const bytes = readBytes(cursor);
		outputs.push({ name, bytes });
	}
	if (cursor.pos !== response.byteLength) {
		throw new ToolProtocolError("the tool response has trailing bytes");
	}
	if (status !== 0) {
		throw new ToolProtocolError(message === "" ? "the tool operation failed" : message);
	}
	if (outputs.length === 0) {
		throw new ToolProtocolError("the tool chunk returned no output");
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(message);
	} catch {
		throw new ToolProtocolError("the tool chunk returned malformed verification data");
	}
	if (!isRecord(parsed)) {
		throw new ToolProtocolError("the tool chunk returned malformed verification data");
	}
	const verification = parsed as unknown as Verification;
	const reports = decodeReports(parsed.outputs, outputs, verification);
	return { verification, reports, outputs };
}

/**
 * Compile and instantiate the tools chunk.
 *
 * The import object is empty **by contract**. The module declares no imports
 * (no wasm-bindgen glue, no JS CSPRNG); if that ever changes, the check below
 * fails before instantiation instead of leaving a page that silently uploaded
 * nothing but also did nothing.
 */
export async function instantiateTools(
	bytes: BufferSource,
	imports: WebAssembly.Imports = {},
): Promise<ToolsWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the tools chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, imports);
	return instance as unknown as ToolsWasm;
}

/** Fetch, compile, and instantiate the tools chunk. */
export async function loadTools(
	fetchImpl: typeof fetch = fetch,
	url: string = TOOLS_WASM_URL,
): Promise<ToolsWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the tools chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the tools chunk is unavailable (${response.status})`);
	}
	const bytes = await response.arrayBuffer();
	return instantiateTools(bytes);
}

/**
 * Run one operation synchronously against an instantiated chunk.
 *
 * The guest memory is copied in and out explicitly; no pointer or guest
 * buffer survives the call. The request and length buffers are freed on every
 * exit path.
 */
export function runTool(wasm: ToolsWasm, request: ToolRequest): ToolResult {
	const encoded = encodeToolRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_tools_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the tools chunk is out of memory");
	}
	const outLenPtr = exports.selis_tools_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_tools_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the tools chunk is out of memory");
	}
	// Views must be created after both allocations: a later allocation can
	// grow memory and detach an earlier `ArrayBuffer`.
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_tools_run(requestPtr, encoded.byteLength, outLenPtr);
	if (responsePtr === 0) {
		exports.selis_tools_free(requestPtr, encoded.byteLength);
		exports.selis_tools_free(outLenPtr, 4);
		throw new ToolProtocolError("the tools chunk could not read the request");
	}
	const responseLen = new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response = new Uint8Array(exports.memory.buffer, responsePtr, responseLen).slice();
	exports.selis_tools_free(requestPtr, encoded.byteLength);
	exports.selis_tools_free(outLenPtr, 4);
	exports.selis_tools_free(responsePtr, responseLen);
	return decodeToolResponse(response);
}

/** Human-readable byte size. */
export function formatBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes < 0) {
		return "—";
	}
	if (bytes < 1024) {
		return `${bytes} B`;
	}
	const units = ["KiB", "MiB", "GiB"];
	let value = bytes / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unit] ?? "KiB"}`;
}

/** The rows the result card displays. */
export interface VerificationRow {
	readonly label: string;
	readonly value: string;
}

/**
 * Turn the verification object into the rows the page shows. The labels are
 * the exact checks the Rust gate ran (`checked`), plus the measured output
 * counts and the verdict.
 */
export function verificationRows(verification: Verification): VerificationRow[] {
	const rows: VerificationRow[] = [];
	const checked = new Set(verification.checked);
	const pair = (before: number | undefined, after: number): string =>
		before === undefined ? String(after) : `${before} → ${after}`;

	if (checked.has("pages") || verification.pages_in !== undefined) {
		rows.push({
			label: "Pages",
			value: pair(verification.pages_in, verification.pages_out),
		});
	}
	if (checked.has("annotations") || verification.annotations_in !== undefined) {
		rows.push({
			label: "Annotations",
			value: pair(verification.annotations_in, verification.annotations_out),
		});
	}
	if (checked.has("fields") || verification.fields_in !== undefined) {
		rows.push({
			label: "Form fields",
			value: pair(verification.fields_in, verification.fields_out),
		});
	}
	if (checked.has("ocgs") || verification.ocgs_in !== undefined) {
		rows.push({
			label: "Layers",
			value: pair(verification.ocgs_in, verification.ocgs_out),
		});
	}
	if (checked.has("outline_entries") || verification.outline_entries_in !== undefined) {
		rows.push({
			label: "Outline entries",
			value: pair(verification.outline_entries_in, verification.outline_entries_out),
		});
	}
	if (checked.has("embedded_files") || verification.embedded_files_in !== undefined) {
		rows.push({
			label: "Embedded files",
			value: pair(verification.embedded_files_in, verification.embedded_files_out),
		});
	}
	rows.push({
		label: "Size",
		value: `${formatBytes(verification.bytes_in)} → ${formatBytes(verification.bytes_out)}`,
	});
	if (verification.repair_mode !== undefined) {
		rows.push({ label: "Recovery", value: verification.repair_mode });
	}
	rows.push({ label: "Verification", value: verification.verdict });
	return rows;
}
