/**
 * Wire protocol for the compare WASM chunk: op 46, a two-input render-and-text
 * diff that returns a JSON report rather than a rewritten document.
 *
 * The request and response framing is IDENTICAL to `protocol.ts` — same magic
 * values, same little-endian layout, same verification JSON and per-output
 * report shape — so the encoder and decoder are shared rather than copied. Only
 * the module URL and the guest export prefix change. Op 46 is stable: persisted
 * references to it and the Rust guest agree on the same number.
 *
 * The page instantiates this module with an empty import object: compare needs
 * the rasteriser and the text ladder, but no clock, entropy or filesystem, and
 * both documents are supplied as in-memory keys `in/0` and `in/1`.
 */

import {
	ToolProtocolError,
	type ToolRequest,
	type ToolResult,
	decodeToolResponse,
	encodeToolRequest,
} from "./protocol.js";

/** The chunk the compare card fetches on first use. */
export const COMPARE_WASM_URL = "/wasm/selis_pdf_wasm_compare.wasm";

/** The guest exports this page uses. The module declares no imports. */
export interface CompareWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_compare_alloc(len: number): number;
		selis_compare_free(ptr: number, len: number): void;
		selis_compare_run(ptr: number, len: number, outLen: number): number;
		readonly selis_compare_chunk_id: (outLen: number) => number;
		readonly selis_compare_available: () => number;
	};
}

/**
 * Compile and instantiate the compare chunk. The import object is empty by
 * contract, exactly as for the tools and optimise chunks: an import would be a
 * capability the guest never needs and would break the no-upload guarantee.
 */
export async function instantiateCompare(
	bytes: BufferSource,
	imports: WebAssembly.Imports = {},
): Promise<CompareWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the compare chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, imports);
	return instance as unknown as CompareWasm;
}

/** Fetch, compile and instantiate the compare chunk. */
export async function loadCompare(
	fetchImpl: typeof fetch = fetch,
	url: string = COMPARE_WASM_URL,
): Promise<CompareWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the compare chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the compare chunk is unavailable (${response.status})`);
	}
	const bytes = await response.arrayBuffer();
	return instantiateCompare(bytes);
}

/**
 * Run one compare request synchronously against an instantiated chunk.
 *
 * The guest memory is copied in and out explicitly; no pointer or guest buffer
 * survives the call. The request and length buffers are freed on every exit
 * path, mirroring `runTool` byte for byte.
 */
export function runCompare(wasm: CompareWasm, request: ToolRequest): ToolResult {
	const encoded = encodeToolRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_compare_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the compare chunk is out of memory");
	}
	const outLenPtr = exports.selis_compare_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_compare_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the compare chunk is out of memory");
	}
	// Views must be created after both allocations: a later allocation can
	// grow memory and detach an earlier `ArrayBuffer`.
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_compare_run(requestPtr, encoded.byteLength, outLenPtr);
	if (responsePtr === 0) {
		exports.selis_compare_free(requestPtr, encoded.byteLength);
		exports.selis_compare_free(outLenPtr, 4);
		throw new ToolProtocolError("the compare chunk could not read the request");
	}
	const responseLen = new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response = new Uint8Array(exports.memory.buffer, responsePtr, responseLen).slice();
	exports.selis_compare_free(requestPtr, encoded.byteLength);
	exports.selis_compare_free(outLenPtr, 4);
	exports.selis_compare_free(responsePtr, responseLen);
	return decodeToolResponse(response);
}
