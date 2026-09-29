/* Selis MV3 service worker (SL-4.EXT.01 + SL-4.EXT.02 + SL-4.EXT.06 + EXT.07).
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
import { createOnboardingStore, planFirstRun } from "./extension/src/options-state.js";
import { buildRedirectRules, isFileRule } from "./extension/src/permissions.js";

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
 *
 * **SL-4.EXT.09 adds a second attempt, and the reason is worth stating.** The
 * ruleset now carries a `file://` rule that only fires once the user has granted
 * access to local files. A rejected `updateDynamicRules` takes the whole call
 * with it, so if any Chrome build refuses that rule while the grant is withheld,
 * the two http(s) rules - the ones that intercept every web PDF, and the reason
 * this extension exists - would go down with it. A viewer that stopped opening
 * web PDFs because of an *optional* local-file feature is strictly worse than
 * one that never learns to open local files, so the retry drops exactly the file
 * rule and keeps the rest. The first error is logged either way, because a
 * browser that needed the fallback is a browser someone should hear about.
 */
async function installInterception() {
	const rules = buildRedirectRules();
	try {
		await chrome.declarativeNetRequest.updateDynamicRules({
			removeRuleIds: rules.map((rule) => rule.id),
			addRules: rules,
		});
	} catch (error) {
		console.warn("selis: PDF interception rules not installed", error);
		const withoutFile = rules.filter((rule) => !isFileRule(rule));
		try {
			await chrome.declarativeNetRequest.updateDynamicRules({
				removeRuleIds: rules.map((rule) => rule.id),
				addRules: withoutFile,
			});
			console.warn(
				"selis: installed the web-PDF interception rules without the local-file rule; local files will not open in the viewer until this is fixed",
			);
		} catch (retryError) {
			console.warn("selis: no interception rules could be installed", retryError);
		}
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

/**
 * SL-4.EXT.07: offer the welcome guide, once, on a fresh install.
 *
 * **`chrome.runtime.openOptionsPage()`, and nothing else.** It is the one way
 * to reach this extension's own page that needs no permission at all, and it
 * cannot be pointed anywhere: there is no URL to get wrong, no origin to
 * mistype, and no `chrome.tabs` — which the EXT.01 review denied and which
 * this task has no reason to reconsider. `chrome.tabs.create` would also have
 * worked without the permission (that permission widens *reads* of tab
 * properties, not tab creation), but `openOptionsPage` says what it means:
 * "show the page this extension declared".
 *
 * The decision is `planFirstRun`'s, and the two ways this can go wrong are both
 * covered there: a guide shown on every update is a nag, and a flag that fails
 * closed hides the guide forever. Neither is repaired here, because this
 * function has no opinion to have — it asks.
 *
 * The failure mode is silence with a log line, not a thrown install handler. A
 * first-run guide is worth opening a tab for and is worth nothing at all next
 * to a half-installed extension, so an unreadable store or a browser without
 * `openOptionsPage` leaves the extension working.
 */
async function offerWelcomeGuide(details) {
	try {
		const { seen } = await createOnboardingStore().read();
		const plan = planFirstRun({ reason: details.reason, seen });
		if (!plan.show) {
			console.info("selis: welcome guide not offered", plan.outcome);
			return;
		}
		if (typeof chrome.runtime.openOptionsPage !== "function") {
			console.warn(
				"selis: this browser cannot open an options page; the guide is in the extension's options page",
			);
			return;
		}
		await chrome.runtime.openOptionsPage();
	} catch (error) {
		console.warn("selis: the welcome guide could not be offered", error);
	}
}

chrome.runtime.onInstalled.addListener((details) => {
	void installInterception();
	void offerWelcomeGuide(details);
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
