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

// Exposed for the browser check and for support. No document data, no URL —
// nothing here could identify a file (ADR-P0017).
globalThis.__selisApp = state;

const root = document.getElementById("selis-app");
if (root !== null) {
	root.removeAttribute("aria-busy");
	if (state.mounted) {
		root.textContent =
			`Selis is ready. Viewer mounted (${state.engine?.exports ?? 0} engine exports). ` +
			"Page rendering lands with the UI host wiring.";
	} else {
		root.textContent = `Selis failed to start: ${state.error ?? "unknown"}`;
	}
}

// ── the offline layer ──────────────────────────────────────────────────────

registerServiceWorker().then((result) => {
	// Which of "registered", "no container", "insecure context" this session
	// got, for the DoD's airplane-mode check and for support.
	globalThis.__selisServiceWorker = result.ok
		? { registered: true, scope: result.scope }
		: { registered: false, reason: result.reason };
});
