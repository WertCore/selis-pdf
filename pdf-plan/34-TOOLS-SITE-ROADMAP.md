# 34 — Tools Site Roadmap: iLovePDF and ihatepdf.cv Feature Matrix

**Status:** execution plan, 27 October 2026. Supersedes the six-tool MVP framing.
**Scope:** every feature offered by iLovePDF and ihatepdf.cv, with editing explicitly deferred to
Phase 5. Browser-native PDF display is the display strategy; a custom viewer is not a launch gate.
**Related:** `11A-PHASE-1A-tools.md`, `28-FIRST-RELEASE-SCOPE.md`, `22-SECURITY-AND-SUPPLY-CHAIN.md`,
ADR-P0008 (owned engine), ADR-P0016 (local-only), ADR-P0031 (tagged merge).

---

## 1. The product decision

The next product surface is a **tool-first site** at `/tools.html`, not another viewer screen. The
viewer remains at `/`. The tools page is a grid of operations; choosing one opens a local workspace
with drag-and-drop, options, run, verification, browser-native preview, and download.

Two rules are non-negotiable:

1. **Document bytes never reach a non-local origin.** There is no upload endpoint to disable. The
   page's CSP is `connect-src 'self'`; the tools WASM chunk has **zero imports**; `place-assets`
   fails the build if that changes; the page installs the existing request recorder and asserts no
   request carried input bytes before showing a result.
2. **No third-party PDF implementation in a product path.** pdf.js, pdf-lib, qpdf, pdfium, MuPDF,
   Ghostscript, and similar tools remain CI oracles only. The browser runs the owned
   `selis-pdf-tools` engine compiled to WASM; the CLI runs the same crate through its native
   filesystem backend.

Browser-native PDF display is deliberate. Inputs and outputs are opened as Blob URLs in the
browser's PDF viewer. We do not build a custom viewer as a launch gate; the existing viewer remains
for rendering, search, and print. This removes a large surface from the tools release and keeps the
claim simple: the browser displays the file, our engine changes it locally.

Editing is deferred. "Edit PDF text" is Phase 5 work under `23-EDIT-MODEL-SPEC.md`; no Phase 1A
tool will claim to reflow text. Redaction is removal, never an overlay; a preview requires the
raster path and is therefore not a Phase 1A/2 shortcut.

---

## 2. What is live now (Phase 1A.0, `/tools.html`)

| Feature | iLovePDF | ihatepdf.cv | Status | Notes |
|---|---|---|---|---|
| Merge PDF | yes | yes | **Live** | Multi-file, ordered list, up/down reorder, annotation/form/OCG/outline reconciliation, verification rows. |
| Split PDF (range) | yes | yes | **Live** | 1-based UI, inclusive range, empty "to" = last page. |
| Rotate PDF | yes | yes | **Live** | 90/180/270, all pages or 1-based selection. |
| Delete pages | yes | yes | **Live** | 1-based selection; refuses to delete every page. |
| Reorder pages | yes (Organize) | yes | **Live** | Full permutation, 1-based input. |
| Edit metadata | yes | yes | **Live** | Info dictionary Title/Author/Subject/Keywords. XMP and strip are next. |
| Browser-native preview | — | — | **Live** | Inputs and outputs open as Blob URLs in the browser's PDF viewer. |
| Verification display | — | — | **Live** | Pages, annotations, fields, layers, outlines, embedded files, size, verdict. |
| Local-only guard | — | — | **Live** | `data-no-upload="verified"` after every successful run; request recorder + CSP + import-free chunk. |

### 2.1 Acceptance criteria for every live tool

- [ ] The output opens in the browser's PDF viewer and in the CLI's `check` command.
- [ ] Structural verification runs before the output is offered; a failed verification produces no
      file and a typed error.
- [ ] The operation exists in the CLI and the browser through the same `selis-pdf-tools` code.
- [ ] The tools WASM chunk has zero imports, is lazy-loaded, and stays under its size budget
      (currently 300 000 B brotli; baseline 137 074 B).
- [ ] A browser run makes only same-origin GETs for the page graph and the WASM chunk; no request
      carries document bytes, a filename, or page text.
- [ ] Keyboard operation, labels for every control, visible focus, and a live status region.
- [ ] No document byte is written to Cache Storage, IndexedDB, or a telemetry body.

---

## 3. The full feature matrix

Legend: **Live** = shipped in this slice; **Next** = Phase 1A.1, no renderer required; **Raster** =
needs the owned raster path; **Text** = needs text/layout extraction; **Forms/Security** = needs
the form, signature, or crypto surface; **Edit** = Phase 5 editing; **AI/Collab** = Phase 8;
**Spec** = Phase 9 compliance work. "ihatepdf" is abbreviated "ihp" where the column is narrow.

### 3.1 Page operations and organization

| Feature | iLovePDF | ihp | Phase | Status / constraint |
|---|---|---|---|---|
| Merge | yes | yes | 1A.0 | Live. |
| Split by range | yes | yes | 1A.0 | Live. |
| Split every N pages | yes | yes | 1A.1 | Writer + protocol must return multiple outputs; one ZIP download. |
| Split in half | — | yes | 1A.1 | Same multi-output path. |
| Split by size | — | yes | 1A.1 | Target bytes; deterministic split points; report on the result card. |
| Split by bookmarks | — | yes | 1A.1 | Needs outline reader; already in the document model. |
| Split by text | — | yes | Text | Needs text extraction and a boundary rule; show the chosen boundaries before committing. |
| Remove pages | yes | yes | 1A.0 | Live as Delete pages. |
| Organize / reorder | yes | yes | 1A.0 | Live with buttons. Drag-to-reorder remains a UI enhancement, not a correctness gap. |
| Insert blank pages | — | yes | 1A.1 | Writer page insertion; no renderer. |
| Duplicate pages | — | yes | 1A.1 | Object copy; verify resource identity and annotations. |
| Rotate | yes | yes | 1A.0 | Live. |
| Crop | — | yes | 1A.1 | Page boxes + content transform; preview via browser PDF viewer. |
| Resize / scale to page size | — | yes | 1A.1 | Content matrix + media box; preserve annotations. |
| N-up (2/4/6/9) | — | yes | 1A.1 | Content placement; needs a page-content builder, already owned. |
| Flip horizontal/vertical | — | yes | 1A.1 | Content matrix; verify against original pixels only when raster exists. |
| Alternate / mix pages | — | yes | 1A.1 | Interleave two documents; preserve page-level resources. |
| Extract pages | yes | yes | 1A.1 | Split path with a multi-output response. |
| Page numbers | yes | yes | 1A.1 | Content builder + font subset; placement, start, format, skip-first. |
| Watermark | yes | yes | 1A.1 | Text/image watermark; image path needs `selis-image`; keep flatten as an option. |
| Headers and footers | — | yes | 1A.1 | Same text layout primitives as page numbers. |
| Bates numbering | — | yes | 1A.1 | Deterministic prefix/suffix, start, padding, position. |
| Bookmarks / outline editing | — | yes | 1A.1 | Outline reader/writer exists; add UI and verification. |
| Attachments extract | — | yes | 1A.1 | CLI exists. Browser protocol needs a multi-output response and per-file download. |
| Attachment add/remove | — | yes | 1A.1 | Writer `/Names/EmbeddedFiles`; sanitize names. |
| Page labels | — | yes | 1A.1 | Preserve/shift on merge; edit as a separate operation. |
| Flatten annotations | — | yes | Raster | Requires appearance-stream rendering; never claim flatten without a render check. |
| Flatten forms | — | yes | Raster/Forms | Same; form appearance generation is its own correctness surface. |
| Invert colors | — | yes | Raster | Content-stream transform or render-to-image; prefer the latter only with an owned raster path. |
| PDF → ZIP | — | yes | 1A.1 | Multi-output path; ZIP writer is a small owned container, not a PDF library. |

### 3.2 Compression, repair, and privacy

| Feature | iLovePDF | ihp | Phase | Status / constraint |
|---|---|---|---|---|
| Compress (lossless) | yes | yes | 1A.1 | CLI exists. Bring to browser: object stream/GC/dedup/flate; no raster needed. |
| Compress (target size / lossy) | yes | yes | Raster | Needs image decode/re-encode and a quality/size search. Show measured result. |
| Repair | yes | yes | 1A.1 | Reader recovery exists; browser must expose salvage vs. strict mode and never silently "repair" by dropping pages. |
| Metadata strip | — | yes | 1A.1 | Info + XMP removal; keep an explicit list of what was removed. |
| Privacy risk scanner | — | yes | 1A.1 | Revision history, metadata/author trails, hidden OC layers, embedded files, JavaScript, external references, text under images (last needs raster). Report only; no auto-fix. |
| Fingerprinting report | — | yes | Raster | Producer/creator, IDs, timestamps, embedded fonts, image hashes, incremental revisions. Structural portion can ship in 1A.1. |
| Auto-redact PII | — | yes | Raster + Edit | Detection can be text/regex; committing redaction must be removal with preview. No overlay. |
| Redact (manual) | — | yes | Raster + Edit | Engine CLI exists; browser needs page rendering for the rectangle picker and a removal verification pass. |
| Invert | — | yes | Raster | See above. |
| Compare two PDFs | — | yes | Raster/Text | Pixel and text diff; render determinism makes the comparison exact. |

### 3.3 Convert

| Feature | iLovePDF | ihp | Phase | Status / constraint |
|---|---|---|---|---|
| JPG → PDF | yes | yes | 1A.1 | `selis-image` decodes JPEG; writer already embeds. Browser must accept image files, not only PDF. |
| PNG → PDF | yes | yes | 1A.1 | Same. |
| WebP → PDF | — | yes | Raster codec | Needs a new owned codec dependency under the dependency policy. |
| HEIC → PDF | — | yes | Raster codec | Same; licence and platform review required. |
| TIFF (multi-page) → PDF | — | yes | Raster codec | Same; define page order and DPI. |
| SVG → PDF | — | yes | Text/Vector | SVG subset renderer; do not claim full SVG. |
| Word → PDF | yes | yes | Text/Office | OOXML layout is a separate product-sized problem; Phase 5 at the earliest. |
| PowerPoint → PDF | yes | yes | Text/Office | Same. |
| Excel → PDF | yes | yes | Text/Office | Same; CSV → PDF can ship earlier as a table renderer. |
| HTML → PDF | yes | yes | Text | Owned HTML/CSS subset; print media; no browser engine dependency in the product path. |
| Markdown → PDF | — | yes | 1A.1/Text | `selis-pdf-convert` exists for Markdown/HTML → PDF; expose it in a lazy convert chunk. |
| LaTeX → PDF | — | yes | Text | Needs a TeX layout subset or an explicit "unsupported" refusal; no shelling out to a TeX distribution. |
| CSV → PDF | — | yes | 1A.1 | Table renderer; delimiter/encoding controls. |
| Audio → PDF | — | yes | AI/Collab | Transcription is an AI capability; the PDF output is a transcript layout. Do not ship a fake "audio to PDF". |
| Ebook (EPUB) → PDF | — | yes | Text | EPUB layout; large scope. |
| PDF → JPG | yes | yes | Raster | Render every page; ZIP for multi-page. |
| PDF → PNG | — | yes | Raster | Same. |
| PDF → images (WebP/TIFF) | — | yes | Raster codec | Same. |
| PDF → Word | yes | yes | Text | Top search volume and hardest fidelity problem; wait for text extraction and label honestly. |
| PDF → PowerPoint | yes | yes | Text | Page images + text boxes; fidelity caveat. |
| PDF → Excel | yes | yes | Text | Table reconstruction; confidence display. |
| PDF → text | — | yes | Text | Extraction quality report and page separators. |
| PDF → HTML | — | yes | Text | Positioned spans first; reflow later. |
| PDF → EPUB | — | yes | Text | Reflow from extracted structure. |
| PDF → PDF/A | yes | — | Spec | ISO 19005; Phase 9 after spec procurement. |
| PDF → audio | — | yes | AI/Collab | Local TTS model; explicit model download; never a cloud default. |
| PDF → metadata | — | yes | 1A.0 | Live as metadata editor; add a read-only JSON export. |
| PDF → ZIP | — | yes | 1A.1 | Bundle attachments or split outputs. |

### 3.4 Security, signatures, and forms

| Feature | iLovePDF | ihp | Phase | Status / constraint |
|---|---|---|---|---|
| Unlock / remove password | yes | yes | Forms/Security | CLI exists. Browser needs a crypto-enabled chunk; the current tools chunk deliberately has no RNG imports. |
| Protect with password | yes | yes | Forms/Security | AESV3/R6 only; separate user/owner passwords; permission copy reviewed. |
| Clear permissions | — | yes | Forms/Security | CLI exists; owner-password override only. |
| Permissions / encrypt | — | yes | Forms/Security | Same surface as protect. |
| Sign PDF | yes | yes | Forms/Security | Certificate/signature UI; keys never leave device; signature invalidation warnings. |
| Fill forms | — | yes | Forms/Security | AcroForm field editing; flatten is a separate raster/forms task. |
| Certificate validation | — | yes | Forms/Security | Trust store and revocation policy; do not claim validity without a chain. |
| Handwriting / thumbmark | — | yes | Raster | Ink capture → vector or image overlay; preview and flatten option. |
| Remove password from scans | — | yes | Forms/Security | Same as unlock. |
| Redaction | — | yes | Raster + Edit | See privacy section. |
| Workflow (approve / route) | — | yes | Edit/Collab | Local workflow state first; sharing is P2P later. No server workflow in the local-only build. |

### 3.5 Scan, OCR, and text

| Feature | iLovePDF | ihp | Phase | Status / constraint |
|---|---|---|---|---|
| Scan to PDF (camera) | yes | yes | Raster | Camera permission is feature-scoped; edge detection and deskew are raster work. |
| OCR | yes | yes | Raster | Owned OCR path; language packs lazy-loaded; confidence and searchable-text verification. |
| Searchable PDF export | yes | yes | Raster | OCR text layer + original image; verify text extraction. |
| Text extraction | — | yes | Text | Already a library surface; expose with layout/confidence. |
| Chat with PDF | — | yes | AI/Collab | On-device model behind explicit download; no default cloud API. |
| Summarize | — | yes | AI/Collab | Same. |
| Privacy scanner | — | yes | 1A.1 | See 3.2. |
| Fingerprinting | — | yes | Raster | See 3.2. |

### 3.6 Business, sharing, and collaboration

| Feature | iLovePDF | ihp | Phase | Status / constraint |
|---|---|---|---|---|
| GST invoice generation | — | yes | AI/Collab/vertical | Template + tax rules; not a PDF-engine feature. Keep out of the core chunk. |
| GST filing export | — | yes | AI/Collab/vertical | Jurisdiction-specific; separate product track. |
| POS billing | — | yes | AI/Collab/vertical | Separate product track. |
| P2P share | — | yes | AI/Collab | WebRTC data channels; no relay that sees document bytes; explicit consent and room codes. |
| Collaborative whiteboard | — | yes | AI/Collab | Separate CRDT surface; PDF export via the writer. |
| Client-side task queue / batch | — | yes | 1A.1 | CLI batch exists; browser needs a multi-file queue with per-file isolation and no cross-file cancellation. |
| Offline PWA | — | yes | 1A.1 | Live. Viewer shell and the complete tools page graph are precached; the browser check cuts the origin, reloads `/tools.html`, and merges again from the worker. |
| SEO landing page per tool | — | — | Ship | One route per operation; the privacy claim in the meta description is the differentiator. |

---

## 4. Phases and exit gates

### Phase 1A.0 — this slice (done when the branch is green)

- Six live tools: merge, split, rotate, delete, reorder, metadata.
- Tool-first page, browser-native preview, verification rows, download.
- `selis-pdf-tools` shared by CLI and browser; in-memory backend has no filesystem names.
- `selis-pdf-wasm-tools` protocol chunk: zero imports, lazy, size-budgeted, no-upload asserted.
- Unit tests for protocol/input parsing; browser check for merge, output download, verification,
  and no document bytes in any request.
- **Exit:** `cargo test` targeted suites, web tests, browser check, size-check, lint, and the
  no-upload network assertions pass. No claim of feature parity.

### Phase 1A.1 — the rest of the object-graph toolkit

Ship: split modes, organize/insert/duplicate, crop/resize, N-up/flip/alternate/mix, page numbers,
watermark, headers/footers, Bates, compression lossless, repair, metadata strip, attachments,
bookmarks, page labels, CSV/Markdown/HTML→PDF, image→PDF for owned codecs, multi-output + ZIP,
batch queue, unlock/protect/clear-permissions (crypto-enabled chunk).
**Exit:** every object-graph tool has CLI + browser parity, verification, a corpus entry, and a
size budget; no raster feature is claimed yet.

### Phase 2 — raster and OCR

Owned render → JPG/PNG/WebP/TIFF, PDF→ZIP of images, OCR + searchable PDF, redaction preview,
flatten annotations/forms, invert, compare, fingerprinting, scan-to-PDF, lossy compression targets.
**Exit:** render hash determinism, pixel/text verification for every output, OCR confidence and
extraction tests, a lazy raster chunk under budget.

### Phase 3 — text and layout

PDF→text/HTML/EPUB, Word/Excel/PowerPoint reconstruction, accessibility extraction, split-by-text,
table extraction, HTML/CSS/Markdown/LaTeX layout refinements.
**Exit:** text extraction ladder and honest confidence; no layout claim above measured fidelity.

### Phase 4 — forms, signatures, security UI

Fill forms, sign, certificates, permissions, encryption UI, handwriting/thumbmark.
**Exit:** keys never leave the device; signature invalidation is disclosed before writing; form
values survive a round trip.

### Phase 5 — editing (deferred)

Edit text, images, vector content, pages; workflows; compare-and-merge; redaction commit UI.
**Exit:** content-rewriting verification (render + text) replaces the structural-only standard;
no redaction overlay ships.

### Phase 8 — AI and collaboration

On-device chat/summarize/audio, P2P share, collaborative whiteboard, business verticals.
**Exit:** an explicit model download, no default cloud path, and a network trace proving document
bytes stay local.

### Phase 9 — compliance

PDF/A, PDF/UA, and any standards requiring procured specifications.
**Exit:** spec-backed conformance claims; no guessed validator.

---

## 5. Architecture guardrails

1. **One tools library, two backends.** `selis-pdf-tools` is byte-oriented; the native backend owns
   atomic file commits and progress/cancellation; the browser backend owns `insert_input`/
   `take_output` and no filesystem reference. Both run the same verification gate.
2. **Lazy, import-free tools chunk.** The tools WASM chunk must not depend on the viewer chunk.
   `place-assets` compiles it in Node and fails on any `WebAssembly.Module.imports` entry. The
   loader checks again before instantiation.
3. **No page renderer in the tools chunk unless a tool needs it.** The six live tools are
   object-graph operations; keeping the renderer out is why the chunk is 137 KB brotli instead of
   the multi-megabyte viewer. A raster tool gets its own lazy chunk.
4. **No RNG in the inert tools chunk.** `selis-crypto`/`selis-pdf-cos` expose an optional `rng`
   feature. Native and viewer builds enable it; the six live tools disable it so `getrandom`'s
   wasm-bindgen glue cannot sneak in. Security tools will use a separate crypto-enabled chunk.
5. **Browser-native display.** Blob URLs, browser PDF viewer, no custom viewer gate. A tool may
   preview its output this way; it may not upload one.
6. **No document persistence.** Cache Storage content-type allow-list excludes PDFs; the tools
   page never writes a document to a cache or database. Download is a Blob URL.
7. **No third-party PDF code in product paths.** Oracles remain CI-only.
8. **Unowned environment failures do not churn the tree.** Build with `pnpm.cmd`, `-j 1` for
   memory-constrained Cargo, and never edit `supply-chain/audits.toml` to paper over a gate.

---

## 6. Launch checklist (tools surface)

- [x] `/tools.html` exists, links the external `tools.css` and compiled `tools-page.js`.
- [x] Six live operations with 1-based page input.
- [x] Verification rows and download.
- [x] Browser-native preview for inputs and outputs.
- [x] Lazy tools chunk copied by `place-assets`; build fails when missing.
- [x] Import-free check in `place-assets` and the loader.
- [x] Size budget + baseline.
- [x] Web unit tests for protocol/input parsing.
- [x] Browser check drives merge end-to-end, downloads the output, and asserts the network log.
      `xtask/browser/tools-page` passed in Edge: 6 same-origin resources, 0 foreign, Pages
      `2 → 2`, `%PDF-` download, `data-no-upload="verified"` before the result appeared.
- [x] Tools page precache/offline promotion. `/tools.html`, `tools.css`, the tools module graph,
      the registrar, and `selis_pdf_wasm_tools.wasm` are in the install set; `CACHE_VERSION` is 2.
      The `tools-page` browser check now merges online, cuts the origin, reloads the page, and
      merges again with zero foreign resources. The document cache exclusion is unchanged.
- [ ] CI green on `feature/tools-site`; fast-forward `origin/main`; no worktree/build on `C:`.

---

## 7. What this plan deliberately does not promise

- Feature-count parity on day one. The matrix is staged because correctness, not a card grid, is
  the wedge.
- Word/Excel/PowerPoint fidelity before the text pipeline.
- Redaction as a black rectangle. Removal or nothing.
- A custom viewer as the tools surface.
- A cloud AI default, a server task queue, or a sharing relay that sees document bytes.
- A "repair" that silently drops content, or a "compress" that changes pixels while claiming
  lossless.
