# Phase 7 — Mobile (Months 13–18)

**Gate G7 exit criteria:** Android in the Play Store · **iOS shipping as an installable PWA** ·
scan → dewarp → OCR → PDF pipeline shipping on Android · annotate/fill/sign working on both ·
share-target and SAF integration on Android · cold-open of a 20 MB PDF under 800 ms on a
four-year-old midrange Android device.

Android is native UI over the C ABI; iOS is the web build installed to the home screen
(ADR-P0022). Scope is deliberately narrower than desktop: view, annotate, fill, sign, scan, and
share. Not text editing — that is a desktop/web job and pretending otherwise produces a bad version
of both.

**iOS is PWA-first by decision, not by omission.** Native iOS is specified in full under
`§7.IOS-NATIVE` and is unblocked by funding, not by engineering. Read ADR-P0022 for exactly which
capabilities the PWA gives up — 120 Hz, share-target, Files provider, Quick Look — before promising
any of them to a user.

---

## 7.FFI — The C ABI

- [ ] **SL-7.FFI.01 — `selis-pdf-ffi` C ABI** · owner: AI+
  - **Do:** Implement `24-BINDINGS-SPEC.md §3`: opaque handles, an out-parameter error convention
    carrying the numeric code, progress via a function pointer, no Rust panics crossing the
    boundary (SL-0.ERR.03), and a generated `selis_pdf.h`.
  - **DoD:** ABI conformance tests from C; a leak test over 10 000 open/close cycles; ASAN clean.
- [ ] **SL-7.FFI.02 — Zero-copy tile delivery** · deps: FFI.01 · owner: AI+
  - **Do:** Render into caller-provided buffers (`CVPixelBuffer` / `HardwareBuffer` backed) so the
    scroll path never copies a bitmap.
  - **DoD:** Measured zero allocations per frame on the scroll path.
- [ ] **SL-7.FFI.03 — `selis-pdf-swift` package** · deps: FFI.01 · owner: AI
  - **Do:** Swift-native types, `async` wrappers over the sync engine, `Sendable` correctness,
    and SwiftPM packaging with an XCFramework build.
- [ ] **SL-7.FFI.04 — `selis-pdf-jni` + Kotlin API** · deps: FFI.01 · owner: AI
  - **Do:** JNI layer, coroutine-friendly wrappers, correct lifecycle (no engine handles leaked
    across Activity recreation), and an AAR build.
- [ ] **SL-7.FFI.05 — Mobile budget profiles and memory pressure** · deps: FFI.01, SL-0.SBX.05 · owner: AI+
  - **Do:** Budgets derived from device class; respond to `didReceiveMemoryWarning` and
    `onTrimMemory` by evicting caches before the OS kills the app.
  - **DoD:** A 500 MB document on a 3 GB-RAM device degrades (lower-res tiles, smaller cache)
    rather than being killed.

---

## 7.IOS — iOS via PWA (the shipping path)

Depends on the Phase 4 web build and `selis-viewmodel` (ADR-P0035), not on the C ABI.

- [ ] **SL-7.IOS.01 — iOS `PlatformAdapter` for the PWA surface** · deps: SL-4.UI.01 · owner: AI+
  - **Do:** Implement the adapter against what Safari actually provides — OPFS storage, Web Share
    (outbound only), `showSaveFilePicker` absent so downloads instead of save-in-place, and
    capability flags that let the UI hide what iOS cannot do rather than failing at the tap.
  - **DoD:** No feature is reachable on iOS that the adapter cannot service.
- [ ] **SL-7.IOS.02 — Installable-PWA packaging** · deps: IOS.01 · owner: AI
  - **Do:** Manifest, icon set, splash screens, standalone display mode, and an Add-to-Home-Screen
    prompt that explains *why* — an uninstalled Safari tab has weaker storage guarantees.
- [ ] **SL-7.IOS.03 — Touch page view: pinch-zoom, scroll, tiled re-render** · deps: SL-4.UI.03 · owner: AI+
  - **Do:** Immediate scaled presentation on pinch with re-render behind it, same strategy as
    native. Budget 60 fps and design the interaction to feel right at 60 rather than pretending.
- [ ] **SL-7.IOS.04 — Touch and Apple Pencil annotation** · deps: SL-5.ANNOT.03 · owner: AI+
  - **Do:** Pointer events carry pressure and tilt for Apple Pencil in Safari; map them onto Ink
    annotations (ADR-P0026). Palm rejection is weaker than PencilKit — tune the touch heuristics.
- [ ] **SL-7.IOS.05 — Storage durability and data-loss prevention** · deps: IOS.02, SL-0.IO.01 · owner: AI+
  - **Do:** Treat OPFS as a cache, never as the only copy. Warn on unexported edits, persist an
    export reminder, and request `navigator.storage.persist()`.
  - **DoD:** A simulated storage eviction loses no user work that was ever marked saved.
  - **Note:** This is the single largest correctness risk of the PWA route. Rule 5 applies.
- [ ] **SL-7.IOS.06 — VoiceOver accessibility over the structure tree** · deps: ADR-P0031 · owner: AI+
  - **Do:** ARIA mapping of the tagged-PDF structure tree; verify with VoiceOver on a real device.
- [ ] **SL-7.IOS.07 — iOS Safari test matrix** · owner: AI
  - **Do:** Two iOS majors × iPhone/iPad, standalone and in-tab. Cover the memory ceiling — Safari
    kills tabs aggressively, so large-document behaviour must degrade, not crash.

---

## 7.IOS-NATIVE — iOS native app (deferred; unblocked by funding)

Fully specified so it can start the day it is funded. Every task here is blocked on
`SL-0.LEAD.02` (Apple Developer Program), which **also gates macOS notarisation**
(`SL-6.DIST.02`) and the Safari extension — so this cost returns at Phase 6 regardless of what iOS
does. Enrol as *Individual* if the Organization D-U-N-S process is the blocker.

- [ ] **SL-7.IOSN.01 — SwiftUI app shell + document browser** · deps: FFI.03, SL-0.LEAD.02 · owner: AI
- [ ] **SL-7.IOSN.02 — Page view with 120 Hz pinch-zoom and scroll** · deps: FFI.02 · owner: AI+
  - **Do:** `CALayer`-backed tiled rendering; immediate scaled presentation on pinch with
    re-render behind it. This is the one capability the PWA cannot reach at all.
- [ ] **SL-7.IOSN.03 — Annotation with PencilKit-quality stylus input** · deps: SL-5.ANNOT.03 · owner: AI+
  - **Do:** Pressure, tilt, and palm rejection mapped onto standard Ink annotations (ADR-P0026).
- [ ] **SL-7.IOSN.04 — Files provider, security-scoped URLs, iCloud Drive** · deps: SL-0.IO.01 · owner: AI+
- [ ] **SL-7.IOSN.05 — Share extension + Quick Look thumbnail/preview extensions** · owner: AI+
  - **Note:** Extensions have hard memory limits (tens of MB). The engine must run inside them
    under a much tighter budget — this is a real constraint, test it early.
- [ ] **SL-7.IOSN.06 — App Store submission, privacy nutrition labels, review** · deps: SL-0.LEAD.02 · owner: HUMAN
  - **Do:** The privacy labels must match ADR-P0016/P0017 exactly. "Data not collected" is a strong
    marketing position and an audit liability if wrong.
- [ ] **SL-7.IOSN.07 — Port the shells' view usage to `selis-viewmodel`** · deps: ADR-P0035 · owner: AI+
  - **Do:** SwiftUI binds to the same view-model the web UI uses. If this task is large, ADR-P0035
    was not being enforced — treat its size as the metric for whether the seam held.

---

## 7.AND — Android app

- [ ] **SL-7.AND.00 — Form filling and signing on mobile (both platforms)** · deps: SL-5.FORM.04 · owner: AI
- [ ] **SL-7.AND.01 — Compose app shell** · deps: FFI.04 · owner: AI
- [ ] **SL-7.AND.02 — Tiled page view with `SurfaceView`/`TextureView`** · deps: FFI.02 · owner: AI+
- [ ] **SL-7.AND.03 — SAF integration: open, save-in-place, document provider** · deps: SL-0.IO.01 · owner: AI+
- [ ] **SL-7.AND.04 — Share target, print-service plugin, intent filters** · owner: AI
- [ ] **SL-7.AND.05 — Stylus support (S Pen and generic)** · deps: SL-5.ANNOT.03 · owner: AI
- [ ] **SL-7.AND.06 — TalkBack accessibility over the structure tree** · deps: ADR-P0031 · owner: AI+
- [ ] **SL-7.AND.07 — Device fragmentation matrix + Play submission** · deps: SL-0.LEAD.04 · owner: HUMAN
  - **Do:** Test on low-RAM devices explicitly; 32-bit ABI decision documented; Play data-safety
    form matching ADR-P0016/P0017.

---

## 7.SCAN — The scan pipeline

- [ ] **SL-7.SCAN.01 — Camera capture with edge detection and auto-shutter** · owner: AI+
- [ ] **SL-7.SCAN.02 — Perspective dewarp and page-curl correction** · deps: SCAN.01 · owner: AI+
  - **Do:** Quadrilateral detection, homography correction, and — for book scans — curvature
    estimation. Implement in `selis-ocr::prep` so desktop and web get it too from an imported photo.
- [ ] **SL-7.SCAN.03 — Enhancement: binarise, colour-correct, shadow removal, despeckle** · deps: SCAN.02 · owner: AI+
- [ ] **SL-7.SCAN.04 — Multi-page capture, reorder, retake** · deps: SCAN.03 · owner: AI
- [ ] **SL-7.SCAN.05 — Compression: JBIG2 for bilevel, JPEG for colour, mixed-raster** · deps: SL-2.FILT.01 · owner: AI+
  - **Do:** A 20-page scan should be a few megabytes, not eighty. This is a visible quality
    signal versus competitors.
- [ ] **SL-7.SCAN.06 — On-device OCR + tagged searchable output** · deps: SL-5.OCR.04 · owner: AI+
  - **DoD:** OCR runs fully on-device with no network (ADR-P0016), and the output is tagged.
- [ ] **SL-7.SCAN.07 — Business-card / receipt / whiteboard modes** · deps: SCAN.03 · owner: AI

---

## 7.SYNC — Cross-device (opt-in only)

- [ ] **SL-7.SYNC.01 — Local-network handoff without a cloud round trip** · owner: AI+
  - **Do:** Send a document phone↔desktop over the local network, encrypted, no server. Genuinely
    differentiating for a privacy-first product, and it works offline.
- [ ] **SL-7.SYNC.02 — Opt-in cloud sync client** · deps: Phase 8 · owner: AI+
- [ ] **SL-7.QUAL.01 — G7 review and go/no-go** · owner: HUMAN
