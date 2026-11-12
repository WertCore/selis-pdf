/**
 * The core engine, as a TypeScript client (SL-4.WASM.01).
 *
 * ## What this module is
 *
 * The editor needs the engine for two things the tools chunk cannot do:
 * **rendering** a page (`render`) and **reading** a page's text layer
 * (`textLayer`). Both are read-only ops on the versioned JSON protocol, and
 * this module is the client for them.
 *
 * ## The rules it keeps
 *
 * - **The guest allocates; the host frees.** Every buffer the engine hands
 *   back is copied out and released. The response block itself belongs to the
 *   guest and is NOT freed — freeing it is a double free — while the
 *   attachment (the rendered pixels) IS the host's.
 * - **Out-words are read after the dispatch.** The call can grow linear
 *   memory, which detaches every typed-array view taken beforehand, so the
 *   lengths are read from a fresh view over the grown buffer.
 * - **The import set is closed.** The engine is a wasm-bindgen build with four
 *   generated shims; an import outside that set throws instead of being
 *   quietly stubbed, so a future wasm-bindgen upgrade fails loudly here and
 *   not as a null dereference deep inside the engine.
 */

import type { Matrix } from "./geometry.js";

/** Where the engine chunk lives. */
export const ENGINE_WASM_URL = "/wasm/selis_pdf_wasm.wasm";

/** The exports this client uses. */
export interface EngineExports {
	readonly memory: WebAssembly.Memory;
	/** Allocate `size` bytes of guest memory; 0 when the guest refuses. */
	selis_input_alloc(size: number): number;
	/** Release a guest allocation. */
	selis_free(ptr: number, size: number): void;
	/** Run one protocol message; the out-block receives three u32 slots. */
	selis_dispatch(
		requestPtr: number,
		requestLen: number,
		payloadPtr: number,
		payloadLen: number,
		outPtr: number,
	): number;
}

/** The response envelope every op answers with. */
export interface WireResponse {
	readonly v: number;
	readonly id: number;
	/** `true` for a value response, `false` for a typed failure. */
	readonly ok?: boolean;
	/** The op's result object. */
	readonly value?: unknown;
	/** The registry error code, on a failure. */
	readonly code?: number;
	/** The registry's English message, on a failure. */
	readonly message?: string;
	/** Engine-owned failure context, on a failure. */
	readonly detail?: string;
}

/** One dispatch: the parsed reply and the binary attachment, if any. */
export interface EngineReply {
	readonly response: WireResponse;
	readonly attachment: Uint8Array | null;
}

/** A typed engine failure. */
export class EngineError extends Error {
	/** The registry's numeric code, when the guest reported one. */
	readonly code: number | null;
	/** The guest's own context, when it reported any. */
	readonly detail: string | null;

	constructor(message: string, code: number | null = null, detail: string | null = null) {
		super(message);
		this.name = "EngineError";
		this.code = code;
		this.detail = detail;
	}
}

/** The document the engine opened. */
export interface OpenedDocument {
	/** The document handle. */
	readonly doc: number;
	/** How many pages it has. */
	readonly pages: number;
	/** Each page's box, in points. */
	readonly pageSizes: readonly { readonly width: number; readonly height: number }[];
}

/** One rendered page: its pixels and the transform they were drawn with. */
export interface RenderedPage {
	/** The raster's width, in device pixels. */
	readonly width: number;
	/** The raster's height, in device pixels. */
	readonly height: number;
	/** The pixels, RGBA, row by row. */
	readonly pixels: Uint8Array;
	/** The user-space → device transform the render used. */
	readonly ctm: Matrix;
}

/** One line of a page's text layer. */
export interface TextLayerLine {
	/** The line's recovered text. */
	readonly text: string;
	/** The glyph box, in PDF user space. */
	readonly rect: {
		readonly x: number;
		readonly y: number;
		readonly width: number;
		readonly height: number;
	};
}

/** One place a search query was found. */
export interface SearchMatch {
	/** The zero-based page the match is on. */
	readonly page: number;
	/** The zero-based line the match is on. */
	readonly line: number;
	/** The text of the line the match is on. */
	readonly text: string;
}

/** What a search found. */
export interface SearchResults {
	/** The matches, in the order the engine found them. */
	readonly matches: readonly SearchMatch[];
	/** How many matches the document holds, which may exceed the list. */
	readonly total: number;
	/** Whether the list was cut short because the engine's cap was reached. */
	readonly truncated: boolean;
}

/** A page's text layer. */
/**
 * One node of the document's tag structure.
 *
 * The engine reports what the document says about itself; it never invents
 * a role, so a node with no title is a node with no title.
 */
export interface StructureNode {
	/** The node's role, as the document states it ("H1", "Figure", ...). */
	readonly role: string;
	/** The node's title, when the document carries one. */
	readonly title?: string;
	/** The node's alternative text, when the document carries one. */
	readonly alt?: string;
	/** The node's children, in reading order. */
	readonly children: readonly StructureNode[];
}

/**
 * What the document says about its own structure.
 *
 * An untagged document answers with `tagged: false` and NO root: "there is
 * nothing here" and "there is nothing navigable in here" are different
 * facts, and a shell that renders both as "no headings" has claimed a
 * measurement it never made.
 */
export interface Structure {
	readonly tagged: boolean;
	readonly root?: StructureNode;
}

export interface PageTextLayer {
	/** The zero-based page number. */
	readonly page: number;
	/** The page's width, in points. */
	readonly width: number;
	/** The page's height, in points. */
	readonly height: number;
	/** Whether text was drawn but nothing could be recovered. */
	readonly lowConfidence: boolean;
	/** The lines, in reading order. */
	readonly lines: readonly TextLayerLine[];
}

/**
 * Build the import object for the engine from the module's OWN import list.
 *
 * Each import is closed over: one outside the known set throws, so a future
 * wasm-bindgen upgrade that adds an import fails here instead of appearing
 * later as a null dereference inside the engine.
 */
export function importObjectFor(module: WebAssembly.Module): WebAssembly.Imports {
	const declared = WebAssembly.Module.imports(module);
	const imports: Record<string, Record<string, unknown>> = {};
	for (const entry of declared) {
		const bucket = imports[entry.module] ?? {};
		imports[entry.module] = bucket;
		if (entry.module === "__wbindgen_placeholder__") {
			if (entry.name === "__wbindgen_describe") {
				bucket[entry.name] = () => {};
			} else if (entry.name.startsWith("__wbg___wbindgen_throw")) {
				bucket[entry.name] = () => {
					throw new Error("the engine threw via the wasm-bindgen shim");
				};
			} else {
				throw new Error(`unexpected engine import ${entry.module}::${entry.name}`);
			}
		} else if (entry.module === "__wbindgen_externref_xform__") {
			if (entry.name === "__wbindgen_externref_table_set_null") {
				bucket[entry.name] = () => {};
			} else if (entry.name === "__wbindgen_externref_table_grow") {
				bucket[entry.name] = () => 0;
			} else {
				throw new Error(`unexpected engine import ${entry.module}::${entry.name}`);
			}
		} else {
			throw new Error(`unexpected engine import module ${entry.module}`);
		}
	}
	return imports as WebAssembly.Imports;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null;
}

function numberField(value: Record<string, unknown>, key: string): number {
	const field = value[key];
	if (typeof field !== "number" || !Number.isFinite(field)) {
		throw new EngineError(`the engine's reply has no usable ${key}`);
	}
	return field;
}

/**
 * A live engine instance.
 *
 * One per page: the editor keeps a single document open and closes it when
 * the document changes, so the instance itself lives as long as the tab.
 */
/**
 * Read one structure node, and its children.
 *
 * The tree is walked with a depth bound: a document is untrusted input, and
 * a structure tree that points at itself must not spin the reader.
 */
function readStructureNode(raw: Record<string, unknown>, depth: number): StructureNode {
	const role = typeof raw.role === "string" ? raw.role : "";
	const node: {
		role: string;
		title?: string;
		alt?: string;
		children: StructureNode[];
	} = { role, children: [] };
	if (typeof raw.title === "string") {
		node.title = raw.title;
	}
	if (typeof raw.alt === "string") {
		node.alt = raw.alt;
	}
	if (depth < 64 && Array.isArray(raw.children)) {
		for (const child of raw.children) {
			if (isRecord(child)) {
				node.children.push(readStructureNode(child, depth + 1));
			}
		}
	}
	return node;
}

export class EngineClient {
	readonly #exports: EngineExports;
	#nextId = 1;

	private constructor(exports: EngineExports) {
		this.#exports = exports;
	}

	/** Fetch, compile and instantiate the engine. */
	static async load(url: string = ENGINE_WASM_URL): Promise<EngineClient> {
		const response = await fetch(url);
		if (!response.ok) {
			throw new EngineError(`the engine could not be fetched (${response.status})`);
		}
		const bytes = await response.arrayBuffer();
		const module = await WebAssembly.compile(bytes);
		const instance = await WebAssembly.instantiate(module, importObjectFor(module));
		return new EngineClient(instance.exports as unknown as EngineExports);
	}

	/**
	 * One JSON-protocol round trip.
	 *
	 * @param body the op body; `v` and `id` are filled in here
	 * @param payload an optional binary attachment
	 */
	dispatch(body: Record<string, unknown>, payload: Uint8Array | null = null): EngineReply {
		const exports = this.#exports;
		const request = new TextEncoder().encode(JSON.stringify({ v: 1, id: this.#nextId++, ...body }));
		const requestPtr = exports.selis_input_alloc(request.length);
		if (!requestPtr) {
			throw new EngineError("the engine refused the request");
		}
		new Uint8Array(exports.memory.buffer, requestPtr, request.length).set(request);

		let payloadPtr = 0;
		let payloadLen = 0;
		if (payload !== null && payload.length > 0) {
			payloadLen = payload.length;
			payloadPtr = exports.selis_input_alloc(payloadLen);
			if (!payloadPtr) {
				exports.selis_free(requestPtr, request.length);
				throw new EngineError("the engine refused the attachment");
			}
			new Uint8Array(exports.memory.buffer, payloadPtr, payloadLen).set(payload);
		}

		const outPtr = exports.selis_input_alloc(12);
		if (!outPtr) {
			exports.selis_free(requestPtr, request.length);
			if (payloadLen > 0) {
				exports.selis_free(payloadPtr, payloadLen);
			}
			throw new EngineError("the engine refused the out-block");
		}

		const responsePtr = exports.selis_dispatch(
			requestPtr,
			request.length,
			payloadPtr,
			payloadLen,
			outPtr,
		);

		// The out-words are read AFTER the dispatch: the call grows linear
		// memory, which detaches any view taken before it.
		const view = new DataView(exports.memory.buffer);
		const responseLen = view.getUint32(outPtr, true);
		const attachmentPtr = view.getUint32(outPtr + 4, true);
		const attachmentLen = view.getUint32(outPtr + 8, true);

		const response = JSON.parse(
			new TextDecoder().decode(new Uint8Array(exports.memory.buffer, responsePtr, responseLen)),
		) as WireResponse;

		// The attachment is copied out before it is freed: a rendered page is
		// megabytes of pixels, and reading them after the free would be reading
		// whatever the allocator reused.
		const attachment =
			attachmentLen > 0
				? new Uint8Array(exports.memory.buffer, attachmentPtr, attachmentLen).slice()
				: null;
		if (attachmentLen > 0) {
			exports.selis_free(attachmentPtr, attachmentLen);
		}
		exports.selis_free(requestPtr, request.length);
		if (payloadLen > 0) {
			exports.selis_free(payloadPtr, payloadLen);
		}
		exports.selis_free(outPtr, 12);
		return { response, attachment };
	}

	/** Run one op and return its value, or throw the guest's typed failure. */
	#value(body: Record<string, unknown>, payload: Uint8Array | null = null): unknown {
		const { response } = this.dispatch(body, payload);
		if (response.ok !== true) {
			throw new EngineError(
				response.message ?? "the engine refused the operation",
				response.code ?? null,
				response.detail ?? null,
			);
		}
		return response.value;
	}

	/**
	 * Open a document.
	 *
	 * The budget surface is `editor` — the profile built for editing
	 * operations on top of reading — and the bytes ride along as the
	 * request's attachment, so the document never crosses the network.
	 */
	open(bytes: Uint8Array): OpenedDocument {
		const value = this.#value(
			{
				op: "open",
				src: { kind: "bytes", len: bytes.length },
				budget: { surface: "editor" },
			},
			bytes,
		);
		if (!isRecord(value)) {
			throw new EngineError("the engine's open reply is malformed");
		}
		const doc = value.doc;
		const pages = value.pages;
		if (typeof doc !== "number" || typeof pages !== "number") {
			throw new EngineError("the engine's open reply is malformed");
		}
		const sizes = Array.isArray(value.pageSizes) ? value.pageSizes : [];
		const pageSizes = sizes.map((size) => {
			if (!isRecord(size)) {
				return { width: 0, height: 0 };
			}
			return { width: numberField(size, "width"), height: numberField(size, "height") };
		});
		return { doc, pages, pageSizes };
	}

	/** Release a document and its guest memory. */
	close(doc: number): void {
		this.dispatch({ op: "close", doc });
	}

	/**
	 * Render one page.
	 *
	 * @param dpi the raster's resolution; the canvas grows with it
	 */
	render(doc: number, page: number, dpi: number): RenderedPage {
		const { response, attachment } = this.dispatch({
			op: "render",
			doc,
			page,
			params: { dpi },
		});
		if (response.ok !== true) {
			throw new EngineError(
				response.message ?? "the engine refused the render",
				response.code ?? null,
				response.detail ?? null,
			);
		}
		const value = response.value;
		if (!isRecord(value) || attachment === null) {
			throw new EngineError("the engine's render reply is malformed");
		}
		const width = numberField(value, "width");
		const height = numberField(value, "height");
		const ctmValue = value.ctm;
		if (!Array.isArray(ctmValue) || ctmValue.length !== 6) {
			throw new EngineError("the engine's render reply carries no transform");
		}
		const ctm = ctmValue.map((part) => {
			if (typeof part !== "number" || !Number.isFinite(part)) {
				throw new EngineError("the engine's render reply carries a malformed transform");
			}
			return part;
		});
		const [a, b, c, d, e, f] = ctm as [number, number, number, number, number, number];
		return {
			width,
			height,
			pixels: attachment,
			ctm: { a, b, c, d, e, f },
		};
	}

	/**
	 * Read the document's tag structure.
	 *
	 * A READ-ONLY report: the engine can read a document's structure but it
	 * cannot write one, so this says what the document carries rather than
	 * changing anything.
	 */
	structure(doc: number): Structure {
		const value = this.#value({ op: "structure", doc });
		if (!isRecord(value)) {
			throw new EngineError("the engine's structure reply is malformed");
		}
		if (value.tagged !== true) {
			return { tagged: false };
		}
		const root = value.root;
		if (!isRecord(root)) {
			return { tagged: false };
		}
		return { tagged: true, root: readStructureNode(root, 0) };
	}

	/** Read one page's text layer. */
	textLayer(doc: number, page: number): PageTextLayer {
		const value = this.#value({ op: "textLayer", doc, page });
		if (!isRecord(value)) {
			throw new EngineError("the engine's text-layer reply is malformed");
		}
		const rawLines = Array.isArray(value.lines) ? value.lines : [];
		const lines: TextLayerLine[] = [];
		for (const raw of rawLines) {
			if (!isRecord(raw)) {
				continue;
			}
			const text = raw.text;
			const rect = raw.rect;
			if (typeof text !== "string" || text === "" || !isRecord(rect)) {
				continue;
			}
			lines.push({
				text,
				rect: {
					x: numberField(rect, "x"),
					y: numberField(rect, "y"),
					width: numberField(rect, "width"),
					height: numberField(rect, "height"),
				},
			});
		}
		return {
			page: typeof value.page === "number" ? value.page : page,
			width: numberField(value, "width"),
			height: numberField(value, "height"),
			lowConfidence: value.lowConfidence === true,
			lines,
		};
	}

	/**
	 * Find the lines that carry a query.
	 *
	 * The engine's own search primitive folds case, so the query is matched
	 * without regard to case; the engine refuses a case-sensitive request
	 * rather than answering it as if it were case-insensitive, and this client
	 * does not ask for one.
	 */
	search(doc: number, query: string, maxMatches = 500): SearchResults {
		const value = this.#value({
			op: "search",
			doc,
			query,
			opts: { matchCase: false, maxMatches },
		});
		if (!isRecord(value)) {
			throw new EngineError("the engine's search reply is malformed");
		}
		const rawMatches = Array.isArray(value.matches) ? value.matches : [];
		const matches: SearchMatch[] = [];
		for (const raw of rawMatches) {
			if (!isRecord(raw)) {
				continue;
			}
			const page = raw.page;
			const line = raw.line;
			const text = raw.text;
			if (typeof page !== "number" || typeof line !== "number" || typeof text !== "string") {
				continue;
			}
			matches.push({ page, line, text });
		}
		return {
			matches,
			total: typeof value.total === "number" ? value.total : matches.length,
			truncated: value.truncated === true,
		};
	}
}
