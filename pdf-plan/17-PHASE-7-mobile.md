# Phase 7 — Mobile (Months 13–18)

**Gate G7 exit criteria:** iOS and Android in the stores · scan → dewarp → OCR → PDF pipeline
shipping · annotate/fill/sign working · share-sheet and Files/SAF integration · cold-open of a
20 MB PDF under 800 ms on a four-year-old midrange device.

Native UI over the C ABI (ADR-P0022). Scope is deliberately narrower than desktop: view, annotate,
fill, sign, scan, and share. Not text editing — that is a desktop/web job and pretending otherwise
produces a bad version of both.

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

## 7.IOS — iOS app

- [ ] **SL-7.IOS.01 — SwiftUI app shell + document browser** · deps: FFI.03 · owner: AI
- [ ] **SL-7.IOS.02 — Page view with 120 Hz pinch-zoom and scroll** · deps: FFI.02 · owner: AI+
  - **Do:** `CALayer`-backed tiled rendering; immediate scaled presentation on pinch with
    re-render behind it. This is the whole reason mobile is native.
- [ ] **SL-7.IOS.03 — Annotation with PencilKit-quality stylus input** · deps: SL-5.ANNOT.03 · owner: AI+
  - **Do:** Pressure, tilt, and palm rejection mapped onto standard Ink annotations (ADR-P0026).
- [ ] **SL-7.IOS.04 — Form filling and signing on mobile** · deps: SL-5.FORM.04 · owner: AI
- [ ] **SL-7.IOS.05 — Files provider, security-scoped URLs, iCloud Drive** · deps: SL-0.IO.01 · owner: AI+
- [ ] **SL-7.IOS.06 — Share extension + Quick Look thumbnail/preview extensions** · owner: AI+
  - **Note:** Extensions have hard memory limits (tens of MB). The engine must run inside them
    under a much tighter budget — this is a real constraint, test it early.
- [ ] **SL-7.IOS.07 — VoiceOver accessibility over the structure tree** · deps: ADR-P0031 · owner: AI+
- [ ] **SL-7.IOS.08 — App Store submission, privacy nutrition labels, review** · deps: SL-0.LEAD.02 · owner: HUMAN
  - **Do:** The privacy labels must match ADR-P0016/P0017 exactly. "Data not collected" is a strong
    marketing position and an audit liability if wrong.

---

## 7.AND — Android app

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
