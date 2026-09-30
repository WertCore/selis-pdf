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

// Exposed for the browser check and for support. No document data, no URL —
// nothing here could identify a file (ADR-P0017).
globalThis.__selisApp = state;

const root = document.getElementById("selis-app");
if (root !== null) {
	root.removeAttribute("aria-busy");
	if (state.mounted) {
		root.textContent =
			`Selis is ready. Viewer mounted (${state.engine?.exports ?? 0} engine exports).`;
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

// ── the offline layer ──────────────────────────────────────────────────────

registerServiceWorker().then((result) => {
	// Which of "registered", "no container", "insecure context" this session
	// got, for the DoD's airplane-mode check and for support.
	globalThis.__selisServiceWorker = result.ok
		? { registered: true, scope: result.scope }
		: { registered: false, reason: result.reason };
});
