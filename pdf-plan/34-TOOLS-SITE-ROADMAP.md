# 34 — Tools Site Roadmap: iLovePDF and ihatepdf.cv Feature Matrix

**Status:** execution plan, updated 28 October 2026. Supersedes the six-tool MVP framing.
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

## 2. What is live now (Phase 1A.0 + the Phase 1A.1 split, organize, overlay, compression, attachment, mix, crop, flip, page-label, metadata-strip, resize, N-up, attach, detach, and repair slices, `/tools.html`)

| Feature | iLovePDF | ihatepdf.cv | Status | Notes |
|---|---|---|---|---|
| Merge PDF | yes | yes | **Live** | Multi-file, ordered list, up/down reorder, annotation/form/OCG/outline reconciliation, verification rows. |
| Mix pages | — | yes | **Live** | Round-robin interleave of two or more PDFs: page 1 of each input, then page 2, and so on. Page labels are materialised per output page when any input has them, so styles cannot leak across inputs. |
| Split PDF (range) | yes | yes | **Live** | 1-based UI, inclusive range, empty "to" = last page. |
| Split every N pages | yes | yes | **Live** | One PDF per group, capped at 64 parts; multi-part runs add a deterministic ZIP bundle. |
| Split in half | — | yes | **Live** | Two PDFs split at the middle page, plus the ZIP bundle. |
| Split by target size | — | yes | **Live** | Target entered in MB; exact measured output bytes; deterministic ranges; a one-page part that cannot fit is marked "over target". |
| Split by bookmarks | — | yes | **Live** | Top-level outline destinations; front matter stays separate; refuses an absent outline or a single resulting part. |
| Extract pages | yes | yes | **Live** | One PDF containing the selected pages in document order. |
| Rotate PDF | yes | yes | **Live** | 90/180/270, all pages or 1-based selection. |
| Crop PDF | — | yes | **Live** | Margins in points, measured in the displayed (rotation-applied) orientation. The new rectangle becomes both `/MediaBox` and `/CropBox`; print boxes outside it are dropped. Content is clipped by the page box, not rasterized. |
| Flip PDF | — | yes | **Live** | Mirrors painted page content horizontally or vertically relative to the displayed orientation. Page boxes, rotation, resources, annotations, and form fields are carried unchanged; annotations are not mirrored. |
| Delete pages | yes | yes | **Live** | 1-based selection; refuses to delete every page. |
| Organize pages | yes | yes | **Live** | One workspace with reorder, duplicate, and insert-blank modes; full permutation, 1-based input. |
| Duplicate pages | — | yes | **Live** | Copies follow their originals; content, resources, and annotations travel with each copy. |
| Insert blank pages | — | yes | **Live** | 1-based boundary (0 = before page 1, empty = append); copies the neighbouring page's media box and rotation; up to 100 blanks per run. |
| Page numbers | yes | yes | **Live** | 1-based selection, corner/edge placement, starting number, `n`, `n of total`, and `Page n` formats, font/color/margins. |
| Text watermark | yes | yes | **Live (text)** | Diagonal or horizontal centered text, opacity via `ExtGState`, 1-based selection. Image and tiled watermarks are not claimed. |
| Headers and footers | — | yes | **Live** | Three independently aligned slots each, `{page}`/`{pages}` placeholders, margins, font/color. |
| Bates numbering | — | yes | **Live** | Prefix, suffix, start, zero-padding, placement, 1-based selection; advances only over selected pages. |
| Compress PDF (lossless) | yes | yes | **Live** | Reachable-object GC, Flate re-encode, byte-identical-object dedup, inherited page attributes, trailer-level `/Info`; encrypted files are refused. |
| Extract attachments | — | yes | **Live** | Reads `/Names/EmbeddedFiles`, decodes the filter chain, and downloads each file; two or more files also get a deterministic ZIP. Names are flattened and deduplicated, encrypted PDFs are refused, and limits are 64 files / 256 MiB each. |
| Embed files | — | — | **Live** | Takes the first selected file as the PDF and stores every later file as a Flate-compressed `/Type /EmbeddedFile` with a `/Type /Filespec` entry in `/Names /EmbeddedFiles`. A same-named file is replaced, not duplicated. Pages and document structure are rewritten and verified; limits are 64 files / 256 MiB each, and encrypted inputs are refused. |
| Remove attachments | — | — | **Live** | Drops `/Names /EmbeddedFiles` and prunes the superseded name-tree, filespec, and stream objects, so their bytes do not linger. Pages, annotations, forms, outlines, page labels, and tags survive. A document with no attachments rewrites cleanly and reports `0 → 0`; encrypted inputs are refused. |
| Page labels | — | yes | **Live** | Decimal, lower/upper Roman, lower/upper letter, or prefix-only numbering with a starting number; 1-based selection renumbered in document order. Unselected pages keep the label they already carried; every output page gets an explicit `/PageLabels` entry. Encrypted inputs are refused. |
| Resize pages | — | yes | **Live** | A4/A5/Letter/Legal/A3 presets or custom points, with fit (keeps aspect ratio, centred) or stretch. Selected pages get `/MediaBox` and `/CropBox` of exactly the target size and a normalized `/Rotate 0`; content is scaled in displayed orientation. Unselected pages are copied unchanged and annotations keep their coordinates. Encrypted inputs are refused. |
| N-up pages | — | yes | **Live** | 2, 4, 6, or 9 pages per sheet (1×2, 2×2, 2×3, 3×3). Every sheet takes the first page's displayed size; each page keeps its aspect ratio and displayed orientation and is centred in its cell. Content is placed through Form XObjects that keep the source page's resources and clip to its visible box. Annotations, links, and form fields are not painted; outlines, page labels, attachments, and layers are reconciled to the sheets. Encrypted inputs are refused. |
| Edit metadata | yes | yes | **Live** | Info dictionary Title/Author/Subject/Keywords. XMP writing is not exposed yet. |
| Remove metadata | — | yes | **Live** | Drops the trailer/catalog `/Info` and the catalog XMP `/Metadata`; pages, annotations, forms, outlines, page labels, attachments, and tags survive. Encrypted inputs are refused. PDF/A conformance and signatures may be invalidated, and the UI says so. |
| Repair PDF | — | yes | **Live** | Opens the strongest way the file allows: the ordinary parser, then the last intact revision, then a rebuilt object index. Every recovered page is rewritten through the same writer and structural gate, the recovered page count is asserted on the output, and the strategy is shown as a Recovery row. Encrypted inputs are refused. |
| Browser-native preview | — | — | **Live** | PDF outputs open as Blob URLs in the browser's viewer. Attachment outputs download as files and are never offered as a PDF preview. |
| Verification display | — | — | **Live** | Pages, annotations, fields, layers, outlines, embedded files, size, verdict. |
| Local-only guard | — | — | **Live** | `data-no-upload="verified"` after every successful run; request recorder + CSP + import-free chunk. |

### 2.1 Acceptance criteria for every live tool

- [ ] PDF outputs open in the browser's PDF viewer and the CLI's `check` command; file outputs
      download as files and are never treated as PDFs.
- [ ] Structural verification runs before the output is offered; a failed verification produces no
      file and a typed error.
- [ ] The operation exists in the CLI and the browser through the same `selis-pdf-tools` code.
- [ ] The tools WASM chunk has zero imports, is lazy-loaded, and stays under its size budget
      (currently 300 000 B brotli; baseline 247 784 B).
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
| Split every N pages | yes | yes | 1A.1 | Live. One PDF per group plus a ZIP bundle; capped at 64 PDF parts. |
| Split in half | — | yes | 1A.1 | Live. Two PDFs plus the ZIP bundle. |
| Split by size | — | yes | 1A.1 | Live. Target MB converted to bytes; exact measured boundaries; over-target parts flagged. |
| Split by bookmarks | — | yes | 1A.1 | Live. Top-level outline targets; front matter separate; one-part results refused. |
| Split by text | — | yes | Text | Needs text extraction and a boundary rule; show the chosen boundaries before committing. |
| Remove pages | yes | yes | 1A.0 | Live as Delete pages. |
| Organize / reorder | yes | yes | 1A.0 | Live with buttons. Drag-to-reorder remains a UI enhancement, not a correctness gap. |
| Insert blank pages | - | yes | 1A.1 | Live. Boundary insertion copies the neighbouring page's media box and rotation; capped at 100 per run. |
| Duplicate pages | - | yes | 1A.1 | Live. Object copy carries content, resources, and annotations; labels, destinations, and tagged structure keep their first occurrence. |
| Rotate | yes | yes | 1A.0 | Live. |
| Crop | — | yes | 1A.1 | Live. Visual-edge margins replace `/MediaBox` and `/CropBox`; print boxes outside the new rectangle are dropped. Content is clipped by the page box, never rasterized. |
| Resize / scale to page size | — | yes | 1A.1 | Live. A4/A5/Letter/Legal/A3 presets or custom points, fit or stretch in displayed orientation; selected pages get fresh `/MediaBox` and `/CropBox` and normalized `/Rotate 0`, unselected pages are unchanged, and annotations keep their coordinates. |
| N-up (2/4/6/9) | - | yes | 1A.1 | Live. Grid placement through Form XObjects on the first page's displayed sheet size; aspect ratio and displayed orientation are kept, and outlines, labels, attachments, and layers are reconciled. |
| Flip horizontal/vertical | — | yes | 1A.1 | Live. Display-relative content-transform wrapper; page boxes, rotation, annotations, and form fields are unchanged and are not mirrored. Pixel comparison waits for the raster path. |
| Alternate / mix pages | — | yes | 1A.1 | Live. Round-robin interleave; page labels are expanded per output page when any input has them. |
| Extract pages | yes | yes | 1A.1 | Live. One PDF, selected pages in document order. |
| Page numbers | yes | yes | 1A.1 | Live. Standard-14 Helvetica overlay; placement, start, `n`/`n of total`/`Page n`, 1-based selection. Skip-first is not implemented. |
| Watermark | yes | yes | 1A.1 | Live for centered text (diagonal/horizontal, opacity, selection). Image/tiled watermarks need `selis-image`; flatten remains an option. |
| Headers and footers | — | yes | 1A.1 | Live. Three aligned slots each with `{page}`/`{pages}` placeholders. |
| Bates numbering | — | yes | 1A.1 | Live. Deterministic prefix/suffix, start, padding, placement, selection. |
| Bookmarks / outline editing | — | yes | 1A.1 | Outline reader/writer exists; add UI and verification. |
| Attachment add/remove | — | yes | 1A.1 | Live. Embed one or more files (Flate-compressed `/Type /EmbeddedFile` + `/Type /Filespec` in `/Names /EmbeddedFiles`) or remove every attachment, pruning the superseded tree. Names are validated and sanitized; same-named files replace rather than duplicate; 64-file / 256 MiB limits; encrypted inputs are refused. |
| Page labels | — | yes | 1A.1 | Live. Decimal/Roman/letter/prefix-only styles, start number, 1-based selection; every output page gets an explicit `/PageLabels` entry. |
| Flatten annotations | — | yes | Raster | Requires appearance-stream rendering; never claim flatten without a render check. |
| Flatten forms | — | yes | Raster/Forms | Same; form appearance generation is its own correctness surface. |
| Invert colors | — | yes | Raster | Content-stream transform or render-to-image; prefer the latter only with an owned raster path. |
| PDF → ZIP | — | yes | 1A.1 | Live for split outputs and extracted attachments; the ZIP writer is a small owned container, not a PDF library. A general file-picker ZIP is next. |

### 3.2 Compression, repair, and privacy

| Feature | iLovePDF | ihp | Phase | Status / constraint |
|---|---|---|---|---|
| Compress (lossless) | yes | yes | 1A.1 | Live in CLI and browser. Reachable-object GC, Flate re-encode, byte-identical-object dedup, trailer `/Info` preservation; encrypted files are refused rather than silently unlocked. |
| Compress (target size / lossy) | yes | yes | Raster | Needs image decode/re-encode and a quality/size search. Show measured result. |
| Repair | yes | yes | 1A.1 | Live in CLI and browser. Strict parse → last intact revision → reconstructed object index; every recovered page is rewritten and the recovered count is asserted on the output, so a repair cannot silently drop pages. The strategy is reported (`strict`, `rolled-back revision`, `reconstructed index`); encrypted inputs are refused. |
| Metadata strip | — | yes | 1A.1 | Live in CLI and browser. Removes `/Info` and the XMP packet while keeping pages, annotations, forms, outlines, page labels, attachments, and tags; encrypted inputs are refused. |
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
| PDF → ZIP | — | yes | 1A.1 | Live for extracted attachments and split outputs; a general file-picker ZIP is next. |

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

### Phase 1A.0 — initial six-tool slice (shipped)

- Six live tools: merge, split by range, rotate, delete, reorder, metadata.
- Tool-first page, browser-native preview, verification rows, download.
- `selis-pdf-tools` shared by CLI and browser; in-memory backend has no filesystem names.
- `selis-pdf-wasm-tools` protocol chunk: zero imports, lazy, size-budgeted, no-upload asserted.
- Unit tests for protocol/input parsing; browser check for merge, output download, verification,
  and no document bytes in any request.
- **Exit:** green targeted suites, web tests, browser check, size-check, lint, and the no-upload
  network assertions. No claim of feature parity.

### Phase 1A.1 — the rest of the object-graph toolkit

Ship: split modes, organize/insert/duplicate, crop/resize, N-up/flip/alternate/mix, page numbers,
watermark, headers/footers, Bates, compression lossless, repair, metadata strip, attachments,
bookmarks, page labels, CSV/Markdown/HTML→PDF, image→PDF for owned codecs, multi-output + ZIP,
batch queue, unlock/protect/clear-permissions (crypto-enabled chunk).

**Shipped 2026-10-27:** split every N pages, split in half, split by target size, split by
bookmarks, extract pages, per-output metadata and downloads, the owned deterministic ZIP bundle,
and the matching protocol/browser coverage. The browser check runs every split mode online and
after an offline reload, reads the actual PDF and ZIP bytes, and checks the per-part over-target
flag for a target no single page can meet. The same slice adds duplicate pages and insert-blank
pages to the Organize pages workspace; duplicate and blank legs run online and offline too.

**Shipped 2026-10-28:** page numbers, centered text watermark, headers/footers with
`{page}`/`{pages}` placeholders, Bates numbering, lossless compression, attachment extraction,
round-robin page mixing, visual-edge cropping, display-relative flipping, page labels, metadata
removal, page resizing, N-up placement, file embedding, and attachment removal. All fifteen operations are exposed through the CLI and the browser and
use the verified full-rewrite path (extraction is read-only). The browser check now drives twenty-two
cards online and after an offline reload, reads the stamped marker strings, attachment bytes, the
written `/PageLabels` tree, the A4 `/MediaBox` from the resize output, the Form XObjects from the
N-up output, and the absence of `/Info`,
checks the ZIP local header, measures the compressed output's page count and size, and interleaves
the three-page report with the one-page fixture to require Pages `4 → 4`. Crop, flip, page labels,
resize, and metadata removal run on the three-page report and require Pages `3 → 3`; the label leg
requires `report-labeled.pdf` with `/PageLabels` and the `(A-)` prefix visible in the downloaded
bytes, the resize leg requires `report-resized.pdf` with `595.28`/`841.89` in the bytes, the N-up
leg requires `report-n-up.pdf` with Pages `3 → 2` and `/Subtype /Form` plus `/Type /XObject` in the
bytes, and the
metadata leg requires `metadata-metadata-removed.pdf` whose bytes contain neither `/Info` nor the
removed title, author, or producer. Overlay placement follows inherited `/MediaBox`, `/Resources`,
and `/Rotate` values; compression preserves trailer-level `/Info`; attachment names are flattened
and deduplicated, and attachment outputs never offer a PDF preview. Mixing reuses merge
reconciliation and materialises page labels per output page when any input carries `/PageLabels`.
Cropping measures margins in display orientation, replaces `/MediaBox` and `/CropBox`, and drops
print boxes outside the new rectangle; flipping wraps painted content in a display-relative mirror
and leaves annotations and form fields unmirrored, as documented. Page labelling rewrites the label
tree so each output page has an explicit entry, advancing the selected pages' numbers without
letting a stale range leak across unselected pages. Resizing wraps painted content in a
display-relative scale/centre matrix, normalizes rotation on selected pages, and copies unselected
pages untouched. N-up turns each page's copied content stream into a Form XObject with the page's
own resources and a rotation-baked matrix, then paints those forms into grid cells on a sheet that
takes the first page's displayed size; page-label keys are remapped many-to-one and deduplicated.
Metadata removal is the same rewrite with `carry_metadata: false`: no `/Info` and
no XMP packet are written, while every visible and structural feature still comes through.
Repair opens a damaged input through the strongest strategy that succeeds — the ordinary parser,
then the last intact revision, then a rebuilt object index — and rewrites every recovered page
through the same writer and structural gate. The recovered page count is the gate's expected
count, so a repair that loses a page fails rather than shipping, and the strategy appears as a
Recovery row (`strict`, `rolled-back revision`, or `reconstructed index`). The browser leg damages
the real fixture's `xref` keyword, requires Pages `3 → 3` and `reconstructed index`, and proves
the damaged marker is absent from the downloaded bytes, online and after an offline reload.

**Remaining in 1A.1:** image/tiled watermark,
CSV/Markdown/HTML→PDF, image→PDF, batch queue, and the crypto-enabled security tools. The
incremental writer for page edits remains deferred; these page operations use the verified
full-rewrite path.

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
3. **No page renderer in the tools chunk unless a tool needs it.** The current twenty-two tools are
   object-graph operations; keeping the renderer out is why the chunk is 248 KB brotli instead of
   the multi-megabyte viewer. A raster tool gets its own lazy chunk.
4. **No RNG in the inert tools chunk.** `selis-crypto`/`selis-pdf-cos` expose an optional `rng`
   feature. Native and viewer builds enable it; the object-graph tools disable it so `getrandom`'s
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
- [x] Twenty-two live operations with 1-based page input.
- [x] Verification rows and download.
- [x] Browser-native preview for PDF inputs and outputs; non-PDF attachments download without a preview.
- [x] Lazy tools chunk copied by `place-assets`; build fails when missing.
- [x] Import-free check in `place-assets` and the loader.
- [x] Size budget + baseline.
- [x] Web unit tests for protocol/input parsing.
- [x] Browser check drives all twenty-two cards end-to-end, downloads every output, asserts the
      network log, reads the stamped marker strings and written label tree from downloaded PDF
      bytes, proves the A4 page box landed in the resize output, proves the N-up sheets carry
      Form XObjects, proves removed metadata is absent
      from the Blob, proves an embedded file's name tree and name are present after attach, proves
      they are absent after detach, and checks extracted attachment bytes plus the ZIP header.
      `xtask/browser/tools-page` passed in Edge with 22 cards online and offline, 0 foreign
      resources, `data-no-upload="verified"` before results appeared, and `%PDF-` downloads.
- [x] Tools page precache/offline promotion. `/tools.html`, `tools.css`, the tools module graph,
      the registrar, and `selis_pdf_wasm_tools.wasm` are in the install set; `CACHE_VERSION` is 2.
      The `tools-page` browser check now runs every leg online, cuts the origin, reloads the page,
      and runs every leg again with zero foreign resources. The document cache exclusion is
      unchanged.
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
