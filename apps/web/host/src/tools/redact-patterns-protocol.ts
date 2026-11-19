/**
 * Wire protocol for the pattern-redaction WASM chunk: op 47, with two separate
 * phases in the options block — `scan` (read-only, masked review JSON out) and
 * `apply` (only the explicitly approved candidate ids, redacted PDF out).
 *
 * The request/response framing is IDENTICAL to `protocol.ts` — same magic
 * values, little-endian layout, verification JSON and per-output report shape
 * — so the encoder and decoder are shared rather than copied. Only the module
 * URL and the guest export prefix change. The chunk declares no imports, and
 * the loader rejects one before instantiation: no clock, entropy or file
 * system reaches a chunk that is handed the user's document bytes.
 *
 * There is deliberately no `scanAndApply` helper here. The only way to get an
 * apply request is to take the ids a reviewer actually checked and pass them
 * explicitly; the guest refuses an empty or unknown approval set.
 */

import {
	ToolProtocolError,
	type ToolRequest,
	type ToolResult,
	decodeToolResponse,
	encodeToolRequest,
} from "./protocol.js";

/** The chunk the Redact patterns card fetches on first scan. */
export const REDACT_PATTERNS_WASM_URL = "/wasm/selis_pdf_wasm_redact_patterns.wasm";

/** The guest exports this page uses. The module declares no imports. */
export interface RedactPatternsWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_redact_patterns_alloc(len: number): number;
		selis_redact_patterns_free(ptr: number, len: number): void;
		selis_redact_patterns_run(ptr: number, len: number, outLen: number): number;
		readonly selis_redact_patterns_chunk_id: (outLen: number) => number;
		readonly selis_redact_patterns_available: () => number;
	};
}

/** Compile and instantiate the chunk with an empty import object. */
export async function instantiateRedactPatterns(
	bytes: BufferSource,
	imports: WebAssembly.Imports = {},
): Promise<RedactPatternsWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the pattern-redaction chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, imports);
	return instance as unknown as RedactPatternsWasm;
}

/** Fetch, compile and instantiate the pattern-redaction chunk. */
export async function loadRedactPatterns(
	fetchImpl: typeof fetch = fetch,
	url: string = REDACT_PATTERNS_WASM_URL,
): Promise<RedactPatternsWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the pattern-redaction chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the pattern-redaction chunk is unavailable (${response.status})`);
	}
	const bytes = await response.arrayBuffer();
	return instantiateRedactPatterns(bytes);
}

/**
 * Run one scan or apply request synchronously against an instantiated chunk.
 *
 * Guest memory is copied in and out explicitly; no pointer or guest buffer
 * survives the call. The request and length buffers are freed on every exit
 * path, mirroring `runTool` byte for byte.
 */
export function runRedactPatterns(wasm: RedactPatternsWasm, request: ToolRequest): ToolResult {
	const encoded = encodeToolRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_redact_patterns_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the pattern-redaction chunk is out of memory");
	}
	const outLenPtr = exports.selis_redact_patterns_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_redact_patterns_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the pattern-redaction chunk is out of memory");
	}
	// Views must be created after both allocations: a later allocation can
	// grow memory and detach an earlier `ArrayBuffer`.
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_redact_patterns_run(requestPtr, encoded.byteLength, outLenPtr);
	if (responsePtr === 0) {
		exports.selis_redact_patterns_free(requestPtr, encoded.byteLength);
		exports.selis_redact_patterns_free(outLenPtr, 4);
		throw new ToolProtocolError("the pattern-redaction chunk could not read the request");
	}
	const responseLen = new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response = new Uint8Array(exports.memory.buffer, responsePtr, responseLen).slice();
	exports.selis_redact_patterns_free(requestPtr, encoded.byteLength);
	exports.selis_redact_patterns_free(outLenPtr, 4);
	exports.selis_redact_patterns_free(responsePtr, responseLen);
	return decodeToolResponse(response);
}
