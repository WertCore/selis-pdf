/* Selis offscreen engine host (SL-4.EXT.01 + SL-4.EXT.06 + SL-4.EXT.03).
 *
 * This document exists so the engine outlives any single viewer page: an MV3
 * service worker is killed after ~30 s idle and has no DOM, and a Worker
 * spawned by the viewer dies with that page. Document bytes are allowed here
 * and nowhere else (24-BINDINGS-SPEC section 5).
 *
 * Chrome delivers every `chrome.runtime.connect({ name })` to the extension
 * contexts holding an `onConnect` listener, so this document is the listener
 * and each viewer page is an initiator. The service worker deliberately
 * registers no listener: a port carrying document bytes must never be handed
 * to the context the browser kills mid-session.
 *
 * One engine serves every page. `startSharedEngineHost` builds it once and
 * gives each delivered port its own serve loop, so a second tab opens its
 * document on the same compiled guest.
 *
 * SL-4.EXT.03: the engine is real now. It is a WASM guest owned by a Worker
 * this document spawns, so a long render does not stop the port from being
 * serviced and a `cancel` from the viewer can still reach the engine
 * mid-render. The Worker and the WASM are both package resources read from
 * this origin: no host permission, no remote code (ADR-P0028).
 *
 * No remote code, no eval: every import below is a compiled module from this
 * same package. */

import {
	CORE_CHUNK_PATH,
	ENGINE_WORKER_PATH,
	startSharedEngineHost,
} from "./extension/src/ext/engine-host.js";
import { createPortLink, hostPortOf } from "./extension/src/ext/engine-link.js";
import { ENGINE_HOST_READY, ENGINE_PORT_NAMES } from "./extension/src/ext/engine-protocol.js";

const host = startSharedEngineHost({
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

// The receiver half of the link. One serve loop per viewer page, all on the
// shared engine; a port with any other name is left to disconnect, because
// nothing in this package connects with another.
chrome.runtime.onConnect.addListener((port) => {
	if (port.name !== ENGINE_PORT_NAMES.request) {
		return;
	}
	host.serve(createPortLink(hostPortOf(port), "requests"));
});

// The listener above is the thing the worker waits for. `createDocument`
// resolves before this module has run, and `connect` into a context that is
// not listening is dropped rather than queued, so the worker answers the
// viewer's `ensure` only once this announcement arrives.
void chrome.runtime.sendMessage({ type: ENGINE_HOST_READY }).catch(() => {
	// A worker that is not there to hear it is not a failure: the next
	// `ensure` sees the document via `getContexts` and returns immediately.
});

// Closing the document is the only teardown there is: it terminates the
// engine Worker, releases the guest's heap and drops every serve loop.
globalThis.addEventListener("pagehide", () => {
	host.stop();
});
