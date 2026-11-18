/**
 * Wire protocol for the optimise WASM chunk: font subset (op 41) and
 * linearise (op 43), moved out of the tools chunk so the common tools page
 * does not download the subsetter or the linearisation writer.
 *
 * The request and response framing is IDENTICAL to `protocol.ts` — same magic
 * values, same little-endian layout, same verification JSON and per-output
 * report shape — so the encoder and decoder are shared rather than copied.
 * Only the module URL and the guest export prefix change. Op codes stay stable
 * (41 and 43), which is what persisted page state and the Rust guest agree on.
 */

import {
	ToolProtocolError,
	decodeToolResponse,
	encodeToolRequest,
	type ToolRequest,
	type ToolResult,
} from "./protocol.js";

/** The chunk the two optimise cards fetch on first use. */
export const OPTIMIZE_WASM_URL = "/wasm/selis_pdf_wasm_optimize.wasm";

/** The guest exports this page uses. The module declares no imports. */
export interface OptimizeWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_optimize_alloc(len: number): number;
		selis_optimize_free(ptr: number, len: number): void;
		selis_optimize_run(ptr: number, len: number, outLen: number): number;
		readonly selis_optimize_chunk_id: (outLen: number) => number;
		readonly selis_optimize_available: () => number;
	};
}

/**
 * Compile and instantiate the optimise chunk. The import object is empty by
 * contract, exactly as for the tools chunk: an import would be a capability
 * the guest never needs and would break the no-upload guarantee.
 */
export async function instantiateOptimize(
	bytes: BufferSource,
	imports: WebAssembly.Imports = {},
): Promise<OptimizeWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the optimise chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, imports);
	return instance as unknown as OptimizeWasm;
}

/** Fetch, compile and instantiate the optimise chunk. */
export async function loadOptimize(
	fetchImpl: typeof fetch = fetch,
	url: string = OPTIMIZE_WASM_URL,
): Promise<OptimizeWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the optimise chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the optimise chunk is unavailable (${response.status})`);
	}
	const bytes = await response.arrayBuffer();
	return instantiateOptimize(bytes);
}

/**
 * Run one optimise operation synchronously against an instantiated chunk.
 *
 * The guest memory is copied in and out explicitly; no pointer or guest buffer
 * survives the call. The request and length buffers are freed on every exit
 * path, mirroring `runTool` byte for byte.
 */
export function runOptimize(wasm: OptimizeWasm, request: ToolRequest): ToolResult {
	const encoded = encodeToolRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_optimize_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the optimise chunk is out of memory");
	}
	const outLenPtr = exports.selis_optimize_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_optimize_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the optimise chunk is out of memory");
	}
	// Views must be created after both allocations: a later allocation can
	// grow memory and detach an earlier `ArrayBuffer`.
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_optimize_run(requestPtr, encoded.byteLength, outLenPtr);
	if (responsePtr === 0) {
		exports.selis_optimize_free(requestPtr, encoded.byteLength);
		exports.selis_optimize_free(outLenPtr, 4);
		throw new ToolProtocolError("the optimise chunk could not read the request");
	}
	const responseLen = new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response = new Uint8Array(exports.memory.buffer, responsePtr, responseLen).slice();
	exports.selis_optimize_free(requestPtr, encoded.byteLength);
	exports.selis_optimize_free(outLenPtr, 4);
	exports.selis_optimize_free(responsePtr, responseLen);
	return decodeToolResponse(response);
}
