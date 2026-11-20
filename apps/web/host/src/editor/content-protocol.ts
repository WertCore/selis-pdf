/**
 * Client for the first-class content-editing WASM chunk (TEXTEDIT.05/06/07/08).
 *
 * The wire format is the tools protocol's, so the request/response codecs are
 * shared rather than copied: {@link encodeToolRequest} and
 * {@link decodeToolResponse} are the same bytes the tools chunk reads. Only the
 * module URL and the export prefix differ. The chunk is fetched on first use,
 * never on page load, and declares no imports — a host import is a hard error,
 * the same ADR-P0011 guarantee the tools chunk keeps.
 */

import {
	ToolProtocolError,
	decodeToolResponse,
	encodeToolRequest,
	type ToolRequest,
	type ToolResult,
} from "../tools/protocol.js";

/** Where the content chunk lives. Fetched on first use, never precached. */
export const CONTENT_WASM_URL = "/wasm/selis_pdf_wasm_content.wasm";

/** The exports this client uses. */
export interface ContentWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_content_alloc(len: number): number;
		selis_content_free(ptr: number, len: number): void;
		selis_content_run(ptr: number, len: number, outLen: number): number;
		readonly selis_content_capabilities: () => number;
		readonly selis_content_chunk_id: (outLen: number) => number;
	};
}

/** Compile and instantiate the chunk, refusing any host import. */
export async function instantiateContent(
	bytes: BufferSource,
	imports: WebAssembly.Imports = {},
): Promise<ContentWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the content chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	return (await WebAssembly.instantiate(module, imports)) as unknown as ContentWasm;
}

/** Fetch, compile and instantiate the content chunk. */
export async function loadContent(
	fetchImpl: typeof fetch = fetch,
	url: string = CONTENT_WASM_URL,
): Promise<ContentWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the content chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the content chunk is unavailable (${response.status})`);
	}
	return instantiateContent(await response.arrayBuffer());
}

/** Run one content op. The guest buffers are copied in and out every call. */
export function runContent(wasm: ContentWasm, request: ToolRequest): ToolResult {
	const encoded = encodeToolRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_content_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the content chunk is out of memory");
	}
	const outLenPtr = exports.selis_content_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_content_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the content chunk is out of memory");
	}
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_content_run(requestPtr, encoded.byteLength, outLenPtr);
	if (responsePtr === 0) {
		exports.selis_content_free(requestPtr, encoded.byteLength);
		exports.selis_content_free(outLenPtr, 4);
		throw new ToolProtocolError("the content chunk could not read the request");
	}
	const responseLen = new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response = new Uint8Array(exports.memory.buffer, responsePtr, responseLen).slice();
	exports.selis_content_free(requestPtr, encoded.byteLength);
	exports.selis_content_free(outLenPtr, 4);
	exports.selis_content_free(responsePtr, responseLen);
	return decodeToolResponse(response);
}
