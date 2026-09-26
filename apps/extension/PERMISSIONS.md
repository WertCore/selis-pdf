# Extension permissions — justification record (SL-4.EXT.01)

Every permission in `manifest.json` needs a written justification for Chrome
Web Store review and for the store listing: reviewers reject unexplained
breadth (14-PHASE-4-web-alpha.md, SL-4.EXT.01). This file is that record. Keep
it in sync with `manifest.json` — `src/manifest.test.ts` fails the build when
a permission ships without a row here, or when a row here has no matching
manifest entry.

## Requested permissions (minimal set)

| Permission | Where | Why it is needed | Why nothing narrower works | Store-listing wording |
|---|---|---|---|---|
| `declarativeNetRequest` | `permissions` | Redirect `application/pdf` main-frame navigations to the bundled viewer page (SL-4.EXT.02, ADR-P0028). Declarative rules are evaluated by the browser — the extension never sees document bytes in the service worker (24-BINDINGS-SPEC §5). | This _is_ the narrow API: it replaces the broad `webRequest`/`webRequestBlocking` interception path. No rule evaluation happens in extension JS. | "Intercepts PDF navigations so they open in the bundled viewer." |
| `offscreen` | `permissions` | Host the WASM engine in an offscreen document (SL-4.EXT.03). MV3 service workers are killed aggressively; parsing/rendering needs a document-scoped lifetime with state recovery. | A service worker cannot hold engine state reliably and a content script must never see document bytes. The offscreen document is the only MV3 surface with a DOM-capable, long-lived context that is still same-extension-origin. | "Runs the local PDF engine in a hidden extension page." |

## Deliberately NOT requested

| Permission / pattern | Status | Reason |
|---|---|---|
| `host_permissions` (`<all_urls>`, `*://*/*`, per-origin) | **Empty in EXT.01** | Interception targets (SL-4.EXT.02 DNR rules) are not settled yet. Host access is added there with one rule-set entry + one justification row per origin pattern — never a pre-emptive `<all_urls>`. Review history shows broad hosts without a wired rule are the top rejection cause. |
| `webRequest` / `webRequestBlocking` | Not requested | Superseded by `declarativeNetRequest` for this use (see above). Firefox port (SL-4.EXT.10) may need `webRequest` under its MV3 — that port carries its own justification row when it lands. |
| `tabs`, `activeTab`, `scripting`, `cookies`, `storage` (unlimited), `file://` pseudo-host | Not requested | No tab inspection, no script injection, no cookie access, no bulk storage in EXT.01. `file://` support (SL-4.EXT.09) is an explicit user-toggled flow with its own onboarding copy — not a silent manifest entry. `storage` (unlimited) arrives only if the CJK post-install payload (SL-4.EXT.05) proves it needs more than the default quota. |
| Remote code (`content_security_policy` relaxations, CDN `src`, `eval`) | Forbidden (ADR-P0028, SL-4.EXT.04) | All code ships in the package. `extension_pages` CSP stays `script-src 'self'; object-src 'self';` — no `unsafe-eval`, no remote hosts. A build check (EXT.04) fails on any remote URL in the bundle. |

## Notes for review

- Content scripts: none in EXT.01 (navigation signal only arrives in EXT.02; the viewer page reuses `apps/web/ui` behind the extension adapter in EXT.06).
- Data handling: documents are processed locally in the offscreen document; nothing is uploaded (ADR-P0016). The store privacy disclosure "does not collect user data" is backed by the egress harness (SL-8.BOUND.02), not by assertion.
- MV3 rationale: service worker (`service-worker.js`, module, event-driven, no DOM) + offscreen document (`offscreen.html`) + viewer page (`viewer.html`). Document bytes never transit the service worker (24-BINDINGS-SPEC §5).
