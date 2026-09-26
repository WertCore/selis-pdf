/* Selis MV3 service worker skeleton (SL-4.EXT.01). Event-driven, no DOM, no remote code (ADR-P0028). PDF interception rules land in SL-4.EXT.02; the offscreen engine host lands in SL-4.EXT.03. Document bytes never transit this worker (24-BINDINGS-SPEC §5). */

chrome.runtime.onInstalled.addListener(() => {
	// EXT.07 onboarding wires first-run here. No-op until then.
});
