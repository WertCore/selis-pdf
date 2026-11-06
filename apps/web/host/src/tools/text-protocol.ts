/**
 * Wire protocol for the lazy text chunk (Phase 3).
 *
 * The chunk writes text, tables, split bundles and reflow articles, and it
 * returns **every** output of one run in a single framed response — a text
 * export is a `.txt` plus its quality report, and opening the document twice to
 * fetch the second file would be a page deciding how many times to read the
 * user's PDF. The frame is the one documented in
 * `crates/selis-pdf-wasm-text/src/lib.rs`:
 *
 * ```text
 * "SLTX" | status | message | count | (name, kind, bytes)*
 * ```
 *
 * The chunk declares no imports, so the page instantiates it with an empty
 * import object; `loadTextChunk` refuses a module that declares any.
 */

/** The chunk the tools page fetches on first text use. */
export const TEXT_WASM_URL = "/wasm/selis_pdf_wasm_text.wasm";

/** Response magic, `SLTX`. */
export const RESPONSE_MAGIC = 0x58544c53;
/** The most outputs one run may return (64 split parts plus a report). */
export const MAX_OUTPUTS = 65;

/** Operations the chunk implements. */
export type TextOp =
	| "text"
	| "tables"
	| "split"
	| "reflow"
	| "epub"
	| "word"
	| "slides"
	| "workbook";

const OP_CODE: Record<TextOp, number> = {
	text: 1,
	tables: 2,
	split: 3,
	reflow: 4,
	epub: 5,
	word: 6,
	slides: 7,
	workbook: 8,
};

/** What an output is, for the row the page renders and the action it offers. */
export type TextOutputKind = "text" | "tables" | "report" | "zip" | "html" | "file";

/** One produced file. */
export interface TextOutput {
	readonly name: string;
	readonly kind: TextOutputKind;
	readonly bytes: Uint8Array;
}

/** The guest exports this page uses. */
export interface TextWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_text_alloc(len: number): number;
		selis_text_free(ptr: number, len: number): void;
		selis_text_run(
			ptr: number,
			len: number,
			op: number,
			optionsPtr: number,
			optionsLen: number,
			outLen: number,
		): number;
		selis_text_last_error(outLen: number): number;
		readonly selis_text_available: () => number;
	};
}

/** A protocol or operation failure that is safe to show to the user. */
export class TextProtocolError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "TextProtocolError";
	}
}

interface Cursor {
	readonly view: DataView;
	pos: number;
}

function readU32(cursor: Cursor): number {
	if (cursor.pos + 4 > cursor.view.byteLength) {
		throw new TextProtocolError("the text chunk's answer ended before its header");
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
		throw new TextProtocolError("the text chunk's answer ended inside a byte block");
	}
	const bytes = new Uint8Array(cursor.view.buffer, cursor.view.byteOffset + start, length).slice();
	cursor.pos = end;
	return bytes;
}

function readText(cursor: Cursor): string {
	return new TextDecoder().decode(readBytes(cursor));
}

function isKind(value: string): value is TextOutputKind {
	return (
		value === "text" ||
		value === "tables" ||
		value === "report" ||
		value === "zip" ||
		value === "html" ||
		value === "file"
	);
}

/**
 * Decode one response. The caller has already copied the bytes out of guest
 * memory, so the arrays are safe to keep.
 */
export function decodeTextResponse(response: Uint8Array): TextOutput[] {
	const cursor: Cursor = {
		view: new DataView(response.buffer, response.byteOffset, response.byteLength),
		pos: 0,
	};
	if (readU32(cursor) !== RESPONSE_MAGIC) {
		throw new TextProtocolError("the text chunk returned an unexpected answer");
	}
	const status = readU32(cursor);
	const message = readText(cursor);
	const count = readU32(cursor);
	if (status !== 0) {
		throw new TextProtocolError(message === "" ? "the text tool failed" : message);
	}
	if (count === 0 || count > MAX_OUTPUTS) {
		throw new TextProtocolError("the text chunk returned no usable output");
	}
	const outputs: TextOutput[] = [];
	for (let index = 0; index < count; index += 1) {
		const name = readText(cursor);
		const kind = readText(cursor);
		if (name === "") {
			throw new TextProtocolError("the text chunk returned an unnamed output");
		}
		if (!isKind(kind)) {
			throw new TextProtocolError(`the text chunk returned an unknown output kind (${kind})`);
		}
		const bytes = readBytes(cursor);
		outputs.push({ name, kind, bytes });
	}
	if (cursor.pos !== response.byteLength) {
		throw new TextProtocolError("the text chunk's answer has trailing bytes");
	}
	return outputs;
}

/**
 * Compile and instantiate the text chunk.
 *
 * The import object is empty **by contract**: a chunk that links a clock, an
 * entropy source or a network shim is not the module this page promises to run,
 * so a declared import is refused before instantiation.
 */
export async function instantiateText(
	bytes: BufferSource,
	imports: WebAssembly.Imports = {},
): Promise<TextWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new TextProtocolError(
			`the text chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, imports);
	return instance as unknown as TextWasm;
}

/** Fetch, compile and instantiate the text chunk. */
export async function loadTextChunk(fetchImpl: typeof fetch = fetch): Promise<TextWasm> {
	let response: Response;
	try {
		response = await fetchImpl(TEXT_WASM_URL, { credentials: "same-origin" });
	} catch {
		throw new TextProtocolError("the text tools could not be downloaded");
	}
	if (!response.ok) {
		throw new TextProtocolError(`the text tools are unavailable (${response.status})`);
	}
	return instantiateText(await response.arrayBuffer());
}

/**
 * Run one text operation synchronously against an instantiated chunk.
 *
 * Every guest allocation is freed on every exit path, and the request and answer
 * buffers do not outlive the call.
 */
export function runTextOp(
	wasm: TextWasm,
	input: Uint8Array,
	op: TextOp,
	options: Record<string, string>,
): TextOutput[] {
	const { exports } = wasm;
	const encoder = new TextEncoder();
	const optionsBytes = encoder.encode(JSON.stringify(options));
	const inputPtr = exports.selis_text_alloc(input.byteLength);
	if (inputPtr === 0) {
		throw new TextProtocolError("the text chunk is out of memory");
	}
	new Uint8Array(exports.memory.buffer, inputPtr, input.byteLength).set(input);
	let optionsPtr = 0;
	if (optionsBytes.byteLength > 0) {
		optionsPtr = exports.selis_text_alloc(optionsBytes.byteLength);
		if (optionsPtr === 0) {
			exports.selis_text_free(inputPtr, input.byteLength);
			throw new TextProtocolError("the text chunk is out of memory");
		}
		new Uint8Array(exports.memory.buffer, optionsPtr, optionsBytes.byteLength).set(optionsBytes);
	}
	const outLenPtr = exports.selis_text_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_text_free(inputPtr, input.byteLength);
		if (optionsPtr !== 0) {
			exports.selis_text_free(optionsPtr, optionsBytes.byteLength);
		}
		throw new TextProtocolError("the text chunk is out of memory");
	}
	const responsePtr = exports.selis_text_run(
		inputPtr,
		input.byteLength,
		OP_CODE[op],
		optionsPtr,
		optionsBytes.byteLength,
		outLenPtr,
	);
	if (responsePtr === 0) {
		exports.selis_text_free(inputPtr, input.byteLength);
		if (optionsPtr !== 0) {
			exports.selis_text_free(optionsPtr, optionsBytes.byteLength);
		}
		exports.selis_text_free(outLenPtr, 4);
		throw new TextProtocolError("the text chunk could not read the request");
	}
	const responseLen = new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response = new Uint8Array(exports.memory.buffer, responsePtr, responseLen).slice();
	exports.selis_text_free(inputPtr, input.byteLength);
	if (optionsPtr !== 0) {
		exports.selis_text_free(optionsPtr, optionsBytes.byteLength);
	}
	exports.selis_text_free(outLenPtr, 4);
	exports.selis_text_free(responsePtr, responseLen);
	return decodeTextResponse(response);
}

/** Read the last guest error, for a failure the framed answer could not carry. */
export function lastTextError(wasm: TextWasm): string {
	const { exports } = wasm;
	const outLenPtr = exports.selis_text_alloc(4);
	if (outLenPtr === 0) {
		return "";
	}
	const ptr = exports.selis_text_last_error(outLenPtr);
	if (ptr === 0) {
		exports.selis_text_free(outLenPtr, 4);
		return "";
	}
	const len = new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const bytes = new Uint8Array(exports.memory.buffer, ptr, len).slice();
	exports.selis_text_free(ptr, len);
	exports.selis_text_free(outLenPtr, 4);
	return new TextDecoder().decode(bytes);
}
