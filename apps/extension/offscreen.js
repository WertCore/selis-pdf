/* Selis offscreen engine host (SL-4.EXT.01 + SL-4.EXT.06).
 *
 * This document exists so the engine outlives any single viewer page: an MV3
 * service worker is killed after ~30 s idle and has no DOM, and a Worker
 * spawned by the viewer dies with that page. Document bytes are allowed here and
 * nowhere else (24-BINDINGS-SPEC §5).
 *
 * It connects a `chrome.runtime` port purely to LISTEN on the engine port name.
 * Chrome delivers a `runtime.connect` to every extension context holding an
 * `onConnect` listener, the service worker included - and the service worker
 * deliberately registers none, so a port carrying document bytes is never
 * delivered to a context that can be torn down mid-session.
 *
 * The engine is null until SL-4.EXT.03 installs it. The host still answers in
 * that state, with a registry-coded refusal, because an unanswered request is
 * indistinguishable from a hung engine and would leave the viewer spinning.
 *
 * No remote code, no eval (ADR-P0028): every import below is a compiled module
 * from this same package. */

import { createPortLink } from "./extension/src/ext/engine-link.js";
import { ENGINE_PORT_NAMES } from "./extension/src/ext/engine-protocol.js";
import { serveEngineHost } from "./extension/src/ext/offscreen-engine.js";

/** SL-4.EXT.03 replaces this with the WASM engine. */
const engine = null;

const link = createPortLink(chrome.runtime.connect({ name: ENGINE_PORT_NAMES.accept }));
const stop = serveEngineHost({ link, engine });

// Closing the document is the only teardown there is: the port drops with it,
// and `serveEngineHost`'s disposer is what closes every document still open on
// the far side. Nothing to do here until EXT.03 gives this document a lifecycle
// worth tearing down.
globalThis.addEventListener("pagehide", () => {
	stop();
	link.close();
});
