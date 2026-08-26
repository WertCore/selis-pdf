# Phase 4 — Web app + Chrome extension (Weeks 24–30 · month 7) — FIRST SHIP

**Gate G4 exit criteria:** web viewer + MV3 extension published · core WASM ≤ 3 MB brotli ·
first page painted < 1.2 s p75 on a 5 MB linearised PDF over Fast 3G · 1 000 external users ·
crash-free session rate ≥ 99.5% · zero document-corruption reports (trivially true — the viewer is
read-only, and proving that structurally is part of this phase).

The beachhead. Everything here is read-only: view, navigate, search, select, copy, print.
Editing is Phase 5. Shipping a *great viewer* first is the point — it is a complete product, it
proves the engine in the harshest environment, and it costs nothing to distribute.

---

## 4.WASM — The WASM binding

- [ ] **SL-4.WASM.01 — `selis-pdf-wasm` surface + Worker protocol** · owner: AI+
  - **Do:** Implement `24-BINDINGS-SPEC.md §2`: a request/response protocol over `postMessage`
    with transferable buffers, correlation ids, cancellation, and progress. The engine lives in a
    Worker; the main thread holds only handles.
  - **DoD:** Protocol conformance tests in Vitest against a real worker; every message type
    round-trips; cancellation mid-render is observed within 50 ms.
- [ ] **SL-4.WASM.02 — Code splitting and lazy chunks** · deps: WASM.01 · owner: AI+
  - **Do:** Split the WASM into: core (COS + render + text), and lazily-loaded chunks for OCR,
    convert, JPX, CJK fonts, and (later) the editor. Chunks are separate modules, not one binary
    with dead code.
  - **DoD:** `size-check` budgets met per chunk; a viewer session never downloads the editor chunk.
- [ ] **SL-4.WASM.03 — Threads with graceful single-threaded fallback** · deps: WASM.01 · owner: AI+
  - **Do:** `wasm-bindgen-rayon` under `SharedArrayBuffer` when COOP/COEP are present; identical
    output single-threaded when they are not (ADR-P0004, SL-2.RAST.10).
  - **DoD:** Both paths hash-equal on the corpus; the extension path (no COOP/COEP) is exercised in CI.
- [ ] **SL-4.WASM.04 — Memory strategy** · deps: WASM.01 · owner: AI+
  - **Do:** Growth policy, the 4 GB wasm32 ceiling, explicit release on document close, and a
    memory-pressure callback that evicts caches before the browser kills the tab.
  - **DoD:** Opening 20 documents sequentially shows no growth after close; a 1.5 GB document
    fails with `BudgetExceeded`, not a tab crash.
- [ ] **SL-4.WASM.05 — `OpfsSource`, `FsaSource`, `BlobSource`** · deps: WASM.01, SL-0.IO.01 · owner: AI+
  - **Do:** The three web `DocSource` adapters. OPFS sync access handles in the Worker; File System
    Access handles for save-in-place (Phase 5 needs this, build it now).
  - **DoD:** Each adapter passes the `DocSource` conformance suite including the fault cases.
- [ ] **SL-4.WASM.06 — `HttpRangeSource` fetch driver** · deps: WASM.01, SL-0.IO.04 · owner: AI
  - **Do:** The JS side of range fetching, with CORS handling, an abort signal wired to
    `CancelToken`, and a documented fallback when the origin refuses ranges or omits CORS headers.
- [ ] **SL-4.WASM.07 — Lazy font chunk loading** · deps: SL-3.FONT.10, WASM.02 · owner: AI+
- [ ] **SL-4.WASM.08 — Deterministic-render CI on WASM** · deps: WASM.03 · owner: AI
  - **DoD:** Headless-browser render of the corpus hash-matches the native render.

---

## 4.UI — The shared application (`apps/web/ui`)

Everything here is reused verbatim by desktop (ADR-P0022), so no `window.chrome`, no direct
`fetch`, no direct storage — all through a `PlatformAdapter` interface.

- [ ] **SL-4.UI.01 — `PlatformAdapter` interface** · owner: AI+
  - **Do:** The one seam between the UI and its host: open/save/pick file, storage, clipboard,
    print, telemetry, window/menu integration, deep links, and capability flags. Web, extension,
    and desktop each implement it.
  - **DoD:** The UI compiles and tests with a `MockAdapter`; a lint forbids platform globals in
    `apps/web/ui`.
  - **Note:** The `PlatformAdapter` is the *host* seam (what the OS can do). ADR-P0035 adds the
    *logic* seam: app state lives in `selis-viewmodel`, and the UI renders published diffs. Keep
    them distinct — conflating them is how logic leaks back into the shell.
- [ ] **SL-4.UI.02 — Virtualised page list + continuous scroll** · deps: UI.01 · owner: AI+
  - **Do:** Windowed rendering with a placeholder→low-res→full-res tile ladder, correct scroll
    anchoring on zoom, and page-fit/width/spread modes.
  - **DoD:** 60 fps sustained scroll on a 2 000-page document on a mid-range laptop; no layout
    shift when a tile resolves.
- [ ] **SL-4.UI.03 — Canvas compositor + tile presentation** · deps: UI.02, WASM.01 · owner: AI+
  - **Do:** `OffscreenCanvas` in the worker, transferred bitmaps, device-pixel-ratio correctness,
    and a zoom path that scales the existing tile immediately and re-renders behind it.
- [ ] **SL-4.UI.04 — Text layer, selection, and copy** · deps: SL-3.TEXT.05 · owner: AI+
  - **Do:** A selectable, accessible text layer aligned to the rendered glyphs. Selection must
    survive zoom and be RTL-correct. Copy preserves reading order, not visual order.
  - **DoD:** Selection accuracy tested against known quads; copy output matches `selis extract`.
- [ ] **SL-4.UI.05 — Search UI** · deps: SL-3.TEXT.06 · owner: AI
  - **Do:** Incremental search with match count, highlight-all, next/previous, and progressive
    results as pages load.
- [ ] **SL-4.UI.06 — Navigation: outline, thumbnails, page labels, destinations, links** · deps: UI.02 · owner: AI
  - **Do:** Link annotations are *activated* here but obey ADR-P0020 — external URIs prompt with
    the full destination shown, and `/Launch` is refused.
- [ ] **SL-4.UI.07 — Accessibility of the viewer itself** · deps: SL-1.DOC.06 · owner: AI+
  - **Do:** Expose the structure tree to AT: proper roles, headings, reading order, alt text for
    figures, table semantics. Full keyboard navigation. This is ADR-P0031 applied to our own UI,
    and it is a differentiator — most web PDF viewers are inaccessible.
  - **DoD:** axe-core clean; a screen-reader script walks a tagged document correctly; keyboard-only
    operation of every control.
- [ ] **SL-4.UI.08 — Print** · deps: UI.03 · owner: AI+
  - **Do:** Render at print resolution to a print-specific canvas or a generated print-ready PDF;
    honour `/PrintScaling` and page size; do not rely on the browser's own PDF printing.
- [ ] **SL-4.UI.09 — Document health panel** · deps: SL-1.COS.11 · owner: AI
  - **Do:** Surface deviations, conformance claims, encryption state, signature presence, and
    tagging status. Honest reporting as a feature.
- [ ] **SL-4.UI.10 — Design system + theming** · owner: AI
  - **Do:** `packages/ui-kit`, light/dark, high contrast, reduced motion, and a density setting.
- [ ] **SL-4.UI.11 — i18n scaffolding** · deps: SL-0.ERR.04 · owner: AI
  - **Do:** Every string a key from day 1 (ADR-P0034). Ship English; wire pseudo-locale into CI.
- [ ] **SL-4.UI.12 — Error and empty states** · deps: SL-0.ERR.01 · owner: AI
  - **Do:** Every `Code` maps to a user-facing state with a recovery action. A damaged file shows
    what we recovered, not a dead end.

---

## 4.WEB — The web deployment (`apps/web/host`)

- [ ] **SL-4.WEB.01 — Static hosting + COOP/COEP + CSP** · owner: AI+
  - **Do:** Cross-origin isolation for threads; a strict CSP with `wasm-unsafe-eval` only; SRI on
    every asset; no third-party scripts on the document-handling path (ADR-P0016).
  - **DoD:** securityheaders.sh A+; a test asserting the app still works with isolation disabled.
- [ ] **SL-4.WEB.02 — Service worker + offline** · deps: WEB.01 · owner: AI
  - **Do:** Cache the app shell and WASM chunks; the app opens local files with no network at all.
  - **DoD:** Airplane-mode test: open a local PDF, view, search, print.
- [ ] **SL-4.WEB.03 — Document handoff without upload** · deps: SL-4.WASM.05 · owner: AI+
  - **Do:** Drag-drop, file picker, paste, and `?src=` URL opening — all local. The one thing this
    app must never do is upload a document, and a CI test asserts no request body ever contains
    document bytes.
- [ ] **SL-4.WEB.04 — Marketing site + honest conformance page** · owner: HUMAN
  - **Do:** Publish the conformance ladder (SL-0.OPS.04). "Here is exactly what we support" is a
    trust asset in a category built on overclaiming.
- [ ] **SL-4.WEB.05 — Analytics posture** · deps: ADR-P0017 · owner: HUMAN
  - **Do:** Privacy-preserving, opt-in, no document-derived data, no third-party tags on the app
    origin. Marketing pages may differ but must be a separate origin.

---

## 4.EXT — The browser extension (`apps/extension`)

- [ ] **SL-4.EXT.01 — MV3 manifest + permissions minimisation** · owner: AI+
  - **Do:** Request the minimum: `declarativeNetRequest`, `offscreen`, and host permissions only
    where required. Every permission needs a written justification for review and for the store
    listing, because reviewers reject unexplained breadth.
- [ ] **SL-4.EXT.02 — PDF navigation interception** · deps: EXT.01 · owner: AI+
  - **Do:** DNR rules redirecting `application/pdf` main-frame navigations to the bundled viewer,
    preserving the original URL, referrer policy, and any auth context the browser would send.
  - **DoD:** Works for: direct `.pdf` URLs, `Content-Type`-only responses, `Content-Disposition:
    inline`, redirects, and POST-produced PDFs (document the cases that cannot work).
  - **Risk:** This is the fiddliest part of the extension and the part users notice when it breaks.
    Build a matrix of ~30 real-world PDF-serving patterns and test against all of them.
- [ ] **SL-4.EXT.03 — Offscreen document hosting the engine** · deps: EXT.01, WASM.01 · owner: AI+
  - **Do:** MV3 service workers are killed aggressively; the engine runs in an offscreen document
    or a dedicated worker with a documented lifecycle and state recovery.
  - **DoD:** A test that the viewer survives service-worker termination mid-session.
- [ ] **SL-4.EXT.04 — Bundled-only build** · deps: EXT.01 · owner: AI+
  - **Do:** No remote code, no CDN, no `eval` (ADR-P0028). A build check fails on any remote URL
    in the bundle.
- [ ] **SL-4.EXT.05 — Extension size budget** · deps: EXT.04, WASM.02 · owner: AI+
  - **Do:** The package carries the WASM. Tighter budget than the web app; CJK fonts are an
    optional post-install download into extension storage, not a bundled asset.
- [ ] **SL-4.EXT.06 — Reuse `apps/web/ui` via the extension adapter** · deps: UI.01 · owner: AI
- [ ] **SL-4.EXT.07 — Options page + first-run onboarding** · deps: EXT.06 · owner: AI
- [ ] **SL-4.EXT.08 — Deep link into the web app for edit actions** · deps: EXT.06 · owner: AI+
  - **Do:** "Edit this" hands the document to the web app **locally** (OPFS handoff or a same-origin
    transfer), never by uploading. The funnel from free viewer to paid editor is this button.
- [ ] **SL-4.EXT.09 — `file://` access permission flow** · deps: EXT.02 · owner: AI+
  - **Do:** Detect that "Allow access to file URLs" is off, explain why it is needed in plain
    language, and deep-link to the toggle. Do not silently fail on local PDFs.
- [ ] **SL-4.EXT.10 — Firefox and Safari ports** · deps: EXT.06 · owner: AI
  - **Do:** Firefox (MV3 with `webRequest` still available — a different interception path) and
    Safari Web Extension (requires the Xcode wrapper and an App Store submission).
  - **Note:** Safari's wrapper means an Apple Developer account (SL-0.LEAD.02) and a store review.
- [ ] **SL-4.EXT.11 — Store submissions** · deps: EXT.05, SL-0.LEAD.04 · owner: HUMAN
  - **Do:** Chrome Web Store, Edge Add-ons, AMO, App Store. Prepare privacy disclosures carefully —
    "does not collect user data" must be true and must be defensible.

---

## 4.SHIP — Launch readiness

- [ ] **SL-4.SHIP.01 — Crash reporting with document-byte stripping** · deps: SL-0.ERR.03 · owner: AI+
  - **DoD:** A test proving no document bytes appear in a report from a deliberately crashing parse.
- [ ] **SL-4.SHIP.02 — Feature flags + staged rollout** · deps: SL-0.WS.02 · owner: AI
- [ ] **SL-4.SHIP.03 — Support: docs, FAQ, "report a rendering bug" flow** · owner: HUMAN
  - **Do:** The bug flow must let a user attach the file *with explicit consent* and must explain
    exactly what is sent. This becomes the highest-value corpus source we have.
- [ ] **SL-4.SHIP.04 — Beta programme: 1 000 users, instrumented** · owner: HUMAN
- [ ] **SL-4.SHIP.05 — G4 review and go/no-go** · owner: HUMAN
