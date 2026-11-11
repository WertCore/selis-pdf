/**
 * The client for the lazy HTML chunk (PDF → HTML).
 *
 * ## Why this is its own module
 *
 * The HTML writer was first written inline in the tools page, because that is
 * where it was first needed. That made it the one export the editor could not
 * have: the editor needs the same writer, on the editor's own bytes, and a
 * client buried in another page's function body is not a client.
 *
 * So the client lives here, beside the tools chunk's and the signing chunk's,
 * and both surfaces call it. The chunk is the core: the writer that turns a
 * PDF's text and positions into a positioned HTML document is Rust, in
 * `crates/selis-pdf-text`, and this module is only the wire.
 *
 * ## The chunk's contract
 *
 * The chunk declares **no imports**, so the module is instantiated with an
 * empty import object: the guest the document bytes reach has nowhere to send
 * them. It takes the document, an optional page selection and an optional
 * title, and hands back one `.html` file — the pages are `<section>`s inside
 * a single document, not one file per page.
 */

/** The chunk the page fetches on first HTML use. */
export const HTML_WASM_URL = "/wasm/selis_pdf_wasm_html.wasm";

/** The guest exports this client uses. */
export interface HtmlWasm {
	readonly exports: {
		readonly memory: WebAssembly.Memory;
		selis_html_alloc(len: number): number;
		selis_html_free(ptr: number, len: number): void;
		selis_html_run(
			inputPtr: number,
			inputLen: number,
			pagesPtr: number,
			pagesLen: number,
			titlePtr: number,
			titleLen: number,
			outLen: number,
			outPages: number,
		): number;
		selis_html_last_error(outLen: number): number;
		readonly selis_html_available: () => number;
	};
}

/** A protocol or operation failure that is safe to show to the user. */
export class HtmlProtocolError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "HtmlProtocolError";
	}
}

/** What the HTML writer produced. */
export interface HtmlExport {
	/** The file's name. */
	readonly name: string;
	/** The document's bytes. */
	readonly bytes: Uint8Array;
	/** How many pages the document carries. */
	readonly pages: number;
}

/**
 * Fetch, compile and instantiate the HTML chunk.
 *
 * The module is refused if it declares an import: an import is a capability,
 * and the guest is handed the reader's document.
 */
export async function loadHtmlWasm(fetchImpl: typeof fetch = fetch): Promise<HtmlWasm> {
	const response = await fetchImpl(HTML_WASM_URL, { credentials: "same-origin" });
	if (!response.ok) {
		throw new HtmlProtocolError("the HTML writer is unavailable");
	}
	const bytes = await response.arrayBuffer();
	const module = await WebAssembly.compile(bytes);
	// No imports: the writer cannot call out, so the document bytes it is
	// handed have nowhere to go but back to this page.
	const instance = await WebAssembly.instantiate(module, {});
	return instance as unknown as HtmlWasm;
}

/**
 * Write one `.html` document for the whole selection, whatever its length.
 *
 * @param wasm the chunk
 * @param bytes the PDF's bytes
 * @param name the document's name, without an extension
 * @param pages an optional page selection ("1-3,5")
 * @param title an optional title for the document
 */
export function writeHtml(
	wasm: HtmlWasm,
	bytes: Uint8Array,
	name: string,
	pages?: string,
	title?: string,
): HtmlExport {
	const exports = wasm.exports;
	const memory = exports.memory;
	const encoder = new TextEncoder();
	const selectionBytes = encoder.encode(pages ?? "");
	const titleBytes = encoder.encode(title ?? name);

	const inputPtr = exports.selis_html_alloc(bytes.length);
	if (inputPtr === 0) {
		throw new HtmlProtocolError("the HTML writer could not allocate input memory");
	}
	new Uint8Array(memory.buffer, inputPtr, bytes.length).set(bytes);

	let pagesPtr = 0;
	if (selectionBytes.length > 0) {
		pagesPtr = exports.selis_html_alloc(selectionBytes.length);
		if (pagesPtr === 0) {
			exports.selis_html_free(inputPtr, bytes.length);
			throw new HtmlProtocolError("the HTML writer could not allocate page memory");
		}
		new Uint8Array(memory.buffer, pagesPtr, selectionBytes.length).set(selectionBytes);
	}

	let titlePtr = 0;
	if (titleBytes.length > 0) {
		titlePtr = exports.selis_html_alloc(titleBytes.length);
		if (titlePtr === 0) {
			exports.selis_html_free(inputPtr, bytes.length);
			if (pagesPtr !== 0) {
				exports.selis_html_free(pagesPtr, selectionBytes.length);
			}
			throw new HtmlProtocolError("the HTML writer could not allocate title memory");
		}
		new Uint8Array(memory.buffer, titlePtr, titleBytes.length).set(titleBytes);
	}

	const outLenPtr = exports.selis_html_alloc(4);
	const outPagesPtr = exports.selis_html_alloc(4);
	if (outLenPtr === 0 || outPagesPtr === 0) {
		exports.selis_html_free(inputPtr, bytes.length);
		if (pagesPtr !== 0) {
			exports.selis_html_free(pagesPtr, selectionBytes.length);
		}
		if (titlePtr !== 0) {
			exports.selis_html_free(titlePtr, titleBytes.length);
		}
		throw new HtmlProtocolError("the HTML writer could not allocate result memory");
	}

	const resultPtr = exports.selis_html_run(
		inputPtr,
		bytes.length,
		pagesPtr,
		selectionBytes.length,
		titlePtr,
		titleBytes.length,
		outLenPtr,
		outPagesPtr,
	);
	exports.selis_html_free(inputPtr, bytes.length);
	if (pagesPtr !== 0) {
		exports.selis_html_free(pagesPtr, selectionBytes.length);
	}
	if (titlePtr !== 0) {
		exports.selis_html_free(titlePtr, titleBytes.length);
	}

	const view = new DataView(memory.buffer);
	const outLen = view.getUint32(outLenPtr, true);
	const outPages = view.getUint32(outPagesPtr, true);
	exports.selis_html_free(outLenPtr, 4);
	exports.selis_html_free(outPagesPtr, 4);

	if (resultPtr === 0) {
		const errLenPtr = exports.selis_html_alloc(4);
		const errPtr = exports.selis_html_last_error(errLenPtr);
		const errLen = new DataView(memory.buffer).getUint32(errLenPtr, true);
		let message = "The pages could not be written as HTML.";
		if (errPtr !== 0 && errLen > 0) {
			message = new TextDecoder().decode(new Uint8Array(memory.buffer, errPtr, errLen));
			exports.selis_html_free(errPtr, errLen);
		}
		exports.selis_html_free(errLenPtr, 4);
		throw new HtmlProtocolError(message);
	}

	const outBytes = new Uint8Array(memory.buffer, resultPtr, outLen).slice();
	exports.selis_html_free(resultPtr, outLen);
	if (outPages === 0) {
		throw new HtmlProtocolError("the HTML writer returned a document with no pages.");
	}
	return { name: `${name}.html`, bytes: outBytes, pages: outPages };
}
