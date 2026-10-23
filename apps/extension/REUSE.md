# Reusing `apps/web/ui` in the extension (SL-4.EXT.06)

The plan's architecture table says the extension reuses `apps/web/ui` in a
"no-network" configuration. This is what that actually means, what it costs,
and what it refuses.

The short version: `apps/ui` is not forked, it is compiled into this package
and driven through a `PlatformAdapter` the extension implements. Everything
below is either a decision worth arguing with or a cost someone should know
about before SL-4.EXT.10 ports this to Firefox.

## What actually ships

Four modules from `apps/ui`, plus two stylesheets from `@selis/ui-kit` — and,
since SL-4.UI.06's extension half, the larger block in the second table below:

| Shipped | Why it is here |
|---|---|
| `ui/src/platform/errors.js` | `AdapterError` / `ErrorCode`. The extension refuses several capabilities, and a refusal with its own error type would break the one rule the seam has. |
| `ui/src/viewer/surface.js` | The `TileSurface` / `FrameClock` ports, and `SurfaceFaultError`. Pulled in by the row below. |
| `ui/src/viewer/worker-surface.js` | `createAnimationFrameClock`, used verbatim. |
| `ui/src/viewer/navigation-wire.js` | The mapper from the engine's navigation vocabulary to the viewer's (SL-4.UI.06). The WASM guest answers outlines, page labels, destinations and links in the *engine's* words (`destKind`, `x0`, `target`), and this is the one copy of the translation. A host that mapped inline would write it a second time — in the web shell and here — and the two copies would drift, which is the failure the module was created to prevent. It is pure and DOM-free, so it costs nothing to ship and nothing to keep honest. |
| `ui-kit/css/tokens.css`, `base.css` | Linked by `viewer.html`. Tokens are generated (UI.14) and must never be edited here. |

Those rows were the whole `apps/ui` story until 2026-10-23, when the extension
half of SL-4.UI.06 mounted the shared navigation controllers into the viewer
page — and that changed the ship list more than any task before it. Nothing is
forked: these are the same `apps/ui` sources the web app runs, pulled in as
rows because the mounted modules import them at run time.

| Shipped | Why it is here |
|---|---|
| `extension/src/viewer-view.js` | This package's own composition. `mountViewerView` builds the page canvas, the outline panel, the external-link prompt and a notes host around the controllers below, fetches destinations in parallel with the outline's first load, and repaints from the states the controllers publish. The web shell's `boot.js` is the other host's half of the same composition. |
| `ui/src/viewer/navigation.js`, `outline.js` | The real outline controller — `role="tree"`/`treeitem`, roving tabindex, its own key handling — and the row arithmetic both the controller and the mount use. The asymmetry with the web host is real: the web page (plan 14, 2026-10-22) renders a plain nested `<ul>` and does not mount this controller, so this package's page is where ArrowDown first actually moves focus inside its own outline. |
| `ui/src/viewer/page-list.js`, `compositor.js`, `tile-ladder.js` | The page canvas. `createPageList` owns the window, the scroll offset and the tile ladder; `page-list.js` imports `tile-ladder.js` at run time, and the mount drives `createTileCompositor` itself. |
| `ui/src/viewer/layout.js`, `windowing.js`, `anchoring.js` | The layout, window and scroll-anchor arithmetic `page-list.js` imports. |
| `ui/src/viewer/links.js`, `page-labels.js` | Link-decision policy and label formatting, imported by `navigation.js`. |
| `ui/src/viewer/keyboard.js` | Shared by `navigation.js` for the tree keys and by the mount, which consults `resolveNavigationKey` only inside the outline tree. |
| `ui/src/viewer/strings.js`, `ui/src/i18n/pseudo-locale.js` | The navigation and page-list catalogues and their factories; `strings.js` imports the pseudo-locale at run time, so it ships rather than being erased with the types. |
| `ui/src/viewer/navigation.css`, `page-list.css` | Linked by `viewer.html` as `shared-asset` rows, verbatim like the ui-kit pair. |

Still out: `thumbnails.ts` (the rail), the search panel, and anything the
viewer page has no call site for — the same policy as before, applied to a
longer list. And `keyboard.js` shipping does not mean the page claims keys:
there is no key map for the page tiles — native scrolling moves the pages, and
the outline tree is the only place the mount consults `resolveNavigationKey`.

One inherited defect is recorded rather than forked. Upstream `navigation.css`
is missing the closing brace of the rule at its line 127, so everything after
it in the file — the thumbnail boxes, the dialog backdrop, the dialog, the
URL, the actions row and the forced-colours block — parses as sitting *inside*
that rule, or is dropped by a stricter parser; either way it stops matching
anything. The rules up to the break still apply, which is why the tree renders
correctly and the visible consequence on the extension page is exactly that
`.selis-navigation__dialog`/`__url`/`__actions` render unstyled. The fix
belongs to `apps/ui`'s owner; forking the sheet here would end the verbatim
reuse that is the whole point of the row.

`PACKAGE_ENTRIES` in `src/bundle-paths.ts` is the authority. The bundled-only
gate fails on any file in the package that is not on that list, and on any
declared file that is missing, so the list cannot drift from reality quietly.

## What the options page reuses, and what it deliberately does not (SL-4.EXT.07)

The options page links the same two stylesheets as the viewer and **imports no
`apps/ui` module at all**. That is a decision, not an oversight: there is no
settings component in `apps/ui` to reuse, and the page's one interactive
element is a checkbox the ui-kit does not yet style. Shipping the viewer
catalogue to reach a private `formatMessage` would have put the page-list and
search string sets into a package that has no page list, which is the same
trade `REUSE.md` already refused when it declined to ship `page-list.css`.

So the options page has its own small i18n seam (`src/options-strings.ts`) in
the same shape `apps/ui/src/viewer/strings.ts` uses — closed key union, one
English catalogue, `{name}` placeholders, a `Partial` override merged over
English. SL-4.UI.11 owns the real runtime; adopting it is a change to the body
of `createOptionsCatalogue` and nothing else, and until then this page is the
second implementation of a thing the plan says should have one. It is recorded
here rather than hidden, because "we wrote it again" is exactly the kind of
decision the next reader of this file needs to see.

### And then the viewer adopted it, and the debt did not grow (SL-4.EXT.09)

`src/viewer-strings.ts` registers the **viewer page's** catalogue against
`apps/ui/src/i18n/runtime.ts`, so the package now ships two more `shared-js`
rows, `i18n/message.js` and `i18n/runtime.js`, and the extension has one
sanctioned runtime rather than two hand-rolled formatters. That is what UI.11's
`i18n-boundary.test.ts` was split for: it names this package as the consumer the
two files were separated out for, and that test is what keeps them importable
here without dragging the viewer's catalogues in.

What this task deliberately did **not** do is rewrite `options-strings.ts` onto
that runtime. Its public surface is pinned by `options-strings.test.ts` and
consumed by a shipped page, the change is a refactor of EXT.07's page rather than
part of a permission flow, and doing it here would have put EXT.07's whole
regression surface inside EXT.09's diff. The honest position is therefore: the
debt is unchanged in size and unchanged in kind, one *new* implementation was not
added, and the conversion is now a small job for whoever owns the options page.

SL-4.EXT.09 did change one thing on that page, and it was not optional: the
welcome guide said local files "are not opened by the extension ... it has not
shipped yet", which the flow turned into a lie on the day it landed. The
capability list also gained a row, for the same reason, in the opposite
direction: a *declared but ungranted* `file:///` pattern now gets its own
sentence rather than the "Website access: {pattern}" one, because a page whose
job is saying what the browser has allowed must not claim access nobody granted.

The one thing the page *does* share with the viewer is the settings key:
`ext/adapter.ts` imports `SETTINGS_KEYS` from `src/options-state.ts` rather
than repeating the string, so the switch on the page and the port in the
adapter cannot drift into two different keys.

## The worker arrangement, and why it is not the web build's

The web build gives the engine a `Worker` the page spawns: the engine's
lifetime is the page's lifetime, and a structured clone carries bytes for free.
MV3 has neither half of that:

- A **service worker** is killed after ~30 s idle and has no DOM. It cannot host
  the WASM engine, and it must not be handed document bytes anyway
  (24-BINDINGS-SPEC §5).
- A **`Worker` spawned by the viewer page** dies with the page — the exact
  lifetime the `offscreen` permission was justified to avoid. A user who
  switches tabs or opens a PDF in a new tab must not lose the engine.
- The **offscreen document** is a real DOM document that outlives any single
  page, and it is the only place an MV3 extension can run a long-lived engine.

So: `viewer.html` <-> `chrome.runtime` port <-> `offscreen.html`.

Only the service worker may call `chrome.offscreen.createDocument`, so the page
asks it to. That message carries a verb and nothing else — the document URL is
fetched by the page and travels on the engine port directly to the offscreen
document. That is the entire reason the service worker is involved at all.

The worker also has **no `chrome.runtime.onConnect` listener**, on purpose.
Chrome delivers a `runtime.connect` to every extension context that has one, so
registering one would hand the worker a port carrying base64 document bytes, in
the context the browser kills after 30 seconds.

## The cost: JSON, and therefore no transfer lists

`chrome.runtime` port messaging serialises with JSON, not the structured clone
algorithm. Three consequences, each a decision rather than an accident:

1. **No `ImageBitmap`, no `ArrayBuffer`, no transfer list.** UI.03's worker path
   moves both by *ownership*; nothing can move by ownership across this link.
   Tiles therefore travel as base64 and the viewer's `TileSurface` has to be a
   main-thread one (`takesOwnership: false`).
2. **Document bytes travel as base64 too** — about 33 % overhead on the way out
   and another decode on the way in. `apps/web/host` gets them for free.
3. **A blob handle cannot cross.** `File` / `Blob` / `FileSystemFileHandle` are
   not serialisable, so the page reads the file and sends bytes.

### What SL-4.EXT.03 measured, and what it decided

SL-4.EXT.03 kept the arrangement and did not re-examine it, for a reason
that only became clear once the engine was real: the base64 hop is on the
*page-to-document* link, and the engine does not live on that link. The
document hands the bytes to its own Worker, which is a structured clone, so
the ~33 % document penalty is paid once on the way in and the tile penalty
is paid on the way out to a surface that has to be main-thread anyway. The
arrangement's real cost turned out to be different from the one EXT.06
predicted, and it is written down below rather than left as a suspicion.

## The engine, and the Worker around it

`offscreen.html` spawns a module Worker that owns the WASM guest. The Worker
is not there for its lifetime - the document already outlives every viewer -
it is there because one WASM call occupies its thread until it returns, and
on the document's main thread that would stop the `chrome.runtime` port from
being serviced. A `cancel` from a viewer would then queue behind the render
it is meant to stop. With the engine in a Worker the document keeps servicing
the port, so the WASM.01 in-flight `cancel` is a real channel rather than a
nominal one.

`wasm-guest.ts` is the only module that knows a pointer exists. Everything
above it speaks bytes and JSON, which is what makes the ABI testable without
a browser - and, more usefully, makes a mistake in it a wrong answer rather
than a memory-safety bug, because every length the guest reports is checked
against its memory before anything is read.

The guest is read from `chrome-extension://<id>/wasm/selis_pdf_wasm.wasm`:
the extension's own origin, so `host_permissions` stays `[]`. The manifest
gains `'wasm-unsafe-eval'` and nothing else.

This is the strongest argument for SL-4.EXT.03 re-examining the arrangement.
The honest summary is that the extension gives up UI.03's zero-copy rendering in
exchange for an engine that survives the page, and nobody has measured which
trade is better yet. A tiled 2x-DPR page at ~1.4 MB of RGBA per full-page tile
is the number to measure first.

## One ship-list row carries more than the extension uses

`worker-surface.js` also contains `createWorkerSurface`, which this package
never calls: it needs an `OffscreenCanvas` transferred to a worker, and there is
no transfer list here. It cannot be dropped without a bundler, and ADR-P0021
rules one out. The call site that would make it live does not exist, which is
what this paragraph is for.

## Capabilities: what the extension supplies, and what it refuses

| Port | Verdict | Why |
|---|---|---|
| `engine` | supplied | The WASM engine, in a Worker the offscreen document owns (SL-4.EXT.03), reached over a `chrome.runtime` port. |
| `navigation` | supplied | The same engine, asked the four navigation questions (SL-4.UI.06): the outline, the `/PageLabels`, the `/Dests` name tree and a page's link annotations. One transport, two ports — the navigation reads share the engine's pending table and id counter, because the link delivers every reply to every subscriber and the correlation id is the only key. |
| `files.pickOpen` | supplied | A transient `<input type="file">`. No MV3 permission needed. |
| `files.pickSave` | **refused** | Save-in-place is a File System Access handle. The viewer is read-only; the only caller would be Phase 5. Returning `null` would read as "the user cancelled", which a UI cannot distinguish from a real cancellation. |
| `storage` | supplied | `localStorage` on the extension origin. `chrome.storage` needs the `storage` permission, which EXT.01 did not approve. |
| `clipboard.writeText` | supplied | The async clipboard API, on a user gesture. |
| `clipboard.readText` | **refused** | Needs `clipboardRead`. Not approved, so `clipboardRead: false`. |
| `print` | supplied | `window.print()`. |
| `telemetry` | supplied, inert | The opt-in is stored and honoured; there is no sink and no permission to reach one, so `record` drops everything. |
| `window.setTitle` | supplied | `document.title`. |
| `window.openExternal` | supplied, narrowed | `http(s)` only — see below. |
| `window.onDeepLink` | **refused** | Nothing routes a deep link in. The options page that would is EXT.07. |

`host_permissions` stays `[]`. The extension therefore has access to **no
document origin at all**, which is why `httpRange` is `false`: a range source
is unreachable by construction. The intercepted-URL path does its one fetch in
the page (EXT.02) and arrives at the engine as `bytes`.

### `openExternal` is narrowed to `http(s)` on purpose

ADR-P0020 disables document-driven navigation by default — `/Launch`,
`/GoToR`, `/SubmitForm`, `/ImportData` — and a launch must never be a silent
side effect. The `WindowPort` contract says the UI prompts with the full
destination and the port only performs, so the **scheme** check lives here,
where a document cannot talk its way past it. A scheme allow-list rather than a
deny-list, because a deny-list has to be updated every time a scheme is
invented and the failure mode is handing a document's `/Launch` target to the
browser.

## What the contract suite says about this

`definePlatformAdapterContract` from `apps/ui` runs against the extension,
driven through the JSON link into an offscreen-document host. 13 of its 14
probes pass unmodified. The one exemption is clipboard read, which the
extension cannot have without a permission it does not request; the exemption
is a named entry with a required reason, not an edited probe, so a host that
stops needing it has to delete something reviewable.

`adapter.test.ts` asserts each refusal is a real registry-coded refusal and
that it performed no host side effect — a stub returning `null` is worse than a
refusal, because a UI reports it as a cancellation of something that never
happened.

### The navigation probes are live now (SL-4.UI.06)

The suite's navigation probes are **conditional on the port's presence**, so
they were silent while the extension had none. They are not silent now: the
adapter supplies a `navigation` port, and the suite's own four probes run
against it — an out-of-range page rejected with `BINDING_BAD_ARGUMENT` (6001),
a closed document rejected with `BINDING_BAD_HANDLE` (6000) and
`docState: "Unchanged"`, and a pre-aborted signal rejected with `CANCELLED`
(4020).

The suite asserts *shape*, so the content is asserted separately, in
`transport.test.ts`: the fixture document carries an outline, page labels, a
named destination and two link annotations, and one of the two is a `/Launch`
— because the transport's job is to report the classes ADR-0020 disables, and a
fixture carrying only safe ones would not prove that.

Two refusals are worth naming because they are **different** refusals, and
collapsing them is the failure this task exists to prevent:

| Situation | Code | What it says |
|---|---|---|
| The host has no navigation capability | `BINDING_UNSUPPORTED_OP` (6017) | "This host cannot read outlines." |
| The page does not exist | `BINDING_BAD_ARGUMENT` (6001) | "Page 9 000 of a 3-page document is not a page." |
| The document has no outline | *(an empty list)* | "This document has no outline." |

The engine's own answer for the second case is `PAGE_OUT_OF_RANGE` (1301),
which talks about the *engine's* page numbering rather than the seam's, so
every hop checks the page itself: the page, the host and the guest. One rule,
one definition (`outOfRangePage` in `engine-protocol.ts`), three enforcement
points.

### What is still missing on the extension's side

The **port** is there; the **panels** are not. The extension's viewer page
still mounts no bookmark tree, no thumbnail rail and no link overlay, because
those are `apps/ui`'s `navigation.ts` and `thumbnails.ts` and the extension
ships neither (see "What actually ships" above). What this task delivers is
the capability under them: `adapter.navigation` is defined, answers, and
refuses honestly. Whether the extension's viewer grows the panels is the
viewer's own task, and the port is what it will need.

**Updated 2026-10-23 (SL-4.UI.06's extension half):** the bookmark tree, the
page canvas and the prompt are there now. `src/viewer-view.ts` mounts the real
`createNavigation` and `createPageList` against this same port, as the row
table in "What actually ships" records. What still does not come with them:

- **A canvas click overlay.** Nothing intercepts a click on the rendered
  canvas, so `controller.activateLink` — and with it the prompt — is reachable
  through tests only (`viewer-view.test.ts` drives the prompt from the
  controller's `pendingExternal`). The prompt is real; its trigger is a
  separate slice.
- **A key map for the page tiles.** Native scrolling moves the pages; the
  mount claims keys only inside the outline tree.
- **The thumbnail rail.** `thumbnails.ts` is still not shipped.
- **The extension's own browser-check leg.** Coverage of the mounted view is
  Node-level only, and the web host's precedent (plan 14, 2026-10-22) is that
  a browser pass finds what Node cannot.

## The gate, and the bug it did not catch

`pnpm --filter @selis/extension build` runs `tsc`, packs, and scans the **built
package**. Its falsifiability was re-verified for this task against this build:
a remote `<script src>` and a computed dynamic import both fail it with file,
line and reason.

It also **was wrong**, and this task is how that was found.
`normalisePackagePath` folded a leading `..` away, so a module importing
`../../../ui/src/platform/errors.js` — which resolves, from inside the package,
to a path *above* it — landed on a shipped filename and was reported clean. The
shipped `adapter.js` had exactly that import and would have 404'd in the
browser. A `..` that steps above the package root is now preserved and reported.
Two tests pin it, one for the escape and one asserting that the legitimate
in-package shape still resolves.

## Known gaps this task leaves behind

- **The engine is installed (SL-4.EXT.03) and the WASM is in the package
  (SL-4.EXT.05).** The engine is a real WASM guest in a Worker the offscreen
  document spawns, and the package now carries `wasm/selis_pdf_wasm.wasm` — a
  ship-list row whose bytes come from the `wasm-opt -O3` output of the same
  `wasm32-unknown-unknown` build `cargo xtask size-check` measures, at a
  measured 4,135,676 raw / 1,326,454 brotli. The typed "the engine is not in
  this build" failure EXT.03 reported is no longer reachable from a green
  build: `pack.mjs` fails when the artefact is absent and the size gate fails on
  a package that somehow lost it. See `SIZE.md`.
- **The CJK payload store ships with no producer and no caller**
  (`extension/src/ext/cjk-payload.js`, ~5 KB of dead code). SL-4.EXT.05
  implemented the store half of "an optional post-install download into
  extension storage" and refused to invent the other half: SL-3.FONT.10 has
  produced no `cjk/manifest.json`, the transport is WASM.07's row, and
  `host_permissions` is `[]`, so there is no origin to fetch one from. The
  module is on the ship list anyway, because a store nobody can reach is not a
  split — the store is here and the bytes it will hold are not, and the size
  gate fails if a font ever appears in the package. The call site arrives with
  WASM.07.
- **The offscreen document is still never closed.** SL-4.EXT.03 gave it a
  teardown (`stop()` terminates the Worker and releases every open document)
  and a trigger (`pagehide`), but nothing *decides* to close it: the service
  worker creates it on demand and no timer or viewer count tears it down. So
  the honest statement is unchanged from EXT.06 - it outlives every viewer
  that uses it - and the engine it holds is released when it does go.
- **The engine is warm-started and its failure is swallowed.** `startEngineHost`
  fires the compile and does not await it, so a viewer that opens nothing pays
  nothing and a viewer that opens something does not pay twice. The failure is
  reported by the first request instead, with the same message.
- **Search is not progressive in the way the threaded shell is.** A
  single-threaded WASM call cannot be interleaved, so the guest answers a
  search in one response and `wasm-engine.ts` slices it into batches. The UI
  sees a quick sequence of slices rather than a page-by-page scan.
- **`SearchOptions.wholeWord` is refused, not ignored.** The WASM.01 search
  options have no word-boundary flag, and a shell that dropped the request
  would answer a different question than the UI asked.
- **The text layer is the most expensive thing this transport carries.** It is
  per-character geometry, in JSON, over a JSON port: a few hundred kilobytes
  for a dense page, and the base64 hop on top. It is resolution-independent,
  which is why it is fetched once per page rather than per zoom, but a
  text-heavy 500-page document is a real memory ceiling.
- **A character width is resolved per code, not per font.** `page_layer` asks
  its callback for a code's advance and the callback is keyed by code alone,
  so the engine reads the widths off the page's own glyphs. A page that draws
  the same character at two sizes gives both occurrences the first one's width.
  A code that never appears yields a zero-width quad, which highlights nothing
  rather than highlighting the wrong place.
- **A pending request does not settle when the port dies.** If the offscreen
  document itself goes away mid-request, the viewer's promise waits rather
  than rejecting. That is EXT.06's transport, not this task's, and it is the
  one thing here I would fix before a beta: the fix is a close notification on
  the link, and it changes shared transport semantics, so it wants its own
  change rather than a late edit from a task that was not asked for it.
- **No page list.** The viewer shows a page count and any failure, not pages.
  That is UI.04+ and the reason `page-list.css` is not shipped. *(No longer
  true as of 2026-10-23: the extension half of SL-4.UI.06 mounts the page
  list, and `page-list.css` ships — see "What actually ships" above. What
  remains of this bullet is the thumbnail rail and the prompt's missing
  on-page trigger, which live in the extension-half record under
  "Capabilities".)*
- **`getContexts` needs Chrome 116** and the manifest floor is 114, so the
  worker falls back to catching Chrome's single-document rejection. Raising the
  floor for a tidier branch was not worth a viewer that stops working on 114.
