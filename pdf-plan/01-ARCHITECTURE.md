# 01 — Architecture

> Read `02-ADRS.md` before proposing anything that contradicts this document.

---

## 1. The one-sentence architecture

**One synchronous, sandboxed, deterministic Rust document engine, with all I/O injected, exposed
through four bindings (WASM, C ABI, native Rust, HTTP) to seven shells (web, extension, desktop,
iOS, Android, CLI, server), where each product is an entitlement set rather than a build.**

---

## 2. Layer model

```text
L5  APPS          apps/web  apps/extension  apps/desktop  apps/ios  apps/android  apps/cli  cloud/
                        │        │              │            │          │           │        │
L4  BINDINGS      selis-pdf-wasm   selis-pdf-ffi   selis-pdf-jni   selis-pdf-swift   selis-pdf-ipc   selis-pdf-service
                        └────────┴──────────┴─────────┴──────────┴──────────┘
                                              │
L3  ORCHESTRATION            selis-pdf-engine        selis-policy        selis-job
                                              │
L2  DOMAIN     selis-pdf-cos  selis-pdf-filter  selis-pdf-doc  selis-font  selis-shape  selis-pdf-content  selis-raster
               selis-pdf-text selis-image  selis-pdf-annot selis-pdf-form  selis-pdf-edit   selis-pdf-redact   selis-pdf-sign
               selis-ocr  selis-pdf-convert selis-pdf-a11y selis-pdf-optimize  selis-pdf-js
                                              │
L1  ABSTRACTION           selis-io      selis-sandbox      selis-crypto
                                              │
L0  FOUNDATION      selis-error   selis-bytes   selis-geom   selis-color   selis-log
```

### The dependency rules

1. A crate may depend on **strictly lower** layers only. No sideways edges within L2 except the
   explicit allowlist in `xtask/layers.toml` (see §3 for the small set of legitimate ones).
2. **No L0–L3 crate may name a filesystem, network, clock, or environment API.** All four are
   injected: `DocSource`/`DocSink` (I/O), `Clock` (time), `Rng` (randomness — signatures need it),
   `Env` (locale, temp policy). Enforced by `xtask check-purity`, which greps the expanded HIR for
   banned paths.
3. **L2 crates never depend on each other's internals.** They compose at L3. The allowed exceptions
   are the four listed in §3, each with a written justification in `layers.toml`.
4. `selis-sandbox` is depended on by every L2 crate and depends on nothing but L0. It is the only
   crate permitted to define allocation policy.
5. Nothing below L4 may be `async`. Nothing above L2 may be in the hot render path.

---

## 3. Full crate inventory

### L0 — Foundation

| Crate | Owns |
|---|---|
| `selis-error` | The error taxonomy, the numeric code registry (`codes.toml` + codegen), `Result` alias, error→user-message mapping tables per locale. Zero dependencies. |
| `selis-bytes` | `Bytes` (refcounted immutable slice), `BytesMut`, aligned buffers, a bounded rope for edit buffers, and the *immutability discipline* that ADR-P0007 depends on. |
| `selis-geom` | `Point`, `Matrix` (PDF's 6-element form), `Rect`, `Path` (the internal path representation), transforms, bounds arithmetic, the fixed-point coordinate type used for determinism. |
| `selis-color` | Colour space model (DeviceGray/RGB/CMYK, CalRGB, Lab, ICCBased, Indexed, Separation, DeviceN), conversion, ICC profile handling, blend-mode maths, rendering intents. |
| `selis-log` | Structured logging, and — separately — the **document operation log** (§11). Two different things; do not merge them. |

### L1 — Abstraction

| Crate | Owns |
|---|---|
| `selis-io` | `DocSource` / `DocSink` traits and their shipped implementations (§5). Range-request coalescing, read-ahead, and the partial-availability model that lets page 1 render before byte 2 000 000 arrives. |
| `selis-sandbox` | `Budget`, `BudgetGuard`, the budget-aware allocator, recursion-depth guards, `CancelToken`, wall-clock deadlines, and the panic→error trampoline. **The safety kernel.** |
| `selis-crypto` | Digests, HMAC, AES/RC4, PKCS#7/CMS, X.509 chain building and validation, RFC 3161 timestamps. Key *material handling* only; key *storage* is a shell concern. |

### L2 — Domain

| Crate | Owns | Notes |
|---|---|---|
| `selis-pdf-cos` | Lexer, object model, xref tables + xref streams, object streams, trailer, incremental-update chains, linearisation parsing, **damaged-file reconstruction**. | Open-sourced (ADR-P0030). The single most fuzzed crate. |
| `selis-pdf-filter` | Flate, LZW, ASCII85, ASCIIHex, RunLength, DCTDecode, CCITTFaxDecode, JBIG2Decode, JPXDecode, Crypt. | Open-sourced. Each filter is budget-bounded and streaming. |
| `selis-pdf-doc` | Catalog, page tree, resource resolution, inheritance, name/number trees, outlines, destinations, optional content (OCG/OCMD), metadata (Info + XMP), **structure tree** (ADR-P0031), embedded files, viewer preferences. | The "document model". |
| `selis-font` | Type1, CFF/Type1C, TrueType, OpenType, CID fonts, Type3, bare CFF; encodings, cmaps, CMap files, ToUnicode; glyph outline extraction; **subsetting and re-embedding** (needed by ADR-P0024); the standard-14 metric-compatible substitution table. | Open-sourced. Depends on `skrifa` (the plan's original `ttf-parser` was declared unmaintained — RUSTSEC-2026-0192). |
| `selis-shape` | Script itemisation, bidi, cluster/grapheme segmentation, shaping via `rustybuzz`, line breaking, justification. Used for *authored* text and reflowed edits, not for replaying existing content. | |
| `selis-pdf-content` | The content-stream interpreter: tokeniser, operator dispatch, the full graphics-state machine, XObject/Form recursion (depth-budgeted), inline images, marked content, and emission of a **display list** rather than direct drawing. | Allowed edge → `selis-font`, `selis-image`, `selis-color`. |
| `selis-raster` | Display list → pixels. Fill/stroke with correct PDF winding rules, clipping (including text clip modes), transparency groups, soft masks, blend modes, shadings 1–7, tiling patterns, anti-aliasing policy, tile decomposition. Backed by `tiny-skia` behind `raster::Backend`. | Determinism owner (ADR-P0012). |
| `selis-pdf-text` | Text extraction, run assembly, reading-order inference (structure-tree-first, geometric fallback), word/line segmentation, search index, selection geometry. | Allowed edge → `selis-pdf-content`, `selis-font`. |
| `selis-image` | Image XObject decode/encode, resampling, colour conversion, image masks, SMasks, downsampling for optimise. | |
| `selis-pdf-annot` | All annotation subtypes, appearance-stream generation and regeneration, flattening, ink/stylus geometry, markup replies, review state. | |
| `selis-pdf-form` | AcroForm field model, widget↔field association, field hierarchy, appearance generation, calculation/validation/format ordering, FDF/XFDF import-export, XFA (read + render-static only). | Allowed edge → `selis-pdf-annot`. |
| `selis-pdf-edit` | The mutation journal, undo/redo, page operations (insert/delete/rotate/reorder/split/merge/crop), object rewriting, and the **incremental save writer**. Normative spec: `23-EDIT-MODEL-SPEC.md`. | Allowed edge → `selis-pdf-cos`, `selis-pdf-doc`. |
| `selis-pdf-redact` | Region-based content removal and the verification pass (ADR-P0023). | `owner: HUMAN`. |
| `selis-pdf-sign` | Signature field creation, byte-range digesting, PKCS#7/CAdES/PAdES packaging, timestamping, LTV (DSS/VRI), validation and revocation checking, document-security-store maintenance, MDP/certification levels. | `owner: HUMAN`. |
| `selis-ocr` | Page image preparation (deskew, dewarp, binarise), layout analysis, engine binding (Tesseract-WASM), and emission of an invisible text layer + structure tags. | |
| `selis-pdf-convert` | PDF ⇄ images; PDF → HTML/text/Markdown; Office → PDF (via a document-model importer, not a headless suite); PDF/A conversion; print-stream (PS/PCL) import. | Largest crate; split into `selis-pdf-convert-*` sub-crates at Phase 8. |
| `selis-pdf-a11y` | Tag generation and repair, PDF/UA rule engine, reading-order editor model, alt-text plumbing, table-structure inference. | |
| `selis-pdf-optimize` | Object deduplication, stream recompression, font subsetting orchestration, image downsampling, unused-object GC, linearisation writer. | |
| `selis-pdf-js` | The budgeted Acrobat-JS subset interpreter (ADR-P0020). Off by default. | `owner: HUMAN`. |

### L3 — Orchestration

| Crate | Owns |
|---|---|
| `selis-pdf-engine` | The `Session` and `Document` handles; the page/display-list/tile cache (ADR-P0025); render scheduling; the composition of L2 crates into user-meaningful operations. This is the only L3 crate an app should need to think about. |
| `selis-policy` | Entitlements (`Feature` enum → allow/deny), budget profiles per surface, `CloudConsent` (ADR-P0016), active-content policy (ADR-P0020), telemetry consent, admin/MDM policy overlay. |
| `selis-job` | Long-running/batch operations: progress, cancellation, resumability, queueing. Used by batch convert, OCR of a 900-page scan, and the server tier. |

### L4 — Bindings

| Crate | Owns |
|---|---|
| `selis-pdf-wasm` | `wasm-bindgen` surface, the Worker protocol, transferable-buffer handling, the OPFS/File-System-Access `DocSource` adapters, and the code-split loading strategy. Spec: `24-BINDINGS-SPEC.md §2`. |
| `selis-pdf-ffi` | The `selis_pdf_*` C ABI: opaque handles, error-out-parameter convention, callback-based progress, and a generated `selis_pdf.h`. Spec: `24-BINDINGS-SPEC.md §3`. |
| `selis-pdf-jni` | Android JNI wrapper over `selis-pdf-ffi` + a Kotlin API surface. |
| `selis-pdf-swift` | Swift package wrapping `selis-pdf-ffi` with Swift-native types and `async` adapters. |
| `selis-pdf-ipc` | Desktop shell ↔ privileged/isolated helper protocol. Used for the JPX sandbox and for OS integrations that need a separate process. |
| `selis-pdf-service` | HTTP/gRPC surface for the Server SKU: tokio front end, bounded blocking pool → engine, multi-tenant budgets, no shared state between documents. |

### L5 — Apps

| Path | What |
|---|---|
| `apps/web/ui` | The React/TypeScript application. **Shared with desktop** (ADR-P0022). |
| `apps/web/host` | The web deployment: static hosting, COOP/COEP headers, service worker, OPFS storage. |
| `apps/extension` | MV3 extension: manifest, DNR rules, viewer page, offscreen document, options page. Reuses `apps/web/ui` in a "no-network" configuration. |
| `apps/desktop` | Tauri v2 shell: window management, native menus, file dialogs, print, OS integration, updater wiring. |
| `apps/ios` | SwiftUI app + share extension + Quick Look thumbnail extension + document provider. |
| `apps/android` | Compose app + SAF document provider + share target + print service plugin. |
| `apps/cli` | The `selis` binary: inspect, render, extract, convert, optimise, sign, redact, batch. |
| `cloud/` | Opt-in services: sync, collaboration relay, OCR/convert workers, e-sign workflow, licence/entitlement (shared per ADR-P0029). |

---

## 4. Process & isolation model

Selis has no privileged helper — it is not a disk tool and needs no elevation. What it *does* need
is **isolation of hostile parsing from the rest of the process**. Three tiers:

```text
Tier 1 — In-process, budgeted (default)
  Everything in selis-pdf-* runs in-process under a Budget. Rust memory safety plus the budget kernel
  is the primary defence. Panics are caught at the binding boundary and converted to errors.

Tier 2 — WASM-in-process sandbox
  JPX (OpenJPEG) and Tesseract are compiled to WASM and executed by an embedded runtime
  (wasmtime on native, the browser's own engine on web) with a linear-memory cap. A memory-safety
  bug in that C code corrupts only its own linear memory.

Tier 3 — Separate process
  The desktop and server shells run document parsing in a child process with OS-level sandboxing
  (seccomp-bpf on Linux, App Sandbox + sandbox_init on macOS, AppContainer/job object on Windows)
  when opening documents from untrusted origins (downloads, email, network shares).
  Communication via selis-pdf-ipc. Web gets this for free from the browser's own site isolation.
```

Which tier applies is a `selis-policy` decision keyed on document provenance, not a per-feature
choice made by the caller.

---

## 5. The core I/O abstraction (`selis-io`)

```rust
/// Random-access, possibly-partial, read-only access to document bytes.
///
/// # Budget
/// Implementations must not block longer than the caller's deadline. `read_at` on an
/// unavailable range returns `Pending` rather than blocking on the network.
pub trait DocSource: Send + Sync {
    fn len(&self) -> Option<u64>;                 // None = unknown (streaming)
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<Availability>;
    fn available(&self) -> &RangeSet;             // what is resident right now
    fn request(&self, ranges: &RangeSet);         // hint: prefetch these
    fn is_random_access(&self) -> bool;           // false = must be read start-to-end
}

pub enum Availability { Filled(usize), Pending { hint: RangeSet }, Eof }

/// Append-or-write destination. Deliberately minimal, deliberately not Seek-by-default:
/// the incremental writer only ever appends (ADR-P0007).
pub trait DocSink: Send {
    fn append(&mut self, bytes: &[u8]) -> Result<()>;
    fn position(&self) -> u64;
    fn finish(self: Box<Self>) -> Result<SinkReceipt>;   // fsync/commit happens here
}
```

**Shipped `DocSource` implementations**

| Impl | Where | Notes |
|---|---|---|
| `MemSource` | everywhere | The fuzz and test workhorse. |
| `FileSource` | native | `mmap` where sane, `pread` where not; handles files changing under us by revalidating a size+mtime fingerprint. |
| `HttpRangeSource` | web, native | Coalesces range requests, honours `Accept-Ranges`, degrades to full download when the server refuses ranges. The reason a linearised 200 MB PDF opens instantly. |
| `OpfsSource` | web | Origin Private File System, sync-access-handle in a Worker. |
| `FsaSource` | web | File System Access API handle — the path to true save-in-place on desktop Chrome. |
| `BlobSource` | web | `File`/`Blob` from a picker or drop. |
| `SafSource` | Android | `content://` via `ParcelFileDescriptor`. |
| `SecurityScopedSource` | iOS | Security-scoped bookmark URLs. |
| `FaultSource` | test only | Injects truncation, corruption, latency, and range-refusal. |

The **partial-availability model** is what makes the web product feel fast: `selis-pdf-cos` is written to
tolerate `Pending` at every read, unwinding to the caller with the ranges it needs rather than
blocking. The engine then re-drives the parse when the data arrives. This is why the parser is a
resumable state machine and not a straight recursive descent over a `&[u8]`.

---

## 6. The sandbox kernel (`selis-sandbox`)

```rust
pub struct Budget {
    pub bytes: u64,        // peak allocation attributable to this operation
    pub wall: Duration,    // deadline, checked at every operator and every stream chunk
    pub depth: u16,        // nesting: form XObjects, patterns, object streams, /Prev chains
    pub objects: u32,      // indirect objects resolved
    pub pixels: u64,       // total rasterised samples — bounds the "one page, 40 gigapixels" file
}

pub struct BudgetGuard<'a> { /* charges on drop, poisons on exhaustion */ }

impl Budget {
    pub fn profile(surface: Surface) -> Budget;  // Thumbnail | Viewer | Editor | Batch | Server
    pub fn charge(&self, g: &mut BudgetGuard, r: Resource, n: u64) -> Result<()>;
}
```

Rules the kernel enforces, mechanically:

- Every allocation inside an L2 crate goes through `sandbox::alloc`, which charges `bytes`.
  A crate that calls the global allocator directly fails `xtask check-alloc`.
- Every loop that can be driven by document content calls `guard.tick()?`, which checks the
  deadline and the cancel token. `check-contracts` requires a `tick` in any `loop`/`while` whose
  condition reads parsed data.
- Recursion is *never* native recursion in `selis-pdf-cos`, `selis-pdf-content`, or `selis-pdf-doc`. Those three use
  explicit worklists with a depth counter, because a stack overflow is an abort we cannot catch.
- A panic anywhere below L4 is a bug, but the binding layer installs a catch-unwind trampoline so
  that a bug is a returned error rather than a lost document.

---

## 7. Document model and the resolution graph (`selis-pdf-doc`)

The single most common source of PDF bugs is *reference resolution*: inheritance, cycles, and
generation numbers. `selis-pdf-doc` centralises it.

```rust
pub struct Doc<'s> { source: &'s dyn DocSource, cos: CosIndex, revisions: Vec<Revision> }

impl<'s> Doc<'s> {
    pub fn page(&self, i: PageIndex) -> Result<Page<'_>>;
    /// Resolve with inheritance (Resources, MediaBox, CropBox, Rotate) walking up the page tree.
    pub fn inherited(&self, page: PageIndex, key: Name) -> Result<Option<Obj<'_>>>;
    /// Cycle-safe: every resolution carries a visited-set from the BudgetGuard.
    pub fn resolve(&self, r: Ref, g: &mut BudgetGuard) -> Result<Obj<'_>>;
    pub fn revisions(&self) -> &[Revision];      // the incremental-update chain, oldest first
    pub fn structure(&self) -> Option<&StructTree>;
}
```

`revisions()` being first-class is what makes ADR-P0007 cheap: a document is a *sequence* of
byte ranges, the newest winning. Undo across sessions is "render revision N−1". Signature
validation is "digest revision K's byte range". Save is "append revision N+1".

---

## 8. Rendering pipeline

```text
Page  ──selis-pdf-content──▶  DisplayList  ──selis-raster──▶  Tiles  ──shell──▶  screen / PNG / print
        │                    │                            │
        │                    │                            └─ cached by (page, matrix, params)
        │                    │                               in selis-pdf-engine, LRU under a budget
        │                    └─ an immutable, serialisable, inspectable IR:
        │                       ops = Fill | Stroke | Glyphs | Image | Shading | BeginGroup |
        │                             SetClip | SetSoftMask | BeginMarked | ...
        └─ interpreter is resumable + budgeted; unknown operators are recorded, not fatal
```

Why a display list rather than immediate-mode drawing into a canvas:

1. **Determinism and diffing.** A DisplayList is comparable — the visual-regression harness can
   diff structure, not just pixels, and tell you *which operator* changed.
2. **Reuse.** Text extraction, hit-testing, selection, redaction region analysis, and the edit
   model's reconstruction pipeline all consume the same IR instead of re-interpreting content.
3. **Retargeting.** The same IR drives raster, PostScript/PCL for print, SVG/HTML for the convert
   product, and a Core Graphics / Skia path on mobile if we ever want native-backed drawing.
4. **Budgeting.** Interpreting is bounded separately from rasterising, so a page with 10 million
   tiny paths fails at a predictable point with a typed error.

---

## 9. Front-end architectures

### Web (`apps/web`)

```text
 Main thread (React)                Worker pool                        Storage
 ┌────────────────────┐   postMessage  ┌──────────────────┐   OPFS   ┌──────────────┐
 │ UI, virtual scroll │◀──────────────▶│ selis-pdf-wasm engine  │◀────────▶│ documents,   │
 │ canvas compositor  │  transferable  │ (1 parse worker, │          │ edit journal │
 │ selection overlay  │   ArrayBuffer  │  N render workers)│          └──────────────┘
 └────────────────────┘                └──────────────────┘
        ▲                                        ▲
        │ OffscreenCanvas tiles                  │ HttpRangeSource / FsaSource
```

- The engine never touches the DOM; the main thread never parses a PDF.
- Tiles are rendered into `OffscreenCanvas` in the worker and transferred, so scrolling never
  blocks on the engine.
- Multi-threaded rasterisation requires `SharedArrayBuffer`, which requires COOP/COEP headers.
  The app **must work without them** (single-threaded fallback) because the extension and
  some embedding contexts cannot set them. `selis-pdf-engine` degrades by policy, not by `#[cfg]`.
- Persistence: OPFS for the document cache and edit journal; File System Access API for
  save-in-place where available; download fallback everywhere else.

### Chrome/Edge/Firefox extension (`apps/extension`)

```text
 declarativeNetRequest rule:  main_frame request with Content-Type application/pdf
        │                     └─▶ redirect to  chrome-extension://<id>/viewer.html?src=<url>
        ▼
 viewer.html  ──▶  same apps/web/ui bundle, "extension" platform adapter
        │
        └──▶ offscreen document (or dedicated worker) hosting selis-pdf-wasm
```

- All code bundled; no remote fetch (ADR-P0028).
- `file://` support requires the user to enable "Allow access to file URLs" — the onboarding flow
  must ask for it explicitly and explain why, because the permission is scary and the default is off.
- The extension is the acquisition funnel: it is a strictly better default PDF viewer, and every
  edit action deep-links into the web app with the document handed over locally (never uploaded).

### Desktop (`apps/desktop`, Tauri v2)

- Same `apps/web/ui`, `platform: "desktop"` adapter. Tauri commands replace `postMessage`.
- The engine runs in the Rust side of Tauri, natively — no WASM, full threading.
- OS integration that must be built per platform: default-handler registration, thumbnail providers
  (Windows `IThumbnailProvider`, macOS Quick Look, Linux thumbnailer spec), print (Win32
  `IPrintDocumentPackageTarget`, macOS `NSPrintOperation`, CUPS), Services/context-menu entries,
  and Spotlight/Windows Search content indexing plugins.
- Untrusted documents open in a Tier-3 sandboxed child (§4).

### Mobile (`apps/ios`, `apps/android`)

- Native UI over `selis-pdf-ffi`. Render tiles are produced by the engine into shared memory and drawn by
  `CALayer`/`SurfaceView` — no bitmap copies on the scroll path.
- iOS extras: share extension, Quick Look thumbnail extension, Files provider, PencilKit-quality
  stylus input mapped onto `selis-pdf-annot` ink geometry.
- Android extras: SAF document provider, share target, and a print-service plugin.
- Scan pipeline (Phase 7): camera → edge detect → perspective dewarp → binarise → JBIG2/JPEG
  encode → `selis-ocr` invisible text layer → tagged PDF.

### CLI (`apps/cli`)

`selis inspect | render | extract | convert | optimise | sign | verify | redact | ocr | batch`.
Every command is `--json`-capable and every command is the exact same code path the GUI uses.
The CLI is not a toy: it is the SDK's smoke test, the corpus harness's driver, and a product.

### SDK (`selis-pdf-ffi` + wrappers)

Opaque handles, no callbacks into Rust from foreign code except a progress function pointer,
errors as out-parameters with a numeric code from the same registry as everything else.
Wrappers for Python, Node, Go, Java, .NET generated from one IDL — see `24-BINDINGS-SPEC.md §4`.

---

## 10. Data flow: opening and rendering a remote PDF, end to end

```text
1.  UI: user navigates to https://example.org/report.pdf
2.  Extension DNR redirects to viewer.html?src=…
3.  Worker constructs HttpRangeSource(src); requests the last 64 KiB.
4.  selis-pdf-cos reads startxref from the tail; requests the xref range; returns Pending twice
    while ranges arrive; builds CosIndex. Budget::profile(Viewer) charged throughout.
5.  Linearisation dict present → selis-pdf-doc knows page 1's object range; requests exactly that.
6.  selis-pdf-engine asks selis-pdf-content for page 1's DisplayList. Fonts resolved via selis-font;
    embedded subset parsed; missing glyph → standard-14 metric substitute from the fallback table.
7.  selis-raster renders the visible tiles at the current matrix. Deterministic, budgeted.
8.  Tiles transferred to the main thread; compositor paints. First paint target < 1.2 s p75.
9.  Background: selis-pdf-text builds the extraction/search index for resident pages only.
10. User scrolls → engine requests further ranges; cache evicts by LRU under the memory budget.
11. User annotates → selis-pdf-annot creates the annotation + appearance stream; selis-pdf-edit journals it.
12. User saves → selis-pdf-edit emits an incremental update appended to the original bytes;
    FsaSource writes in place, or the shell downloads the concatenation. Original bytes: untouched.
```

---

## 11. Logging vs. the operation log

Two separate systems; never merge them.

- **Structured logging** (`selis-log::log`) is for engineers. Off in release except at `warn`.
  Never contains document content (ADR-P0017).
- **The operation log** (`selis-log::oplog`) is a user-facing, per-document record of every mutation:
  what changed, when, by whom, with the revision it produced. It is what powers undo history,
  "compare versions", the audit trail the enterprise SKU sells, and the answer to "what did this
  tool do to my file?". It is persisted in the `.selis` session bundle and, optionally, as a
  private metadata stream in the PDF.

---

## 12. Repository layout

```text
recto/
├── Cargo.toml                  # workspace, [workspace.dependencies] is the only place versions live
├── rust-toolchain.toml
├── deny.toml  supply-chain/    # cargo-deny + cargo-vet
├── xtask/                      # all automation, in Rust
│   ├── layers.toml  coverage.toml  size-budgets.toml  oracles.toml
├── crates/
│   ├── selis-error/ selis-bytes/ selis-geom/ selis-color/ selis-log/
│   ├── selis-io/ selis-sandbox/ selis-crypto/
│   ├── selis-pdf-cos/ selis-pdf-filter/ selis-pdf-doc/ selis-font/ selis-shape/ selis-pdf-content/ selis-raster/
│   ├── selis-pdf-text/ selis-image/ selis-pdf-annot/ selis-pdf-form/ selis-pdf-edit/ selis-pdf-redact/ selis-pdf-sign/
│   ├── selis-ocr/ selis-pdf-convert/ selis-pdf-a11y/ selis-pdf-optimize/ selis-pdf-js/
│   ├── selis-pdf-engine/ selis-policy/ selis-job/
│   └── selis-pdf-wasm/ selis-pdf-ffi/ selis-pdf-jni/ selis-pdf-swift/ selis-pdf-ipc/ selis-pdf-service/
├── apps/
│   ├── web/{ui,host}/  extension/  desktop/  ios/  android/  cli/
├── cloud/                      # opt-in services (ADR-P0016)
├── corpus/                     # manifests + expectations, NOT the files (licensing)
├── fuzz/                       # cargo-fuzz targets, one per parser entry point
├── bench/                      # criterion + the perf-budget harness
└── docs/{specs,legal,adr,provenance}/
```

---

## 13. What deliberately does NOT exist

- **No plugin ABI in year 1.** A plugin system on a hostile-input engine is an attack surface and a
  compatibility anchor. Revisit at Phase 9 with a WASM-component plugin model, not a native ABI.
- **No server fallback for client rendering** (ADR-P0033).
- **No proprietary annotation sidecar as the source of truth** (ADR-P0026).
- **No second updater / licence system** (ADR-P0029).
- **No XFA authoring.** Read and render static XFA; never generate it. It is a dead format and
  supporting authoring would double the form model's complexity for a shrinking population.
- **No PDF "repair by re-writing" on open.** If a file is damaged we reconstruct an *in-memory*
  index and tell the user; we never silently rewrite their file to make it valid.
