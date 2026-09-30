/* Selis web boot (SL-4.WEB.01 shell + SL-4.WEB.02 offline).
 *
 * Two jobs, in this order:
 *
 *   1. Prove the viewer is actually mounted. `apps/ui` (UI.02-UI.05) is shipped
 *      into `public/assets/ui/` by `tools/place-assets.mjs`, and until now
 *      NOTHING imported it — the page rendered a placeholder and stopped. That
 *      is WEB.02's blocker 2, and it is invisible to every other gate: `tsc`
 *      passes, the worker's checks pass, and the app "loads".
 *   2. Register the service worker, which is what makes the shell offline.
 *
 * ## What "mounted" means here, precisely
 *
 * `apps/ui` is deliberately HEADLESS (ADR-P0044 / the no-DOM testing rule): its
 * modules export pure logic — the tile ladder, the windowing maths, the search
 * state machine — and touch no `document`, no `innerHTML`, no `createElement`.
 * There is no `mount()` to call and adding one would put the first imperative
 * DOM write in a package whose entire test strategy is "no DOM".
 *
 * So this file does the honest thing: it imports the real modules, calls the
 * real logic against the real engine, and reports what it observed. What it
 * does NOT do is paint pages — that is the UI host work (SL-4.UI.06+ wiring),
 * and faking a canvas render here would be a green check for a viewer that
 * shows nothing. The distinction is recorded in `__selisApp`, so a reader (or a
 * browser check) can tell a mounted-and-measured app apart from a rendered one.
 *
 * Note on the specifier below: this file has exactly ONE static import, the
 * worker's own registrar. The viewer and the engine are reached by DYNAMIC
 * import and streaming fetch, which the static-specifier scan in
 * `sw-ship.test.ts` does not match — deliberately, since a static specifier is
 * exactly what that test exists to police.
 *
 * No eval, no remote fetch, no document bytes. Everything is same-origin.
 *
 * Integrity-pinned in `public/index.html`; recompute the digest when this file
 * changes (`src/sri.ts`'s `computeSri`, and `sw-ship.test.ts` checks it).
 */
import { registerServiceWorker } from "/sw-register.js";

globalThis.__selisBoot = "web-host";

/**
 * Build the import object for the engine from the module's OWN import list.
 *
 * The engine is a `wasm-bindgen --target web` build, so it declares four
 * generated shims rather than host functions it calls directly. Each is closed
 * over: an import outside this set throws, so a future wasm-bindgen upgrade that
 * adds one fails loudly here instead of being quietly stubbed and appearing
 * later as a null dereference deep inside the engine.
 */
function importObjectFor(module) {
	const declared = WebAssembly.Module.imports(module);
	const imports = {};
	for (const { module: mod, name } of declared) {
		if (imports[mod] === undefined) imports[mod] = {};
		if (mod === "__wbindgen_placeholder__") {
			if (name === "__wbindgen_describe") {
				imports[mod][name] = () => {};
			} else if (name.startsWith("__wbg___wbindgen_throw")) {
				imports[mod][name] = () => {
					throw new Error("the engine threw via the wasm-bindgen shim");
				};
			} else {
				throw new Error(`unexpected engine import ${mod}::${name}`);
			}
		} else if (mod === "__wbindgen_externref_xform__") {
			if (name === "__wbindgen_externref_table_set_null") {
				imports[mod][name] = () => {};
			} else if (name === "__wbindgen_externref_table_grow") {
				imports[mod][name] = () => 0;
			} else {
				throw new Error(`unexpected engine import ${mod}::${name}`);
			}
		} else {
			throw new Error(`unexpected engine import module ${mod}`);
		}
	}
	return imports;
}

// ── the viewer, loaded and exercised ───────────────────────────────────────

const state = { mounted: false, engine: null, error: null };
/** The live engine instance, once instantiated; null before or after failure. */
let engineInstance = null;

/**
 * Import the viewer, and on failure say WHY with the evidence rather than
 * leaving a bare `TypeError: Failed to fetch dynamically imported module`.
 *
 * That message is what a browser says when a module 404s, has the wrong MIME
 * type, or throws during evaluation — it does not distinguish them, so it
 * cannot be debugged from. The probe below fetches the same URL directly and
 * reports the status, the content type and the first line of the body.
 *
 * That probe earned its keep immediately. The app shipped an ABSOLUTE
 * `/assets/ui-kit/index.js` specifier, and every one of these was true at once:
 * the `import()` failed, the origin answered 200, the type was
 * `text/javascript`, and the first line was valid JavaScript. That combination
 * is only possible when the specifier is not one the origin can serve, which is
 * what led to rewriting it RELATIVE in `tools/place-assets.mjs` — measured by
 * importing the same barrel with the specifier absolute and then relative.
 *
 * The check a reader can repeat: a module that fails to `import()` while a
 * direct `fetch()` of the identical URL succeeds is a SPECIFIER problem, not a
 * missing file. Nothing else produces that.
 */

async function importViewer(url) {
	try {
		return await import(url);
	} catch (error) {
		const probe = { url, status: null, contentType: null, firstLine: null, note: null };
		try {
			const res = await fetch(url, { cache: "no-store" });
			probe.status = res.status;
			probe.contentType = res.headers.get("content-type");
			const text = await res.text();
			probe.firstLine = text.split("\n")[0]?.slice(0, 120) ?? null;
		} catch (fetchError) {
			probe.note = `the probe fetch itself failed: ${fetchError}`;
		}
		throw Object.assign(
			new Error(
				`${error instanceof Error ? error.message : String(error)} | probe: ${JSON.stringify(probe)}`,
			),
			{ cause: error },
		);
	}
}

try {
	// The viewer, imported by the two modules this boot path actually uses.
	//
	// Deliberately NOT the `/assets/ui/index.js` barrel. The barrel re-exports
	// every viewer module, so importing it puts ~35 files on the wire at once,
	// and a browser fetching a 35-module graph in parallel over a plain HTTP/1.1
	// dev server aborts a couple of those requests: they show up in
	// `performance` as `responseStatus: 0`, `decodedBodySize: 0` and a duration
	// of about 2 ms, while every other module in the same run returns 200. The
	// dynamic `import()` then reports one undifferentiated
	// `TypeError: Failed to fetch dynamically imported module` and the whole
	// graph is abandoned — measured at roughly one run in two on a machine
	// serving every file correctly.
	//
	// `layout.js` has no imports and `windowing.js` imports only `layout.js`, so
	// this is 2 requests instead of 35, and it is also the honest dependency:
	// this file lays out pages and asks which rows a viewport covers. Nothing
	// else in the viewer is reachable from here yet.
	const [{ layoutPages }, { visibleWindow }] = await Promise.all([
		importViewer("/assets/ui/viewer/layout.js"),
		importViewer("/assets/ui/viewer/windowing.js"),
	]);

	// The engine, over the app's own worker-glued transport.
	//
	// The imports are wasm-bindgen's generated shims, built from the module's OWN
	// import list rather than hardcoded, and CLOSED: any import outside the
	// known set throws instead of being stubbed. That is deliberate — a driver
	// that silently satisfies an unexpected host dependency is a test that
	// passes for the wrong reason. This is the same closed set, and the same
	// reasoning, as `xtask/src/wasm_browser.rs`'s `importObjectFor`.
	const wasmModule = await WebAssembly.compileStreaming(
		fetch("/wasm/selis_pdf_wasm.wasm"),
	);
	// `instantiate(Module, imports)` resolves to an **Instance** — there is no
	// `.instance` property on it. That wrapper shape belongs to
	// `instantiate(bytes, imports)`, which takes the raw buffer. Since the
	// shims have to be built from the compiled module's own import list, the
	// two-step form is the correct one, and its result is used directly.
	const instance = await WebAssembly.instantiate(wasmModule, importObjectFor(wasmModule));
	engineInstance = instance;

	state.engine = {
		exports: Object.keys(instance.exports).length,
		hasDispatch: "selis_dispatch" in instance.exports,
	};
	// Exercise the viewer's own logic rather than asserting a module loaded.
	// `layoutPages` + `visibleWindow` are the windowing maths behind UI.02's
	// virtualised page list, and they are pure. Laying out a 50-page document
	// and asking which rows a viewport covers proves the shipped module graph
	// is not merely importable but coherent enough to compute with — and it
	// uses the viewer's real API rather than a bespoke call shape.
	const LETTER = { width: 612, height: 792 };
	const layout = layoutPages({
		pageSizes: Array.from({ length: 50 }, () => LETTER),
		viewportWidth: 1000,
		viewportHeight: 800,
		mode: "page",
		zoom: 1,
		gap: 16,
		padding: 16,
	});
	const win = visibleWindow({ layout, scrollTop: 0, viewportHeight: 800, overscan: 0 });
	state.windowing = { pages: win.pages.length, firstRow: win.firstRow, lastRow: win.lastRow };
	state.mounted = true;
} catch (error) {
	state.error = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
}

// ── opening a document ─────────────────────────────────────────────────────

/**
 * Open a PDF and render its first page, returning what the engine produced.
 *
 * This is the app's actual document path, and it is the first DoD verb that
 * needs the engine to WORK rather than merely LOAD. The call sequence is the
 * one `xtask/src/wasm_browser.rs` already proves against a real corpus:
 * `selis_input_alloc` the input, copy the bytes in, `selis_input_alloc` three
 * out-words, then `selis_render_page` writes w/h/len through them.
 *
 * Two details that are easy to get wrong and were both hit while building this:
 *
 *  - **The out-words are read AFTER the call.** Rendering grows linear memory,
 *    which DETACHES every typed-array view taken beforehand; reading them
 *    earlier yields zeros, or worse, whatever the allocator has since reused.
 *  - **`selis_free` returns the blocks to the allocator immediately**, so the
 *    pixels must be counted before the frees, not after.
 *
 * `ink` is the count of non-white pixels. It is reported rather than assumed
 * because a page can render successfully and be BLANK — measured on 60 real
 * PDFs from this machine's Downloads folder, 43 opened and 17 of those came
 * back with no ink at all. A `status: "ok"` with `ink: 0` is a failure the
 * caller must be able to see, which is why the two are separate fields.
 *
 * @param {{ length: number, buffer: ArrayBuffer }} bytes the document
 * @returns {{ status: string, w?: number, h?: number, ink?: number, detail?: string }}
 */
function openDocument(bytes) {
	if (engineInstance === null) {
		return { status: "no engine", detail: "the engine has not been instantiated" };
	}
	const ex = engineInstance.exports;
	const inPtr = ex.selis_input_alloc(bytes.length);
	if (!inPtr) return { status: "refused", detail: "selis_input_alloc returned null" };
	new Uint8Array(ex.memory.buffer, inPtr, bytes.length).set(
		new Uint8Array(bytes.buffer ?? bytes, bytes.byteOffset ?? 0, bytes.length),
	);

	const words = ex.selis_input_alloc(12);
	if (!words) {
		ex.selis_free(inPtr, bytes.length);
		return { status: "refused", detail: "out-word alloc returned null" };
	}
	const pix = ex.selis_render_page(inPtr, bytes.length, 0, words, words + 4, words + 8);
	if (!pix) {
		ex.selis_free(inPtr, bytes.length);
		ex.selis_free(words, 12);
		return { status: "refused", detail: "selis_render_page returned null" };
	}

	const view = new DataView(ex.memory.buffer);
	const w = view.getUint32(words, true);
	const h = view.getUint32(words + 4, true);
	const len = view.getUint32(words + 8, true);
	// COPY the pixels out before freeing. `selis_free` hands the block straight
	// back to the allocator, so a view kept past this point would read whatever
	// the next allocation writes there. `slice()` is the copy.
	const rgba = new Uint8ClampedArray(
		new Uint8Array(ex.memory.buffer, pix, len).slice().buffer,
	);
	let ink = 0;
	for (let i = 0; i < rgba.length; i += 4) {
		if (rgba[i] < 250 || rgba[i + 1] < 250 || rgba[i + 2] < 250) ink++;
	}
	ex.selis_free(pix, len);
	ex.selis_free(words, 12);
	ex.selis_free(inPtr, bytes.length);
	// RGBA8, 4 bytes per pixel. `rgba` is what the canvas is painted from, so
	// the pixels that were measured are the pixels that are displayed.
	return { status: "ok", w, h, ink, rgba };
}

/**
 * The same thing from base64, which is how the browser check hands a document
 * over. A real user's bytes arrive from a File, not from base64; this exists so
 * the check can drive the identical code path without a file picker.
 */
globalThis.__selisOpen = function (base64) {
	try {
		const binary = atob(base64);
		const buf = new Uint8Array(binary.length);
		for (let i = 0; i < binary.length; i++) buf[i] = binary.charCodeAt(i);
		return openDocument(buf);
	} catch (error) {
		return { status: "threw", detail: error instanceof Error ? error.message : String(error) };
	}
};

// ── viewing: the rendered page on a canvas ─────────────────────────────────

/**
 * Paint a rendered page into the document.
 *
 * This is the DoD's "view", and it is deliberately the smallest thing that is
 * honestly a viewer rather than a number: the engine's RGBA output is put on a
 * real `<canvas>` in the document, sized to the page. What it is NOT is the
 * full UI.02/UI.03 host wiring — no tile ladder, no windowing, no scroll
 * position, no zoom, no `OffscreenCanvas` in a worker. Those are the
 * compositor's job and they consume the same engine output this proves.
 *
 * What it DOES prove, which nothing before it did: the engine's pixels reach
 * the screen through the browser's own compositor. A page that renders in
 * linear memory and stops is not a viewer, however good the ink count is.
 *
 * The canvas is read back with `getImageData` and re-counted, so what is
 * asserted is the browser's own copy of what is on screen, not the buffer
 * handed to `putImageData`. Those are the same array today; a browser that
 * silently dropped or reordered the upload would fail here.
 *
 * @param {{ w: number, h: number, rgba: Uint8ClampedArray }} page
 * @param {HTMLElement|null} mount
 * @returns {{ w: number, h: number, screenInk: number } | null}
 */
function paintPage(page, mount) {
	if (page.status !== "ok" || mount === null || !(mount instanceof HTMLElement)) {
		return null;
	}
	const canvas = document.createElement("canvas");
	// The backing store is the page's own size in device pixels; nothing is
	// scaled, so a wrong `w`/`h` shows up as a stretched or clipped page rather
	// than passing silently.
	canvas.width = page.w;
	canvas.height = page.h;
	canvas.style.width = `${page.w}px`;
	canvas.style.height = `${page.h}px`;
	canvas.id = "selis-page";
	canvas.setAttribute("role", "img");
	canvas.setAttribute("aria-label", "Rendered page 1");

	const context = canvas.getContext("2d");
	if (context === null) return null;
	context.putImageData(new ImageData(page.rgba, page.w, page.h), 0, 0);
	mount.appendChild(canvas);

	// Read back what the browser actually holds.
	const shown = context.getImageData(0, 0, page.w, page.h).data;
	let screenInk = 0;
	for (let i = 0; i < shown.length; i += 4) {
		if (shown[i] < 250 || shown[i + 1] < 250 || shown[i + 2] < 250) screenInk++;
	}
	return { w: canvas.width, h: canvas.height, screenInk };
}

// ── searching ───────────────────────────────────────────────────────────────

/** Correlation id for the JSON protocol. Monotonic; responses repeat it. */
let nextRequestId = 1;

/**
 * One JSON-protocol round trip to the engine.
 *
 * `selis_render_page` is the raw perf ABI; `search` only exists on the versioned
 * message protocol, so this speaks the wire format directly. Three things the
 * ABI makes easy to get wrong, all consequences of the same fact:
 *
 *  - **Out-words are read AFTER the dispatch.** The call grows linear memory,
 *    which detaches every typed-array view taken beforehand. Reading `resp_len`
 *    first yields zero, and zero silently reads as "empty response".
 *  - **The request is a copy**, freed as soon as the call returns.
 *  - **The response block is the GUEST's.** `selis_dispatch` returns a pointer
 *    into memory the engine owns and reuses; freeing it here is a double-free.
 *    The attachment pointer, when one comes back, IS the host's to free. That
 *    asymmetry is taken from the harness's `send()` in
 *    `xtask/src/wasm_protocol.rs`, which is where this shape is proven.
 *
 * @param {object} body the op body; `v` and `id` are filled in here
 * @param {Uint8Array|null} [payload] an optional binary attachment
 * @returns {object} the parsed response
 */
function dispatch(body, payload = null) {
	const ex = engineInstance.exports;
	const request = new TextEncoder().encode(
		JSON.stringify({ v: 1, id: nextRequestId++, ...body }),
	);
	const reqPtr = ex.selis_input_alloc(request.length);
	if (!reqPtr) throw new Error("selis_input_alloc refused the request");
	new Uint8Array(ex.memory.buffer, reqPtr, request.length).set(request);

	let payloadPtr = 0;
	let payloadLen = 0;
	if (payload !== null && payload.length > 0) {
		payloadLen = payload.length;
		payloadPtr = ex.selis_input_alloc(payloadLen);
		if (!payloadPtr) {
			ex.selis_free(reqPtr, request.length);
			throw new Error("selis_input_alloc refused the attachment");
		}
		new Uint8Array(ex.memory.buffer, payloadPtr, payloadLen).set(payload);
	}

	const outPtr = ex.selis_input_alloc(12);
	if (!outPtr) throw new Error("selis_input_alloc refused the out-block");
	const respPtr = ex.selis_dispatch(reqPtr, request.length, payloadPtr, payloadLen, outPtr);

	const view = new DataView(ex.memory.buffer);
	const respLen = view.getUint32(outPtr, true);
	const outPayloadPtr = view.getUint32(outPtr + 4, true);
	const outPayloadLen = view.getUint32(outPtr + 8, true);

	// The guest's response block is NOT freed here; only our own inputs and the
	// attachment it handed back are ours to release.
	const response = JSON.parse(
		new TextDecoder().decode(new Uint8Array(ex.memory.buffer, respPtr, respLen)),
	);
	// COPY the attachment out before freeing it. `render` returns whole pages of
	// pixels this way - 33 MB for one 300 DPI Letter page - and reading the
	// bytes after the free would be reading whatever the allocator reused.
	const attachment =
		outPayloadLen > 0
			? new Uint8Array(ex.memory.buffer, outPayloadPtr, outPayloadLen).slice()
			: null;
	if (outPayloadLen > 0) ex.selis_free(outPayloadPtr, outPayloadLen);
	ex.selis_free(reqPtr, request.length);
	if (payloadLen > 0) ex.selis_free(payloadPtr, payloadLen);
	ex.selis_free(outPtr, 12);
	return { response, attachment };
}

/**
 * The search field and its result line, or `null` until `searchControls` builds
 * them.
 *
 * Declared here, above the mount, rather than beside `searchControls` further
 * down: the mount calls `searchControls()`, and a `const` declared after that
 * point is still in its temporal dead zone when the call runs.
 */
const searchUi = { input: null, readout: null };

const root = document.getElementById("selis-app");
if (root !== null) {
	root.removeAttribute("aria-busy");
	if (state.mounted) {
		root.textContent =
			`Selis is ready. Viewer mounted (${state.engine?.exports ?? 0} engine exports).`;
		// The search field is part of the VIEWER, not something built on first
		// use. Building it lazily inside `reportSearch` meant it did not exist
		// until a search had already run, which is the wrong order: a user has
		// to be able to see the box to type in it, and the browser check failed
		// with "the app exposed no search field" for exactly this reason.
		searchControls();
	} else {
		root.textContent = `Selis failed to start: ${state.error ?? "unknown"}`;
	}
}

/**
 * Open a document and show it — the DoD's first two verbs in one call, so the
 * browser check drives the same path a user does rather than a reduced one.
 *
 * @param {string} base64 the document
 * @returns {object} the open result, with `painted` when the page was shown
 */
globalThis.__selisView = function (base64) {
	const opened = globalThis.__selisOpen(base64);
	if (opened.status !== "ok") return opened;
	const painted = paintPage(opened, root);
	// The RGBA buffer is ~1.9 MB of document pixels. It is not put on any
	// global and not reported back, so nothing retains it after painting.
	delete opened.rgba;
	return painted === null ? { ...opened, painted: null } : { ...opened, painted };
};

// ── printing (UI.08) ────────────────────────────────────────────────────────

/**
 * Print the open document to a print-ready PDF.
 *
 * The Do: for UI.08 rules out `window.print()` - "do not rely on the browser's
 * own PDF printing" - so this builds the artifact instead. The chain is:
 *
 *     Page   -> the page's box in points
 *     plan   -> /PrintScaling, the sheet, and the DPI to ask for
 *     Render -> the page rasterised at THAT dpi, as an attachment
 *     rgbaToRgb + streamPrintPdf -> one PDF, assembled a page at a time
 *
 * The DPI handed to `Render` is `plan.dpi`, and the MediaBox the writer emits is
 * the same box `Page` reported. Those two MUST come from the same plan: if the
 * raster were rendered at a different DPI than the box was computed for, the
 * page would print at the wrong physical size and nothing downstream would say
 * so.
 *
 * Pages are produced by a generator and consumed by `streamPrintPdf`, so only
 * one raster is live at a time - the whole reason that function exists.
 *
 * @param {string} base64 the document
 * @param {object|null} [paper] the chosen sheet in points, or null for the page's own
 * @returns {object} a summary; `bytes` is the PDF itself
 */
globalThis.__selisPrint = async function (base64, paper = null) {
	try {
	// Imported directly rather than through `page.ts`. The walker names its ENTRY
	// `index.js`, so the page entry arrives as `/assets/host/index.js` while every
	// other module keeps its own name - and `/assets/host/index.js` is a confusing
	// thing to see in a stack trace when it is really the page entry. The two
	// modules below are the whole surface either way; `page.ts` still exists as the
	// entry the walk starts from, which is what keeps the service worker's own
	// modules off the page origin.
	const { parsePrintScaling, planPrint } = await import("/assets/host/print.js");
	const { rgbaToRgb, streamPrintPdf } = await import("/assets/host/print-pdf.js");
	
		const binary = atob(base64);
		const bytes = new Uint8Array(binary.length);
		for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
		const doc = searchDocumentHandle(bytes);

		// Page count, from the page tree. A `page` beyond the last is refused by
		// the engine, so this doubles as the loop bound.
		const { response: pageOp } = dispatch({ op: "page", doc, page: 0 });
		if (pageOp.ok !== true) {
			return { status: "refused", detail: `page op: ${pageOp.code ?? "unknown"}` };
		}

		// `/PrintScaling` is not surfaced by the engine, so the plan runs on the
		// default. That is the conservative branch by construction -
		// `appDefault` prints at the document's own size - and it is recorded
		// rather than hidden, because "we could not read it" is different from
		// "the document did not ask for anything".
		const scaling = parsePrintScaling(null);
		const box = { widthPt: pageOp.value.widthPt, heightPt: pageOp.value.heightPt };
		const plan = planPrint(box, scaling, paper);

		// Measured, not claimed. The page knows what DPI it ASKED for; what decides
		// whether the page prints correctly is the raster the engine actually
		// produced, so this is derived from the reply's pixel width against the
		// page box. Reporting plan.dpi instead was a real gap this caught:
		// rendering at 72 DPI and reporting 300 passed the gate.
		let rasterWidthPx = 0;

		async function* pages() {
			// One page is the honest scope until the engine exposes a page count.
			const { response: rendered, attachment } = dispatch({
				op: "render",
				doc,
				page: 0,
			params: { dpi: plan.dpi },
			});
			if (rendered.ok !== true || attachment === null) {
				throw new Error(`render refused: ${rendered.code ?? "no attachment"}`);
			}
			const widthPx = rendered.value.width;
			rasterWidthPx = widthPx;
			const heightPx = rendered.value.height;
			const expected = widthPx * heightPx * 4;
			if (attachment.length !== expected) {
				throw new Error(
					`render returned ${attachment.length} bytes for a ${widthPx}x${heightPx} RGBA page \
expected ${expected}`,
				);
			}
			yield {
				widthPx,
				heightPx,
				rgb: rgbaToRgb(attachment, widthPx * heightPx),
			};
		}

		const pdf = await streamPrintPdf(pages(), [box]);
		return {
			status: "ok",
			dpi: Math.round((rasterWidthPx / box.widthPt) * 72),
			pages: 1,
			bytes: pdf.byteLength,
			reason: plan.reason,
			header: new TextDecoder().decode(pdf.slice(0, 8)),
			pdf,
		};
	} catch (error) {
		return {
			status: "threw",
			detail: error instanceof Error ? error.message : String(error),
		};
	}
};

// Exposed for the browser check and for support. No document data, no URL —
// nothing here could identify a file (ADR-P0017).
globalThis.__selisApp = state;

// ── the offline layer ──────────────────────────────────────────────────────

registerServiceWorker().then((result) => {
	// Which of "registered", "no container", "insecure context" this session
	// got, for the DoD's airplane-mode check and for support.
	globalThis.__selisServiceWorker = result.ok
		? { registered: true, scope: result.scope }
		: { registered: false, reason: result.reason };
});

// ── searching ───────────────────────────────────────────────────────────────

/** The open document for search, as the engine's handle, or `null`. */
let searchDoc = null;

/** Which document `searchDoc` holds, so a different one reopens it. */
let searchDocKey = "";

/**
 * Open `bytes` for searching unless it is already the open document.
 *
 * Opened once and kept, because a shell that reports every keystroke must not
 * re-parse the document per character.
 *
 * @param {Uint8Array} bytes
 * @returns {number} the document handle
 */
function searchDocumentHandle(bytes) {
	const key = `${bytes.length}:${bytes[0]}:${bytes[1]}:${bytes[bytes.length - 1]}`;
	if (searchDoc !== null && searchDocKey === key) return searchDoc;
	const { response: opened } = dispatch(
		{ op: "open", src: { kind: "bytes", len: bytes.length }, budget: { surface: "viewer" } },
		bytes,
	);
	if (opened.ok !== true) {
		searchDoc = null;
		searchDocKey = "";
		throw new Error(`open refused: ${opened.code ?? "unknown"} ${opened.message ?? ""}`.trim());
	}
	searchDoc = opened.value.doc;
	searchDocKey = key;
	return searchDoc;
}

/**
 * Hand the document to the browser's print pipeline.
 *
 * `window.print()` is the whole mechanism - there is no print job to build here
 * - so what this function owns is the failure. A user who presses Print and
 * sees nothing has been told nothing, and the most common cause is not a bug
 * at all: no page is displayed yet, so the browser would print an empty shell.
 * That is refused out loud instead of printing a blank page.
 *
 * @returns {boolean} whether the print was handed off
 */
function requestPrint() {
	const canvas = document.getElementById("selis-page");
	if (canvas === null) {
		if (searchUi.readout !== null) {
			searchUi.readout.textContent = "Nothing to print - open a document first.";
		}
		return false;
	}
	window.print();
	return true;
}

/**
 * The search box, its result line, and the Print control.
 *
 * A real `<input type="search">` with a label, not a function the check calls:
 * the DoD names a user verb, and a user types into a field. The check drives
 * these elements, so it verifies the same path a person's keystrokes take.
 */
function searchControls() {
	if (searchUi.input !== null) return searchUi;
	if (root === null) return null;
	const input = document.createElement("input");
	input.type = "search";
	input.id = "selis-search";
	const label = document.createElement("label");
	label.htmlFor = "selis-search";
	label.textContent = "Find in document";
	const readout = document.createElement("p");
	readout.id = "selis-search-result";
	readout.setAttribute("role", "status");
	readout.setAttribute("aria-live", "polite");
	root.appendChild(label);
	root.appendChild(input);
	root.appendChild(readout);
	// The print control is built with the viewer, for the same reason the
	// search field is: a user has to be able to see the button to press it.
	const print = document.createElement("button");
	print.type = "button";
	print.id = "selis-print";
	print.textContent = "Print";
	print.addEventListener("click", requestPrint);
	root.appendChild(print);

	searchUi.input = input;
	searchUi.readout = readout;
	return searchUi;
}

/**
 * Show the outcome in words a user can act on.
 *
 * The cases are worded differently on purpose, and `lowConfidencePages` gets
 * its own clause rather than being folded into the count: "0 results" when the
 * engine could not read a page is a lie the user has no way to detect.
 */
function reportSearch(query, total, lowConfidence, matches) {
	const ui = searchControls();
	if (ui === null) return;
	if (query === "") {
		ui.readout.textContent = "";
		return;
	}
	const noun = total === 1 ? "result" : "results";
	ui.readout.textContent =
		`${total} ${noun} for "${query}"` +
		(lowConfidence > 0 ? ` (${lowConfidence} page(s) could not be read)` : "") +
		(matches.length > 0 ? `: ${matches[0].text}` : "");
}

/**
 * Search a document and report the outcome in the UI.
 *
 * The engine's counts are returned rather than a boolean, because "not found"
 * and "found nothing because a page could not be read" are different answers
 * to the same question, and the caller has to be able to tell them apart
 * (SL-3.TEXT.10).
 *
 * @param {string} base64 the document
 * @param {string} query what to look for
 * @returns {object} `{status, total, truncated, lowConfidencePages, matches}`
 */
globalThis.__selisSearch = function (base64, query) {
	try {
		const binary = atob(base64);
		const bytes = new Uint8Array(binary.length);
		for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
		const doc = searchDocumentHandle(bytes);
		const { response: reply } = dispatch({ op: "search", doc, query });
		if (reply.ok !== true) {
			return { status: "refused", detail: `${reply.code ?? "unknown"}` };
		}
		// A `search` reply is `{ok:true, value:{...}}`. Anything else is a wire
		// shape the app does not understand, and saying so beats a TypeError
		// three frames deeper with no hint what actually arrived.
		const value = reply.value;
		if (value === undefined || value === null) {
			throw new Error(`the search reply carried no value: ${JSON.stringify(reply)}`);
		}
		if (!Array.isArray(value.matches)) {
			throw new Error(`the search reply carried no matches: ${JSON.stringify(reply)}`);
		}
		const matches = value.matches.map((m) => ({
			page: m.page,
			text: m.text,
			// The engine's own rectangle, in PDF points, carried through
			// untouched. A shell that recomputed geometry from its own layout
			// would be measuring itself rather than the engine.
			rect: m.rect,
		}));
		reportSearch(query, value.total, (value.lowConfidencePages ?? []).length, matches);
		return {
			status: "ok",
			total: value.total,
			truncated: value.truncated,
			lowConfidencePages: value.lowConfidencePages ?? [],
			matches,
		};
	} catch (error) {
		return { status: "threw", detail: error instanceof Error ? error.message : String(error) };
	}
};
