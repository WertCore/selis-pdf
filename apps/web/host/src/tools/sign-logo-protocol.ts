/**
 * Wire protocol for the lazy signature-logo WASM chunk (SIGN.07).
 *
 * The layout is documented in `crates/selis-pdf-wasm-sign-logo/src/lib.rs`. It
 * is the tools-protocol envelope — magic, version, operation, options JSON,
 * named files — but carries TWO files: the document and the caller's local
 * logo bytes (PNG/JPEG for sign, expected `/AP /N` bytes for the read-back
 * op). The logo is a file the signer picked in this tab; it is never fetched
 * from a URL, and no host import (clock, entropy, filesystem, network) reaches
 * the guest. The private key still arrives in the options JSON, so the request
 * buffer is zeroed as soon as the call returns.
 */

import type { ToolOutput, ToolOutputReport, Verification, VerificationRow } from "./protocol.js";
import { ToolProtocolError } from "./protocol.js";
import type { SignOutcome } from "./sign-protocol.js";

/** The chunk the sign card fetches only when a logo is selected. */
export const SIGN_LOGO_WASM_URL = "/wasm/selis_pdf_wasm_sign_logo.wasm";

/** Request magic, `SLLG`. */
export const SIGN_LOGO_REQUEST_MAGIC = 0x474c_4c53;
/** Response magic, `SLRL`. */
export const SIGN_LOGO_RESPONSE_MAGIC = 0x4c52_4c53;
/** Protocol version. */
export const SIGN_LOGO_PROTOCOL_VERSION = 1;
/** Maximum input name length the guest accepts. */
export const MAX_SIGN_LOGO_NAME_BYTES = 128;
/** The options carry a PEM key and its certificate chain. */
export const MAX_SIGN_LOGO_OPTIONS_BYTES = 256 * 1024;

/** The two operations this chunk implements. */
export type SignLogoOp = "sign-logo" | "verify-appearance";

const OP_CODE: Record<SignLogoOp, number> = {
	"sign-logo": 1,
	"verify-appearance": 2,
};

/** One requested operation: a document, a second local payload and options. */
export interface SignLogoRequest {
	readonly op: SignLogoOp;
	readonly options?: Record<string, unknown>;
	readonly file: { readonly name: string; readonly bytes: Uint8Array };
	/**
	 * PNG/JPEG bytes for `sign-logo`; expected `/AP /N` bytes for
	 * `verify-appearance`. Chosen by the caller, never fetched by the guest.
	 */
	readonly second: { readonly name: string; readonly bytes: Uint8Array };
}

/** A successful document-producing run. */
export interface SignLogoResult {
	readonly kind: "document";
	readonly verification: Verification;
	readonly report: ToolOutputReport;
	readonly output: { readonly name: string; readonly bytes: Uint8Array };
	readonly outcome: SignOutcome;
}

/** A successful read-only `/AP /N` comparison. */
export interface AppearanceVerificationResult {
	readonly kind: "appearance-verification";
	readonly field: string;
	readonly hasLogo: boolean;
	/** Whether the logo carried a soft mask that the read-back verified. */
	readonly hasSMask: boolean;
	readonly appearanceBytes: number;
}

export type SignLogoResponse = SignLogoResult | AppearanceVerificationResult;

/** The guest exports this page uses. No import object is ever supplied. */
export interface SignLogoWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_sign_logo_alloc(len: number): number;
		selis_sign_logo_free(ptr: number, len: number): void;
		selis_sign_logo_run(ptr: number, len: number, outLen: number): number;
	};
}

function writeU32(target: Uint8Array, offset: number, value: number): number {
	target[offset] = value & 0xff;
	target[offset + 1] = (value >>> 8) & 0xff;
	target[offset + 2] = (value >>> 16) & 0xff;
	target[offset + 3] = (value >>> 24) & 0xff;
	return offset + 4;
}

/** Encode a request, refusing a malformed one before the guest sees it. */
export function encodeSignLogoRequest(request: SignLogoRequest): Uint8Array {
	const opCode = OP_CODE[request.op];
	if (opCode === undefined) {
		throw new ToolProtocolError(`unknown signature-logo operation: ${String(request.op)}`);
	}
	const encoder = new TextEncoder();
	const name = encoder.encode(request.file.name);
	const secondName = encoder.encode(request.second.name);
	if (name.length === 0 || secondName.length === 0) {
		throw new ToolProtocolError("both inputs need a name");
	}
	if (name.length > MAX_SIGN_LOGO_NAME_BYTES || secondName.length > MAX_SIGN_LOGO_NAME_BYTES) {
		throw new ToolProtocolError("an input name is too long");
	}
	if (request.file.bytes.length === 0 || request.second.bytes.length === 0) {
		throw new ToolProtocolError("the document and the second input must not be empty");
	}
	const options =
		request.options === undefined
			? new Uint8Array(0)
			: encoder.encode(JSON.stringify(request.options));
	if (options.length > MAX_SIGN_LOGO_OPTIONS_BYTES) {
		throw new ToolProtocolError("signing options are too large");
	}

	const total =
		16 +
		options.length +
		4 +
		4 +
		name.length +
		4 +
		request.file.bytes.length +
		4 +
		secondName.length +
		4 +
		request.second.bytes.length;
	if (total > 0xffff_ffff) {
		throw new ToolProtocolError("the request exceeds the 4 GiB protocol limit");
	}
	const out = new Uint8Array(total);
	let offset = 0;
	offset = writeU32(out, offset, SIGN_LOGO_REQUEST_MAGIC);
	offset = writeU32(out, offset, SIGN_LOGO_PROTOCOL_VERSION);
	offset = writeU32(out, offset, opCode);
	offset = writeU32(out, offset, options.length);
	out.set(options, offset);
	offset += options.length;
	offset = writeU32(out, offset, 2);
	for (const input of [request.file, request.second]) {
		const encodedName = input === request.file ? name : secondName;
		offset = writeU32(out, offset, encodedName.length);
		out.set(encodedName, offset);
		offset += encodedName.length;
		offset = writeU32(out, offset, input.bytes.length);
		out.set(input.bytes, offset);
		offset += input.bytes.length;
	}
	return out;
}

interface Cursor {
	readonly view: DataView;
	pos: number;
}

function readU32(cursor: Cursor): number {
	if (cursor.pos + 4 > cursor.view.byteLength) {
		throw new ToolProtocolError("the signature-logo response ended before its header");
	}
	const value = cursor.view.getUint32(cursor.pos, true);
	cursor.pos += 4;
	return value;
}

function readBytes(cursor: Cursor, where: string): Uint8Array {
	const length = readU32(cursor);
	const end = cursor.pos + length;
	if (end > cursor.view.byteLength) {
		throw new ToolProtocolError(`the signature-logo response ended inside ${where}`);
	}
	const bytes = new Uint8Array(cursor.view.buffer, cursor.view.byteOffset + cursor.pos, length);
	cursor.pos = end;
	return bytes.slice();
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null;
}

function optionalString(value: unknown, field: string): string | null {
	if (value === undefined || value === null) {
		return null;
	}
	if (typeof value !== "string") {
		throw new ToolProtocolError(`the signature-logo chunk returned a malformed ${field}`);
	}
	return value;
}

/** Decode one response; the byte slices are copies and survive a later alloc. */
export function decodeSignLogoResponse(response: Uint8Array): SignLogoResponse {
	const cursor: Cursor = {
		view: new DataView(response.buffer, response.byteOffset, response.byteLength),
		pos: 0,
	};
	if (readU32(cursor) !== SIGN_LOGO_RESPONSE_MAGIC) {
		throw new ToolProtocolError("the signature-logo chunk returned an unexpected response");
	}
	const status = readU32(cursor);
	const message = new TextDecoder().decode(readBytes(cursor, "its message"));
	if (status !== 0) {
		throw new ToolProtocolError(message === "" ? "the signature-logo operation failed" : message);
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(message);
	} catch {
		throw new ToolProtocolError("the signature-logo chunk returned malformed result data");
	}
	if (!isRecord(parsed)) {
		throw new ToolProtocolError("the signature-logo chunk returned malformed result data");
	}
	const outputCount = readU32(cursor);

	if (outputCount === 0) {
		if (cursor.pos !== response.byteLength) {
			throw new ToolProtocolError("the signature-logo response has trailing bytes");
		}
		const field = optionalString(parsed.field, "field");
		if (field === null || typeof parsed.verified !== "boolean") {
			throw new ToolProtocolError("the signature-logo chunk returned a malformed read-back");
		}
		return {
			kind: "appearance-verification",
			field,
			hasLogo: parsed.hasLogo === true,
			hasSMask: parsed.hasSMask === true,
			appearanceBytes:
				typeof parsed.appearanceBytes === "number" ? parsed.appearanceBytes : 0,
		};
	}
	if (outputCount !== 1) {
		throw new ToolProtocolError("a signature-logo run returns at most one document");
	}
	const name = new TextDecoder().decode(readBytes(cursor, "an output name"));
	const bytes = readBytes(cursor, "an output");
	if (cursor.pos !== response.byteLength) {
		throw new ToolProtocolError("the signature-logo response has trailing bytes");
	}
	const reports = parsed.outputs;
	if (!Array.isArray(reports) || reports.length !== 1 || !isRecord(reports[0])) {
		throw new ToolProtocolError("the signature-logo chunk returned mismatched output metadata");
	}
	const report = reports[0];
	if (report.name !== name || report.kind !== "pdf") {
		throw new ToolProtocolError("the signature-logo chunk described a different output than it wrote");
	}
	const { pages, verdict, bytes: claimedBytes } = report;
	if (
		typeof pages !== "number" ||
		!Number.isInteger(pages) ||
		pages < 0 ||
		typeof claimedBytes !== "number" ||
		claimedBytes !== bytes.length ||
		typeof verdict !== "string"
	) {
		throw new ToolProtocolError("the signature-logo chunk returned malformed output metadata");
	}
	const output: ToolOutput = { name, bytes };
	const typedReport: ToolOutputReport = {
		name,
		kind: "pdf",
		pages,
		bytes: bytes.length,
		verdict,
		overTarget: false,
	};
	const field = optionalString(parsed.field, "field");
	return {
		kind: "document",
		verification: parsed as unknown as Verification,
		report: typedReport,
		output,
		outcome: {
			operation: typeof parsed.operation === "string" ? parsed.operation : "sign-logo",
			field: field ?? undefined,
			page: typeof parsed.page === "number" ? parsed.page : undefined,
			subFilter: optionalString(parsed.subFilter, "sub-filter") ?? undefined,
		},
	};
}

/**
 * Compile and instantiate the chunk. The import object is empty by contract:
 * an import would be a capability this module never needs and a hole in the
 * no-upload promise, so a module that declares one is rejected before it runs.
 */
export async function instantiateSignLogo(bytes: BufferSource): Promise<SignLogoWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the signature-logo chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, {});
	return instance as unknown as SignLogoWasm;
}

/** Fetch, compile, instantiate. This is the ONLY network use, and it is the
 * local, same-origin WASM asset — never the user's document or logo. */
export async function loadSignLogo(
	fetchImpl: typeof fetch = fetch,
	url: string = SIGN_LOGO_WASM_URL,
): Promise<SignLogoWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the signature-logo chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the signature-logo chunk is unavailable (${response.status})`);
	}
	return instantiateSignLogo(await response.arrayBuffer());
}

/** Run one operation, zeroing the request buffer (which may hold the key). */
export function runSignLogo(wasm: SignLogoWasm, request: SignLogoRequest): SignLogoResponse {
	const encoded = encodeSignLogoRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_sign_logo_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the signature-logo chunk is out of memory");
	}
	const outLenPtr = exports.selis_sign_logo_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_sign_logo_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the signature-logo chunk is out of memory");
	}
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_sign_logo_run(requestPtr, encoded.byteLength, outLenPtr);
	const responseLength =
		responsePtr === 0 ? 0 : new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response =
		responsePtr === 0
			? null
			: new Uint8Array(exports.memory.buffer, responsePtr, responseLength).slice();
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).fill(0);
	exports.selis_sign_logo_free(requestPtr, encoded.byteLength);
	exports.selis_sign_logo_free(outLenPtr, 4);
	if (response === null) {
		throw new ToolProtocolError("the signature-logo chunk could not read the request");
	}
	exports.selis_sign_logo_free(responsePtr, responseLength);
	return decodeSignLogoResponse(response);
}

/**
 * The typed DOM seam: the logo is the first file in the sign card's
 * `logo` input. A missing file is how "no logo selected" is expressed, so a
 * signer who never picks one never fetches this chunk.
 */
export function selectedLogoFile(input: HTMLInputElement | null): File | null {
	// Duck-typed rather than `instanceof`: the page hands us a real file input,
	// a test hands us the typed DOM seam's minimal `{ files }` shape, and both
	// must behave the same. A null input is "no logo selected".
	if (input === null || !("files" in input)) {
		return null;
	}
	return input.files?.[0] ?? null;
}

/** Read a selected logo into guest-bound bytes inside the tab. */
export async function readLogoBytes(file: File): Promise<Uint8Array> {
	return new Uint8Array(await file.arrayBuffer());
}

/** The rows the result card shows for a logo signature. */
export function signLogoRows(result: SignLogoResult): VerificationRow[] {
	const rows: VerificationRow[] = [];
	if (result.outcome.field !== undefined) {
		rows.push({ label: "Signature field", value: result.outcome.field });
	}
	if (result.outcome.page !== undefined) {
		rows.push({ label: "Page", value: String(result.outcome.page) });
	}
	rows.push({ label: "Logo", value: "decoded in this browser, embedded in the /AP" });
	rows.push({ label: "Appearance", value: "read back byte-for-byte and verified" });
	return rows;
}
