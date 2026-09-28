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
| `engine` | supplied | Offscreen document over a port. |
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

- **The engine is not installed.** `offscreen.js` passes `engine: null` until
  SL-4.EXT.03. The host still answers, with a coded refusal, because an
  unanswered request is indistinguishable from a hung engine and would leave the
  viewer spinning.
- **The offscreen document is never closed.** The service worker creates it on
  demand and nothing tears it down. A lifecycle for it is EXT.03's scope, and
  the honest statement is that today it outlives every viewer that uses it.
- **No page list.** The viewer shows a page count and any failure, not pages.
  That is UI.04+ and the reason `page-list.css` is not shipped.
- **`getContexts` needs Chrome 116** and the manifest floor is 114, so the
  worker falls back to catching Chrome's single-document rejection. Raising the
  floor for a tidier branch was not worth a viewer that stops working on 114.
