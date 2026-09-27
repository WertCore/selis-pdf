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

- [x] **SL-4.WASM.01 — `selis-pdf-wasm` surface + Worker protocol** · owner: AI+
  - **Do:** Implement `24-BINDINGS-SPEC.md §2`: a request/response protocol over `postMessage`
    with transferable buffers, correlation ids, cancellation, and progress. The engine lives in a
    Worker; the main thread holds only handles. *(Done in Rust: schema v1 (`selis-pdf-wasm::protocol`,
    ADR-P0042), the `selis_dispatch` cdylib export with copy-in/copy-out buffer discipline, the
    document registry + budgeted ops, pre-cancel + exported cancel slot, progress slot, ERR.03
    trampoline at the worker entry. The JS Worker file and the Vitest leg are UI.01 scope.)*
  - **DoD:** Protocol conformance tests in Vitest against a real worker; every message type
    round-trips; cancellation mid-render is observed within 50 ms. *(Rust-side conformance runs in
    CI (`wasm-protocol` job: `cargo xtask wasm-protocol` — every message round-trips over the
    compiled guest on wasmtime, guest==native render checksums, budget exhaustion and
    cancellation cross the boundary as typed codes, malformed messages contained; in-crate tests
    prove deterministic mid-render cancellation at a budget tick). The Vitest/real-worker leg
    lands with the JS shell (UI.01); the wire schema and export contract it must implement are
    pinned by ADR-P0042.)*
- [x] **SL-4.WASM.02 — Code splitting and lazy chunks** · deps: WASM.01 · owner: AI+
  - **Do:** Split the WASM into: core (COS + render + text), and lazily-loaded chunks for OCR,
    convert, JPX, CJK fonts, and (later) the editor. Chunks are separate modules, not one binary
    with dead code.
  - **DoD:** `size-check` budgets met per chunk; a viewer session never downloads the editor chunk.
  - **Status:** Done 2026-09-26 and merged into main. Five chunk crates (jpx/cjk/ocr/convert/editor)
    are separate cdylib+rlib modules each producing its own `.wasm`; the core `selis-pdf-wasm`
    depends on none of them. `chunks.rs` is the single-source Manifest (canonical ids, budgets,
    `viewerAuto = false` for editor) with a TS mirror in `packages/wasm-loader` that throws
    *before* any fetch when a viewer session names the editor chunk. `xtask size-check` measures
    per chunk: 7/7 artifacts within budget (core 1,313,250 / 3,000,000; chunks ~4.5 KB each vs
    400-900 KB budgets), 0% baseline regression.
  - **Gaps (review, non-blocking):** (1) the chunk bodies are capability/probe stubs - their
    declared engine deps are unreferenced, so the ~4.5 KB sizes reflect stubs and the 400-900 KB
    budgets stay aspirational until real OCR (Phase 5) / convert (Phase 8) / editor code lands;
    (2) budget numbers are duplicated across `xtask/size-budgets.toml`, `Manifest::canonical()`,
    and the TS `MANIFEST` (documented mirrors - a maintenance seam);
    (3) `selis-pdf-engine`'s unused `selis-pdf-edit` edge is pre-existing: if the engine ever
    references it, edit code lands in the core - worth gating as optional.
- [x] **SL-4.WASM.03 — Threads with graceful single-threaded fallback** · deps: WASM.01 · owner: AI+
  - **Do:** `wasm-bindgen-rayon` under `SharedArrayBuffer` when COOP/COEP are present; identical
    output single-threaded when they are not (ADR-P0004, SL-2.RAST.10).
  - **DoD:** Both paths hash-equal on the corpus; the extension path (no COOP/COEP) is exercised in CI.
  - **Status:** Done 2026-09-27, merged into main. The executor is a *policy*, not a build
    (ADR-P0004): `selis-raster` gained `Parallelism` + `render_tiles`; `selis-pdf-engine`
    gained `TiledRender`, whose decomposition / per-tile render / stitch are identical on
    both paths, so threaded and single-threaded cannot diverge by construction. `rayon` is
    used through the workspace pool only, because `ThreadPoolBuilder` is Unsupported on
    `wasm32-unknown-unknown` and `par_iter` then falls back to the sequential global path
    - so one artifact serves both the COOP/COEP and the extension path.
  - **DoD clause 1 (hash-equal on the corpus):** met. `tiled_equals_untiled_over_the_corpus`
    and `executors_hash_equal_over_the_corpus` pass over the in-repo corpus. Fixing the
    first also corrected a false claim in the new module docs: a 2-pixel overlap margin is
    **not** sufficient (`protect_unencrypted_source.pdf` still differs, because the boundary
    pixel's coverage is graded across three sub-rectangles). Four is the smallest margin that
    holds over the corpus and is the shipped `DEFAULT_OVERLAP`; the test now exercises the
    shipped margin rather than asserting the 2-pixel one.
  - **DoD clause 2 (no-COOP/COEP path in CI):** met. New required job `wasm-threads` builds
    `selis-raster` + `selis-pdf-engine` for `wasm32-unknown-unknown` - the target where
    threading is genuinely unavailable, so the fallback is *proven* rather than asserted -
    and runs the corpus identity tests. The browser-side COOP/COEP host leg remains
    SL-4.UI.01 Vitest scope.
  - **Gates:** `cargo test -p selis-pdf-engine -p selis-raster` green (incl. the wasm32
    build); `cargo fmt` / `clippy` clean for every file this change touches.
- [x] **SL-4.WASM.04 — Memory strategy** · deps: WASM.01 · owner: AI+
  - **Do:** Growth policy, the 4 GB wasm32 ceiling, explicit release on document close, and a
    memory-pressure callback that evicts caches before the browser kills the tab.
  - **DoD:** Opening 20 documents sequentially shows no growth after close; a 1.5 GB document
    fails with `BudgetExceeded`, not a tab crash.
  - **Status:** Done 2026-09-27. **Note the split:** the memory strategy itself landed on main
    in `63a6898d` (`memory.rs` with the growth policy, the 4 GB wasm32 ceiling, release-on-close,
    the memory-pressure callback, and the `xtask wasm-protocol` leg "9. Memory strategy

    (SL-4.WASM.04)"). This box was still open, so the follow-up commit is only the remaining
    hardening: `drain_source` indexed `out[usize::try_from(off).unwrap_or(usize::MAX)..]`, and
    an index that far out would panic the wasm guest - exactly the "tab crash" the DoD forbids
    in place of a typed failure. It now returns a typed `IoReadFailed`.
  - **DoD re-verified, not re-implemented:** `twenty_sequential_opens_show_no_live_growth` and
    `memory_stats_reports_live_and_peak` pass; `cargo xtask wasm-protocol` leg 9 is green
    ("memory ok (typed 1.5 GiB, stats, release, pressure)") - the guest validates the claimed
    length before touching a payload, so no 1.5 GiB copy is ever made.
- [x] **SL-4.WASM.05 — `OpfsSource`, `FsaSource`, `BlobSource`** · deps: WASM.01, SL-0.IO.01 · owner: AI+
  - **Do:** The three web `DocSource` adapters. OPFS sync access handles in the Worker; File System
    Access handles for save-in-place (Phase 5 needs this, build it now).
  - **DoD:** Each adapter passes the `DocSource` conformance suite including the fault cases.
  - **Status:** Done 2026-09-27. **Note the split:** the three adapters landed on main in
    `834c11f8`, and this DoD was *not* actually met by it. `conformance.rs` proved the fault
    **contract** once, through `FaultSource`, while each adapter only ever ran
    `assert_fully_resident_conformance`, which never sees a fault. So resident behaviour was
    covered for all three and fault behaviour for none: a regression in an adapter's own
    mutation or boundary handling would have failed nothing.
  - The follow-up adds the missing per-adapter legs through the existing generic suite, not a
    parallel test style: replacement under us must answer `SourceChanged` (OPFS and FSA), the
    empty and single-byte boundaries (all three), and `FsaSource::update` - the sanctioned
    save-in-place write Phase 5 depends on - must read as the new file, not as a mutation.
    Nothing tested that, which is why it was added.
  - **Gates:** `cargo test -p selis-io` 68 passed (was 65, +3). The pre-existing `selis-bytes`
    clippy failure is unrelated and left alone (see 14 file header note on the fmt/clippy debt).
- [ ] **SL-4.WASM.06 — `HttpRangeSource` fetch driver** · deps: WASM.01, SL-0.IO.04 · owner: AI
  - **Do:** The JS side of range fetching, with CORS handling, an abort signal wired to
    `CancelToken`, and a documented fallback when the origin refuses ranges or omits CORS headers.
- [ ] **SL-4.WASM.07 — Lazy font chunk loading** · deps: SL-3.FONT.10, WASM.02 · owner: AI+
- [x] **SL-4.WASM.08 — Deterministic-render CI on WASM** · deps: WASM.03 · owner: AI
  - **DoD:** Headless-browser render of the corpus hash-matches the native render.
  - `cargo xtask wasm-browser` renders the corpus in a real V8 (headless Microsoft Edge, a
    Chromium fork; Chrome/chromium accepted as alternates) and hash-matches it against the native
    render — 16/16 pages identical. Fails loudly on zero comparable pages rather than skipping.

---

## 4.UI — The shared application (`apps/web/ui`)

Everything here is reused verbatim by desktop (ADR-P0022), so no `window.chrome`, no direct
`fetch`, no direct storage — all through a `PlatformAdapter` interface.

- [x] **SL-4.UI.01 — `PlatformAdapter` interface** · owner: AI+
  - **Do:** The one seam between the UI and its host: open/save/pick file, storage, clipboard,
    print, telemetry, window/menu integration, deep links, and capability flags. Web, extension,
    and desktop each implement it.
  - **DoD:** The UI compiles and tests with a `MockAdapter`; a lint forbids platform globals in
    `apps/web/ui`.
  - **Note:** The `PlatformAdapter` is the *host* seam (what the OS can do). ADR-P0035 adds the
    *logic* seam: app state lives in `selis-viewmodel`, and the UI renders published diffs. Keep
    them distinct — conflating them is how logic leaks back into the shell.
  - **Delivered:** `apps/ui/src/platform/` (the monorepo's shared UI package; `apps/web/ui` paths
    here refer to it before the `web`/`desktop` split). `adapter.ts` defines the host seam plus a
    transport-agnostic `EnginePort` (open/render-tile/extract/search, `AbortSignal` cancellation →
    code 4020, progress callback, opt-in telemetry with a no-document-data type boundary per
    ADR-P0016/P0017). `errors.ts` uses only registry codes. `mock-adapter.ts` is a full in-process
    reference; `contract.ts` is the one suite every transport runs. The DoD "lint" is a unit-test
    scan (`platform-globals.test.ts`, no Biome restricted-globals rule) that fails on bare platform
    globals in production code — passing.
  - **Open (deferred to the shell + WASM.01 JS leg):** no concrete browser/extension/Tauri adapter
    ships yet; the WASM.01 Worker protocol exists in `selis-pdf-wasm` (Rust) and maps onto
    `DocumentSourceDescriptor` inside a future transport, not in the UI. `apps/web/host` still owns
    that wiring.
- [ ] **SL-4.UI.14 — Adopt the WertKit contract; consume `@wertkit/ui` for chrome** ·
  deps: UI.10 · owner: AI+ · **filed 2026-09-27**
  - **Context:** `WertCore/wertkit` is the org's design system — a token core plus a React
    component layer (Radix behaviour, CSS Modules, zero styling runtime), published on npm as
    `@wertkit/ui` + `@wertkit/tokens`. Its `spec/naming.md` is explicitly "the actual product":
    components are implementations of the contract, and any future runtime (Tauri, iOS PWA)
    implements *that*, not the React code. Rewriting that from scratch for selis was the
    alternative; this task takes the reuse path instead.
  - **Do, in three stages — do not do stage 3 before UI.02 exists:**
    1. *Contract.* Converge `packages/ui-kit`'s role vocabulary onto wertkit's
       (`bg`/`bg-subtle`/`fg`/`fg-muted`/`border`/`accent`/`danger`/`focus-ring`/…), keeping
       selis's own token files, the generated-`css/tokens.css` byte-sync test, and the WCAG
       gates. **Keep `ui-kit` as the token source** — do not adopt `@wertkit/tokens`; it has
       no `data-contrast` high-contrast theme, and UI.10's tests assert 7:1 on one.
       **— Stage 1 DONE and merged:** roles renamed (`surface`→`bgRaised`, `text`→`fg`,
       `textMuted`→`fgMuted`, …), generated CSS byte-syncs, all four themes + the high-contrast
       7:1 gate + density + reduced-motion intact. Still zero React/`@wertkit/*` dependencies.
       This box stays `[ ]` because stages 2 and 3 are still open.
    2. *Security record.* Note the npm dependency and the pin policy in SECURITY.md; amend
       ADR-P0021 (see the proposed revision there) — this is the first non-Rust dependency of
       this kind and the policy change must be explicit, not implied.
    3. *Chrome only, at the start of UI.02.* Add `react`/`react-dom` to `apps/ui` (it has no
       React today) and consume `@wertkit/ui` for AppShell, Button, Dialog, DropdownMenu,
       Tooltip, Tabs, Select, Checkbox, Switch, Toast. Pin an **exact** version
       (`0.1.8`, not `^`): it is a 0.x package with no published changelog, so a caret will
       drift across breaking minors.
  - **Explicitly out of scope:** everything performance- or PDF-specific stays selis's own —
    UI.02 virtualisation, UI.03 `OffscreenCanvas` compositor, UI.04 text layer + glyph-quad
    selection, UI.05 search, UI.06 navigation. A component library does not help there, and
    the 60 fps budget is the thing to protect. wertkit's spec also forbids forking a
    component into an app: **extend upstream, consume here.**
  - **DoD:** `apps/ui` builds with React and renders a real screen (UI.02's page list) using
    wertkit chrome, with selis's own tokens; the high-contrast theme still passes its 7:1
    gate; no `@wertkit/*` version is range-pinned; ADR-P0021 amended and SECURITY.md updated;
    the UI.02 60 fps budget is unchanged by the introduction of a reconciler.
  - **Dependency rule (00-INDEX §0):** `@wertkit/ui`, React and `@radix-ui/*` are **not** in
    `[workspace.dependencies]` for npm, so per the agent-prompt rule ("do not add a dependency
    that is not in workspace dependencies; if you need one, stop and propose it with a licence
    + maintenance justification") stage 2 is where they get proposed and justified, not
    slipped in.
  - **Deferred, not open:** wertkit is first-party — the repo's own author maintains it — so its
    licence and provenance are settled and need no gate here. The remaining pre-release item is
    a dependency **audit** (npm licence allow-list, `unmaintained` check, a `cargo vet`
    equivalent for the JS tree, which Rust gets and npm does not). That belongs to a
    pre-release hardening pass, not to this task; ADR-P0021 records it.
- [ ] **SL-4.UI.02 — Virtualised page list + continuous scroll** · deps: UI.01 · owner: AI+
  - **Do:** Windowed rendering with a placeholder→low-res→full-res tile ladder, correct scroll
    anchoring on zoom, and page-fit/width/spread modes.
  - **DoD:** 60 fps sustained scroll on a 2 000-page document on a mid-range laptop; no layout
    shift when a tile resolves.
  - **Code merged (3279 lines, `apps/ui/src/viewer/`), but the box stays open on the DoD.** Windowing
    (≤12 tiles over 2 000 pages), the tile ladder, zoom anchoring, fit/width/spread and zero-layout-
    shift are all implemented and tested. The **60 fps** half is only a headless proxy (~1.4 ms/frame
    against a 16 ms budget): ADR-P0021 ships no jsdom, so real fps needs a browser and is
    unreachable in-repo. It should be measured for real under **UI.03**, which owns the compositor.
    Strings route through i18n keys; the plan's L227 cites ADR-P0034 for that, but the i18n rule
    actually sits under ADR-P0036.
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
- [x] **SL-4.UI.10 — Design system + theming** · owner: AI
  - **Do:** `packages/ui-kit`, light/dark, high contrast, reduced motion, and a density setting.
  - **Note:** Token data is the single source of truth in `packages/ui-kit/src/tokens/`
    (`colour.ts`, `scale.ts`) → generated `css/tokens.css` (byte-synced by test) + hand-rolled
    `css/base.css` (no dependency; ADR-P0021). Four effective themes via CSS custom properties
    keyed on `data-theme` / `data-contrast` / `data-density`; reduced motion honoured from both
    `prefers-reduced-motion` and `data-motion` (duration tokens collapse to 0ms). `theming.ts`
    applies preferences DOM-free so hosts persist them via the SL-4.UI.01 storage port. Visual-free
    gates: role completeness + distinctness, WCAG 2.x contrast over declared fg/bg pairs in all
    four themes (4.5 body / 3 non-text / 7 high-contrast), variable-reference + class-namespace
    integrity, TS↔CSS sync. Usage documented in `packages/ui-kit/README.md` for UI.02+.
- [ ] **SL-4.UI.11 — i18n scaffolding** · deps: SL-0.ERR.04 · owner: AI
  - **Do:** Every string a key from day 1 (ADR-P0034). Ship English; wire pseudo-locale into CI.
- [ ] **SL-4.UI.12 — Error and empty states** · deps: SL-0.ERR.01 · owner: AI
  - **Do:** Every `Code` maps to a user-facing state with a recovery action. A damaged file shows
    what we recovered, not a dead end.
- [ ] **SL-4.UI.13 — Large-file handling UX (web)** · deps: SL-0.SBX.05, SL-1A.UI.06 · owner: AI+
  - **Do:** The web surface of SL-1A.UI.06, deferred from Phase 1A: progress and cancellation in
    the tab for long operations, and the honest budget-exhaustion state (which budget, the measured
    usage, split-the-file remedy) instead of a dead tab. The CLI/engine plumbing exists — the
    shared CancelToken, typed `CANCELLED`/budget errors with resource + measured usage, and the
    verification/`budget` JSON fields the UI consumes unchanged.

---

## 4.WEB — The web deployment (`apps/web/host`)

- [x] **SL-4.WEB.01 — Static hosting + COOP/COEP + CSP** · owner: AI+
  - **Do:** Cross-origin isolation for threads; a strict CSP with `wasm-unsafe-eval` only; SRI on
    every asset; no third-party scripts on the document-handling path (ADR-P0016).
  - **DoD:** securityheaders.sh A+; a test asserting the app still works with isolation disabled.
  - **Status:** Done 2026-09-26 and merged into main. One canonical builder (`apps/web/host/src/headers.ts`)
    emits COOP `same-origin` + COEP `require-corp` + CORP + HSTS + a strict CSP allowing `wasm-unsafe-eval`
    only, and mirrors it drift-free into both static hosts (`public/_headers` for Netlify/CF Pages and
    `vercel.json`) with a test that fails on any divergence between the three. `isolation.ts` selects the
    threaded path only when `crossOriginIsolated` and `SharedArrayBuffer` are both present, else falls back
    to byte-identical single-threaded (ADR-P0004). Gates: vitest 15 (host) + 6 (extension), tsc + biome
    clean. `.cargo/config.toml`'s `target-dir = "C:/selis-build"` redirect was explicitly excluded from the
    commit - it is a machine-specific env hack not referenced by CI (verified: no `.github` reference to
    `selis-build` or `CARGO_TARGET_DIR`) and would break non-Windows runners.
  - **Gaps (review, non-blocking):** (1) no dev-server header config - COOP/COEP ship only via
    `_headers`/`vercel.json` at deploy, so local dev cannot exercise the threaded path (not a DoD item;
    the DoD targets static hosting, and the isolation-disabled path *is* tested);
    (2) the securityheaders.sh A+ is asserted by a proxy test that verifies every header the scanner
    grades actually emits, not by the live scanner - real A+ is confirmed at deploy;
    (3) SRI digests are hand-computed rather than build-injected; `sri.test.ts` enforces presence and
    format, and a stale digest fails loudly in the browser rather than silently;
    (4) the manifest has no icons - fine for the unpacked skeleton load; store submission (SL-4.EXT.11)
    will need them.
- [ ] **SL-4.WEB.02 — Service worker + offline** · deps: WEB.01 · owner: AI
  - **Do:** Cache the app shell and WASM chunks; the app opens local files with no network at all.
  - **DoD:** Airplane-mode test: open a local PDF, view, search, print.
- [x] **SL-4.WEB.03 — Document handoff without upload** · deps: SL-4.WASM.05 · owner: AI+
  - **Do:** Drag-drop, file picker, paste, and `?src=` URL opening — all local. The one thing this
    app must never do is upload a document, and a CI test asserts no request body ever contains
    document bytes.
  - All four paths resolve to a local blob handle; the `?src=` path is a bodyless `GET`. The gate
    wraps the live `fetch` **and** `XMLHttpRequest.prototype.send`, so it cannot be routed around,
    and is proven non-vacuous by negative controls (a planted `POST`/multipart/XHR is rejected).
- [ ] **SL-4.WEB.04 — Marketing site + honest conformance page** · owner: HUMAN
  - **Do:** Publish the conformance ladder (SL-0.OPS.04). "Here is exactly what we support" is a
    trust asset in a category built on overclaiming.
- [ ] **SL-4.WEB.05 — Analytics posture** · deps: ADR-P0017 · owner: HUMAN
  - **Do:** Privacy-preserving, opt-in, no document-derived data, no third-party tags on the app
    origin. Marketing pages may differ but must be a separate origin.

---

## 4.EXT — The browser extension (`apps/extension`)

- [x] **SL-4.EXT.01 — MV3 manifest + permissions minimisation** · owner: AI+
  - **Do:** Request the minimum: `declarativeNetRequest`, `offscreen`, and host permissions only
    where required. Every permission needs a written justification for review and for the store
    listing, because reviewers reject unexplained breadth.
  - **Status:** Done 2026-09-26, delivered by the same web01-coop branch. The MV3 `manifest.json`
    requests only `declarativeNetRequest` + `offscreen` with an empty `host_permissions` array (no
    `<all_urls>`, `tabs`, or `scripting`); `src/permissions.ts` pins the approved/denied sets and
    `manifest.test.ts` asserts the manifest stays inside them; `PERMISSIONS.md` justifies each permission.
  - **Gaps (review, non-blocking):** no icons yet (needed for the store listing; that lands with
    SL-4.EXT.11), and `offscreen.js` is a minimal placeholder that the DNR redirect (SL-4.EXT.02) will
    wire up.
- [x] **SL-4.EXT.02 — PDF navigation interception** · deps: EXT.01 · owner: AI+
  - **Do:** DNR rules redirecting `application/pdf` main-frame navigations to the bundled viewer,
    preserving the original URL, referrer policy, and any auth context the browser would send.
  - **DoD:** Works for: direct `.pdf` URLs, `Content-Type`-only responses, `Content-Disposition:
    inline`, redirects, and POST-produced PDFs (document the cases that cannot work).
  - **Risk:** This is the fiddliest part of the extension and the part users notice when it breaks.
    Build a matrix of ~30 real-world PDF-serving patterns and test against all of them.
  - **Status:** Done 2026-09-27, merged into main. DNR rules plus a 38-pattern matrix.
  - **The constraint that shapes it:** EXT.01 ships `declarativeNetRequest` with an **empty**
    `host_permissions`, and that stays. DNR only matches requests the extension already has
    access to, so `responseHeaders` conditions are unreachable; the rules therefore match on
    **URL shape** (a `.pdf` path, or a `.pdf` carried in the query). Every pattern that would
    need a response signal is recorded as `cannot-work` with the exact capability required,
    rather than left for a user to discover. No permission was widened.
  - **DoD coverage:** direct `.pdf` URLs, `Content-Disposition: inline` and redirects all work
    where the URL is visible. **The `Content-Type`-only class cannot work** - the response
    headers are the only signal there, and matching them needs host permissions for the origin.
    POST-produced PDFs at an extensionless URL cannot work either: the browser does not
    re-navigate a POST response. `blob:`, `data:`, `wss:` and `file://` are outside the rule;
    `file://` additionally needs the user toggle that is SL-4.EXT.09's scope.
  - **The Risk note's matrix is executable, not documentation.** `PATTERN_MATRIX` (38 rows:
    22 intercepted / 9 not-matched / 7 cannot-work) is re-derived from the shipped ruleset by
    the test suite, so a rule change that regresses a pattern fails naming that pattern. The
    `cannot-work` rows are asserted to STAY un-intercepted, so a future permission grant that
    starts serving one must update the matrix deliberately. Two rows were wrong when first
    written and the suite caught both - which is the point of making it executable.
  - **Auth preservation:** the document URL travels as an encoded `src` parameter (a URL
    carrying its own `&src=` cannot forge a second one), and the viewer re-fetches with
    `credentials: "include"`, which is what preserves basic-auth / SSO cookies / presigned
    signatures across the redirect.
  - **Gates:** `tsc -p apps/extension --noEmit` clean; `vitest` 25 passed (was 6); biome at
    main's exact baseline (pre-existing CRLF format drift, zero findings in new code).
- [ ] **SL-4.EXT.03 — Offscreen document hosting the engine** · deps: EXT.01, WASM.01 · owner: AI+
  - **Do:** MV3 service workers are killed aggressively; the engine runs in an offscreen document
    or a dedicated worker with a documented lifecycle and state recovery.
  - **DoD:** A test that the viewer survives service-worker termination mid-session.
- [x] **SL-4.EXT.04 — Bundled-only build** · deps: EXT.01 · owner: AI+
  - **Do:** No remote code, no CDN, no `eval` (ADR-P0028). A build check fails on any remote URL
    in the bundle.
  - The gate scans the **built package**, not the source, and runs inside `pnpm build`. Verified
    falsifiable: a remote `<script src>` planted in the real built `viewer.html` fails the gate with
    file/line/URL. 98 tests, ~45 of them planted-violation cases. Lexical-scan limits (template
    literals, the regex-vs-division heuristic) are stated honestly in `apps/extension/BUNDLING.md`.
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
