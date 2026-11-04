/**
 * Wire protocol for the lazy signing WASM chunk: visual stamps, certificate
 * signatures and signature reports (Phase 4 — sign PDF).
 *
 * The layout is the one documented in `crates/selis-pdf-wasm-sign/src/lib.rs`,
 * and it is the same envelope the security chunk speaks — magic, version,
 * operation, options JSON, then exactly one file — with one difference that
 * comes from what these operations are: the options may carry a PEM private
 * key, so the page zeroes the request buffer in guest memory as soon as the
 * call returns. The chunk never echoes a key back, and it has **no imports**,
 * so it cannot call out even if it wanted to.
 *
 * Two operations write one PDF; the report writes nothing and returns the
 * signature JSON instead.
 */

import type { ToolOutput, ToolOutputReport, Verification, VerificationRow } from "./protocol.js";
import { ToolProtocolError } from "./protocol.js";

/** The chunk the signing and certificate cards fetch on first use. */
export const SIGN_WASM_URL = "/wasm/selis_pdf_wasm_sign.wasm";

/** Request magic, `SLSG`. */
export const SIGN_REQUEST_MAGIC = 0x4753_4c53;
/** Response magic, `SLRG`. */
export const SIGN_RESPONSE_MAGIC = 0x4752_4c53;
/** Protocol version. */
export const SIGN_PROTOCOL_VERSION = 1;
/** The maximum input name length the chunk accepts. */
export const MAX_SIGN_NAME_BYTES = 128;
/** The maximum options JSON length: it carries a PEM key and its chain. */
export const MAX_SIGN_OPTIONS_BYTES = 256 * 1024;

/** The three operations this chunk implements. */
export type SignOp = "stamp" | "sign" | "report";

/**
 * `YYYYMMDDHHMMSSZ`, the UTC form the signing chunk writes as the signature's
 * `/M` (it prefixes the PDF's own `D:`). The page is the clock boundary: the
 * chunk never reads wall time itself.
 */
export function signingStamp(unixSeconds: number): string {
	if (!Number.isSafeInteger(unixSeconds) || unixSeconds < 0) {
		throw new ToolProtocolError("a signing time needs a whole number of Unix seconds");
	}
	const date = new Date(unixSeconds * 1000);
	const pad = (value: number, width = 2): string => String(value).padStart(width, "0");
	return `${pad(date.getUTCFullYear(), 4)}${pad(date.getUTCMonth() + 1)}${pad(
		date.getUTCDate(),
	)}${pad(date.getUTCHours())}${pad(date.getUTCMinutes())}${pad(date.getUTCSeconds())}Z`;
}

/** The wire operation number for each. */
const OP_CODE: Record<SignOp, number> = {
	stamp: 1,
	sign: 2,
	report: 3,
};

/** One requested operation over exactly one document. */
export interface SignRequest {
	readonly op: SignOp;
	readonly options?: Record<string, unknown>;
	readonly file: { readonly name: string; readonly bytes: Uint8Array };
}

/** What the chunk says it wrote, read back out of the produced bytes. */
export interface SignOutcome {
	readonly operation: string;
	/** Stamp only. */
	readonly page?: number | undefined;
	readonly lines?: number | undefined;
	readonly strokes?: number | undefined;
	/** Certificate sign only. */
	readonly field?: string | undefined;
	readonly subFilter?: string | undefined;
}

/** A successful file-producing run: one document and the words that describe it. */
export interface SignResult {
	readonly kind: "document";
	readonly verification: Verification;
	readonly report: ToolOutputReport;
	readonly output: { readonly name: string; readonly bytes: Uint8Array };
	readonly outcome: SignOutcome;
}

/** One certificate in a signature's chain, with the verdict for that link. */
export interface TrustLink {
	readonly subject: string;
	readonly issuer: string;
	readonly notBefore: number;
	readonly notAfter: number;
	readonly signatureOk: boolean;
	readonly timeOk: boolean;
	readonly anchor: boolean;
}

/** The chain verdict, which ends at the anchors the caller supplied. */
export interface TrustVerdict {
	readonly subject: string;
	readonly issuer: string;
	readonly trusted: boolean;
	readonly reason: string | null;
	/** Always false in this build: there is no network to fetch a revocation list. */
	readonly revocationChecked: boolean;
	readonly chain: readonly TrustLink[];
}

/** One signature dictionary, checked against the bytes it names. */
export interface SignatureEntry {
	readonly field: string;
	readonly page: number | null;
	readonly subFilter: string | null;
	readonly signer: string | null;
	readonly reason: string | null;
	readonly location: string | null;
	readonly signingTime: string | null;
	readonly byteRange: readonly number[] | null;
	readonly coversWholeDocument: boolean;
	readonly verified: boolean;
	readonly digest: string | null;
	readonly error: string | null;
	readonly trust: TrustVerdict | null;
}

/** The read-only signature report. */
export interface SignaturesReport {
	readonly status: string;
	readonly signatures: readonly SignatureEntry[];
	readonly unsignedFields: number;
	readonly revocationChecked: boolean;
	readonly anchorCount: number;
}

/** A successful report run: no output document, the JSON only. */
export interface SignatureReportResult {
	readonly kind: "report";
	readonly report: SignaturesReport;
}

export type SignResponse = SignResult | SignatureReportResult;

/** The guest exports this page uses. */
export interface SignWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_sign_alloc(len: number): number;
		selis_sign_free(ptr: number, len: number): void;
		selis_sign_run(ptr: number, len: number, outLen: number): number;
	};
}

function writeU32(target: Uint8Array, offset: number, value: number): number {
	target[offset] = value & 0xff;
	target[offset + 1] = (value >>> 8) & 0xff;
	target[offset + 2] = (value >>> 16) & 0xff;
	target[offset + 3] = (value >>> 24) & 0xff;
	return offset + 4;
}

/** Encode a request for `selis_sign_run`, refusing a malformed one before the guest sees it. */
export function encodeSignRequest(request: SignRequest): Uint8Array {
	const opCode = OP_CODE[request.op];
	if (opCode === undefined) {
		throw new ToolProtocolError(`unknown signing operation: ${String(request.op)}`);
	}
	const encoder = new TextEncoder();
	const name = encoder.encode(request.file.name);
	if (name.length === 0) {
		throw new ToolProtocolError("the document needs a name");
	}
	if (name.length > MAX_SIGN_NAME_BYTES) {
		throw new ToolProtocolError("the file name is too long");
	}
	if (request.file.bytes.length === 0) {
		throw new ToolProtocolError("the document is empty");
	}
	const options =
		request.options === undefined
			? new Uint8Array(0)
			: encoder.encode(JSON.stringify(request.options));
	if (options.length > MAX_SIGN_OPTIONS_BYTES) {
		throw new ToolProtocolError("signing options are too large");
	}

	const total = 16 + options.length + 4 + 4 + name.length + 4 + request.file.bytes.length;
	if (total > 0xffff_ffff) {
		throw new ToolProtocolError("the request exceeds the 4 GiB protocol limit");
	}
	const out = new Uint8Array(total);
	let offset = 0;
	offset = writeU32(out, offset, SIGN_REQUEST_MAGIC);
	offset = writeU32(out, offset, SIGN_PROTOCOL_VERSION);
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
		throw new ToolProtocolError("the signing response ended before its header");
	}
	const value = cursor.view.getUint32(cursor.pos, true);
	cursor.pos += 4;
	return value;
}

function readBytes(cursor: Cursor, where: string): Uint8Array {
	const length = readU32(cursor);
	const end = cursor.pos + length;
	if (end > cursor.view.byteLength) {
		throw new ToolProtocolError(`the signing response ended inside ${where}`);
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
		throw new ToolProtocolError(`the signing chunk returned a malformed ${field}`);
	}
	return value;
}

function optionalNumber(value: unknown, field: string): number | null {
	if (value === undefined || value === null) {
		return null;
	}
	if (typeof value !== "number" || !Number.isFinite(value)) {
		throw new ToolProtocolError(`the signing chunk returned a malformed ${field}`);
	}
	return value;
}

function optionalBoolean(value: unknown, field: string): boolean | null {
	if (value === undefined || value === null) {
		return null;
	}
	if (typeof value !== "boolean") {
		throw new ToolProtocolError(`the signing chunk returned a malformed ${field}`);
	}
	return value;
}

function trustLink(value: unknown): TrustLink {
	if (!isRecord(value)) {
		throw new ToolProtocolError("the signing chunk returned a malformed chain link");
	}
	const subject = optionalString(value.subject, "chain subject");
	const issuer = optionalString(value.issuer, "chain issuer");
	const notBefore = optionalNumber(value.notBefore, "chain notBefore");
	const notAfter = optionalNumber(value.notAfter, "chain notAfter");
	const signatureOk = optionalBoolean(value.signatureOk, "chain signatureOk");
	const timeOk = optionalBoolean(value.timeOk, "chain timeOk");
	const anchor = optionalBoolean(value.anchor, "chain anchor");
	if (
		subject === null ||
		issuer === null ||
		notBefore === null ||
		notAfter === null ||
		signatureOk === null ||
		timeOk === null ||
		anchor === null
	) {
		throw new ToolProtocolError("the signing chunk returned an incomplete chain link");
	}
	return { subject, issuer, notBefore, notAfter, signatureOk, timeOk, anchor };
}

function trustVerdict(value: unknown): TrustVerdict | null {
	if (value === undefined || value === null) {
		return null;
	}
	if (!isRecord(value) || !Array.isArray(value.chain)) {
		throw new ToolProtocolError("the signing chunk returned a malformed trust verdict");
	}
	const subject = optionalString(value.subject, "trust subject");
	const issuer = optionalString(value.issuer, "trust issuer");
	const trusted = optionalBoolean(value.trusted, "trust verdict");
	const revocationChecked = optionalBoolean(value.revocationChecked, "revocation flag");
	if (subject === null || issuer === null || trusted === null || revocationChecked === null) {
		throw new ToolProtocolError("the signing chunk returned an incomplete trust verdict");
	}
	return {
		subject,
		issuer,
		trusted,
		reason: optionalString(value.reason, "trust reason"),
		revocationChecked,
		chain: value.chain.map(trustLink),
	};
}

function signatureEntry(value: unknown): SignatureEntry {
	if (!isRecord(value)) {
		throw new ToolProtocolError("the signing chunk returned a malformed signature entry");
	}
	const field = optionalString(value.field, "signature field");
	const covers = optionalBoolean(value.coversWholeDocument, "coverage flag");
	const verified = optionalBoolean(value.verified, "verified flag");
	if (field === null || covers === null || verified === null) {
		throw new ToolProtocolError("the signing chunk returned an incomplete signature entry");
	}
	const byteRange = value.byteRange;
	if (
		byteRange !== undefined &&
		byteRange !== null &&
		(!Array.isArray(byteRange) || byteRange.some((item) => typeof item !== "number"))
	) {
		throw new ToolProtocolError("the signing chunk returned a malformed byte range");
	}
	return {
		field,
		page: optionalNumber(value.page, "signature page"),
		subFilter: optionalString(value.subFilter, "sub-filter"),
		signer: optionalString(value.signer, "signer"),
		reason: optionalString(value.reason, "reason"),
		location: optionalString(value.location, "location"),
		signingTime: optionalString(value.signingTime, "signing time"),
		byteRange: Array.isArray(byteRange) ? (byteRange as number[]) : null,
		coversWholeDocument: covers,
		verified,
		digest: optionalString(value.digest, "digest"),
		error: optionalString(value.error, "error"),
		trust: trustVerdict(value.trust),
	};
}

/**
 * Decode one response. The bytes are already copies, so nothing here survives a
 * later guest allocation.
 */
export function decodeSignResponse(response: Uint8Array): SignResponse {
	const cursor: Cursor = {
		view: new DataView(response.buffer, response.byteOffset, response.byteLength),
		pos: 0,
	};
	if (readU32(cursor) !== SIGN_RESPONSE_MAGIC) {
		throw new ToolProtocolError("the signing chunk returned an unexpected response");
	}
	const status = readU32(cursor);
	const message = new TextDecoder().decode(readBytes(cursor, "its message"));
	if (status !== 0) {
		// A refusal writes its reason here and no document, so this check has to
		// come before the output-count rule — otherwise the honest text ("this
		// key does not match the certificate") is replaced by a protocol
		// complaint.
		throw new ToolProtocolError(message === "" ? "the signing operation failed" : message);
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(message);
	} catch {
		throw new ToolProtocolError("the signing chunk returned malformed result data");
	}
	if (!isRecord(parsed)) {
		throw new ToolProtocolError("the signing chunk returned malformed result data");
	}
	const outputCount = readU32(cursor);

	if (outputCount === 0) {
		if (cursor.pos !== response.byteLength) {
			throw new ToolProtocolError("the signing response has trailing bytes");
		}
		const signatures = parsed.signatures;
		const statusText = parsed.status;
		const revocation = parsed.revocationChecked;
		const unsigned = parsed.unsignedFields;
		if (
			!Array.isArray(signatures) ||
			typeof statusText !== "string" ||
			typeof revocation !== "boolean" ||
			typeof unsigned !== "number"
		) {
			throw new ToolProtocolError("the signing chunk returned a malformed signature report");
		}
		return {
			kind: "report",
			report: {
				status: statusText,
				signatures: signatures.map(signatureEntry),
				unsignedFields: unsigned,
				revocationChecked: revocation,
				anchorCount: optionalNumber(parsed.anchorCount, "anchor count") ?? 0,
			},
		};
	}
	if (outputCount !== 1) {
		throw new ToolProtocolError("a signing operation returns at most one document");
	}
	const name = new TextDecoder().decode(readBytes(cursor, "an output name"));
	const bytes = readBytes(cursor, "an output");
	if (cursor.pos !== response.byteLength) {
		throw new ToolProtocolError("the signing response has trailing bytes");
	}
	const reports = parsed.outputs;
	if (!Array.isArray(reports) || reports.length !== 1 || !isRecord(reports[0])) {
		throw new ToolProtocolError("the signing chunk returned mismatched output metadata");
	}
	const report = reports[0];
	if (report.name !== name || report.kind !== "pdf") {
		throw new ToolProtocolError("the signing chunk described a different output than it wrote");
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
		throw new ToolProtocolError("the signing chunk returned malformed output metadata");
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
	const outcome: SignOutcome = {
		operation: typeof parsed.operation === "string" ? parsed.operation : "sign",
		page: optionalNumber(parsed.page, "page") ?? undefined,
		lines: optionalNumber(parsed.lines, "line count") ?? undefined,
		strokes: optionalNumber(parsed.strokes, "stroke count") ?? undefined,
		field: optionalString(parsed.field, "field") ?? undefined,
		subFilter: optionalString(parsed.subFilter, "sub-filter") ?? undefined,
	};
	return { kind: "document", verification, report: typedReport, output, outcome };
}

/**
 * Compile and instantiate the signing chunk.
 *
 * The import object is empty **by contract**: a clock, entropy, filesystem or
 * network import would be a capability this chunk never needs and a hole in the
 * no-upload promise, so a module that declares one is rejected before it runs.
 */
export async function instantiateSign(bytes: BufferSource): Promise<SignWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the signing chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, {});
	return instance as unknown as SignWasm;
}

/** Fetch, compile, and instantiate the signing chunk. */
export async function loadSign(
	fetchImpl: typeof fetch = fetch,
	url: string = SIGN_WASM_URL,
): Promise<SignWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the signing chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the signing chunk is unavailable (${response.status})`);
	}
	const bytes = await response.arrayBuffer();
	return instantiateSign(bytes);
}

/**
 * Run one operation. The request buffer holds the user's private key when one is
 * present, so it is zeroed before it is freed: `free` only releases the
 * allocation, and the bytes would otherwise stay readable in the chunk's linear
 * memory.
 */
export function runSign(wasm: SignWasm, request: SignRequest): SignResponse {
	const encoded = encodeSignRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_sign_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the signing chunk is out of memory");
	}
	const outLenPtr = exports.selis_sign_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_sign_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the signing chunk is out of memory");
	}
	// Views are created after both allocations: a later allocation can grow
	// memory and detach an earlier `ArrayBuffer`.
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_sign_run(requestPtr, encoded.byteLength, outLenPtr);
	const responseLength =
		responsePtr === 0 ? 0 : new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response =
		responsePtr === 0
			? null
			: new Uint8Array(exports.memory.buffer, responsePtr, responseLength).slice();
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).fill(0);
	exports.selis_sign_free(requestPtr, encoded.byteLength);
	exports.selis_sign_free(outLenPtr, 4);
	if (response === null) {
		throw new ToolProtocolError("the signing chunk could not read the request");
	}
	exports.selis_sign_free(responsePtr, responseLength);
	return decodeSignResponse(response);
}

/**
 * The rows the result card shows for a stamp or signature. These are read out
 * of the produced document's verification, not echoed from the form.
 */
export function signRows(result: SignResult): VerificationRow[] {
	const { outcome } = result;
	const rows: VerificationRow[] = [];
	if (outcome.page !== undefined) {
		rows.push({ label: "Page", value: String(outcome.page) });
	}
	if (outcome.lines !== undefined || outcome.strokes !== undefined) {
		rows.push({
			label: "Stamp",
			value: `${outcome.lines ?? 0} line(s), ${outcome.strokes ?? 0} stroke(s)`,
		});
	}
	if (outcome.field !== undefined) {
		rows.push({ label: "Signature field", value: outcome.field });
	}
	if (outcome.subFilter !== undefined) {
		rows.push({ label: "Sub-filter", value: outcome.subFilter });
	}
	return rows;
}

/**
 * The rows the signature report shows, one signature at a time. A signature
 * that did not verify is shown with its error rather than hidden: the point of
 * the report is to disclose, and "modified after signing" is the disclosure.
 */
export function signatureRows(report: SignaturesReport): VerificationRow[] {
	const rows: VerificationRow[] = [];
	if (report.signatures.length === 0) {
		rows.push({
			label: "Signatures",
			value:
				report.unsignedFields === 0
					? "none — this document carries no signature"
					: `none signed; ${report.unsignedFields} unsigned signature field(s)`,
		});
	}
	for (const [index, signature] of report.signatures.entries()) {
		const number = index + 1;
		rows.push({
			label: `Signature ${number}`,
			value: `${signature.field}: ${signature.verified ? "verified" : "NOT verified"}`,
		});
		if (signature.signer !== null) {
			rows.push({ label: `Signer ${number}`, value: signature.signer });
		}
		rows.push({
			label: `Coverage ${number}`,
			value: signature.coversWholeDocument
				? "the whole document"
				: "part of the document — later bytes are not covered",
		});
		if (signature.error !== null) {
			rows.push({ label: `Problem ${number}`, value: signature.error });
		}
		if (signature.trust !== null) {
			const trust = signature.trust;
			rows.push({
				label: `Trust ${number}`,
				value: trust.trusted
					? `trusted — the chain ends at a supplied anchor (${trust.subject})`
					: `NOT trusted — ${trust.reason ?? "the chain does not end at a supplied anchor"}`,
			});
		}
	}
	rows.push({
		label: "Revocation",
		value: report.revocationChecked
			? "checked"
			: "not checked — this build has no network, so no CRL or OCSP response was fetched",
	});
	return rows;
}
