/* Selis web boot (SL-4.WEB.01 shell + SL-4.WEB.02 offline).
 *
 * The full UI bundle lands in SL-4.UI.02+; this file is the boot path that
 * exists today, and it does the one thing the offline layer needs from the
 * page: register the service worker. No eval, no remote fetch, no document
 * bytes.
 *
 * Registration is fire-and-forget and failure-tolerant on purpose. A browser
 * that refuses it (no container, `file://`, an insecure context) still gets a
 * working viewer - it simply is not offline-ready, and `registerServiceWorker`
 * says which condition applied rather than throwing. Nothing here awaits the
 * engine or the document.
 *
 * Integrity-pinned in `public/index.html`; recompute the digest when this file
 * changes (`src/sri.ts`'s `computeSri`, and `sw-ship.test.ts` checks it).
 */
import { registerServiceWorker } from "/sw-register.js";

globalThis.__selisBoot = "web-host";

registerServiceWorker().then((result) => {
	// Exposed for the DoD's manual airplane-mode check and for support: which of
	// "registered", "no container", "insecure context" this session got. No
	// document data, no URL - nothing here could identify a file (ADR-P0017).
	globalThis.__selisServiceWorker = result.ok
		? { registered: true, scope: result.scope }
		: { registered: false, reason: result.reason };
});
