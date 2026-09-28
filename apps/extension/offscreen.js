/* Selis offscreen engine host (SL-4.EXT.01 + SL-4.EXT.06 + SL-4.EXT.03).
 *
 * This document exists so the engine outlives any single viewer page: an MV3
 * service worker is killed after ~30 s idle and has no DOM, and a Worker
 * spawned by the viewer dies with that page. Document bytes are allowed here
 * and nowhere else (24-BINDINGS-SPEC section 5).
 *
 * It connects a `chrome.runtime` port purely to LISTEN on the engine port
 * name. Chrome delivers a `runtime.connect` to every extension context
 * holding an `onConnect` listener, the service worker included - and the
 * service worker deliberately registers none, so a port carrying document
 * bytes is never delivered to a context that can be torn down mid-session.
 *
 * SL-4.EXT.03: the engine is real now. It is a WASM guest owned by a Worker
 * this document spawns, so a long render does not stop the port from being
 * serviced and a `cancel` from the viewer can still reach the engine
 * mid-render. The Worker and the WASM are both package resources read from
 * this origin: no host permission, no remote code (ADR-P0028).
 *
 * No remote code, no eval: every import below is a compiled module from this
 * same package. */

import { createPortLink } from "./extension/src/ext/engine-link.js";
import {
	CORE_CHUNK_PATH,
	ENGINE_WORKER_PATH,
	startEngineHost,
} from "./extension/src/ext/engine-host.js";
import { ENGINE_PORT_NAMES } from "./extension/src/ext/engine-protocol.js";

const link = createPortLink(chrome.runtime.connect({ name: ENGINE_PORT_NAMES.accept }));

const host = startEngineHost({
	link,
	// A module Worker from this package, by a literal path. The compiled
	// script is a ship-list row, so the bundled-only gate knows about it;
	// `chrome.runtime.getURL` is what turns that path into a same-origin URL
	// an extension page may read without a host permission.
	createWorker: () => {
		const worker = new Worker(chrome.runtime.getURL(ENGINE_WORKER_PATH), {
			type: "module",
			name: "selis-engine",
		});
		// A `Worker`'s own method is `postMessage`; the engine is written
		// against the narrower `post`, so the two are bridged here rather than
		// in a wrapper the bundler-less package would then have to ship.
		return {
			post: (message, transfer) => worker.postMessage(message, transfer ?? []),
			onMessage: (listener) => {
				const handler = (event) => listener(event.data);
				worker.addEventListener("message", handler);
				return () => worker.removeEventListener("message", handler);
			},
			onError: (listener) => {
				const handler = (event) => listener(new Error(event.message));
				worker.addEventListener("error", handler);
				return () => worker.removeEventListener("error", handler);
			},
			terminate: () => worker.terminate(),
		};
	},
	wasmUrl: chrome.runtime.getURL(CORE_CHUNK_PATH),
});

// Closing the document is the only teardown there is: the port drops with it,
// and `stop()` is what closes every document still open on the far side,
// terminates the engine Worker, and releases the guest's heap. Before
// SL-4.EXT.03 there was nothing to terminate, which is why the comment here
// used to say there was nothing worth tearing down.
globalThis.addEventListener("pagehide", () => {
	host.stop();
	link.close();
});