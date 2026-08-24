# Phase 6 — Desktop (Months 10–15)

**Gate G6 exit criteria:** signed and notarised installers for Windows/macOS/Linux · default-PDF-
handler registration working on all three · print pipeline shipping · offline licence · crash-free
session rate ≥ 99.7% over 5 000 sessions · feature parity with the web editor.

Runs partly in parallel with Phase 5 — the shell work does not depend on the editor being finished,
only on the UI contract (`SL-4.UI.01`).

---

## 6.SHELL — The Tauri shell

- [ ] **SL-6.SHELL.01 — Tauri v2 project + the desktop `PlatformAdapter`** · deps: SL-4.UI.01 · owner: AI+
  - **Do:** Wire `apps/web/ui` into Tauri with a desktop adapter: Tauri commands replace worker
    `postMessage`, and the engine runs natively (no WASM) with full threading.
  - **DoD:** The same UI bundle runs in browser and desktop; a CI check prevents desktop-only code
    leaking into `apps/web/ui`.
- [ ] **SL-6.SHELL.02 — Native engine host with a bounded worker pool** · deps: SHELL.01 · owner: AI+
  - **Do:** Engine calls off the UI thread, cancellation wired to window close and navigation,
    memory budget derived from system RAM rather than hardcoded.
- [ ] **SL-6.SHELL.03 — Multi-window, tabs, and session restore** · deps: SHELL.01 · owner: AI
  - **Do:** Restore open documents, scroll positions, and unsaved edit journals (`.selis`) after a
    crash or a restart. Users expect an editor to lose nothing.
- [ ] **SL-6.SHELL.04 — Native menus, shortcuts, and platform conventions** · deps: SHELL.01 · owner: AI
  - **Do:** Per-platform menu structure and keybindings that match each OS's conventions, not a
    lowest-common-denominator web menu.
- [ ] **SL-6.SHELL.05 — File dialogs, recent files, and drag-drop** · deps: SHELL.01 · owner: AI
- [ ] **SL-6.SHELL.06 — Tier-3 process sandbox for untrusted documents** · deps: SHELL.02, ADR-P0016 · owner: HUMAN
  - **Do:** Parse and render documents of untrusted provenance (downloads, email attachments,
    network shares) in a child process sandboxed with seccomp-bpf (Linux), App Sandbox +
    `sandbox_init` (macOS), and an AppContainer + job object (Windows). Communication over `selis-pdf-ipc`.
  - **DoD:** A test that the child cannot open a file, make a network connection, or exceed its
    memory cap; provenance detection tested per platform (`com.apple.quarantine`, Mark-of-the-Web,
    `origin` xattrs).
  - **Note:** This is the single biggest security advantage a desktop PDF app can have, and almost
    nobody in the category does it properly.

---

## 6.OS — Operating-system integration

- [ ] **SL-6.OS.01 — Default handler registration + file associations** · owner: AI+
  - **Do:** Windows (per-user `HKCU` associations, the Default Apps flow, and the fact that Windows
    will not let an installer silently steal the default), macOS (`LSHandlerRank`, `UTImportedType`),
    Linux (`.desktop` + `mimeapps.list`).
  - **DoD:** Association survives an OS update; the "set as default" flow is documented per platform.
- [ ] **SL-6.OS.02 — Thumbnail providers** · owner: AI+
  - **Do:** Windows `IThumbnailProvider` (out-of-process COM), macOS Quick Look thumbnail extension
    + preview extension, Linux thumbnailer spec.
  - **Risk:** These run inside the OS shell. A crash or a hang is attributed to Explorer/Finder.
    Strict budgets, and the Tier-3 sandbox posture applies here more than anywhere.
- [ ] **SL-6.OS.03 — Print pipeline** · deps: SL-4.UI.08 · owner: AI+
  - **Do:** Windows `IPrintDocumentPackageTarget`/XPS or direct GDI, macOS `NSPrintOperation`,
    Linux CUPS. Booklet, N-up, poster/tiling, scaling, duplex, paper-source selection, and
    "print as image" for problem documents.
- [ ] **SL-6.OS.04 — Content indexing plugins** · owner: AI
  - **Do:** Windows Search `IFilter`, macOS Spotlight importer, Linux Tracker. Makes PDFs
    searchable by the OS using our extraction, which is better than the built-in ones.
- [ ] **SL-6.OS.05 — Context menu and share integration** · owner: AI
  - **Do:** Explorer context menu (Windows 11 sparse-package style), macOS Services + Share
    extension, Linux file-manager actions.
- [ ] **SL-6.OS.06 — Virtual printer ("Print to Selis PDF")** · owner: HUMAN
  - **Do:** A print driver that creates PDFs from any application. Windows: a v4 print driver or a
    Print Support App — needs Partner Center and a driver-signing path. macOS: a CUPS backend/PPD.
    Linux: a CUPS backend.
  - **Note:** This is a headline feature for the Adobe/Foxit comparison and it is the single most
    operationally expensive item in the desktop plan (driver signing, per-OS-version breakage).
    Decide deliberately whether it is worth it; if yes, start the Partner Center account at
    SL-0.LEAD stage. Consider deferring to Phase 9 and shipping without it at G6.

---

## 6.DIST — Packaging, signing, distribution

- [ ] **SL-6.DIST.01 — Windows installer (MSI/MSIX) + code signing** · deps: SL-0.LEAD.03 · owner: AI+
  - **Do:** Per-user and per-machine installs, silent-install switches for IT deployment, upgrade
    and rollback, and Azure Trusted Signing in CI.
- [ ] **SL-6.DIST.02 — macOS DMG/PKG, Developer ID, notarisation, stapling** · deps: SL-0.LEAD.02 · owner: AI+
  - **Do:** Hardened runtime, correct entitlements (as few as possible — document each), notarytool
    in CI, and a separate App Store build if that channel is pursued (it forbids some entitlements
    the sandbox work may need — check early).
- [ ] **SL-6.DIST.03 — Linux packaging: deb, rpm, AppImage, Flatpak** · owner: AI
  - **Note:** Flatpak's sandbox interacts with file access and the print system; budget time for
    portals.
- [ ] **SL-6.DIST.04 — Auto-update (TUF)** · deps: ADR-P0029 · owner: AI+
  - **Do:** Consume the shared updater. Requirements specific to us: an update must never be
    applied while a document has unsaved changes, and a rollback must be possible.
- [ ] **SL-6.DIST.05 — Reproducible builds + SBOM per artefact** · owner: AI
- [ ] **SL-6.DIST.06 — Crash reporting with document-byte stripping** · deps: SL-4.SHIP.01 · owner: AI+
- [ ] **SL-6.DIST.07 — Enterprise deployment: GPO/ADMX, MDM profiles, MSI transforms** · owner: AI+
  - **Do:** Admin-lockable policy for: scripting (ADR-P0020), cloud features (ADR-P0016),
    telemetry, update channel, and default save behaviour. Enterprises buy on manageability.

---

## 6.CLI — The `selis` CLI

- [ ] **SL-6.CLI.01 — Command surface** · owner: AI
  - **Do:** `inspect render extract convert optimise merge split rotate sign verify redact ocr
    form batch diff`, all `--json`-capable, all sharing the GUI's code paths exactly.
- [ ] **SL-6.CLI.02 — Batch engine with progress and resume** · deps: SL-0.WS.02, CLI.01 · owner: AI+
  - **Do:** Parallel batch over a file set with per-file isolation (one bad document never kills a
    batch), a machine-readable report, and resumability.
- [ ] **SL-6.CLI.03 — Shell completions, man pages, exit-code contract** · deps: CLI.01 · owner: AI
  - **Do:** Exit codes map to the error-code ranges of `03-CONVENTIONS.md §3` so scripts can branch.
- [ ] **SL-6.CLI.04 — CLI as the corpus harness driver** · deps: CLI.01 · owner: AI
  - **Note:** Make the corpus harness call the shipped CLI rather than a test-only binary. It means
    the CLI is exercised on 100k documents every night for free.

---

## 6.QUAL — Desktop quality

- [ ] **SL-6.QUAL.01 — OS matrix CI** · owner: AI
  - **Do:** Windows 10/11 (+ Insider), macOS current − 2 (+ beta), Ubuntu LTS ×2, Fedora, and one
    rolling distro. Beta-OS breakage should be found by us, not by users.
- [ ] **SL-6.QUAL.02 — Large-document soak** · owner: AI
  - **Do:** 8-hour sessions with 1 GB+ documents, 500-page edits, and continuous scrolling; assert
    no memory growth and no handle leaks.
- [ ] **SL-6.QUAL.03 — Accessibility on desktop** · deps: SL-4.UI.07 · owner: AI+
  - **Do:** Platform AT integration — UIA (Windows), NSAccessibility (macOS), AT-SPI (Linux) —
    exposing the document structure tree, not just the chrome. Tauri/WebView AT plumbing needs
    explicit work; do not assume the web a11y work carries over.
- [ ] **SL-6.QUAL.04 — G6 review and go/no-go** · owner: HUMAN
