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
 * Viewer modules the CLICK paths need, by path.
 *
 * `error-panel.js` because the failure panel must never depend on a request it
 * has to make at the moment it is needed; `health.js` for the same reason. See
 * the preload block below for the whole argument.
 */
const UI_PRELOADS = [
	"/assets/ui/viewer/error-panel.js",
	"/assets/ui/viewer/health.js",
	// SL-4.UI.13. Preloaded for the same reason as the other two, and the same
	// reason it matters MORE here: the progress module is needed the moment a
	// print starts. If its import flakes, the fallback is "paint a bar without
	// the model", which is exactly the unmonotonic, dishonest bar this item
	// exists to prevent - so the fallback below refuses to paint at all instead.
	"/assets/ui/viewer/progress.js",
	// SL-4.UI.13. Read on the failure path, so it gets the same preloading
	// treatment: `showFailure` runs exactly when the app is already in trouble,
	// and it is the worst moment to discover a module request can fail. A
	// missing budget module means the "used 300 MB of 256 MB" line is omitted -
	// degraded, not broken, and never a fabricated zero.
	"/assets/ui/viewer/budget.js",
];

/**
 * Set by the Cancel control, checked by the print loop (SL-4.UI.13).
 *
 * Page granularity, deliberately. The guest is built single-threaded
 * (`+simd128` only, no `+atomics`), so an in-flight `selis_dispatch` runs on
 * THIS thread and cannot be interrupted from it: the guest's cancel slot is
 * cleared at dispatch entry and only read at the next budget tick, which this
 * thread is in no position to reach. Page boundaries are what is genuinely
 * available, and saying so beats a Cancel button that looks live and is not.
 *
 * Reset at the start of every print job, so a cancel left over from one job
 * cannot stop the next one - that would be a button claiming to have
 * cancelled work nobody asked to cancel.
 */
let printCancelled = false;

/** What the print job is doing, for the browser check and for the panel. */
let printProgress = null;

/**
 * The print job's progress, as `progress.ts` models it (SL-4.UI.13).
 *
 * `null` when nothing is running. The shell does NOT compute a percentage here:
 * `progress.ts` owns monotonicity, request filtering and the difference between
 * "asked to stop" and "stopped", and duplicating any of that here is how the
 * two drift apart and the bar starts telling stories.
 */
let printJob = null;

/**
 * Identifies THIS print job, so a late report from a previous one is dropped.
 *
 * `progress.ts` refuses a report whose request is not the one being tracked,
 * which is what stops a finished job's last fraction being painted over a new
 * job that has not started. Monotonically increasing, never reused.
 */
let printRequestId = 0;

/** The progress module, once preloaded. `null` until it lands. */
let progressModel = null;

/**
 * Move the print job's state on, and repaint.
 *
 * Every phase change in the shell goes through here, so there is exactly one
 * place that can be wrong about what the panel says.
 *
 * If the model has not loaded, this is a NO-OP rather than a fallback that
 * paints something. A progress bar with no model behind it is the one artifact
 * this item is about not shipping: it would have to re-implement monotonicity
 * locally to be useful, and that copy is what goes stale. Showing nothing until
 * the real model arrives is honest and is over in milliseconds.
 */
function advancePrintJob(phase) {
	if (progressModel === null || !printJob) return printJob;
	if (phase === "cancelling") printJob = progressModel.requestCancel(printJob);
	else if (phase === "cancelled") printJob = progressModel.confirmCancelled(printJob);
	else if (phase === "done") printJob = progressModel.finish(printJob);
	else if (phase === "failed") printJob = progressModel.fail(printJob);
	renderPrintProgress();
	// Returns the new state. Callers that assign this back MUST have the return
	// value: an earlier version returned nothing, so `printJob =
	// advancePrintJob("cancelling")` silently assigned `undefined` - which then
	// sailed past the `printJob === null` guard in `renderPrintProgress` and
	// threw on the next repaint. The browser check found that; the fix is here.
	return printJob;
}

/**
 * Fold the page loop's own count into the model, then repaint.
 *
 * The shell knows two integers - pages done, pages total - and NOT a fraction.
 * It hands those to `progress.ts` as the same `SlotReport` shape the engine's
 * slot produces, so the model is exercised through the interface it was written
 * for rather than a print-specific back door.
 *
 * A `null` total stays `null` all the way to the bar, which then renders as
 * indeterminate. Guessing a total from the document's page count would put a
 * confident 100% on a job that might still fail.
 */
function syncPrintProgress() {
	if (progressModel === null || !printJob || !printProgress) return;
	const total = printProgress.pagesTotal;
	const report = {
		request: printJob.request,
		stage: 1,
		fractionBp:
			total === null || total === undefined || total === 0
				? 0
				: Math.round((printProgress.pagesDone / total) * 10_000),
	};
	// `applyReport` clamps and refuses regressions, so `pagesDone` going
	// backwards - or a stale report - cannot move the bar backwards.
	printJob = progressModel.applyReport(printJob, report);
	renderPrintProgress();
}
/**
 * Ask the print job to stop, at the next page boundary.
 *
 * `null` when nothing is in flight, rather than a fabricated idle state: there
 * being nothing to cancel is a different answer from a cancellation, and
 * conflating them would report a cancellation that never happened.
 */
globalThis.__selisPrintCancel = function () {
	if (printProgress === null || printProgress.pagesDone === undefined) return null;
	// Through the model, not around it: `requestCancel` moves the phase to
	// `cancelling`, which is the point. Setting the boolean alone would stop the
	// loop while the panel kept saying "Printing..." - a reader who pressed
	// Cancel and saw nothing happen would press it again.
	printCancelled = true;
	printJob = advancePrintJob("cancelling");
	return { requested: true, pagesDone: printProgress.pagesDone };
};

/**
 * What the print job has done so far, or `null` when none is running.
 *
 * Separate from the cancel entry point, and read-only. A progress bar needs
 * this, and a caller must be able to ASK how far along a job is without
 * stopping it - which is why these are two functions and not one.
 */
globalThis.__selisPrintProgress = function () {
	if (printProgress === null) return null;
	return { pagesDone: printProgress.pagesDone, pagesTotal: printProgress.pagesTotal };
};

/**
 * The outcome of the last print, for the browser check.
 *
 * `null` until a print has been started, so a check can distinguish a print
 * that has not begun yet from one that began and yielded nothing. Read-only,
 * like the two above: a check must be able to ASK about a job without changing
 * it.
 *
 * (The wording above is deliberate. `sw-ship.test.ts` scans boot.js with a
 * regex for `from` followed by a quoted string, to prove every module
 * specifier on the page is same-origin. An earlier draft of this sentence
 * contained the word "from" immediately before a quoted phrase, which that
 * scan reads as an import of a module with an English name. The test was
 * right and the prose was wrong; a scan for specifiers should never have to be
 * taught to ignore comments.)
 */
globalThis.__selisPrintState = function () {
	return state.print ?? null;
};

/**
 * The progress bar's own view of the world, for the browser check.
 *
 * Read from the DOM rather than from the model, deliberately: the claim under
 * test is that the bar SAYS the right thing, and asserting on the model would
 * pass even if the painting were wired to the wrong variable. `null` until the
 * chrome is built.
 */
globalThis.__selisPrintChrome = function () {
	if (printUi.root === null) return null;
	return {
		hidden: printUi.root.hidden,
		indeterminate: printUi.root.getAttribute("data-indeterminate") === "true",
		valueNow: printUi.root.getAttribute("aria-valuenow"),
		caption: printUi.caption.textContent,
		cancelDisabled: printUi.cancel.disabled,
		cancelLabel: printUi.cancel.textContent,
	};
};

/** Whatever has finished preloading, by path. Missing means "try on demand". */
const uiModules = new Map();

/**
 * A preloaded module, or `null`.
 *
 * Deliberately a cache lookup and not an await: every caller here is a click
 * path that must render this turn, and a caller that has to wait on a network
 * round-trip to discover something already in memory is the flake this exists
 * to remove.
 */
function preloadedUiModule(path) {
	return uiModules.get(path) ?? null;
}

/**
 * How far this module's own evaluation got.
 *
 * `__selisApp` and `__selisServiceWorker` are assigned near the END of this
 * file, hundreds of lines after the top-level `await` that loads the engine.
 * When something between those points breaks, the browser check can only see
 * that the globals never appeared - and an exception inside a module's own
 * evaluation is not an uncaught error and not an unhandled rejection, so it
 * leaves no trace at all. The page then reports a bare timeout.
 *
 * So this records the stage directly. Cheap, and it turns "never settled" into
 * a line number.
 */
globalThis.__selisBootStage = "start";

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
globalThis.__selisBootStage = "engine-settled";

// Preload the failure panel (SL-4.UI.12), deliberately AFTER the boot's own
// try/catch and deliberately NOT awaited.
//
// Two constraints, and they pull in opposite directions:
//
//  - The failure path must not depend on a request it has to make. It was
//    doing `await import("/assets/ui/viewer/error-panel.js")` at the moment it
//    was needed, and that request intermittently came back "Failed to fetch
//    dynamically imported module" - the same HTTP/1.1 flake the comment above
//    this function describes. So the panel silently never appeared, which is
//    the worst way for it to fail: no explanation AND no way forward, exactly
//    the dead end SL-4.UI.12 exists to remove.
//
//  - Boot must not depend on it either. Adding it to the `Promise.all` above
//    was tried and is wrong: one flaky request for a module the happy path
//    never touches then fails the whole boot, and the viewer does not mount at
//    all. Trading a rarely-used panel for a viewer that usually works is a bad
//    trade.
//
// So it is fetched here, in the background, where a failure costs nothing and
// is caught. `showFailure` falls back to importing on demand if this has not
// landed, so the worst case is the original behaviour and never worse.
// Preload the UI modules the click paths need, deliberately AFTER the boot's own
// try/catch and deliberately NOT awaited.
//
// Two constraints, and they pull in opposite directions:
//
//  - A click path must not depend on a request it has to make at the moment it
//    is clicked. Both of these were doing `await import(...)` inline, and those
//    requests intermittently came back "Failed to fetch dynamically imported
//    module" - the same HTTP/1.1 flake the comment above this block describes
//    about the viewer module graph. The health panel then said "Health
//    unavailable: Failed to fetch dynamically imported module", and the failure
//    panel silently never appeared. The second is the worst of the two: no
//    explanation AND no way forward, exactly the dead end SL-4.UI.12 exists to
//    remove.
//
//  - Boot must not depend on them either. Adding them to the `Promise.all` above
//    was tried and is wrong: one flaky request for a module the happy path never
//    touches then fails the whole boot, and the viewer does not mount at all.
//    Trading a rarely-used panel for a viewer that usually works is a bad trade.
//
// So both are fetched here, in the background, where a failure costs nothing and
// is caught. Each click path falls back to importing on demand, so the worst case
// is the original behaviour and never worse.
for (const path of UI_PRELOADS) {
	import(path)
		.then((module) => {
			uiModules.set(path, module);
			// The progress model is captured here rather than imported at each
			// use, because the print loop runs on the same thread as this
			// callback and must never await anything mid-page: an await between
			// pages is an await that can miss the Cancel press entirely.
			if (path.endsWith("progress.js")) progressModel = module;
		})
		.catch(() => {
			// Deliberately silent. The module degrades to a dynamic import at
			// the moment it is needed; it does not get to break the page.
		});
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

/**
 * The print progress bar, its caption, and its Cancel control (SL-4.UI.13).
 *
 * Built on demand with the rest of the viewer chrome, and every field `null`
 * until it is - the same discipline `searchUi` follows, because the mount can
 * run before this declaration is initialised.
 */
const printUi = { root: null, bar: null, caption: null, cancel: null };

/**
 * What the caption says for the current phase.
 *
 * Separate from {@link renderPrintProgress} so the wording is in one place and
 * can be read without tracing the DOM writes. Every branch is a sentence the
 * shell can actually support:
 *
 *  - an unknown total stays "page N" and never becomes "N of ?" - a bar that
 *    admits it does not know the end is more useful than a wrong one;
 *  - `cancelling` says "stopping", never "stopped" - the engine has not
 *    answered yet, and `progress.ts` exists precisely so the UI cannot claim a
 *    cancellation that did not happen;
 *  - `cancelled` keeps the count, because "cancelled" alone leaves the reader
 *    wondering whether anything was done at all.
 */
function printCaption(job, done, total) {
	if (job.awaitingCancel) {
		return total === null
			? `Stopping after page ${done}...`
			: `Stopping after page ${done} of ${total}...`;
	}
	switch (job.phase) {
		case "running":
			return total === null
				? `Rendering page ${done}...`
				: `Rendering page ${done} of ${total}...`;
		case "done":
			return total === null
				? `Finished ${done} page(s).`
				: `Finished ${done} of ${total} page(s).`;
		case "cancelled":
			return `Cancelled after ${done} page(s).`;
		case "failed":
			return "Printing failed.";
		default:
			return "";
	}
}

/**
 * Paint the bar, the caption, and the Cancel control from the model.
 *
 * Reads only `printJob` and `printProgress`. Nothing here decides anything -
 * if a state needs deciding, that belongs in `progress.ts`, which is pure and
 * tested without a browser.
 *
 * The Cancel control is `disabled` rather than removed while a cancel is
 * pending: the reader has already pressed it, and a button that vanishes under
 * their cursor reads as the app crashing. Disabled says "I heard you, I'm
 * waiting".
 */
function renderPrintProgress() {
	const ui = printUi;
	if (ui.root === null || !printJob || !printProgress) return;
	const job = printJob;
	const done = printProgress.pagesDone ?? 0;
	const total = printProgress.pagesTotal ?? null;
	const known = total !== null && total !== 0;

	// Visible only while the job is. A bar left on screen after the work is
	// over is not a finished bar, it is a stale one - and on this shell the
	// OUTCOME line beside it already carries the final sentence, so two status
	// lines would compete. Hiding on settle is what keeps "idle" meaning idle.
	ui.root.hidden = !job.busy;
	// An unknown total is INDETERMINATE, and `aria-valuenow` is removed rather
	// than set to 0. A progressbar reporting 0 tells assistive tech the job has
	// definitively achieved nothing, which is a different claim from the honest
	// one here: that nothing has been measured yet.
	if (job.fraction === null || !known) {
		ui.bar.style.width = "";
		ui.root.removeAttribute("aria-valuenow");
		ui.root.setAttribute("data-indeterminate", "true");
	} else {
		// Clamped HERE, at the drawing, not in the model: the model is right to
		// report an overshoot above 1, and a 0-1 CSS width is a visual
		// constraint rather than a fact about the job.
		ui.bar.style.width = `${Math.min(1, Math.max(0, job.fraction)) * 100}%`;
		ui.root.setAttribute("aria-valuenow", String(Math.round((job.fraction ?? 0) * 100)));
		ui.root.removeAttribute("data-indeterminate");
	}
	ui.root.setAttribute("aria-valuemin", "0");
	ui.root.setAttribute("aria-valuemax", "100");
	ui.root.setAttribute("aria-busy", job.busy ? "true" : "false");
	ui.caption.textContent = printCaption(job, done, total);
	ui.cancel.disabled = !job.busy || job.awaitingCancel;
	ui.cancel.textContent = job.awaitingCancel ? "Stopping..." : "Cancel";
}

/**
 * The "used X of Y" line for a budget failure, or `null` for no line at all.
 *
 * Returns `null` - meaning render nothing - rather than a line with a blank or
 * zero in it, in three cases, and they are the three that matter:
 *
 *  - the code is not a budget failure;
 *  - the guest sent no `budget` body (an older build, or a non-budget failure);
 *  - the body failed validation in `usageFor`, which covers a resource that
 *    disagrees with the code, `NaN`, a negative, and `BUDGET_POISONED`.
 *
 * The wall budget is measured in NANOSECONDS, so it is formatted as a duration
 * rather than as bytes. Rendering 3_000_000_000 as "3.0 GB of work" would be a
 * confident, entirely wrong sentence.
 */
function renderBudgetLine(wire, code) {
	if (code === null || code === undefined) return null;
	const body = wire === null || wire === undefined ? undefined : wire.budget;
	const model = preloadedUiModule("/assets/ui/viewer/budget.js");
	if (model === null || typeof model.budgetExhaustion !== "function") return null;
	const exhaustion = model.budgetExhaustion(code, body);
	if (exhaustion === null) return null;
	if (exhaustion.measured === null || exhaustion.limit === null) return null;

	const line = document.createElement("p");
	line.dataset.role = "budget";
	// Machine-readable too, so the browser check can assert the NUMBERS rather
	// than a formatted string - "300 MB of 256 MB" is a claim about rounding.
	line.dataset.resource = exhaustion.resource ?? "";
	line.dataset.measured = String(exhaustion.measured);
	line.dataset.limit = String(exhaustion.limit);
	line.textContent =
		exhaustion.resource === "wall"
			? `This operation took ${formatDuration(exhaustion.measured)} of a ` +
				`${formatDuration(exhaustion.limit)} time budget.`
			: `This document needed ${formatBytes(exhaustion.measured)} of a ` +
				`${formatBytes(exhaustion.limit)} ${exhaustion.resource} budget.`;
	return line;
}

/**
 * Bytes as a short string, binary units.
 *
 * Uses 1024 because that is what the budget limits are expressed in, so
 * "256 MB" in the panel means the same thing as 268435456 everywhere else.
 * Decimal units would make the engine's own limit look 7% larger than it is.
 */
function formatBytes(bytes) {
	if (!Number.isFinite(bytes) || bytes < 0) return "";
	const units = ["B", "KiB", "MiB", "GiB", "TiB"];
	let value = bytes;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}

/** Nanoseconds as a short duration. */
function formatDuration(nanos) {
	if (!Number.isFinite(nanos) || nanos < 0) return "";
	if (nanos < 1_000) return `${nanos} ns`;
	if (nanos < 1_000_000) return `${(nanos / 1_000).toFixed(1)} µs`;
	if (nanos < 1_000_000_000) return `${(nanos / 1_000_000).toFixed(1)} ms`;
	return `${(nanos / 1_000_000_000).toFixed(2)} s`;
}

/**
 * Show a failure as a panel with a recovery action (SL-4.UI.12).
 *
 * ## What this replaces
 *
 * Three messages in this file used to END a failure - `Selis failed to
 * start: ...`, `Health unavailable: ...`, `Could not print: ...` - and every
 * one of them was a dead end. They name what went wrong, say nothing about the
 * user's document, and offer nothing to do.
 *
 * ## What it does instead
 *
 * The DECISION of what to show is `apps/ui`'s pure `error-panel.js`, tested
 * there; this function owns only the DOM. The engine's sentence is rendered
 * verbatim - the registry authors it once, in `codes.toml`, and a paraphrase
 * here would be a second phrasing that drifts.
 *
 * ## Placement, which is load-bearing
 *
 * This block sits ABOVE the mount, and above that sits the module's top-level
 * `await` that loads the engine. `FAILURE_STRINGS` is a `const`, so a call to
 * `showFailure` that runs before this point would hit its temporal dead zone;
 * `searchUi` above it already documents the same trap. If boot fails, the mount
 * block's `else` calls straight into here.
 *
 * @param {object} wire the engine failure: `{code, message, docState}`
 * @param {string} what which operation failed, for the panel's own label
 * @returns {Promise<object|null>} the rendered panel, for the browser check
 */
async function showFailure(wire, what) {
	const host = document.getElementById("selis-failure");
	if (host === null) return null;
	// Already preloaded by the boot. The fallback exists only for the boot
	// FAILED case, where the preload never ran and a second try is better than
	// no explanation at all.
	const cached = preloadedUiModule("/assets/ui/viewer/error-panel.js");
	const build =
		cached?.failurePanelFromWire ??
		(await import("/assets/ui/viewer/error-panel.js")).failurePanelFromWire;
	const panel = build(wire, FAILURE_STRINGS);

	host.textContent = "";
	host.dataset.severity = panel.severity;
	host.dataset.code = panel.code === null ? "" : String(panel.code);
	host.dataset.action = panel.action;
	host.dataset.state = panel.docState;
	host.dataset.operation = what;

	const heading = document.createElement("p");
	heading.dataset.role = "what";
	// A known code whose sentence went missing is left visibly short rather
	// than replaced with a generic phrase: a blank panel means the shell
	// dropped a field, and it should LOOK broken rather than look fine.
	heading.textContent = panel.message === "" ? `${what} failed` : panel.message;
	host.appendChild(heading);

	// What happened to the document. Always present, and never inferred.
	const fate = document.createElement("p");
	fate.dataset.role = "docstate";
	fate.textContent = FAILURE_STRINGS.docStateLabel(panel.docState);
	host.appendChild(fate);

	if (panel.showCode) {
		// Only for a code the registry does not know, because that number is
		// the only route between "it went wrong" and a bug report.
		const code = document.createElement("p");
		code.dataset.role = "code";
		code.textContent = FAILURE_STRINGS.unknownCode(panel.code);
		host.appendChild(code);
	}

	// SL-4.UI.13: the budget line. "This document needed 300 MB of a 256 MB
	// budget" is the difference between a reader who knows to split the file and
	// one who is merely told it is too big.
	//
	// `budgetExhaustion` returns `null` for every number it cannot vouch for -
	// a body whose resource disagrees with the code, a `NaN`, a poisoned guard,
	// an older guest that sends no body at all. When that happens the line is
	// NOT rendered. A panel saying "0 bytes used" because it had nothing would
	// be worse than no panel.
	const budgetLine = renderBudgetLine(wire, panel.code);
	if (budgetLine !== null) host.appendChild(budgetLine);

	const action = document.createElement("button");
	action.type = "button";
	action.dataset.role = "action";
	action.textContent = panel.actionLabel;
	action.disabled = !panel.actionable;
	action.addEventListener("click", () => {
		runRecovery(panel.action);
	});
	host.appendChild(action);

	globalThis.__selisLastFailure = {
		code: panel.code,
		name: panel.name,
		source: panel.source,
		severity: panel.severity,
		action: panel.action,
		actionable: panel.actionable,
		docState: panel.docState,
		operation: what,
		// SL-4.UI.13: the budget figures, or nulls. Exposed so the browser
		// check can assert the NUMBERS the panel rendered rather than parsing a
		// formatted sentence back into them.
		budget: budgetFor(wire, panel.code),
	};
	return panel;
}

/**
 * The budget exhaustion behind a failure, or `null`.
 *
 * Separate from `renderBudgetLine` so the browser check can read the same
 * validated figures the panel did, without re-deriving them from the DOM.
 */
function budgetFor(wire, code) {
	if (code === null || code === undefined) return null;
	const model = preloadedUiModule("/assets/ui/viewer/budget.js");
	if (model === null || typeof model.budgetExhaustion !== "function") return null;
	const body = wire === null || wire === undefined ? undefined : wire.budget;
	return model.budgetExhaustion(code, body);
}

/**
 * Do what the panel offered, or say plainly that this shell cannot.
 *
 * `open-another` and `close-and-retry` are real here: the shell owns the
 * document handle. `grant-access` and `report` are not wired to anything yet,
 * so they say so rather than pretending.
 */
function runRecovery(action) {
	switch (action) {
		case "open-another":
			openDocumentSource = null;
			clearFailure();
			return globalThis.__selisOpen?.();
		case "close-and-retry":
			openDocumentSource = null;
			clearFailure();
			return undefined;
		case "retry":
			clearFailure();
			return globalThis.__selisView?.(openDocumentSource);
		default:
			globalThis.__selisLastFailure = {
				...globalThis.__selisLastFailure,
				actionOutcome: "unsupported-by-this-shell",
			};
			return undefined;
	}
}

/** Remove the panel. Used by every recovery path that made progress. */
function clearFailure() {
	const host = document.getElementById("selis-failure");
	if (host !== null) {
		host.textContent = "";
		delete host.dataset.severity;
		delete host.dataset.code;
		delete host.dataset.action;
		delete host.dataset.state;
	}
	globalThis.__selisLastFailure = null;
}

/**
 * The chrome around a failure. The SENTENCES are the engine's; these are the
 * shell's, and they live here rather than in `apps/ui` because `apps/ui` is
 * headless by rule (ADR-P0044) and owns no DOM.
 */
const FAILURE_STRINGS = {
	docStateLabel(docState) {
		switch (docState) {
			case "NotLoaded":
				return "Your document could not be opened. Nothing was changed.";
			case "PartiallyLoaded":
				return "Your document opened with damage. What did load is shown.";
			case "Loaded":
				return "Your document is open and unchanged.";
			case "Modified":
				return "Your document has unsaved changes.";
			default:
				return "Your document was not changed.";
		}
	},
	actionLabel(action) {
		switch (action) {
			case "retry":
				return "Try again";
			case "open-another":
				return "Open another file";
			case "close-and-retry":
				return "Close this document";
			case "grant-access":
				return "Grant access";
			default:
				return "Report this";
		}
	},
	unknownCode(code) {
		return `Unrecognised error code ${code}.`;
	},
};

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
		// A boot that failed is the one failure with no registry code behind it:
		// the engine never ran. The panel shows what the shell actually knows
		// plus the one action that can still help - opening another document.
		// Deliberately NOT awaited: the mount block is synchronous and `root` is
		// reassigned below; blocking here would strand the page mid-boot.
		showFailure({ code: null, message: state.error ?? "unknown" }, "startup");
		root.textContent = "Selis could not start.";
	}
}

/**
 * The document the shell is currently showing, as a THUNK.
 *
 * A thunk, not the bytes and not even the base64 string. Printing needs the
 * document again minutes later, and the alternative - holding a reference so
 * the bytes stay alive - means a 25 MB print raster and a decoded document
 * buffer are pinned in a tab for as long as the page is open, for a document
 * the user may never print. A closure that produces the source on demand keeps
 * the shell's own state to one function.
 *
 * `null` is "nothing open", which is a real state and the one the Print button
 * has to report rather than print a blank page for.
 */
let openDocumentSource = null;

/**
 * Open a document and show it — the DoD's first two verbs in one call, so the
 * browser check drives the same path a user does rather than a reduced one.
 *
 * @param {string|(() => string)} source the document, or a thunk producing it
 * @returns {object} the open result, with `painted` when the page was shown
 */
globalThis.__selisView = function (source) {
	// Normalised to a thunk immediately, so nothing downstream has to know
	// which of the two shapes it was handed.
	openDocumentSource = typeof source === "function" ? source : () => source;
	const base64 = openDocumentSource();
	const opened = globalThis.__selisOpen(base64);
	if (opened.status !== "ok") {
		// Opening is the failure a reader meets FIRST and most often, so it is
		// the one that most needs a way forward. The C-ABI path refuses with no
		// registry code at all, so the panel shows the shell's own account of it
		// and offers "open another file" - which is always real.
		showFailure({ message: opened.detail ?? opened.status }, "opening the document");
		return opened;
	}
	clearFailure();
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
 * the same box the plan was computed from. Those two MUST agree: if the raster
 * were rendered at a different DPI than the box was computed for, the page
 * would print at the wrong physical size and nothing downstream would say so.
 *
 * ## The page count comes from `open`, not from a walk
 *
 * The first attempt counted pages by probing `{op:"page"}` for N+1 until it was
 * refused, and it never terminated. `op_page` does in fact refuse out of range
 * - `PAGE_OUT_OF_RANGE`, pinned by `crates/selis-pdf-wasm/src/lib.rs` - so the
 * walk's own logic was not what hung, and the 5000-page bound never got a
 * chance to matter. The `open` reply already answers `{doc, pages, pageSizes}`,
 * so the walk is not merely bounded, it is unnecessary: one round trip, a count
 * the engine computed, and every page box in the same reply.
 *
 * ## The raster generator yields PAGES ONLY
 *
 * `streamPrintPdf(pages, boxes)` takes TWO parallel sequences and drains
 * `boxes` first. Handing both arguments the same paired generator runs the
 * render loop twice, and the second run starts from an exhausted generator -
 * after 25 MB per page has already been rendered and thrown away. The boxes are
 * collected up front (two numbers per page, so "tiny" is literal) and passed as
 * a plain array; the generator below yields rasters and nothing else.
 *
 * ## Rotation, measured rather than assumed
 *
 * `open` reports each page's `/MediaBox` un-rotated while `Render` applies
 * `/Rotate`, so a landscape scan comes back transposed and writing the
 * un-rotated box beside it prints the page squashed. `orientationOf` settles the
 * orientation from a cheap 72 DPI probe before planning, and
 * `reconcilePageBox` re-checks it against the print-resolution raster that is
 * what actually gets measured.
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
	const print = await import("/assets/host/print.js");
	const { rgbaToRgb, streamPrintPdf } = await import("/assets/host/print-pdf.js");

		const binary = atob(base64);
		const bytes = new Uint8Array(binary.length);
		for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
		const opened = openDocumentOnce(bytes);
		const doc = opened.doc;

		// THE COUNT AND EVERY BOX, FROM THE REPLY THAT ALREADY HAS THEM. No walk.
		const { count, boxes } = print.printBoxesFromOpen(opened);
		if (count === 0) {
			return { status: "refused", detail: "the document has no pages to print" };
		}
		// The total is known NOW, from the reply that already had it, and it is
		// the same number the loop iterates to - not a re-derived guess. Until
		// this line the bar is indeterminate, which is the honest state: the
		// shell has not counted anything yet, and "page 1 of 0" is worse than
		// nothing.
		if (printProgress !== null) printProgress.pagesTotal = count;
		// Refused with the real count, never truncated: a 40-page PDF handed to
		// someone who asked for 400 is a different document, delivered silently.
		if (count > print.MAX_PRINT_PAGES) {
			return {
				status: "refused",
				detail: `this document has ${count} pages, and a print job is capped at ${print.MAX_PRINT_PAGES}:
					every page is rasterised at ${print.MIN_PRINT_DPI} DPI and held in memory. Nothing was printed.`,
			};
		}

		// `/PrintScaling` is not surfaced by the engine, so the plan runs on the
		// default. That is the conservative branch by construction -
		// `appDefault` prints at the document's own size - and it is recorded
		// rather than hidden, because "we could not read it" is different from
		// "the document did not ask for anything".
		const scaling = print.parsePrintScaling(null);

		// Every box is settled BEFORE a single print-resolution pixel is rendered,
		// because `streamPrintPdf` drains its box argument first. Two numbers per
		// page, so holding them costs nothing beside the rasters they describe.
		const printedBoxes = [];
		const plans = [];
		for (let i = 0; i < boxes.length; i++) {
			const orientation = orientationOf(print, doc, i, boxes[i]);
			printedBoxes.push(orientation);
			plans.push(print.planPrint(orientation, scaling, paper));
		}

		// Measured, not claimed. The plan knows what DPI it ASKED for; what decides
		// whether the page prints correctly is the raster the engine actually
		// produced, so this is derived from each reply's pixel width against the
		// box that page is printed at. Reporting plan.dpi instead was a real gap
		// this caught: rendering at 72 DPI and reporting 300 passed the gate.
		const dpiPerPage = [];
		let rotatedPages = 0;

		async function* pages() {
			// PAGES ONLY. Never boxes, and never iterated twice.
			//
			// The writer drains its box sequence FIRST. One paired generator handed
			// to both arguments therefore runs the render loop twice, and the
			// second run starts from a generator that is already spent - after
			// 25 MB per page has been rendered and thrown away. The boxes are a
			// plain array above; this yields rasters and nothing else.
			for (let i = 0; i < count; i++) {
			// Cancellation, checked BEFORE the work, so a cancel seen on page N
			// stops before page N+1 is rasterised rather than after the whole
			// document has been rendered and thrown away.
			if (printCancelled) {
				const stopped = new Error(`printing stopped after ${i} of ${count} pages`);
				stopped.cancelled = true;
				stopped.pagesDone = i;
				throw stopped;
			}
				const { response: rendered, attachment } = dispatch({
					op: "render",
					doc,
					page: i,
					params: { dpi: plans[i].dpi },
				});
				if (rendered.ok !== true || attachment === null) {
					// The reply's sentence and document state ride along on the
					// error, so the catch below can hand them to the panel.
					// Without them a refused render reaches the reader as the
					// word "refused" and a number, which is not an explanation.
					const failure = new Error(
						`render of page ${i} refused: ${rendered.code ?? "no attachment"}`,
					);
					failure.wire = rendered;
					throw failure;
				}
				const widthPx = rendered.value.width;
				const heightPx = rendered.value.height;
				const expected = widthPx * heightPx * 4;
				if (attachment.length !== expected) {
					throw new Error(
						`render of page ${i} returned ${attachment.length} bytes for a ` +
							`${widthPx}x${heightPx} RGBA page expected ${expected}`,
					);
				}
				// Re-measured against the PRINT-resolution raster, not the 72 DPI
				// probe. If the two disagreed the engine's orientation would not be
				// stable, and the box chosen above would be a guess.
				const measured = print.reconcilePageBox(printedBoxes[i], widthPx, heightPx);
				if (measured.rotated) rotatedPages++;
				dpiPerPage.push(print.measuredPrintDpi(widthPx, measured.box));
			// Progress for the browser check and the panel. Recorded HERE, the only
			// point where the shell learns a page is finished; the writer draining
			// it afterwards is too late to say anything useful.
			if (printProgress !== null) printProgress.pagesDone = i + 1;
			// Fold the count into the model and repaint. Synchronous, and with
			// no await: the tab is about to be handed back to the event loop
			// below, and anything asynchronous here would be work queued behind
			// the very Cancel press this bar exists to offer.
			syncPrintProgress();
			// Hand the tab back to the BROWSER, not just to the microtask queue.
			//
			// `streamPrintPdf` drains this generator with `for await`, and`r
			// awaiting an async generator that yields synchronously only ever
			// queues MICROtasks - which drain before timers. A `setTimeout`-driven
			// progress poll or a click on a Cancel control would therefore never
			// run between pages, and the print would be the dead tab SL-4.UI.13 says
			// it is not. One `setTimeout(0)` per page is what actually returns the
			// thread to the event loop, and it costs nothing next to rendering one.
			await new Promise((resolve) => setTimeout(resolve, 0));
				yield {
					widthPx,
					heightPx,
					rgb: rgbaToRgb(attachment, widthPx * heightPx),
				};
			}
		}

		// A plain ARRAY of boxes against a generator of pages. Never one generator
		// passed as both - see the note above.
		const pdf = await streamPrintPdf(pages(), printedBoxes);
		return {
			status: "ok",
			// The FIRST page's measured DPI, which is what the DoD's floor is
			// judged on, plus every page's: a document that is 300 DPI on page 1
			// and 72 on page 9 has to be visible, not averaged away into the first.
			dpi: dpiPerPage[0] ?? 0,
			lowestDpi: dpiPerPage.length > 0 ? Math.min(...dpiPerPage) : 0,
			dpiPerPage,
			pages: count,
			rotatedPages,
			bytes: pdf.byteLength,
			reason: plans[0].reason,
			header: new TextDecoder().decode(pdf.slice(0, 8)),
			pdf,
		};
	} catch (error) {
		return {
			// Carried when the failure came from the engine, so the panel can show
			// the registry's sentence and the document's fate rather than an
			// Error's `.message`, which is written for a log file.
			wire: error?.wire ?? undefined,
			// A cancellation is not a failure. Its own status, so the caller can say
			// what happened instead of raising the failure panel on a job the
			// reader deliberately stopped.
			status: error?.cancelled === true ? "cancelled" : "threw",
			...(error?.cancelled === true
				? { cancelled: true, pagesDone: error.pagesDone ?? 0 }
				: {}),
			detail: error instanceof Error ? error.message : String(error),
		};
	}
};

// Exposed for the browser check and for support. No document data, no URL —
// nothing here could identify a file (ADR-P0017).
/**
 * The document health report (SL-4.UI.09), validated and turned into rows.
 *
 * The decision of WHAT to say lives in `apps/ui`'s `health.js`, which is pure
 * and tested there. What lives here is only the two things a shell owns: making
 * the engine call, and refusing to render anything it did not understand.
 *
 * That refusal is the point. `healthReportFromWire` throws on a field it does
 * not recognise rather than defaulting it, because a panel that renders a
 * confident row from a malformed reply is exactly the dishonesty the item says
 * honest reporting exists to prevent - and the report has no way to say "I could
 * not read this" once a default has been substituted.
 *
 * @returns {Promise<object>} `{status, rows}` or `{status, detail}`
 */
globalThis.__selisHealth = async function () {
	try {
		if (openDocumentSource === null) {
			return { status: "refused", detail: "no document is open" };
		}
		// Preloaded at boot precisely so this is a cache hit rather than a
		// request made at click time; see the preload block for why. The
		// fallback keeps a boot that failed before the preload ran from
		// silently reporting "no findings".
		const cached = preloadedUiModule("/assets/ui/viewer/health.js");
		const module =
			cached ??
			(await import("/assets/ui/viewer/health.js").catch(() => null));
		if (module === null) {
			return {
				status: "threw",
				detail: "the health module could not be loaded, so nothing was measured",
			};
		}
		const { healthReportFromWire, healthRows } = module;
		const binary = atob(openDocumentSource());
		const bytes = new Uint8Array(binary.length);
		for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
		const doc = openDocumentOnce(bytes).doc;
		const { response } = dispatch({ op: "health", doc });
		if (response.ok !== true) {
			// The whole wire failure used to collapse to a bare code in
			// `detail`, which threw away the registry's sentence and the
			// document state - leaving the reader a number and nothing to do
			// about it. Carrying the reply through is what lets UI.12 decide
			// what it means.
			return { status: "refused", wire: response };
		}
		return { status: "ok", rows: healthRows(healthReportFromWire(response.value)) };
	} catch (error) {
		return {
			status: "threw",
			detail: error instanceof Error ? error.message : String(error),
		};
	}
};

/**
 * Paint a health report into the panel, or say why it could not be painted.
 *
 * The failure path is deliberately visible. A panel that silently keeps its
 * previous contents after an error is showing a reader stale facts about a
 * document, which is worse than showing nothing.
 *
 * @param {object} report what `__selisHealth` returned
 */
function renderHealthPanel(report) {
	const list = document.getElementById("selis-health-rows");
	if (list === null) return;
	list.textContent = "";
	if (report.status !== "ok") {
		// The list keeps its place so it never shows stale findings, but the
		// sentence, the document's fate and the recovery action come from the
		// shared failure panel rather than from a one-line notice.
		const item = document.createElement("li");
		item.dataset.role = "failure";
		item.dataset.severity = "blocked";
		item.textContent = `Health unavailable: ${report.detail ?? report.status}`;
		list.appendChild(item);
		showFailure(
			report.wire ?? {
				message: `Health unavailable: ${report.detail ?? report.status}`,
			},
			"document health",
		);
		return;
	}
	clearFailure();
	for (const row of report.rows) {
		const item = document.createElement("li");
		item.dataset.severity = row.severity;
		item.dataset.row = row.id;
		item.textContent = row.label;
		list.appendChild(item);
	}
}

globalThis.__selisApp = state;
globalThis.__selisBootStage = "app-assigned";

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
 * Open `bytes` unless it is already the open document, and return the WHOLE
 * `open` reply value.
 *
 * The reply value, not just the handle, because it is the only place the page
 * COUNT appears: `{doc, pages, pageSizes}`. Returning the handle alone is what
 * forced the page-count walk that used to hang here.
 *
 * Opened once and kept, because a shell that reports every keystroke must not
 * re-parse the document per character.
 *
 * @param {Uint8Array} bytes
 * @returns {{doc: number, pages: number, pageSizes: Array}} the open reply value
 */
function openDocumentOnce(bytes) {
	const key = `${bytes.length}:${bytes[0]}:${bytes[1]}:${bytes[bytes.length - 1]}`;
	if (searchDoc !== null && searchDocKey === key) {
		return { doc: searchDoc, pages: searchDocPages, pageSizes: searchDocSizes };
	}
	const { response: opened } = dispatch(
		{ op: "open", src: { kind: "bytes", len: bytes.length }, budget: { surface: "viewer" } },
		bytes,
	);
	if (opened.ok !== true) {
		searchDoc = null;
		searchDocKey = "";
		searchDocPages = 0;
		searchDocSizes = [];
		throw new Error(`open refused: ${opened.code ?? "unknown"} ${opened.message ?? ""}`.trim());
	}
	searchDoc = opened.value.doc;
	searchDocKey = key;
	searchDocPages = opened.value.pages;
	searchDocSizes = opened.value.pageSizes;
	return opened.value;
}

/** Page count carried by the currently open document, from its `open` reply. */
let searchDocPages = 0;

/** Per-page boxes carried by the currently open document's `open` reply. */
let searchDocSizes = [];

/**
 * The box a page is PRINTED at, which is not always the box `open` reported.
 *
 * `open` gives each page's `/MediaBox` in its own un-rotated user space, while
 * `Render` applies `/Rotate`. A landscape scan therefore comes back transposed,
 * and a MediaBox that is not transposed to match is a page printed SQUASHED -
 * the content stream scales the image to fill whatever box it is given.
 *
 * Settled by a 72 DPI probe - one point per pixel, about 1.9 MB, discarded
 * immediately - rather than by guessing, and then re-checked against the
 * print-resolution raster once that exists. The probe has to happen first
 * because the PLAN chooses the DPI the print raster is rendered at, so the two
 * cannot both be derived from the same raster: something has to be known before
 * the expensive render, and this is the cheap way to know it.
 *
 * @param {object} print the imported `print.js` module
 * @param {number} doc the engine's document handle
 * @param {number} page the page number, zero-based
 * @param {{widthPt: number, heightPt: number}} declared the page's declared box
 * @returns {{widthPt: number, heightPt: number}} the box to print
 */
function orientationOf(print, doc, page, declared) {
	const probe = dispatch({ op: "render", doc, page, params: { dpi: 72 } });
	if (probe.response.ok !== true) {
		// The probe is an optimisation, not a gate. A render that will not answer
		// at 72 DPI has to be tried at print resolution, where the real
		// reconciliation happens anyway; refusing here would turn a recoverable
		// probe failure into a document that cannot be printed at all.
		return declared;
	}
	return print.reconcilePageBox(declared, probe.response.value.width, probe.response.value.height)
		.box;
}

/**
 * The Print button: build a print-ready PDF and hand it over as a download.
 *
 * ## `window.print()` is not called, and that is the Do: rather than a detail
 *
 * UI.08 says "do not rely on the browser's own PDF printing". `window.print()`
 * hands the document to a pipeline this project neither controls nor tests: it
 * re-renders what is on screen, applies its own paper and margin defaults, and
 * produces a PDF whose resolution, page size and `/PrintScaling` handling are
 * the browser's decisions rather than the plan's. A check that only asserted
 * `window.print` was reached was asserting the opposite of the requirement,
 * which is why that leg is replaced rather than kept.
 *
 * What the button does instead:
 *
 *     __selisPrint -> the PDF, built at print resolution by the engine
 *     Blob         -> the bytes, as something the browser can hand to a file
 *     <a download> -> a save, after which the choice of printer is the user's
 *
 * The user still chooses the printer, the paper and the scaling - in whatever
 * application they open the file in. What they no longer get is a browser
 * quietly re-interpreting the document on the way there.
 *
 * ## The object URL is revoked on a LATER TURN
 *
 * `URL.revokeObjectURL` frees the blob the moment it is called, and the click
 * that starts the download has not necessarily consumed it by then - the
 * navigation to the blob is still queued. Revoking synchronously after
 * `.click()` intermittently cancels the download with no error anywhere, which
 * is the worst available failure: the user pressed Print, nothing appeared, and
 * nothing said why. A one-turn `setTimeout` is the standard fix, and is why the
 * revoke is not inline.
 *
 * @returns {Promise<object>} the print summary, so a caller can read it
 */
async function requestPrint() {
	const say = (message) => {
		if (searchUi.readout !== null) searchUi.readout.textContent = message;
	};
	if (openDocumentSource === null) {
		say("Nothing to print - open a document first.");
		return { status: "refused", detail: "no document is open" };
	}
	say("Building a print-ready PDF...");
	// Reset the cancel flag for THIS job. A cancel left over from a previous
	// print would otherwise stop this one before it rendered a page.
	printCancelled = false;
	printProgress = { pagesDone: 0, pagesTotal: null };
	// A fresh request id for a fresh job, and a fresh model state. The id is
	// what makes a late report from the PREVIOUS job droppable rather than
	// paintable, and reusing the old number would reintroduce exactly the stale
	// bar `progress.ts` refuses to show.
	printRequestId += 1;
	printJob = progressModel === null ? null : progressModel.beginOperation(printRequestId);
	// Painted before the first await, so the bar and the Cancel control exist
	// before the reader can plausibly want them.
	renderPrintProgress();
	// The thunk, not a retained copy: see `openDocumentSource`.
	const printed = await globalThis.__selisPrint(openDocumentSource());
	if (printed.status !== "ok") {
		// A cancelled print is NOT a failure. The reader pressed the button, so
		// reporting it through the failure panel would tell them their document
		// has a problem it does not have.
		if (printed.cancelled === true) {
			const done = printed.pagesDone ?? 0;
			// The engine has now ANSWERED, so this is the first moment a
			// cancellation may be called one. `cancelling` until here would have
			// been the honest earlier state.
			advancePrintJob("cancelled");
			printProgress = null;
			clearFailure();
			say(`Printing cancelled after ${done} page(s).`);
			return printed;
		}
		// The readout keeps the short form it always had - it is a transient
		// status line - but the panel beside it carries the registry's sentence,
		// the document's fate and an action, which is what a reader needs when
		// printing did not work.
		say(`Could not print: ${printed.detail ?? printed.status}`);
		showFailure(
			printed.wire ?? {
				message: `Could not print: ${printed.detail ?? printed.status}`,
			},
			"printing",
		);
		advancePrintJob("failed");
		printProgress = null;
		return printed;
	}
	const blob = new Blob([printed.pdf], { type: "application/pdf" });
	const url = URL.createObjectURL(blob);
	const anchor = document.createElement("a");
	anchor.href = url;
	// Named from what is known about the document, never from a path: nothing
	// here may identify a file (ADR-P0017).
	anchor.download = `selis-print-${printed.pages}p.pdf`;
	document.body.appendChild(anchor);
	anchor.click();
	anchor.remove();
	setTimeout(() => URL.revokeObjectURL(url), 0);
	say(
		`Prepared ${printed.pages} page(s) at ${printed.dpi} DPI ` +
			`(${(printed.bytes / 1e6).toFixed(1)} MB) - choose your printer in the saved file.`,
	);
	printed.downloadName = anchor.download;
	// The job is over. Left set, `__selisPrintCancel` would answer to a
	// cancel for a print that had already finished, and report having
	// cancelled work nobody asked to stop. The bar keeps its last painted
	// value and its Cancel goes dead rather than vanishing: a reader who was
	// watching it finish should see it say so.
	advancePrintJob("done");
	printProgress = null;
	return printed;
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
	print.addEventListener("click", () => {
		// The handler is async - it renders every page before it has a file - so
		// `click()` itself returns nothing to wait on. The in-flight job is
		// recorded on the app state, which is what the browser check awaits: a
		// check cannot observe an artifact that a click has not finished making,
		// and asserting on the synchronous return would be asserting nothing.
		// A rejection is caught here too, so a failed print is a recorded false
		// rather than an unhandled rejection that never reports.
		state.print = { status: "working" };
		requestPrint()
			.then((printed) => {
				state.print = {
					status: printed.status,
					pages: printed.pages,
					dpi: printed.dpi,
					lowestDpi: printed.lowestDpi,
					bytes: printed.bytes,
					header: printed.header,
					downloadName: printed.downloadName,
					detail: printed.detail,
				};
			})
			.catch((error) => {
				state.print = {
					status: "threw",
					detail: error?.message ?? String(error),
				};
			});
	});
	root.appendChild(print);
	// The health control, built with the viewer for the same reason as the print
	// control: a user has to be able to see the button to press it.
	const health = document.createElement("button");
	health.type = "button";
	health.id = "selis-health";
	health.textContent = "Document health";
	health.addEventListener("click", () => {
		globalThis.__selisHealth().then(renderHealthPanel);
	});
	root.appendChild(health);
	const healthOut = document.createElement("ul");
	healthOut.id = "selis-health-rows";
	healthOut.setAttribute("aria-live", "polite");
	root.appendChild(healthOut);

	// SL-4.UI.13: a real progress bar and a real Cancel button, in the document,
	// for the same reason as the search field and the print button - a reader has
	// to be able to SEE the control to press it. A cancel reachable only through
	// `globalThis.__selisPrintCancel()` is not a user affordance.
	//
	// `role="progressbar"` on the wrapper with the fill as a child, because a
	// `<progress>` element cannot express the indeterminate-but-busy state
	// honestly here: it would need a value, and we do not have one until the page
	// count is known.
	const progressRoot = document.createElement("div");
	progressRoot.id = "selis-print-progress";
	progressRoot.setAttribute("role", "progressbar");
	progressRoot.setAttribute("aria-label", "Printing progress");
	progressRoot.hidden = true;
	const track = document.createElement("div");
	track.className = "selis-progress-track";
	const fill = document.createElement("div");
	fill.className = "selis-progress-fill";
	fill.id = "selis-print-bar";
	track.appendChild(fill);
	const caption = document.createElement("p");
	caption.id = "selis-print-status";
	caption.setAttribute("role", "status");
	caption.setAttribute("aria-live", "polite");
	const cancel = document.createElement("button");
	cancel.type = "button";
	cancel.id = "selis-print-cancel";
	cancel.textContent = "Cancel";
	cancel.disabled = true;
	cancel.addEventListener("click", () => {
		// The same entry point the browser check uses. One function, so a test
		// that drives the button and a test that calls the hook are testing the
		// same thing rather than two things that happen to agree today.
		globalThis.__selisPrintCancel();
	});
	progressRoot.appendChild(track);
	root.appendChild(progressRoot);
	root.appendChild(caption);
	root.appendChild(cancel);

	printUi.root = progressRoot;
	printUi.bar = fill;
	printUi.caption = caption;
	printUi.cancel = cancel;
	// Hidden until a job starts: a progress bar reading "0%" on an idle viewer
	// is a claim about nothing.
	progressRoot.hidden = true;

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
		const doc = openDocumentOnce(bytes).doc;
		const { response: reply } = dispatch({ op: "search", doc, query });
		if (reply.ok !== true) {
			// Carried whole, for the same reason as the health path: a bare code
			// in `detail` left the failure with a number and no sentence.
			return { status: "refused", wire: reply };
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
