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
| `storage` | `permissions` | Hold the optional CJK font payload in extension storage after install (SL-4.EXT.05), and the first-run flag the service worker has to read (SL-4.EXT.07). `storage.local` stores bytes, which the settings store (`localStorage`, EXT.06) cannot, and an MV3 service worker has no `localStorage` at all — so the one piece of state that gates the welcome guide cannot live there. | The payload is deliberately *not* a bundled asset, so it has to live somewhere; extension storage is the only per-extension, persistent, permission-scoped place to put it. `unlimitedStorage` is **not** requested: the store is budgeted at 8 MiB against `storage.local`'s 10 MB default quota, so the wider permission has not been earned. | "Stores optional downloadable fonts on your device, and your settings." |

## Deliberately NOT requested

| Permission / pattern | Status | Reason |
|---|---|---|
| `host_permissions` (`<all_urls>`, `*://*/*`, per-origin) | **Still empty after EXT.02 and after EXT.09** | The EXT.02 ruleset matches by **URL shape** (a `.pdf` path, or a `.pdf` carried in the query) precisely because no host permission is held. This is a deliberate, documented limit, not an oversight: `responseHeaders` conditions need host access, so `Content-Type: application/pdf` and `Content-Disposition: inline` are invisible to the rules. Those serving patterns are enumerated as `cannot-work` in `PATTERN_MATRIX` (`src/permissions.ts`) with the exact permission each would need. Widening to per-origin hosts is a reviewable follow-up that would convert a documented subset of those rows — it is not taken here, because EXT.01's review established that host access without a settled rule is the top store-rejection cause, and the reverse (settled rules, no hosts) is not. |
| `optional_host_permissions: ["file:///"]` | **Declared, and granted by nobody until the reader acts** | See "Local files (SL-4.EXT.09)" below. Declared rather than required, so the only thing this extension asks a reader for at install is still nothing. |
| `webRequest` / `webRequestBlocking` | Not requested | Superseded by `declarativeNetRequest` for this use (see above). Firefox port (SL-4.EXT.10) may need `webRequest` under its MV3 — that port carries its own justification row when it lands. |
| `tabs`, `activeTab`, `scripting`, `cookies`, `unlimitedStorage` | Not requested | No tab inspection, no script injection, no cookie access, no bulk storage. The local-file flow reaches Chrome's own details page with `window.open` on a user gesture, so it needs none of them — `viewer-page.test.ts` asserts the flow names no `chrome.tabs`. `unlimitedStorage` stays refused while the CJK payload store (SL-4.EXT.05) is budgeted at 8 MiB; it is the escalation if a future payload proves it needs more than `storage.local`'s 10 MB default. |
| the `chrome.permissions` API | **Not called at all** | The `file://` grant is not a runtime request. Chrome documents it in one place: "If your extension needs to run on `file://` URLs… users must give the extension access on its details page… to detect whether the user has allowed access, you can call `extension.isAllowedFileSchemeAccess()`." A `permissions.request()` here would either do nothing or show a dialog granting something the reader cannot then find or verify. Asking in the only way the platform allows — a link to the page that has the switch, plus the written steps for a build that does not surface it — is the whole design, and `src/local-files.ts` says so at the exact line a future reader would want to "fix". |
| Remote code (`content_security_policy` relaxations, CDN `src`, `eval`) | Forbidden (ADR-P0028, SL-4.EXT.04) | All code ships in the package. `extension_pages` CSP is `script-src 'self' 'wasm-unsafe-eval'; object-src 'self';`. The bare `'unsafe-eval'` is still forbidden and is not what `'wasm-unsafe-eval'` means: the latter permits compiling WebAssembly and nothing else — no string-to-code, no `Function`, no remote host. SL-4.EXT.03 needs it to run the engine, MV3 provides it for exactly that case, and `manifest.test.ts` bans the bare source while requiring this one so neither can drift. A build check (EXT.04) fails on any remote URL in the bundle. |

## Local files (SL-4.EXT.09)

This is the one capability the extension declares and does not hold, and the
section exists because that is what a reviewer most needs spelled out.

| Field | Pattern | Why it is needed | Why nothing narrower works | Store-listing wording |
|---|---|---|---|---|
| `optional_host_permissions` | `file:///` | So a `file://` navigation of a `.pdf` can be redirected to the bundled viewer, and so the viewer can read that file. Both are inert until the reader turns on the browser's switch, and both are what "on grant it proceeds" means: with the switch off nothing intercepts, nothing fails and no error is raised, so a flow that waited for an interception would have nothing to wait for. | A `file://` URL *is* the narrowest thing that can name a local document. The two-slash spellings are not narrower versions of this pattern — Chrome's match-pattern reference is explicit that it "requires three slashes, not two" — and they are in `DENIED_HOST_PATTERNS`. | "Optionally reads a PDF you already have on this computer, and only after you switch on 'Allow access to file URLs' for Selis in your browser's extension settings. You can open a local PDF without this by choosing the file yourself." |

### Why *optional*, and why the required list did not move

Three reasons, in the order a reviewer will weigh them:

1. **No install-time warning.** Chrome's declare-permissions guidance is explicit
   that "adding or changing match patterns in the `host_permissions` field will
   also trigger a warning". A `file:///` match pattern in `host_permissions` is
   the pattern a reader reads as *reads your disk*, and it would be shown to
   every reader of every web PDF to serve a feature most of them never use.
   `optional_host_permissions` grants nothing and warns about nothing until the
   reader acts.
2. **EXT.01's minimisation is preserved literally, not just in spirit.**
   `host_permissions` is `[]`, `ALLOWED_PERMISSIONS` is unchanged, and
   `manifest.test.ts` asserts both — so the claim is checkable against the file
   rather than taken on trust.
3. **There is nothing to request at runtime anyway.** The grant is the
   details-page toggle, so the extension calls no permission API at all.
   Declaring the pattern is what makes the toggle meaningful; requesting it is not
   a thing Chrome offers here. A later task adding an `http(s)` origin should
   reach for `permissions.request` *then*, and only for that pattern.

### The trade this accepts, stated plainly

- **A reader who turns the switch on gives up Chrome's own local PDF viewer** for
  `.pdf` files reached by navigation, because the DNR rule now takes them. The
  extension's viewer replaces it and turning the switch off puts it back. This is
  the cost of "on grant it proceeds", and it is the reader's decision, which is
  the only reason it is acceptable.
- **The switch may not be surfaced for an *optional* pattern** on every Chrome
  build. The flow does not depend on it: the written steps (right-click the Selis
  icon → Manage extension → "Allow access to file URLs") are on screen
  unconditionally, and "Check again" re-reads the real state through
  `isAllowedFileSchemeAccess()`. If a build turned out never to surface the switch
  for an optional pattern, promoting the row to `host_permissions` is a one-line
  change with an install-time warning — not taken, because the flow is complete
  without it and EXT.01's minimisation is the stronger default.

### The route that needs no permission at all

`files.pickOpen` is a transient `<input type="file">` (`ext/host-env.ts`), so a
reader can open any local PDF today, with no grant of any kind. The flow offers it
**first**, in every state including the denial one. That is the honest answer to
"can this avoid a permission change entirely": for opening a local file, yes. What
the toggle buys is the other thing — Selis taking over when the reader navigates to
a local PDF themselves, which no file picker can do.

## What the options page needed, and what it did not (SL-4.EXT.07)

An options page needs no permission of its own. `options_page` is a manifest
key; the page is reached with `chrome.runtime.openOptionsPage()`, which needs
no permission and cannot be pointed anywhere; and the page reads and writes the
two stores already described above — `localStorage` for the telemetry opt-in
(the key `ext/adapter.ts` reads, now imported from `src/options-state.ts` rather
than repeated) and `chrome.storage.local` for the first-run flag.

**The permission set is unchanged by SL-4.EXT.07.** `manifest.test.ts` asserts
that, and `options-page.test.ts` adds the second half: the shipped page and the
service worker name no `chrome.tabs`, no `chrome.permissions` and no network
API, so "just add a link to our website" fails the suite rather than the
review. `OPTIONS.md` records what the page does and does not say.

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
