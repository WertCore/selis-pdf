# 35 — Parity checklist: iLovePDF and ihatepdf.cv

**Status:** living checklist, opened 31 October 2026. Companion to
`34-TOOLS-SITE-ROADMAP.md`; this file is the one that gets ticked.
**Rule:** a box is ticked only when the slice has shipped through the owned engine, has a
CLI + browser path, has unit/browser evidence, and is documented. A card, a mock, or an
unverified claim never ticks a box.

## How to use this file

- Work top to bottom inside a phase; later phases stay blocked until their exit gate passes.
- Each slice is finished in one change set: engine + CLI + WASM + web + harness + docs.
- Mark the box in the same commit that ships the slice, and add the commit hash in the
  trailing note.
- When a slice is too large for one change set, split it into named sub-boxes here first.
- Do not tick a box for a feature that only works on a fixture or only in one surface.

### Verification gate for every slice

- [x] `cargo fmt --all -- --check`
- [x] Targeted `cargo test` (engine crate, WASM crate, xtask verdicts)
- [x] `pnpm.cmd --filter @selis/web-host test`
- [x] `cargo xtask size-check --strict` (tools chunk under 300 000 B brotli)
- [x] `pnpm.cmd --filter @selis/web-host build`
- [x] `cargo xtask browser-check --only tools-page --strict` (online + offline legs)
- [x] No document bytes in any request; no PDF in Cache Storage or IndexedDB
- [x] Docs updated (`34-TOOLS-SITE-ROADMAP.md`, `11A-PHASE-1A-tools.md`, this file)
- [ ] Commit, fast-forward `main`, push `feature/tools-site` and `main`

---

## Phase 1A.1 — finish the object-graph toolkit

Exit gate: every object-graph tool has CLI + browser parity, verification, a browser leg,
and a size budget; no raster feature is claimed.

### Already shipped (checked for context)

- [x] Merge, mix pages, split by range/every N/half/size/bookmarks, extract pages
- [x] Rotate, crop, flip, resize, N-up, delete, organize (reorder/duplicate/blank)
- [x] Page numbers, text watermark (centered + tiled), header/footer, Bates numbering
- [x] Compress (lossless), repair, metadata edit, metadata strip, page labels
- [x] Extract attachments, embed files, remove attachments
- [x] Multi-output downloads + deterministic ZIP bundle
- [x] Browser-native preview, verification rows, no-upload assertion
- [x] Offline PWA precache for the tools page and tools chunk

### Remaining object-graph work

- [x] **Image → PDF (PNG/JPEG)** — one page per image; page size fit/A4/Letter with margin;
      decode through `selis-image`, embed a Flate-compressed DeviceRGB XObject; browser accepts
      image files, not only PDFs. Acceptance: 2 images → 2-page PDF, `/Subtype /Image` and
      `/Filter /FlateDecode` in the downloaded bytes, online and offline.
      *Shipped:* engine `selis-pdf-tools::image_to_pdf` + `embed_image_rgba_flate`, WASM op 23,
      CLI `img2pdf` shim, web card with PNG/JPEG validation. Evidence: 97 cos / 84 tools /
      27 wasm-tools tests, 239 web tests, browser leg reports 2 pages with all three markers
      found twice each (once per image) and zero foreign requests, online and offline.
      Tools chunk 278 579 B brotli of 300 000.
- [x] **Markdown → PDF** — expose `selis-pdf-convert::markdown_to_pdf` in the browser through a
      lazy convert chunk; document the supported subset; no HTML engine claim.
      Acceptance: headings/lists/code/links render, page count asserted, CLI and browser agree.
      *Shipped:* `selis-pdf-wasm-convert` speaks a pointer ABI (`selis_convert_alloc`,
      `selis_convert_run`, `selis_convert_last_error`) and the page loads it lazily from
      `/wasm/selis_pdf_wasm_convert.wasm`. The chunk is **import-free** (0 imports, 62 712 B
      brotli of 600 000) after dropping `selis-pdf-cos/rng`, which had been dragging
      `getrandom`'s wasm-bindgen shims in and making the module uninstantiable in a browser.
      `place-assets` now copies it and fails the build if it is missing or has any import.
      Evidence: browser leg renders a Markdown heading to a 1-page PDF whose real Blob carries
      `/BaseFont /Helvetica-Bold` and the heading text, online and offline with zero foreign
      requests; the offline leg is served from Cache Storage.
- [x] **HTML → PDF** — the documented HTML subset, same convert chunk; refuse unsupported
      constructs rather than silently dropping them where the subset promises support.
      *Shipped:* `html_to_pdf` in the same chunk, selected by the format option or the file
      extension. The documented subset is in `selis-pdf-convert`'s crate docs: headings,
      paragraphs, lists, `pre`, blockquote, bold/italic/code, `a href`, block containers;
      `<script>`/`<style>` dropped, unknown tags unwrap to text. No general HTML engine claim.
- [x] **CSV → PDF** — owned table renderer: delimiter, header row, column widths, page breaks;
      no spreadsheet engine claim. Acceptance: a 100-row CSV paginates with repeated headers.
      *Shipped:* `csv_to_pdf` in the same chunk. Splits on commas with quoted-field support,
      sizes columns evenly, repeats the header row on every page, and truncates a cell that
      overflows its column. This is a table dump, not a spreadsheet renderer, and the UI says so.
- [x] **Image watermark** — reuse the image→PDF embedding primitive; position, scale, opacity,
      tiled option, 1-based selection. Acceptance: `/Subtype /Image` inside the page resources
      and the expected placement count.
      *Shipped:* `selis-pdf-tools::add_image_watermark` decodes through `selis-image`, embeds
      one Flate-compressed DeviceRGB XObject for the whole document, and draws it with a
      placement matrix per stamp (opacity via a local `ExtGState`, display-space offsets that
      follow page rotation). WASM op 25 takes the PDF and the image as two wire inputs; the web
      card classifies the pair by header, so pick order does not matter. Evidence: engine tests
      assert one XObject and one/≥6 placements for single/tiled, the wasm dispatch test asserts
      the image object, and the browser leg reports the image XObject plus 16 tiled placements
      in the downloaded bytes, online and offline with zero foreign requests. Tools chunk
      284 092 B brotli of 300 000.
- [ ] **Bookmarks / outline editing** — add, rename, retarget, delete outline entries; keep the
      existing reader/writer; verification counts entries in/out. No drag UI required.
- [x] **Drag-to-reorder** — pointer/keyboard reordering for the file list and organize order,
      with the existing buttons kept as the accessible path. UI enhancement, not correctness.
- [x] **General PDF → ZIP** — a file-picker ZIP over any produced outputs (not only split parts
      and attachments), deterministic entry order and timestamps.
      *Shipped:* WASM op 24 `Zip` bundles the selected files by their user-visible names through
      `selis_pdf_tools::bundle_zip`. This slice also had to repair the tools chunk, which had not
      compiled since it landed: the `Zip` arm referenced an out-of-scope `files` binding and the
      private `verify_report` module. The chunk now collects the parsed inputs and uses the
      re-exported `Verification`.
- [ ] **PDF → metadata JSON** — read-only export of Info/XMP and structural facts; no mutation.
- [ ] **Batch queue** — multi-file queue with per-file isolation, per-file results, one failure
      never cancels another, and no cross-file document access. Acceptance: 3 files in, 3
      independent verified results, one deliberate failure leaves the other two intact.
- [ ] **Unlock / remove password** — crypto-enabled lazy chunk; AESV3/R6; never silently claim
      an unencrypted output when the password was wrong. Human review required.
- [ ] **Protect with password** — AESV3/R6, separate user/owner passwords, permission copy
      reviewed; keys stay on device. Human review required.
- [ ] **Clear permissions** — owner-password override only; report what changed. Human review.
- [ ] **Permissions / encrypt** — same surface as protect; no "remove all restrictions" claim
      without the owner password. Human review required.

### Phase 1A.1 exit checks

- [ ] Every remaining box above is either ticked or explicitly deferred with a reason.
- [ ] Tools chunk still under 300 000 B brotli with every new card lazy-loaded.
- [ ] Browser check drives every card online and after an offline reload.
- [ ] `34-TOOLS-SITE-ROADMAP.md` "Remaining in 1A.1" is empty or lists only named deferrals.

---

## Phase 2 — raster and OCR

Exit gate: render hash determinism, pixel/text verification for every output, OCR confidence
and extraction tests, and a lazy raster chunk under budget. No raster claim before this gate.

### Rendering and image outputs

- [ ] **PDF → JPG** — render every page, quality control, ZIP for multi-page.
- [ ] **PDF → PNG** — same, lossless.
- [ ] **PDF → WebP** — needs an owned codec under the dependency policy.
- [ ] **PDF → TIFF** — multi-page container; define page order and DPI.
- [ ] **PDF → images ZIP** — deterministic bundle of the above.
- [ ] **Lossy compression with a target** — quality/size search, measured result shown,
      never labelled lossless.
- [ ] **Invert colours** — render-based; verify pixel inversion on a known fixture.
- [ ] **Flatten annotations** — render appearance streams; never claim flatten without a
      render check.
- [ ] **Flatten forms** — form appearance generation is its own correctness surface.
- [ ] **Compare two PDFs** — pixel and text diff with render determinism.
- [ ] **Fingerprinting report** — producer/creator, IDs, timestamps, fonts, image hashes,
      incremental revisions; structural part may ship before the raster part.
- [ ] **Privacy risk scanner** — revisions, metadata/author trails, hidden OC layers, embedded
      files, JavaScript, external references, text under images; report only, no auto-fix.

### OCR and scanning

- [ ] **Scan to PDF (camera)** — feature-scoped camera permission, edge detection, deskew.
- [ ] **OCR** — owned OCR path, lazy language packs, confidence reported.
- [ ] **Searchable PDF export** — OCR text layer + original image; verify extracted text.
- [ ] **Auto-redact PII** — detection may be text/regex; committing redaction is removal with
      a render preview, never an overlay.
- [ ] **Manual redaction** — rectangle picker on rendered pages + removal verification pass.
- [ ] **Handwriting / thumbmark** — ink capture to vector or image overlay, preview, flatten.

---

## Phase 3 — text and layout

Exit gate: a text-extraction ladder with honest confidence; no layout claim above measured
fidelity.

- [ ] **PDF → text** — extraction quality report and page separators.
- [ ] **PDF → HTML** — positioned spans first, reflow later.
- [ ] **PDF → EPUB** — reflow from extracted structure.
- [ ] **Split by text** — boundary rule shown before committing.
- [ ] **Table extraction** — confidence display.
- [ ] **PDF → Word** — text + layout reconstruction; label fidelity honestly.
- [ ] **PDF → PowerPoint** — page images + text boxes; fidelity caveat.
- [ ] **PDF → Excel** — table reconstruction with confidence.
- [ ] **Word → PDF** — OOXML layout is product-sized; Phase 5 at the earliest.
- [ ] **PowerPoint → PDF** — same.
- [ ] **Excel → PDF** — same; CSV → PDF ships earlier as a table renderer.
- [ ] **LaTeX → PDF** — TeX layout subset or an explicit unsupported refusal; never shell out.
- [ ] **SVG → PDF** — documented SVG subset; do not claim full SVG.
- [ ] **EPUB → PDF** — EPUB layout; large scope.
- [ ] **HTML/CSS/Markdown refinements** — print media, pagination, tables, links.

---

## Phase 4 — forms, signatures, security UI

Exit gate: keys never leave the device; signature invalidation is disclosed before writing;
form values survive a round trip. Human-owned correctness.

- [ ] **Fill forms** — AcroForm field editing; flatten is a separate raster/forms task.
- [ ] **Sign PDF** — certificate/signature UI, local keys, invalidation warnings.
- [ ] **Certificate validation** — trust store and revocation policy; no validity claim
      without a chain.
- [ ] **Permissions / encryption UI** — completes the 1A.1 crypto tools with a reviewed flow.
- [ ] **Workflow (approve / route)** — local workflow state first; sharing is P2P later.

---

## Phase 5 — editing (deferred)

Exit gate: content-rewriting verification (render + text) replaces the structural-only
standard; no redaction overlay ships.

- [ ] **Edit text** — reflow with content verification.
- [ ] **Edit images and vector content** — same gate.
- [ ] **Edit pages** — insert/delete/reorder with content verification.
- [ ] **Compare-and-merge** — after the text and raster ladders exist.
- [ ] **Redaction commit UI** — removal, never a black rectangle.

---

## Phase 8 — AI and collaboration

Exit gate: an explicit model download, no default cloud path, and a network trace proving
document bytes stay local.

- [ ] **Chat with PDF** — on-device model behind explicit download.
- [ ] **Summarize** — same.
- [ ] **PDF → audio** — local TTS, explicit model download.
- [ ] **Audio → PDF** — transcript layout, no fake transcription.
- [ ] **P2P share** — WebRTC data channels, no relay that sees document bytes.
- [ ] **Collaborative whiteboard** — CRDT surface; PDF export through the writer.
- [ ] **GST invoice / filing / POS billing** — vertical templates, separate product track.

---

## Phase 9 — compliance

- [ ] **PDF/A** — ISO 19005; Phase 9 after spec procurement.
- [ ] **PDF/UA** — same; spec-backed conformance, no guessed validator.

---

## Launch gates (not feature parity, but release blockers)

- [ ] **SEO landing page per tool** — one route per operation; privacy claim in the meta
      description.
- [ ] **Pricing page** — plans and entitlements, no document upload.
- [ ] **Chrome Web Store listing** — extension package, screenshots, privacy policy.
- [ ] **CI green on `feature/tools-site`** — the `RUSTSEC-2026-0387` lint/supply-chain gate is
      human-owned; do not edit `supply-chain/audits.toml` to paper over it.
- [ ] **Full lint pass** — pre-existing `selis-io`/`selis_error` `result_large_err` warnings
      resolved or explicitly allowed with a note.
- [ ] **Release size review** — engine + tools chunk budgets measured on the release profile.
- [ ] **Privacy statement verified** — no upload endpoint, no telemetry body with document
      bytes, Cache Storage excludes PDFs, extension permissions reviewed.

---

## Next slices, in order

1. Bookmarks / outline editing.
2. Batch queue.
3. Metadata → JSON export.
4. Crypto-enabled security chunk (unlock, protect, clear permissions) — human review.
5. Phase 2 raster chunk, starting with PDF → PNG/JPG.
