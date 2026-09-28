# Reusing `apps/web/ui` in the extension (SL-4.EXT.06)

The plan's architecture table says the extension reuses `apps/web/ui` in a
"no-network" configuration. This is what that actually means, what it costs,
and what it refuses.

The short version: `apps/ui` is not forked, it is compiled into this package
and driven through a `PlatformAdapter` the extension implements. Everything
below is either a decision worth arguing with or a cost someone should know
about before SL-4.EXT.10 ports this to Firefox.

## What actually ships

Three modules from `apps/ui`, plus two stylesheets from `@selis/ui-kit`:

| Shipped | Why it is here |
|---|---|
| `ui/src/platform/errors.js` | `AdapterError` / `ErrorCode`. The extension refuses several capabilities, and a refusal with its own error type would break the one rule the seam has. |
| `ui/src/viewer/surface.js` | The `TileSurface` / `FrameClock` ports, and `SurfaceFaultError`. Pulled in by the row below. |
| `ui/src/viewer/worker-surface.js` | `createAnimationFrameClock`, used verbatim. |
| `ui-kit/css/tokens.css`, `base.css` | Linked by `viewer.html`. Tokens are generated (UI.14) and must never be edited here. |

Nothing else from `apps/ui` is in the package. The rest is either type-only
(erased at compile, so it costs nothing) or belongs to a viewer this package
does not contain yet — the virtualised page list and its stylesheet are UI.04+'s
to mount. Shipping them now would put a half-built viewer into a package that
SL-4.EXT.05 has to fit a size budget.

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
  That is UI.04+ and the reason `page-list.css` is not shipped.
- **`getContexts` needs Chrome 116** and the manifest floor is 114, so the
  worker falls back to catching Chrome's single-document rejection. Raising the
  floor for a tidier branch was not worth a viewer that stops working on 114.
