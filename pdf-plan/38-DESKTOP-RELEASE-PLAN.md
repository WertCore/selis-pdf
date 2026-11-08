# 38 — Desktop Release Plan

**Status:** execution plan, opened 8 November 2026. Companion to `16-PHASE-6-desktop.md`
(the full Phase-6 scope). This file is the one that gets ticked on the road from *nothing built*
to a **signed, notarised desktop release with auto-update**.
**Scope:** the desktop app only — the Tauri shell, the native bindings, native file open/save,
packaging, signing/notarisation, auto-update, and OS integration. The CLI (`6.CLI`) and the
mobile shells are out of scope here.
**Related:** `16-PHASE-6-desktop.md`, `01-ARCHITECTURE.md §9` (Desktop), `02-ADRS.md`
(ADR-P0022 native shells / Tauri-only, ADR-P0035 `selis-viewmodel`, ADR-P0036 Tauri-for-now,
ADR-P0044 host-blind `apps/ui`), `00-INDEX.md §3` (`SL-0.LEAD.02`, `.03`), `25-RISK-REGISTER.md`.

**Rule (inherited from `35-PARITY-CHECKLIST.md`):** a box is ticked only when the slice has
shipped, builds in CI, and has evidence. A card, a mock, a stub adapter, or an unverified claim
never ticks a box.

---

## 1. Current state — honest

**There is no desktop app.** `apps/` contains exactly four workspaces — `cli`, `extension`, `ui`,
`web` (`apps/` listing). There is **no `apps/desktop`**, no `src-tauri`, no `tauri.conf.json`, and
**no reference to Tauri anywhere in the repository** — not in any `Cargo.toml`, `package.json`,
workflow, or `.rs` file (a repo-wide grep for `tauri` outside `pdf-plan/` returns nothing). Desktop
is, at this moment, a plan in `16-PHASE-6-desktop.md` and two ADRs — **nothing more is built.**

**`selis-viewmodel` does not exist.** ADR-P0035 specifies a format-neutral L3 crate that owns all
app logic above the engine (page/zoom, active tool, selection, undo/redo presentation, command
enablement, dirty state) and publishes **diffs**. The `crates/` directory has 39 crates; none is
`selis-viewmodel`, and the architecture's own crate table (`01-ARCHITECTURE.md §3`, L3) lists it as
planned. Today that logic lives in TypeScript inside `apps/ui/src/viewer/*` (page-list, selection,
search, navigation) — i.e. it is **already implemented once, in the shell**, which is exactly the
single-shell situation ADR-P0035 exists to prevent before a second shell (desktop) is added.

What **does** exist, and is the real foundation the desktop build stands on:

| Asset | Where (verified) | Why it matters to desktop |
|---|---|---|
| **A host-blind UI** | `apps/ui` (`@selis/ui`), `apps/ui/src/{viewer,chrome,platform,i18n}` | ADR-P0044: production TS under `apps/ui/src` names none of 18 platform globals and runs its tests in plain Node. The same bundle is meant to run in browser, extension, and desktop. |
| **A `PlatformAdapter` seam** | `apps/ui/src/platform/{adapter.ts,contract.ts,mock-adapter.ts}` | `contract.ts` already names "the **desktop Tauri transport** later" as a conformance target. Today only **mock / worker / extension** adapters exist — **no desktop adapter**. |
| **Native-ready core crates** | `crates/selis-pdf-*`, `crates/selis-*` | These are bytes-in/bytes-out and already linked **natively** by `apps/cli` (see below) — not only through WASM. |
| **`selis-pdf-tools` with `native-fs`** | `crates/selis-pdf-tools/Cargo.toml` (`default = ["native-fs"]`), `src/io.rs`, `src/write_gate.rs` | The native filesystem input/output path plus a chunked, cancellable, atomic commit — the desktop's open/save backend already exists as a crate feature. |
| **A native engine with a render surface** | `crates/selis-pdf-engine/src/session.rs` (`Session`, `render_page_rgba` at l.1293, `render_page`, `render_page_lazy`) | The desktop viewer calls this directly — no WASM, full threading (ADR-P0004 / ADR-P0022). |
| **A native file source + append sink** | `crates/selis-io` (`native-io` feature; `src/append_sink.rs` = `AppendFileSink`, fsync-commit) | `FileSource`/`AppendFileSink` back `DocSource`/`DocSink` on disk; `ADR-P0037` atomic-replace lives in `selis-io`. |
| **The whole stack, proven natively** | `apps/cli` (`selis-cli`) | `apps/cli/Cargo.toml` links `selis-pdf-engine` (with `builtin-fallback-fonts`), `selis-pdf-tools` (`native-fs,security,sign,text`), `selis-pdf-edit`, `selis-pdf-content`, `selis-pdf-convert`, `selis-policy`, `selis-job`, `selis-io` (`native-io`) — i.e. **every crate the desktop needs is already compiled and tested as a native binary.** |
| **A local-first egress gate** | `apps/web/host/src/no-upload.ts` (+ `.test.ts`), `xtask/src/purity.rs` | The web "no-upload" assertion (`assertNoUpload`, WEB.03) and the Rust purity check (no net API below L4) are the templates the desktop must mirror. |

**The one-line summary of current state:** the **engine, the tools, native file I/O, and a
host-blind UI all exist and are proven by the CLI and the web app; the desktop *shell* that binds
them — the Tauri app, its `PlatformAdapter`, and `selis-viewmodel` — is 0% built.** Desktop is a
*binding and packaging* problem, not an engine problem. That is the good news; the bad news is that
binding and packaging is most of the operational cost, and the signing lead times are long.

### What CI does and does not cover today

`.github/workflows/ci.yml` has a 6-target build matrix, but **every job `runs-on: ubuntu-24.04`**,
the Windows target is `allow_fail: true`, and there is **no macOS or Windows runner, no Tauri build,
and no OS-matrix**. There is therefore no CI evidence that anything runs on a real desktop OS yet.
`16-PHASE-6-desktop.md §6.QUAL.01` (OS matrix CI) is unstarted.

---

## 2. Big remaining items

The large pieces between here and a desktop release, biggest first. Each maps to tasks in §3 and,
where it already has one, to the parent task in `16-PHASE-6-desktop.md`.

1. **`selis-viewmodel` (the missing crate).** ADR-P0035's L3 view-model does not exist. Shipping a
   second shell (desktop) **without it** means re-implementing page/zoom, selection, undo
   presentation, command enablement, and dirty-state a second time in Rust/Tauri — three chances to
   diverge from the TS viewer on questions like "does highlighting mark the document dirty." This is
   the single biggest *architecture* item and it gates a clean shell. It is large because today's
   app logic lives in `apps/ui/src/viewer/*` (TS) and must be lifted into a format-neutral crate that
   publishes diffs, with the TS viewer refactored to consume those diffs. **Decision required** (see
   §5, R-D3): build it before the desktop shell, or accept a deliberate, time-boxed divergence and
   file the debt.

2. **The Tauri shell app itself (`apps/desktop`).** Nothing exists. This is `SL-6.SHELL.01–05`:
   a Tauri v2 project, a window, the desktop `PlatformAdapter` (Tauri commands replacing the
   worker `postMessage` the web uses), the native engine host (engine off the UI thread, cancel
   wired to window-close), multi-window/tabs/session-restore, native menus/shortcuts, and file
   dialogs/recent-files/drag-drop. ADR-P0022 and ADR-P0036 lock the toolkit to **Tauri v2, not
   Electron** — do not relitigate.

3. **Native file open/save wired end-to-end.** The web runs in-memory: the tools chunk routes reads
   and commits through `MemorySession` (`crates/selis-pdf-tools/src/memory.rs`, thread-local
   `insert_input`/`take_output`) and the viewer opens a mock fixture (`apps/ui/src/chrome/app.tsx`
   uses `createMockAdapter`; "UI.07 owns file opening" and is unbuilt). The desktop replaces this
   with real paths: `FileSource`/`FsaSource`-equivalent on open, `AppendFileSink` + the `native-fs`
   `write_gate` atomic commit (`temp + fsync + rename`, ADR-P0037) on save. The **crate pieces exist**;
   the **wiring through the shell and a real file-open UI does not.**

4. **Wiring the shipped tool operations natively.** The 42-tool surface (`34`/`35`/`36`) runs in the
   browser through `selis-pdf-wasm-tools` (ADR-P0045). The desktop calls the **same `selis-pdf-tools`
   crate directly** — no WASM, no chunk ABI — exactly as `apps/cli` already does. This is mostly
   adapter plumbing (map a UI tool invocation to a native `selis-pdf-tools` call) because the CLI has
   already proven every operation on the native filesystem backend.

5. **A viewer / render surface in the shell.** `Session::render_page_rgba` produces RGBA tiles
   natively today; the desktop must paint them (Tauri webview compositor or a native surface) and
   feed input back. The engine already owns the tile cache (ADR-P0025), so the shell is a *painter*,
   not a renderer.

6. **Packaging** — macOS `.app`/`.dmg`, Windows `.msi`/`.msix`, Linux AppImage/deb/rpm/Flatpak
   (`SL-6.DIST.01/02/03`). None exists.

7. **Code signing + notarisation** — macOS Developer ID + `notarytool` + stapling (`SL-6.DIST.02`,
   **blocked on `SL-0.LEAD.02` Apple Developer Program**), Windows Azure Trusted Signing
   (`SL-6.DIST.01`, **blocked on `SL-0.LEAD.03`**). These are the **long-lead, HUMAN-owned blockers**;
   `00-INDEX.md §3` quotes 5–14 days for the Apple D-U-N-S alone and 1–6 weeks for Windows org
   validation. Start them first because nothing downstream can be *released* without them.

8. **Auto-update** (`SL-6.DIST.04`, ADR-P0029). Consume the shared updater (TUF). The Selis-specific
   requirement: **an update must never apply while a document has unsaved changes**, and rollback
   must be possible. ADR-P0029 is Provisional with a revisit at G6 — if Wertcore's updater is not
   ready, Selis builds a minimal updater rather than block.

9. **OS integration** — default-PDF-handler registration + file associations (`SL-6.OS.01`),
   thumbnail providers / Quick Look (`SL-6.OS.02`), print pipeline (`SL-6.OS.03`), menus. All
   unstarted and per-platform.

10. **Local-first enforcement on desktop.** ADR-P0016's claim ("no document byte leaves the device
    without consent") is enforced on the web by `no-upload.ts` and by `xtask check-purity`
    (`xtask/src/purity.rs`). The desktop needs the **same mechanism at the shell boundary**: an
    egress test over the Tauri process, plus the policy chokepoint. Note `crates/selis-policy`
    (356 lines) today exposes `Permissions`/`check_permissions`/`ContentOp` (active-content policy,
    ADR-P0020) but **not** the `CloudConsent`/`Feature`/`Surface` enums ADR-P0016/P0164 describe — so
    the policy crate itself is only partway to the chokepoint the desktop will lean on.

11. **Crash reporting with document-byte stripping** (`SL-6.DIST.06`, ADR-P0017) and the
    **power-loss atomicity harness** (`SL-6.QUAL.05`) — both desktop-GA quality gates.

---

## 3. Phased plan

Task IDs continue the house scheme (`SL-6.DESK.NN`) and sit *under* the existing `16-PHASE-6` areas
rather than replacing them: `6.DESK` is the ordered release track; `6.SHELL/OS/DIST/QUAL` remain the
canonical task definitions. Owner legend is `00-INDEX.md §0` (`AI` / `AI+` / `HUMAN`).

Four phases: **D0 Foundations → D1 Desktop MVP → D2 Feature-complete unsigned → D3 Signed/notarised
GA with auto-update.** The HUMAN long-lead items (Apple, Windows signing) run **in parallel from D0**
because their calendar time, not their effort, is the critical path.

### Phase D0 — Foundations (unblock everything)

Runs in parallel with Phase 5; depends only on the UI contract (`SL-4.UI.01`, shipped) and the
native crates (shipped).

- [ ] **SL-6.DESK.00 — Start the signing long-leads (do this first)** · owner: HUMAN
  - **Do:** Begin `SL-0.LEAD.02` (Apple Developer Program — Individual enrolment carries the same
    signing rights if the Organization D-U-N-S is the blocker) and `SL-0.LEAD.03` (Windows Azure
    Trusted Signing). Neither blocks engineering; both block *release* (D3).
  - **Gate note:** this is the item most likely to slip the GA date. File the enrolment the same
    week this plan is approved.
- [ ] **SL-6.DESK.01 — `selis-viewmodel` crate (ADR-P0035)** · deps: — · owner: AI+
  - **Do:** Create `crates/selis-viewmodel` (L3, Tier B). Lift app logic out of `apps/ui/src/viewer`
    into a format-neutral crate: page/zoom, active tool, selection, undo/redo presentation, search
    match state, command enablement, dirty state, panel/dialog state. Publish **diffs, not
    snapshots**. No widgets, no platform calls.
  - **DoD:** `cargo xtask check-layers` passes with the new crate; a property test that apply-then-
    publish yields a diff the TS viewer can replay to an identical state; the TS viewer is refactored
    to consume diffs (or the divergence debt is explicitly filed — see §5 R-D3).
  - **Risk:** biggest single item; state crosses a language boundary on every update.
- [ ] **SL-6.DESK.02 — Desktop egress gate + policy chokepoint parity** · deps: — · owner: AI+
  - **Do:** Port the web `assertNoUpload` posture (`apps/web/host/src/no-upload.ts`) and
    `xtask check-purity` (`xtask/src/purity.rs`) to the desktop process: a test that fails if any
    document-derived byte leaves the Tauri app without a `selis-policy` consent token. Add the
    missing `CloudConsent`/`Feature` surface to `selis-policy` if D1 needs it.
  - **DoD:** a desktop no-egress test runs in CI and fails on a seeded leak; mirrors ADR-P0016.
- [ ] **SL-6.DESK.03 — OS-matrix CI skeleton** (= `SL-6.QUAL.01` start) · deps: — · owner: AI
  - **Do:** Add macOS and Windows runners to `.github/workflows/ci.yml` (today all jobs are
    `ubuntu-24.04`; Windows is `allow_fail`). Build the workspace on real macOS-arm64 and
    Windows-msvc, non-`allow_fail`.
  - **DoD:** the engine + CLI build and test green on macOS and Windows runners.

**Gate GD0:** `selis-viewmodel` exists and `check-layers` enforces it (or the divergence debt is
filed with an owner); a desktop no-egress test exists; the workspace builds green on macOS and
Windows CI. *No shell yet — this gate is about removing the blockers.*

### Phase D1 — Desktop MVP (open a PDF, run the shipped tools, save)

The first thing a user can double-click. No signing, no OS integration, no auto-update.

- [ ] **SL-6.DESK.10 — `apps/desktop` Tauri v2 project + desktop `PlatformAdapter`**
  (= `SL-6.SHELL.01`) · deps: DESK.01 · owner: AI+
  - **Do:** Create `apps/desktop` as a Tauri v2 app that loads the **same `@selis/ui` bundle**.
    Implement the desktop `PlatformAdapter` so it passes the existing
    `apps/ui/src/platform/contract.ts` conformance suite (the one that already names "desktop Tauri
    transport"). Tauri commands replace the worker `postMessage` path.
  - **DoD:** the same UI bundle runs in browser and desktop; the desktop adapter passes the platform
    contract; a CI check prevents desktop-only code leaking into `apps/ui` (ADR-P0044 gate extended).
- [ ] **SL-6.DESK.11 — Native engine host** (= `SL-6.SHELL.02`) · deps: DESK.10 · owner: AI+
  - **Do:** Run `selis-pdf-engine` natively on the Rust side of Tauri (no WASM, full threading,
    ADR-P0004). Engine calls off the UI thread; cancellation wired to window close/navigation;
    memory budget derived from system RAM (`selis-sandbox`).
  - **DoD:** a 100 MB document opens without blocking the UI; closing the window cancels an in-flight
    render within one budget tick.
- [ ] **SL-6.DESK.12 — Native open: real file → `Session`** (part of `SL-6.SHELL.05`)
  · deps: DESK.11 · owner: AI+
  - **Do:** Replace the mock fixture (`apps/ui/src/chrome/app.tsx` `createMockAdapter`) on the
    desktop path with a real file open: native file dialog → `FileSource` (`selis-io` `native-io`) →
    `Session`. Render with `Session::render_page_rgba`; paint tiles in the webview compositor.
  - **DoD:** a user opens a PDF from disk and sees it render; scrolling is served from the engine
    tile cache (ADR-P0025).
- [ ] **SL-6.DESK.13 — Shipped tools, natively** · deps: DESK.11 · owner: AI+
  - **Do:** Wire the shipped tool operations (merge/split/rotate/unlock/protect/compress and the
    Phase-3 text tools) to call `selis-pdf-tools` **directly** (feature set `native-fs,security,sign,
    text`), exactly as `apps/cli` does — no WASM chunk, no `selis-pdf-wasm-tools` ABI.
  - **DoD:** every tool that ships in the web/CLI runs from the desktop UI against on-disk files and
    produces byte-identical output to the CLI for the same input (reuse the CLI's test fixtures).
- [ ] **SL-6.DESK.14 — Native save: atomic commit to disk** (part of `SL-6.SHELL.05`)
  · deps: DESK.12, DESK.13 · owner: AI+
  - **Do:** Save through the `native-fs` `write_gate` (`crates/selis-pdf-tools/src/write_gate.rs`):
    structural verify → `temp + fsync + rename` atomic commit (`selis-io::atomic_replace`,
    ADR-P0037). Incremental-append by default (ADR-P0007); a cancel before rename leaves the
    destination untouched.
  - **DoD:** save-in-place works; a kill before the rename leaves the original file intact; output
    opens in Acrobat and PDFium.

**Gate GD1 (Desktop MVP):** on at least one OS, a user can **open a PDF, run every shipped tool, and
save**, with the atomic-commit and no-egress guarantees holding. Engine runs natively off the UI
thread. *Still unsigned, single-window, no OS integration.*

### Phase D2 — Feature-complete, unsigned

Everything a user expects of a desktop editor except the signed installer.

- [ ] **SL-6.DESK.20 — Multi-window, tabs, session restore** (= `SL-6.SHELL.03`) · deps: DESK.14
  · owner: AI
  - **Do:** Restore open documents, scroll positions, and unsaved edit journals (the `.selis`
    session bundle) after a crash/restart.
- [ ] **SL-6.DESK.21 — Native menus, shortcuts, platform conventions** (= `SL-6.SHELL.04`)
  · deps: DESK.10 · owner: AI
- [ ] **SL-6.DESK.22 — File dialogs, recent files, drag-drop** (rest of `SL-6.SHELL.05`)
  · deps: DESK.12 · owner: AI
- [ ] **SL-6.DESK.23 — Default-handler registration + file associations** (= `SL-6.OS.01`)
  · owner: AI+
  - **Do:** Per-platform (Windows `HKCU` + Default Apps flow; macOS `LSHandlerRank`/`UTImportedType`;
    Linux `.desktop` + `mimeapps.list`).
- [ ] **SL-6.DESK.24 — Print pipeline** (= `SL-6.OS.03`) · deps: DESK.12 · owner: AI+
  - **Do:** Windows XPS/GDI, macOS `NSPrintOperation`, Linux CUPS; scaling, duplex, N-up,
    "print as image" fallback.
- [ ] **SL-6.DESK.25 — Thumbnail providers / Quick Look** (= `SL-6.OS.02`) · owner: AI+
  - **Risk:** these run inside the OS shell; a crash is blamed on Explorer/Finder. Strict budgets.
- [ ] **SL-6.DESK.26 — Unsigned installers for all three OSes** (packaging half of
  `SL-6.DIST.01/02/03`) · deps: DESK.14 · owner: AI
  - **Do:** Produce `.app`/`.dmg`, `.msi`/`.msix`, and AppImage/deb from CI — **without** signing
    yet, so the packaging is proven before the certs arrive.
- [ ] **SL-6.DESK.27 — Crash reporting with document-byte stripping** (= `SL-6.DIST.06`)
  · owner: AI+
  - **Do:** Mirror ADR-P0017: report the code path, strip document buffers, separate consent.

**Gate GD2:** feature parity with the web editor on the desktop; unsigned installers build in CI for
Win/mac/Linux; default-handler registration and print work; crash reports carry no document bytes.
*The only thing between here and GA is signatures, auto-update, and the GA quality bars.*

### Phase D3 — Signed, notarised GA with auto-update (= Gate G6)

Everything here depends on the long-leads started at D0 actually having landed.

- [ ] **SL-6.DESK.30 — macOS Developer ID signing, notarisation, stapling** (= `SL-6.DIST.02`)
  · deps: SL-0.LEAD.02, DESK.26 · owner: AI+
  - **Do:** Hardened runtime, the minimum entitlements (document each), `notarytool` + stapling in
    CI. Decide early whether to pursue the App Store build (it forbids some entitlements the
    sandbox work may need).
  - **Blocks on HUMAN:** Apple enrolment (`SL-0.LEAD.02`).
- [ ] **SL-6.DESK.31 — Windows code signing** (= `SL-6.DIST.01`) · deps: SL-0.LEAD.03, DESK.26
  · owner: AI+
  - **Do:** Azure Trusted Signing in CI; per-user and per-machine installs; silent-install switches;
    upgrade/rollback.
  - **Blocks on HUMAN:** Windows signing identity (`SL-0.LEAD.03`).
- [ ] **SL-6.DESK.32 — Linux packaging hardening** (= `SL-6.DIST.03`) · deps: DESK.26 · owner: AI
  - **Do:** deb/rpm/AppImage/Flatpak; budget time for Flatpak portals (sandbox vs file access +
    print).
- [ ] **SL-6.DESK.33 — Auto-update (TUF)** (= `SL-6.DIST.04`, ADR-P0029) · deps: DESK.30, DESK.31
  · owner: AI+
  - **Do:** Consume the shared updater. **Never apply an update while a document has unsaved
    changes; rollback must be possible.** If the shared updater is not ready at G6, build the minimal
    updater (ADR-P0029 revisit trigger) rather than block.
- [ ] **SL-6.DESK.34 — Power-loss atomicity harness** (= `SL-6.QUAL.05`) · deps: DESK.14 · owner: AI+
  - **Do:** Upgrade the WRITE.06 process-kill harness to VM-snapshot power loss; assert the
    three-state guarantee (absent / original / complete-or-recoverable) on NTFS, APFS, ext4.
- [ ] **SL-6.DESK.35 — Large-document soak + desktop a11y** (= `SL-6.QUAL.02/03`) · owner: AI+
  - **Do:** 8-hour soak with 1 GB+ docs, no memory growth / handle leaks; platform AT integration
    (UIA / NSAccessibility / AT-SPI) exposing the document structure tree, not just the chrome.
- [ ] **SL-6.DESK.36 — G6 review and go/no-go** (= `SL-6.QUAL.04`) · owner: HUMAN

**Gate GD3 = G6 (Desktop GA)** (from `00-INDEX.md §2`): signed + notarised installers for
Win/mac/Linux; default-PDF-handler registration; print pipeline; offline licence; crash-free session
rate ≥ 99.7% over 5 000 sessions; feature parity with the web editor.

---

## 4. Dependency order at a glance

```
SL-0.LEAD.02 (Apple) ─────────────────────────────┐  (calendar-critical; start D0)
SL-0.LEAD.03 (Windows signing) ───────────────────┤
                                                   ▼
DESK.01 viewmodel ─┐                        DESK.30/31 signing ─┐
DESK.02 egress    ─┤                                            │
DESK.03 OS-CI     ─┘                                            │
        │                                                       │
        ▼                                                       │
DESK.10 shell ─▶ DESK.11 host ─▶ DESK.12 open ─▶ DESK.14 save ──┤──▶ DESK.33 update
                      │           DESK.13 tools ─┘              │    DESK.34 power-loss
                      └─────────────────────────▶ DESK.20..27 ──┘──▶ DESK.36 G6 go/no-go
```

The engineering path (DESK.01 → shell → MVP → feature-complete) can run to GD2 entirely without the
HUMAN long-leads. Only D3 blocks on them, which is why DESK.00 starts them on day one.

---

## 5. Risks

Format mirrors `25-RISK-REGISTER.md`: every risk has an owner, a **trigger**, and a **kill /
fallback criterion**.

### R-D1 — Signing & notarisation lead time slips the GA date
- **Severity:** high (it gates *release*, not code) · **Owner:** HUMAN
- **Why:** `00-INDEX.md §3` quotes 5–14 days for the Apple Organization D-U-N-S alone and 1–6 weeks
  for Windows org validation. The code can be GA-ready (GD2) weeks before it can be *shipped*.
- **Trigger:** either `SL-0.LEAD.02` or `SL-0.LEAD.03` is still open when GD2 passes.
- **Mitigation / fallback:** start both at DESK.00. Ship **Linux first** (no third-party signing
  authority) and the unsigned-but-packaged builds (DESK.26) for internal testing while the certs
  clear. Enrol Apple as *Individual* if the Organization D-U-N-S is the blocker (same signing
  rights, ADR-P0022 note).

### R-D2 — OPFS-equivalent data-loss on desktop (the durability trap)
- **Severity:** high · **Owner:** AI+ (save path)
- **Why:** the web path keeps work in `MemorySession` / OPFS; ADR-P0022's own table flags home-screen
  PWA storage as "at risk — never treat OPFS as the only copy of user data." The desktop analogue is
  a save that tears, or an unsaved journal lost on crash.
- **Trigger:** any kill-test (DESK.34 / `SL-6.QUAL.05`) produces a state that is neither the original
  file nor a recoverable document; or session-restore (DESK.20) loses an unsaved `.selis` journal.
- **Mitigation:** the mechanism already exists — `write_gate` structural-verify + `temp/fsync/rename`
  (ADR-P0037), incremental-append by default (ADR-P0007). Gate GD1 on the kill-before-rename test;
  gate GD3 on the VM power-loss harness.
- **Kill criterion:** if the three-state guarantee cannot be made to hold on all three filesystems,
  GA does not pass — this is the one correctness property a desktop editor cannot ship without.

### R-D3 — `selis-viewmodel` skipped → three-shell divergence
- **Severity:** high (architectural debt that compounds) · **Owner:** AI+
- **Why:** if the desktop ships before DESK.01, app logic is implemented twice (TS viewer + desktop
  shell) and later a third time (Android native over the C ABI), exactly the failure ADR-P0035 names.
- **Trigger:** DESK.10 (shell) starts with DESK.01 (viewmodel) still open.
- **Mitigation:** DESK.01 is sequenced *before* the shell in D0. If schedule pressure forces the
  shell first, file the divergence as explicit debt with a named owner and a payback task — do not
  let it ship silently as "parity."
- **Kill criterion:** none (not existential) — but the debt must be visible, not discovered.

### R-D4 — Tauri gap forces a toolkit change
- **Severity:** medium · **Owner:** AI+
- **Why:** ADR-P0022/P0036 lock Tauri v2 and rule out Electron; a required native surface or a
  WebView version floor (e.g. for WASM threads) could become blocking.
- **Trigger:** a desktop feature cannot be delivered on the OS WebView within budget.
- **Mitigation:** ADR-P0035's thin shell makes a swap a bounded rewrite. Any change is a **new ADR
  superseding P0022/P0036**, not a quiet dependency add — and ADR-P0036 defers the native-GUI
  question to `SL-9.PLAT.07` *with profiling data*, after GA, on purpose.

### R-D5 — Auto-update applied mid-edit destroys work
- **Severity:** high · **Owner:** AI+
- **Why:** ADR-P0029's Selis-specific requirement is explicit: an update must never apply while a
  document has unsaved changes.
- **Trigger:** an update test can trigger a restart with a dirty document.
- **Mitigation:** gate the updater on `selis-viewmodel` dirty-state; rollback path mandatory.
- **Dependency risk:** ADR-P0029 is **Provisional (revisit at G6)** — if the shared Wertcore updater
  is not ready, build the minimal updater rather than block GA.

### R-D6 — OS-shell integrations crash the host process
- **Severity:** medium (reputational — the crash is attributed to Explorer/Finder) · **Owner:** AI+
- **Why:** thumbnail providers and Quick Look extensions run *inside* the OS shell (`SL-6.OS.02`).
- **Trigger:** any hang or crash in an out-of-process provider under the soak test.
- **Mitigation:** strict budgets (`selis-sandbox`) and the Tier-3 sandbox posture (`01-ARCHITECTURE.md
  §4`); treat these as untrusted-parse surfaces even though they are ours.

### R-D7 — Local-first claim becomes false on desktop
- **Severity:** high (it is the product's sharpest differentiator) · **Owner:** AI+
- **Why:** the web enforces ADR-P0016 with `no-upload.ts` + `xtask check-purity`; the desktop has
  neither yet, and `selis-policy` does not yet expose the `CloudConsent`/`Feature` chokepoint.
- **Trigger:** the desktop ships without the DESK.02 egress gate in CI.
- **Mitigation:** DESK.02 is a D0 foundation; GD0 does not pass without it. The claim must be a
  *mechanism*, not a promise — a CI test that fails if any document byte leaves the process without a
  consent token.

---

## 6. The two-line verdict

Desktop is a **binding-and-packaging** problem, not an engine problem: the engine, the native tools,
native file I/O, and a host-blind UI are all built and proven by the CLI and the web app. The work
that remains is `selis-viewmodel`, the Tauri shell, native open/save wiring, packaging, and the
long-lead HUMAN signing items — which, because of their calendar time, are the real critical path to
a signed, notarised, auto-updating release.
