/* Selis MV3 service worker (SL-4.EXT.01 + SL-4.EXT.02 + SL-4.EXT.06).
 *
 * Event-driven, no DOM, no remote code (ADR-P0028). Document bytes never transit
 * this worker (24-BINDINGS-SPEC §5) - see the note on the message handler.
 *
 * There is deliberately NO `chrome.runtime.onConnect` listener here. Chrome
 * delivers a `runtime.connect` to every extension context that has one, so
 * registering one would hand this worker a port carrying base64 document bytes
 * and base64 tiles, in a context the browser kills after ~30 s idle. The
 * offscreen document listens instead (`offscreen.js`). */

import { ENSURE_ENGINE_HOST } from "./extension/src/ext/engine-protocol.js";
import { buildRedirectRules } from "./extension/src/permissions.js";

/** The page that hosts the engine, and the one it is created for. */
const OFFSCREEN_PATH = "offscreen.html";

/**
 * SL-4.EXT.02: install the PDF navigation interception ruleset.
 *
 * `updateDynamicRules` (not `updateSessionRules`) so the redirect survives
 * service-worker termination - MV3 kills this worker aggressively, and session
 * rules die with it. A failure here must not break install: an extension with
 * no redirect rules still works as a toolbar viewer, whereas a thrown install
 * leaves a half-installed extension. So the error is logged and swallowed
 * deliberately.
 */
async function installInterception() {
	try {
		const rules = buildRedirectRules();
		await chrome.declarativeNetRequest.updateDynamicRules({
			removeRuleIds: rules.map((rule) => rule.id),
			addRules: rules,
		});
	} catch (error) {
		console.warn("selis: PDF interception rules not installed", error);
	}
}

/**
 * Is an offscreen document already running?
 *
 * `chrome.runtime.getContexts` arrived in Chrome 116 and this extension's floor
 * is 114, so it is used when present and the caller falls back to catching
 * Chrome's "only a single offscreen document may be created" rejection when it
 * is not. The floor is not being raised for this: a viewer that works on 114 is
 * worth more than a tidier branch.
 */
async function hasOffscreenDocument() {
	const getContexts = chrome.runtime.getContexts;
	if (typeof getContexts !== "function") {
		return false;
	}
	const contexts = await getContexts({ contextTypes: ["OFFSCREEN_DOCUMENT"] });
	return contexts.length > 0;
}

/**
 * SL-4.EXT.06: make sure one offscreen document exists.
 *
 * Only the service worker may call `chrome.offscreen.createDocument`, which is
 * why the viewer page has to ask - and why the request it sends is a verb with
 * no payload. The document URL, when there is one, is fetched by the page and
 * travels on the engine port directly to the offscreen document; it never comes
 * through here.
 *
 * The reason is `BLOBS`: the engine is handed document bytes and reads them as
 * blobs. `WORKERS` would also be defensible, and `BLOBS` is the one that
 * describes what actually happens. Chrome requires the justification to be
 * truthful, and the store review reads it.
 */
async function ensureEngineHost() {
	if (await hasOffscreenDocument()) {
		return;
	}
	try {
		await chrome.offscreen.createDocument({
			url: OFFSCREEN_PATH,
			reasons: ["BLOBS"],
			justification:
				"Hold the PDF engine in a document that outlives the viewer tab, so an open document survives the page being closed.",
		});
	} catch (error) {
		// The single-document limit is a race, not a failure: another context
		// created it between the check and the call, which is the outcome we
		// wanted anyway.
		if (!String(error).includes("Only a single offscreen")) {
			throw error;
		}
	}
}

chrome.runtime.onInstalled.addListener(() => {
	// EXT.07 onboarding wires first-run here.
	void installInterception();
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
	if (typeof message !== "object" || message === null || message.type !== ENSURE_ENGINE_HOST) {
		return false;
	}
	ensureEngineHost().then(
		() => sendResponse({ ok: true }),
		(error) => sendResponse({ ok: false, message: String(error) }),
	);
	// Keep the channel open for the async reply.
	return true;
});
