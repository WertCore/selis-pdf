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
| `host_permissions` (`<all_urls>`, `*://*/*`, per-origin) | **Still empty after EXT.02** | The EXT.02 ruleset matches by **URL shape** (a `.pdf` path, or a `.pdf` carried in the query) precisely because no host permission is held. This is a deliberate, documented limit, not an oversight: `responseHeaders` conditions need host access, so `Content-Type: application/pdf` and `Content-Disposition: inline` are invisible to the rules. Those serving patterns are enumerated as `cannot-work` in `PATTERN_MATRIX` (`src/permissions.ts`) with the exact permission each would need. Widening to per-origin hosts is a reviewable follow-up that would convert a documented subset of those rows — it is not taken here, because EXT.01's review established that host access without a settled rule is the top store-rejection cause, and the reverse (settled rules, no hosts) is not. |
| `webRequest` / `webRequestBlocking` | Not requested | Superseded by `declarativeNetRequest` for this use (see above). Firefox port (SL-4.EXT.10) may need `webRequest` under its MV3 — that port carries its own justification row when it lands. |
| `tabs`, `activeTab`, `scripting`, `cookies`, `storage` (unlimited), `file://` pseudo-host | Not requested | No tab inspection, no script injection, no cookie access, no bulk storage in EXT.01. `file://` support (SL-4.EXT.09) is an explicit user-toggled flow with its own onboarding copy — not a silent manifest entry. `storage` (unlimited) arrives only if the CJK post-install payload (SL-4.EXT.05) proves it needs more than the default quota. |
| Remote code (`content_security_policy` relaxations, CDN `src`, `eval`) | Forbidden (ADR-P0028, SL-4.EXT.04) | All code ships in the package. `extension_pages` CSP is `script-src 'self' 'wasm-unsafe-eval'; object-src 'self';`. The bare `'unsafe-eval'` is still forbidden and is not what `'wasm-unsafe-eval'` means: the latter permits compiling WebAssembly and nothing else — no string-to-code, no `Function`, no remote host. SL-4.EXT.03 needs it to run the engine, MV3 provides it for exactly that case, and `manifest.test.ts` bans the bare source while requiring this one so neither can drift. A build check (EXT.04) fails on any remote URL in the bundle. |

## Interception coverage (SL-4.EXT.02)

`src/permissions.ts` holds `PATTERN_MATRIX`: 38 real-world PDF-serving patterns,
each with a verdict and a reason. `src/manifest.test.ts` re-derives every
verdict from the shipped ruleset, so the table cannot drift into fiction.

| Verdict | Count | Meaning |
|---|---|---|
| `intercepted` | 22 | Redirected to `viewer.html?src=<url>` by URL shape. |
| `not-matched` | 9 | Not redirected. Mostly deliberate: `main_frame`-only, so an app's own `fetch()` and an `<embed>` are untouched. |
| `cannot-work` | 7 | No permission this extension holds could redirect them. Each names the capability required (host permissions for the origin, or a non-`http(s)` scheme). |

The `cannot-work` set, by class:

- **Content-Type only** (`application/pdf` at an extensionless URL; a numeric
  id path; a REST endpoint; `Content-Disposition: attachment; filename=…`) —
  the response headers are the only signal and `responseHeaders` conditions
  need host access.
- **POST-produced PDF at an extensionless URL** — the browser does not
  re-navigate a POST response, so there is no navigation to intercept.
- **`blob:`, `data:`, `wss:` and `file://`** — not main-frame navigations, or
  outside the `^https?://` rule. `file://` additionally needs the user's
  "Allow access to file URLs" toggle, which is SL-4.EXT.09's scope.

Two classes are intercepted but worth naming in review: a `.pdf`-path URL that
actually serves `text/html` (a soft 404) now shows a parse error in the viewer
rather than the browser's page, and a `Content-Disposition: attachment` PDF is
shown rather than downloaded. Both follow from matching on URL shape, and both
are recorded in the matrix.

## Notes for review

- Content scripts: none in EXT.01 (navigation signal only arrives in EXT.02; the viewer page reuses `apps/web/ui` behind the extension adapter in EXT.06).
- Data handling: documents are processed locally in the offscreen document; nothing is uploaded (ADR-P0016). The store privacy disclosure "does not collect user data" is backed by the egress harness (SL-8.BOUND.02), not by assertion.
- MV3 rationale: service worker (`service-worker.js`, module, event-driven, no DOM) + offscreen document (`offscreen.html`) + viewer page (`viewer.html`). Document bytes never transit the service worker (24-BINDINGS-SPEC §5).
