/* Selis MV3 service worker (SL-4.EXT.01 + SL-4.EXT.02). Event-driven, no DOM, no remote code (ADR-P0028). The offscreen engine host lands in SL-4.EXT.03. Document bytes never transit this worker (24-BINDINGS-SPEC §5). */

import { buildRedirectRules } from "./src/permissions.js";

/**
 * SL-4.EXT.02: install the PDF navigation interception ruleset.
 *
 * `updateDynamicRules` (not `updateSessionRules`) so the redirect survives
 * service-worker termination — MV3 kills this worker aggressively, and session
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

chrome.runtime.onInstalled.addListener(() => {
	// EXT.07 onboarding wires first-run here. No-op until then.
	void installInterception();
});
