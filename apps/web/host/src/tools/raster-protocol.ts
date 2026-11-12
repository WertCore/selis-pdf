/**
 * The client for the lazy raster chunk (PDF → PNG, JPEG, TIFF, WebP).
 *
 * ## Why this is its own module
 *
 * The raster chunk was first driven inline in the tools page, because that is
 * where the four image cards live. That made it the one export the editor
 * could not have: the editor needs the same writer, on the editor's own
 * bytes, and a client buried in another page's function body is not a client.
 *
 * So the client lives here, beside the HTML writer's and the text writer's,
 * and both surfaces call it. The chunk is the core: the renderer that turns
 * a page into pixels is Rust, in `crates/selis-pdf-raster`, and this module
 * is only the wire.
 *
 * ## The chunk's contract
 *
 * The chunk declares **no imports**, so the module is instantiated with an
 * empty import object: the guest the document bytes reach has nowhere to send
 * them. It takes the document, an optional page selection, a resolution, a
 * format number and a quality, and hands back ONE file: a single image when
 * one page was rendered, and a ZIP of them when there was more than one.
 */

/** The chunk the page fetches on first image use. */
export const RASTER_WASM_URL = "/wasm/selis_pdf_wasm_raster.wasm";

/** The guest exports this client uses. */
export interface RasterWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_raster_alloc(len: number): number;
		selis_raster_free(ptr: number, len: number): void;
		selis_raster_run(
			inputPtr: number,
			inputLen: number,
			pagesPtr: number,
			pagesLen: number,
			dpi: number,
			format: number,
			quality: number,
			outLen: number,
			outFiles: number,
		): number;
		selis_raster_last_error(outLen: number): number;
		readonly selis_raster_available: () => number;
	};
}

/** A protocol or operation failure that is safe to show to the user. */
export class RasterProtocolError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "RasterProtocolError";
	}
}

/** The image formats the chunk writes. */
export type RasterFormat = "png" | "jpg" | "tiff" | "webp";

/**
 * The chunk's format numbers.
 *
 * The chunk numbers its writers 0..3 and the client must agree with it, so
 * the table is the one place the two are tied together.
 */
export const RASTER_FORMAT_CODES: Readonly<Record<RasterFormat, number>> = {
	png: 0,
	jpg: 1,
	tiff: 2,
	webp: 3,
};

/** The suffix each format's files get. */
export const RASTER_EXTENSIONS: Readonly<Record<RasterFormat, string>> = {
	png: "png",
	jpg: "jpg",
	tiff: "tif",
	webp: "webp",
};

/** What the chunk produced. */
export interface RasterOutput {
	/** The file's name: the image, or a ZIP of them. */
	readonly name: string;
	/** The file's bytes. */
	readonly bytes: Uint8Array;
	/** How many pages were rendered. */
	readonly files: number;
	/** Whether the result is a ZIP rather than one image. */
	readonly bundle: boolean;
}

/** Fetch, compile, and instantiate the raster chunk. */
export async function loadRasterWasm(
	fetchImpl: typeof fetch = fetch,
	url: string = RASTER_WASM_URL,
): Promise<RasterWasm> {
	let response: Response;
	try {
		response = await fetchImpl(url, { credentials: "same-origin" });
	} catch {
		throw new RasterProtocolError("the renderer could not be downloaded");
	}
	if (!response.ok) {
		throw new RasterProtocolError(`the renderer is unavailable (${response.status})`);
	}
	const bytes = await response.arrayBuffer();
	const module = await WebAssembly.compile(bytes);
	const declared = WebAssembly.Module.imports(module);
	if (declared.length !== 0) {
		throw new RasterProtocolError(
			`the renderer has ${declared.length} unexpected import(s); it must be self-contained`,
		);
	}
	const instance = await WebAssembly.instantiate(module, {});
	return instance as unknown as RasterWasm;
}

/**
 * Render the document's pages to images.
 *
 * @param pages a page selection in the engine's own syntax; empty means every
 *   page
 */
export function renderPages(
	wasm: RasterWasm,
	request: {
		readonly bytes: Uint8Array;
		readonly name: string;
		readonly format: RasterFormat;
		readonly pages?: string;
		readonly dpi?: number;
		readonly quality?: number;
	},
): RasterOutput {
	const exports = wasm.exports;
	const memory = exports.memory;
	const dpi = request.dpi ?? 150;
	const quality = request.quality ?? 90;
	const selection = new TextEncoder().encode(request.pages ?? "");
	const inputPtr = exports.selis_raster_alloc(request.bytes.length);
	if (inputPtr === 0) {
		throw new RasterProtocolError("the renderer could not allocate input memory");
	}
	new Uint8Array(memory.buffer, inputPtr, request.bytes.length).set(request.bytes);
	let pagesPtr = 0;
	if (selection.length > 0) {
		pagesPtr = exports.selis_raster_alloc(selection.length);
		if (pagesPtr === 0) {
			exports.selis_raster_free(inputPtr, request.bytes.length);
			throw new RasterProtocolError("the renderer could not allocate page memory");
		}
		new Uint8Array(memory.buffer, pagesPtr, selection.length).set(selection);
	}
	const outLenPtr = exports.selis_raster_alloc(4);
	const outFilesPtr = exports.selis_raster_alloc(4);
	if (outLenPtr === 0 || outFilesPtr === 0) {
		exports.selis_raster_free(inputPtr, request.bytes.length);
		if (pagesPtr !== 0) {
			exports.selis_raster_free(pagesPtr, selection.length);
		}
		throw new RasterProtocolError("the renderer could not allocate result memory");
	}
	const resultPtr = exports.selis_raster_run(
		inputPtr,
		request.bytes.length,
		pagesPtr,
		selection.length,
		dpi,
		RASTER_FORMAT_CODES[request.format],
		quality,
		outLenPtr,
		outFilesPtr,
	);
	exports.selis_raster_free(inputPtr, request.bytes.length);
	if (pagesPtr !== 0) {
		exports.selis_raster_free(pagesPtr, selection.length);
	}
	const view = new DataView(memory.buffer);
	const outLen = view.getUint32(outLenPtr, true);
	const files = view.getUint32(outFilesPtr, true);
	exports.selis_raster_free(outLenPtr, 4);
	exports.selis_raster_free(outFilesPtr, 4);
	if (resultPtr === 0) {
		const errLenPtr = exports.selis_raster_alloc(4);
		const errPtr = exports.selis_raster_last_error(errLenPtr);
		const errLen = new DataView(memory.buffer).getUint32(errLenPtr, true);
		let message = "The pages could not be rendered.";
		if (errPtr !== 0 && errLen > 0) {
			message = new TextDecoder().decode(new Uint8Array(memory.buffer, errPtr, errLen));
			exports.selis_raster_free(errPtr, errLen);
		}
		exports.selis_raster_free(errLenPtr, 4);
		throw new RasterProtocolError(message);
	}
	const outBytes = new Uint8Array(memory.buffer, resultPtr, outLen).slice();
	exports.selis_raster_free(resultPtr, outLen);
	if (files === 0) {
		throw new RasterProtocolError("the renderer returned no pages");
	}
	const stem = request.name.replace(/\.[^.]+$/, "");
	const extension = RASTER_EXTENSIONS[request.format];
	// A TIFF is a container, so it is one file however many pages there are.
	const bundle = files > 1 && request.format !== "tiff";
	return {
		name: bundle ? `${stem}-pages.zip` : `${stem}.${extension}`,
		bytes: outBytes,
		files,
		bundle,
	};
}
