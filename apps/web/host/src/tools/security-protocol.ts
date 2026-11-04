/**
 * Wire protocol for the lazy security WASM chunk: remove a password, add one,
 * and clear permission restrictions (SL-1A rows 1-4).
 *
 * The byte layout is the one documented in
 * `crates/selis-pdf-wasm-security/src/lib.rs`, and it is the same envelope the
 * tools chunk speaks — magic, version, operation, options JSON, then the files —
 * with two differences that come from what these operations are. There is
 * exactly one document: a password operation on a batch of files is a caller
 * mistake, not a smaller queue. And the request carries secrets, so the page
 * zeroes the request buffer in guest memory as soon as the call returns.
 *
 * The chunk has **no imports**, which is what makes the "keys stay on device"
 * claim mechanical rather than aspirational: it cannot call out, and the page
 * refuses to instantiate a build that can. Because it cannot ask for entropy
 * either, `protect` needs the 64 bytes of key material injected here, from
 * `crypto.getRandomValues`.
 */

import type { ToolOutput, ToolOutputReport, Verification, VerificationRow } from "./protocol.js";
import { ToolProtocolError } from "./protocol.js";

/** The chunk the password cards fetch on first use. */
export const SECURITY_WASM_URL = "/wasm/selis_pdf_wasm_security.wasm";

/** Request magic, `SLSS`. */
export const SECURITY_REQUEST_MAGIC = 0x5353_4c53;
/** Response magic, `SLRS`. */
export const SECURITY_RESPONSE_MAGIC = 0x5352_4c53;
/** Protocol version. */
export const SECURITY_PROTOCOL_VERSION = 1;
/** The maximum input name length the chunk accepts. */
export const MAX_SECURITY_NAME_BYTES = 128;
/** The maximum options JSON length the chunk accepts. */
export const MAX_SECURITY_OPTIONS_BYTES = 64 * 1024;
/** The key material one protection needs: a 256-bit file key and four 8-byte salts. */
export const KEY_MATERIAL_BYTES = 64;
/** The same material as the hex text the wire carries. */
export const KEY_MATERIAL_HEX_CHARS = KEY_MATERIAL_BYTES * 2;

/** The three operations this chunk implements. */
export type SecurityOp = "unlock" | "protect" | "clear_permissions";

/** The wire operation number for each. */
const OP_CODE: Record<SecurityOp, number> = {
	unlock: 1,
	protect: 2,
	clear_permissions: 3,
};

/** One requested operation over exactly one document. */
export interface SecurityRequest {
	readonly op: SecurityOp;
	readonly options?: Record<string, unknown>;
	readonly file: { readonly name: string; readonly bytes: Uint8Array };
}

/** What the chunk says the protection it just wrote (or removed) amounts to. */
export interface SecurityOutcome {
	readonly operation: string;
	/** Unlock only: whether the input carried an `/Encrypt` at all. */
	readonly encrypted?: boolean | undefined;
	/** Protect only. */
	readonly algorithm?: string | undefined;
	readonly granted?: readonly string[] | undefined;
	readonly denied?: readonly string[] | undefined;
	readonly encryptMetadata?: boolean | undefined;
	/** Clear permissions only. */
	readonly previouslyDenied?: readonly string[] | undefined;
	readonly summary?: string | undefined;
}

/** A successful security run: one document and the words that describe it. */
export interface SecurityResult {
	readonly verification: Verification;
	readonly report: ToolOutputReport;
	readonly output: { readonly name: string; readonly bytes: Uint8Array };
	readonly outcome: SecurityOutcome;
}

/** The guest exports this page uses. */
export interface SecurityWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_security_alloc(len: number): number;
		selis_security_free(ptr: number, len: number): void;
		selis_security_run(ptr: number, len: number, outLen: number): number;
	};
}

function writeU32(target: Uint8Array, offset: number, value: number): number {
	target[offset] = value & 0xff;
	target[offset + 1] = (value >>> 8) & 0xff;
	target[offset + 2] = (value >>> 16) & 0xff;
	target[offset + 3] = (value >>> 24) & 0xff;
	return offset + 4;
}

/** Encode a request for `selis_security_run`, refusing a malformed one before the guest sees it. */
export function encodeSecurityRequest(request: SecurityRequest): Uint8Array {
	const opCode = OP_CODE[request.op];
	if (opCode === undefined) {
		throw new ToolProtocolError(`unknown security operation: ${String(request.op)}`);
	}
	const encoder = new TextEncoder();
	const name = encoder.encode(request.file.name);
	if (name.length === 0) {
		throw new ToolProtocolError("the document needs a name");
	}
	if (name.length > MAX_SECURITY_NAME_BYTES) {
		throw new ToolProtocolError("the file name is too long");
	}
	if (request.file.bytes.length === 0) {
		throw new ToolProtocolError("the document is empty");
	}
	const options =
		request.options === undefined
			? new Uint8Array(0)
			: encoder.encode(JSON.stringify(request.options));
	if (options.length > MAX_SECURITY_OPTIONS_BYTES) {
		throw new ToolProtocolError("security options are too large");
	}

	const total = 16 + options.length + 4 + 4 + name.length + 4 + request.file.bytes.length;
	if (total > 0xffff_ffff) {
		throw new ToolProtocolError("the request exceeds the 4 GiB protocol limit");
	}
	const out = new Uint8Array(total);
	let offset = 0;
	offset = writeU32(out, offset, SECURITY_REQUEST_MAGIC);
	offset = writeU32(out, offset, SECURITY_PROTOCOL_VERSION);
	offset = writeU32(out, offset, opCode);
	offset = writeU32(out, offset, options.length);
	out.set(options, offset);
	offset += options.length;
	// Exactly one document, always.
	offset = writeU32(out, offset, 1);
	offset = writeU32(out, offset, name.length);
	out.set(name, offset);
	offset += name.length;
	offset = writeU32(out, offset, request.file.bytes.length);
	out.set(request.file.bytes, offset);
	return out;
}

interface Cursor {
	readonly view: DataView;
	pos: number;
}

function readU32(cursor: Cursor): number {
	if (cursor.pos + 4 > cursor.view.byteLength) {
		throw new ToolProtocolError("the security response ended before its header");
	}
	const value = cursor.view.getUint32(cursor.pos, true);
	cursor.pos += 4;
	return value;
}

function readBytes(cursor: Cursor, where: string): Uint8Array {
	const length = readU32(cursor);
	const end = cursor.pos + length;
	if (end > cursor.view.byteLength) {
		throw new ToolProtocolError(`the security response ended inside ${where}`);
	}
	const bytes = new Uint8Array(cursor.view.buffer, cursor.view.byteOffset + cursor.pos, length);
	cursor.pos = end;
	return bytes.slice();
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null;
}

function stringArray(value: unknown, field: string): readonly string[] | undefined {
	if (value === undefined) {
		return undefined;
	}
	if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
		throw new ToolProtocolError(`the security chunk returned a malformed ${field} list`);
	}
	return value as string[];
}

/**
 * Decode one response. The bytes are already copies, so nothing here survives a
 * later guest allocation.
 */
export function decodeSecurityResponse(response: Uint8Array): SecurityResult {
	const cursor: Cursor = {
		view: new DataView(response.buffer, response.byteOffset, response.byteLength),
		pos: 0,
	};
	if (readU32(cursor) !== SECURITY_RESPONSE_MAGIC) {
		throw new ToolProtocolError("the security chunk returned an unexpected response");
	}
	const status = readU32(cursor);
	const message = new TextDecoder().decode(readBytes(cursor, "its message"));
	if (status !== 0) {
		// A refusal writes its reason here and no document, so this check has to
		// come before the "exactly one output" rule — otherwise the honest text
		// ("the password is wrong") is replaced by a protocol complaint. The
		// chunk's refusals never repeat a password, so this is safe verbatim.
		throw new ToolProtocolError(message === "" ? "the security operation failed" : message);
	}
	const outputCount = readU32(cursor);
	if (outputCount !== 1) {
		throw new ToolProtocolError("a security operation returns exactly one document");
	}
	const name = new TextDecoder().decode(readBytes(cursor, "an output name"));
	const bytes = readBytes(cursor, "an output");
	if (cursor.pos !== response.byteLength) {
		throw new ToolProtocolError("the security response has trailing bytes");
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(message);
	} catch {
		throw new ToolProtocolError("the security chunk returned malformed verification data");
	}
	if (!isRecord(parsed)) {
		throw new ToolProtocolError("the security chunk returned malformed verification data");
	}
	const reports = parsed.outputs;
	if (!Array.isArray(reports) || reports.length !== 1 || !isRecord(reports[0])) {
		throw new ToolProtocolError("the security chunk returned mismatched output metadata");
	}
	const report = reports[0];
	if (report.name !== name || report.kind !== "pdf") {
		throw new ToolProtocolError("the security chunk described a different output than it wrote");
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
		throw new ToolProtocolError("the security chunk returned malformed output metadata");
	}
	const verification = parsed as unknown as Verification;
	// The report's byte count is checked against the bytes themselves, not taken
	// on trust: a chunk that claims a size it did not write is a bug, not a hint.
	const output: ToolOutput = { name, bytes };
	const typedReport: ToolOutputReport = {
		name,
		kind: "pdf",
		pages,
		bytes: bytes.length,
		verdict,
		overTarget: false,
	};
	const outcome: SecurityOutcome = {
		operation: typeof parsed.operation === "string" ? parsed.operation : "security",
		encrypted: typeof parsed.encrypted === "boolean" ? parsed.encrypted : undefined,
		algorithm: typeof parsed.algorithm === "string" ? parsed.algorithm : undefined,
		granted: stringArray(parsed.granted, "granted"),
		denied: stringArray(parsed.denied, "denied"),
		encryptMetadata:
			typeof parsed.encryptMetadata === "boolean" ? parsed.encryptMetadata : undefined,
		previouslyDenied: stringArray(parsed.previouslyDenied, "previously denied"),
		summary: typeof parsed.summary === "string" ? parsed.summary : undefined,
	};
	return { verification, report: typedReport, output, outcome };
}

/**
 * Compile and instantiate the security chunk.
 *
 * The import object is empty **by contract**: a clock, entropy, filesystem or
 * network import would be a capability this chunk never needs and a hole in the
 * no-upload promise, so a module that declares one is rejected before it runs.
 */
export async function instantiateSecurity(bytes: BufferSource): Promise<SecurityWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the security chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, {});
	return instance as unknown as SecurityWasm;
}

/** Fetch, compile, and instantiate the security chunk. */
export async function loadSecurity(
	fetchImpl: typeof fetch = fetch,
	url: string = SECURITY_WASM_URL,
): Promise<SecurityWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the security chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the security chunk is unavailable (${response.status})`);
	}
	const bytes = await response.arrayBuffer();
	return instantiateSecurity(bytes);
}

/**
 * Run one operation. The request buffer holds the user's password, so it is
 * zeroed before it is freed: `free` only releases the allocation, and the bytes
 * would otherwise stay readable in the chunk's linear memory.
 */
export function runSecurity(wasm: SecurityWasm, request: SecurityRequest): SecurityResult {
	const encoded = encodeSecurityRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_security_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the security chunk is out of memory");
	}
	const outLenPtr = exports.selis_security_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_security_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the security chunk is out of memory");
	}
	// Views are created after both allocations: a later allocation can grow
	// memory and detach an earlier `ArrayBuffer`.
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_security_run(requestPtr, encoded.byteLength, outLenPtr);
	const responseLength =
		responsePtr === 0 ? 0 : new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response =
		responsePtr === 0
			? null
			: new Uint8Array(exports.memory.buffer, responsePtr, responseLength).slice();
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).fill(0);
	exports.selis_security_free(requestPtr, encoded.byteLength);
	exports.selis_security_free(outLenPtr, 4);
	if (response === null) {
		throw new ToolProtocolError("the security chunk could not read the request");
	}
	exports.selis_security_free(responsePtr, responseLength);
	return decodeSecurityResponse(response);
}

/**
 * The rows the result card shows for what the operation did to the protection.
 * These are read out of the flag the writer wrote, not echoed from the form: a
 * document that was never encrypted says so rather than claiming a removal.
 */
export function securityRows(result: SecurityResult): VerificationRow[] {
	const { outcome } = result;
	const rows: VerificationRow[] = [];
	if (outcome.encrypted !== undefined) {
		rows.push({
			label: "Password",
			value: outcome.encrypted
				? "Removed — the output opens without one and carries no /Encrypt"
				: "Nothing to remove — this PDF was not password-protected, so its bytes were copied",
		});
	}
	if (outcome.algorithm !== undefined) {
		rows.push({ label: "Encryption", value: outcome.algorithm });
	}
	if (outcome.granted !== undefined) {
		rows.push({
			label: "Granted",
			value: outcome.granted.length === 0 ? "none" : outcome.granted.join(", "),
		});
	}
	if (outcome.denied !== undefined) {
		rows.push({
			label: "Restricted",
			value: outcome.denied.length === 0 ? "none" : outcome.denied.join(", "),
		});
	}
	if (outcome.encryptMetadata !== undefined) {
		rows.push({
			label: "Document metadata",
			value: outcome.encryptMetadata ? "encrypted" : "left in the clear",
		});
	}
	if (outcome.previouslyDenied !== undefined) {
		rows.push({
			label: "Now granted",
			value:
				outcome.previouslyDenied.length === 0
					? "nothing — no permission was restricted before"
					: outcome.previouslyDenied.join(", "),
		});
	}
	if (outcome.summary !== undefined) {
		rows.push({ label: "How", value: outcome.summary });
	}
	return rows;
}

/**
 * The page's own entropy: 64 random bytes as 128 hex characters, drawn from the
 * browser and handed to a guest that cannot ask for any. The source array is
 * zeroed once the text exists.
 */
export function randomKeyMaterial(): string {
	const source = new Uint8Array(KEY_MATERIAL_BYTES);
	if (typeof globalThis.crypto?.getRandomValues !== "function") {
		throw new ToolProtocolError(
			"this browser offers no random source, and a protection key that is not random is not a key",
		);
	}
	globalThis.crypto.getRandomValues(source);
	let text = "";
	for (const byte of source) {
		text += byte.toString(16).padStart(2, "0");
	}
	source.fill(0);
	if (text.length !== KEY_MATERIAL_HEX_CHARS) {
		throw new ToolProtocolError("the key material is not the length this operation needs");
	}
	return text;
}
