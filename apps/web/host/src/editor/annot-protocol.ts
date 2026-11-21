/**
 * Client for the dedicated annotation WASM chunk (SL-5.ANNOT.02/03).
 *
 * The wire format is the tools protocol's, so the request/response codecs are
 * shared rather than copied: {@link encodeToolRequest} and
 * {@link decodeToolResponse} are the same bytes the tools chunk reads. Only the
 * module URL and the export prefix differ. The chunk is fetched on first use,
 * never on page load, and declares no imports — a host import is a hard error,
 * the same ADR-P0011 guarantee every other chunk keeps.
 *
 * The chunk adds one field to the verification object: `annotation`, the typed
 * report of what the engine actually did (how many `/AP` streams it rebuilt and
 * whether `/NeedAppearances` was left set; or how many strokes went into the
 * `/InkList`). {@link runAnnot} reads it defensively so a malformed guest
 * report is a typed error rather than a broken status line.
 */

import {
	ToolProtocolError,
	decodeToolResponse,
	encodeToolRequest,
	type ToolRequest,
	type ToolResult,
	type Verification,
} from "../tools/protocol.js";

/** Where the annotation chunk lives. Fetched on first use, never precached. */
export const ANNOT_WASM_URL = "/wasm/selis_pdf_wasm_annot.wasm";

/** What the annotation engine reports, beyond the shared verification. */
export type AnnotReport =
	| {
			readonly kind: "appearances";
			readonly pages: number;
			readonly annotations: number;
			readonly rebuilt: number;
			readonly need_appearances: boolean;
	  }
	| {
			readonly kind: "ink";
			readonly page: number;
			readonly strokes: number;
			readonly annots: number;
	  };

/** A successful annotation run, with the typed engine report attached. */
export interface AnnotResult extends ToolResult {
	readonly report: AnnotReport;
}

/** The exports this client uses. */
export interface AnnotWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_annot_alloc(len: number): number;
		selis_annot_free(ptr: number, len: number): void;
		selis_annot_run(ptr: number, len: number, outLen: number): number;
		readonly selis_annot_capabilities: () => number;
		readonly selis_annot_chunk_id: (outLen: number) => number;
	};
}

/** Compile and instantiate the chunk, refusing any host import. */
export async function instantiateAnnot(
	bytes: BufferSource,
	imports: WebAssembly.Imports = {},
): Promise<AnnotWasm> {
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new ToolProtocolError(
			`the annotation chunk has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	return (await WebAssembly.instantiate(module, imports)) as unknown as AnnotWasm;
}

/** Fetch, compile and instantiate the annotation chunk. */
export async function loadAnnot(
	fetchImpl: typeof fetch = fetch,
	url: string = ANNOT_WASM_URL,
): Promise<AnnotWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new ToolProtocolError("the annotation chunk could not be downloaded");
	}
	if (!response.ok) {
		throw new ToolProtocolError(`the annotation chunk is unavailable (${response.status})`);
	}
	return instantiateAnnot(await response.arrayBuffer());
}

function integer(value: unknown, field: string): number {
	if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
		throw new ToolProtocolError(`the annotation report has a malformed ${field}`);
	}
	return value;
}

/** Read the guest's `annotation` report defensively. */
export function readAnnotReport(verification: Verification): AnnotReport {
	const raw = (verification as unknown as Record<string, unknown>).annotation;
	if (typeof raw !== "object" || raw === null) {
		throw new ToolProtocolError("the annotation chunk returned no report");
	}
	const record = raw as Record<string, unknown>;
	if (record.kind === "appearances") {
		if (typeof record.need_appearances !== "boolean") {
			throw new ToolProtocolError("the annotation report has no /NeedAppearances flag");
		}
		return {
			kind: "appearances",
			pages: integer(record.pages, "page count"),
			annotations: integer(record.annotations, "annotation count"),
			rebuilt: integer(record.rebuilt, "rebuilt count"),
			need_appearances: record.need_appearances,
		};
	}
	if (record.kind === "ink") {
		return {
			kind: "ink",
			page: integer(record.page, "page"),
			strokes: integer(record.strokes, "stroke count"),
			annots: integer(record.annots, "annotation count"),
		};
	}
	throw new ToolProtocolError("the annotation chunk returned an unknown report");
}

/** Run one annotation op. The guest buffers are copied in and out every call. */
export function runAnnot(wasm: AnnotWasm, request: ToolRequest): AnnotResult {
	const encoded = encodeToolRequest(request);
	const { exports } = wasm;
	const requestPtr = exports.selis_annot_alloc(encoded.byteLength);
	if (requestPtr === 0) {
		throw new ToolProtocolError("the annotation chunk is out of memory");
	}
	const outLenPtr = exports.selis_annot_alloc(4);
	if (outLenPtr === 0) {
		exports.selis_annot_free(requestPtr, encoded.byteLength);
		throw new ToolProtocolError("the annotation chunk is out of memory");
	}
	// Views are created after both allocations: a later allocation can grow
	// memory and detach an earlier `ArrayBuffer`.
	new Uint8Array(exports.memory.buffer, requestPtr, encoded.byteLength).set(encoded);
	const responsePtr = exports.selis_annot_run(requestPtr, encoded.byteLength, outLenPtr);
	if (responsePtr === 0) {
		exports.selis_annot_free(requestPtr, encoded.byteLength);
		exports.selis_annot_free(outLenPtr, 4);
		throw new ToolProtocolError("the annotation chunk could not read the request");
	}
	const responseLen = new DataView(exports.memory.buffer).getUint32(outLenPtr, true);
	const response = new Uint8Array(exports.memory.buffer, responsePtr, responseLen).slice();
	exports.selis_annot_free(requestPtr, encoded.byteLength);
	exports.selis_annot_free(outLenPtr, 4);
	exports.selis_annot_free(responsePtr, responseLen);
	const result = decodeToolResponse(response);
	return { ...result, report: readAnnotReport(result.verification) };
}
