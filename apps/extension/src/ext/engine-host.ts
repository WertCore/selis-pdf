/**
 * SL-4.EXT.03 - the offscreen document's composition root.
 *
 * EXT.06 shipped the host with `engine: null` and a coded refusal, because
 * there was no engine to install. This is the file that installs one: it
 * spawns the Worker that owns the WASM guest, builds the `EnginePort` over it,
 * and hands that to the host half of the JSON transport.
 *
 * ## Why the Worker and the two URLs are passed in
 *
 * `new Worker(...)` and `chrome.runtime.getURL(...)` are the only two platform
 * touches this arrangement needs, and both stay in `offscreen.js` - a
 * hand-written, reviewable root file. Everything below is ordinary TypeScript
 * that a Node test can drive, which is why the test for "the viewer survives
 * service-worker termination" is a test of this file rather than of a
 * browser.
 *
 * ## The lifecycle, which is this task's actual subject
 *
 * MV3 kills a service worker after roughly 30 seconds of idleness, and it has
 * no DOM. Three consequences shape what is here:
 *
 * - **The service worker owns nothing.** It creates the offscreen document and
 *   then forgets it. Nothing in this module reads state from it, imports from
 *   it, or depends on it still being alive - so its termination is not an
 *   event this host can observe, let alone be broken by. The engine lives
 *   here, in a document whose lifetime is its own.
 * - **The engine is warmed, not lazy, once the document exists.** A viewer
 *   that opens a document should not pay for a WASM compile first, and a
 *   viewer that opens nothing should not either - so the warm-up is
 *   fire-and-forget and its failure is deliberately swallowed, because the
 *   first request reports the same failure with the same message.
 * - **Closing the document is the only teardown.** The page that owned the
 *   documents is gone by then, and `serveEngineHost` releases them on
 *   disconnect; what is left is the Worker, and `dispose()` terminates it.
 */

import type { EnginePort } from "../../../ui/src/platform/adapter.js";
import type { EngineLink } from "./engine-link.js";
import { serveEngineHost } from "./offscreen-engine.js";
import { type EngineChannel, createWasmEngine } from "./wasm-engine.js";
import { CORE_CHUNK_PATH } from "./wasm-worker.js";

/**
 * The compiled Worker script, as a path inside the package.
 *
 * A package-relative literal rather than `new URL("./wasm-worker.js",
 * import.meta.url)`: this module lives in `extension/src/ext/`, while the
 * Worker is loaded by the document at the package root, and a computed specifier
 * is exactly the shape the bundled-only gate cannot resolve. A test asserts
 * this string is on the ship list, so a file that is loaded but not shipped
 * fails here rather than 404ing in the browser.
 */
export const ENGINE_WORKER_PATH = "extension/src/ext/wasm-worker.js";

/**
 * The channel `createWasmEngine` needs, as a real `Worker` presents it.
 *
 * `post`, not `postMessage`: a `Worker`'s own method is named `postMessage`
 * and the engine is written against the narrower name, so the adapter is a
 * one-line lambda in the root file rather than a wrapper class. Naming it
 * here is what lets a test hand over a plain object.
 */
export type WorkerLike = EngineChannel;

/** What `startEngineHost` hands back, so a caller can tear it down. */
export interface EngineHost {
	/** The engine, for the host half of the transport to serve. */
	readonly engine: EnginePort;
	/** Stop serving, release every open document, and terminate the Worker. */
	stop(): void;
	/** Resolves when the engine has compiled, or rejects with why it did not. */
	ready(): Promise<void>;
}

/**
 * Serve `link` from a Worker-owned WASM engine.
 *
 * `createWorker` is called once. It is a factory rather than a `Worker` so
 * that this file never names a browser global, and so a test can hand over a
 * channel it controls.
 */
export function startEngineHost(options: {
	link: EngineLink;
	createWorker: () => WorkerLike;
	/** Absolute URL of the core WASM chunk, inside the package. */
	wasmUrl: string;
}): EngineHost {
	const { link, createWorker, wasmUrl } = options;
	const worker = createWorker();
	const engine = createWasmEngine({ channel: worker, wasmUrl });
	const stopHost = serveEngineHost({ link, engine });

	// Warm the compile. Deliberately not awaited and deliberately not
	// reported: the first request that needs the engine surfaces the same
	// failure with the same message, and an unhandled rejection here would be
	// a second, less informative report of one problem.
	const ready = engine.start();
	void ready.catch(() => {});

	return {
		engine,
		ready: () => ready,
		stop() {
			stopHost();
			engine.dispose();
		},
	};
}

/** The core chunk's package path, re-exported for the root file to resolve. */
export { CORE_CHUNK_PATH };
