# 37 — Aspose.PDF parity matrix

**Status:** living comparison, opened 8 November 2026. Companion to
`34-TOOLS-SITE-ROADMAP.md` and `35-PARITY-CHECKLIST.md` — those two files are the source
of truth for Selis status; this file only maps that status onto a different competitor's
capability surface.

## What this document is, and what it is not

The earlier parity files (`34`, `35`) measure Selis against **iLovePDF** and
**ihatepdf.cv** — consumer tool sites with the same local-first positioning. This file
measures Selis against **Aspose.PDF**, which is a different kind of product, and the
comparison axis is deliberately narrow: **functionality**. It asks, feature by feature,
"Aspose.PDF does X — where does Selis stand on X?" It is not a licensing, deployment, or
go-to-market comparison, and it does not claim the two products serve the same buyer.

They do not. The honest framing up front:

- **Aspose.PDF** is a mature commercial **developer SDK** (and a set of hosted REST/app
  endpoints) for .NET, Java, C++, Python, and Node. You embed it in your own server or
  desktop application, call its API, and it generates, converts, and manipulates PDF on
  *your* machine or Aspose's cloud. Its reach is enormous: it is one of the most complete
  PDF capability surfaces on the market, with two decades of format coverage. It is not a
  product an end user installs; it is a component a developer builds against, under a
  per-developer/per-deployment commercial licence.
- **Selis** is a **local-first, own-Rust-engine, no-upload PDF product** delivered as a
  web app, a browser extension, a CLI, and (later) a desktop/mobile shell. Document bytes
  never leave the device: the browser runs the owned `selis-pdf-tools` engine compiled to
  WASM with **zero network imports**, and the CLI runs the same crate natively
  (`34-TOOLS-SITE-ROADMAP.md §1`; ADR-P0016). A future **Selis SDK** (C ABI + WASM + five
  language wrappers) is planned at Phase 9 (`26-PRODUCT-SURFACE.md §2`), which is the point
  at which a *direct* SDK-to-SDK comparison with Aspose becomes fair; today it is not, and
  the rows below that touch the SDK say so.

So why compare them at all? Because Aspose.PDF is the most complete published inventory of
"things a PDF library can do," and mapping Selis onto it is the cleanest way to see the
true size of the remaining surface — larger than the consumer-tool matrix in `34` shows,
because Aspose covers prepress, tagging, 3D, barcodes, and LaTeX/MathML that iLovePDF never
attempts. Where Selis differs by **philosophy** — own engine over glue code, provable
per-feature correctness over "returns Ok", local processing over a server SDK, no upload —
that is recorded as positioning, not merely as a gap.

One naming note worth stating because it is not a coincidence: Selis deliberately adopts
**Aspose's brand architecture** — a format-neutral master brand with format-named product
lines (`Aspose.PDF`/`Aspose.Words`/`Aspose.Cells` → `Selis PDF`/`Selis Words`/`Selis
Sheets`), recorded in ADR-P0034. The structural homage is intentional; the engine,
licensing, and deployment model are the opposite.

---

## Positioning & monetization — why this matrix is the revenue roadmap

This file is not a feature wishlist. **Aspose parity is a primary product goal**, because the
Selis SDK — the thing this matrix measures Selis against — is the business. The monetization
model (`27-COST-AND-LICENSING.md §4A`; proposed ADR-P0046) is:

- **Free for individuals, forever** — both the desktop app and the web/tools app, with no
  account, no watermark, and no daily limit. This is affordable because the work runs on the
  user's device (ADR-P0016), so an individual user costs nothing to serve.
- **Paid for commercial / enterprise use** — the **SDK** (C ABI + WASM + language wrappers),
  the Server/API, fleet deployment, and the enterprise/compliance add-ons. This is the
  Aspose/iText per-developer + per-deployment commercial model, inverted on the consumer side:
  the incumbents charge individuals *and* enterprises; Selis charges only enterprises.

The consequence for this document: **Selis competes directly with Aspose.PDF, Foxit, and Adobe
Acrobat**, and on the SDK front Aspose.PDF is the reference capability surface to match and
undercut. So every 🟡/📋/⛔ row below is not merely a gap in a consumer tool site — it is a line
on the **revenue roadmap**, because the SDK cannot be sold into an Aspose-shaped procurement
until the engine reaches SDK-grade completeness on the capabilities that buyer expects. Read the
gaps that way: closing them is what makes the paid product worth buying, and the §29 "Priority
gaps to close next" list is ordered with that leverage in mind.

The usual caveat still stands (and is sharpened by the model): a *direct* SDK-to-SDK comparison
with Aspose is only fully fair at Phase 9, when the Selis SDK ships. Until then, parity is
measured through the engine, CLI, and browser surfaces that are live today, and rows that depend
on the not-yet-shipped SDK say so.

---

## Status legend

| Marker | Meaning |
|---|---|
| ✅ Shipped | Implemented in the owned engine, exercised through CLI **and** browser (or CLI where the row is explicitly CLI-first), with unit/browser evidence. Equivalent to a ticked box in `35-PARITY-CHECKLIST.md`, **or** an unticked row whose note states it is implemented + tested (engine/CLI) with only the real-Edge `browser-check`/`size-check` harness outstanding. |
| 🟡 Partial | A real, honest subset ships and the product says so; named constructs are refused rather than silently mangled. |
| 📋 Planned | On the roadmap with a phase/row citation; not yet implemented, or implemented only as a stub. |
| ⛔ Out of scope | Deliberately not built, with the reason. Not the same as "not yet." |

A note on the ✅ threshold, per the task's grounding rule: several of the most recent rows
in `35-PARITY-CHECKLIST.md` are implemented + tested at the engine and CLI level on the
tools branch but their checkboxes are **not** ticked, because the only thing outstanding is
the real-Edge `browser-check` leg and the `size-check`, which need a WASM/Edge build
harness that is absent in the authoring environment. Per the task instruction ("treat a row
as shipped if its note says it's implemented+tested"), those are marked ✅ here with the
open-gate caveat named in the Notes column. The affected rows are: **fingerprint report**,
**privacy risk scanner**, **invert colours**, **flatten annotations** (CLI render-check
done; web card deferred), **compare two PDFs** (CLI done; web card deferred), **HTML
page-break control**, and **reflow links + tables**.

---

## 1. Document generation / creation

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Create PDF from a document-object model | Build pages, paragraphs, tables, images, graphs programmatically via a DOM | 🟡 Partial | Selis generates PDFs from *source formats* (Markdown/HTML-subset/CSV + Office/LaTeX/SVG/EPUB), through `selis-pdf-convert`'s shared block layout engine. There is no public page-composition DOM API today; authoring from scratch is Phase 5 editing + the Phase 9 SDK. `34 §3.3`. |
| Programmatic text/paragraph/table/image insertion | Full fluent builder API | 🟡 Partial | The block layout engine already emits headings, paragraph runs, list levels, real tables with header rows and per-column wrapping, and embedded images — but only as the output of a converter, not an exposed builder. |
| Generate from HTML/Markdown | HTML-to-PDF and Markdown-to-PDF | ✅ Shipped | Owned HTML subset + Markdown → PDF via the lazy, import-free convert chunk (0 imports, 62 712 B brotli). Documented subset; `<script>`/`<style>` dropped; unknown tags unwrap to text. No general HTML engine claim. `35` Phase 1A.1. |
| Generate from templates / mail-merge | Template + data-source merge | ⛔ Out of scope | Not a PDF-engine primitive; verticals like GST invoicing are an explicit separate product track (`34 §3.6`), not core. |

---

## 2. Conversion

Aspose.PDF's conversion surface is its headline strength. Selis converts through its own
`selis-pdf-convert` crate and renders through its own raster/text pipelines — never a
headless Office suite, never a shelled-out TeX distribution (ADR-P0008; `01 §3`).

### 2.1 Into PDF

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Images → PDF (PNG/JPEG) | Any raster → PDF | ✅ Shipped | PNG + baseline/progressive JPEG decoded on-device via `selis-image`, embedded as Flate `DeviceRGB` XObject, one image per page. `35` Phase 1A.1. |
| WebP → PDF | Supported | 📋 Planned | Reader decodes only the VP8L profile Selis itself writes; it refuses by name every construct libwebp emits, so an inbound card must widen the reader first. Codec no longer the blocker. `34 §3.3`. |
| TIFF (multi-page) → PDF | Supported | 📋 Planned | `selis-image::tiff` already decodes the baseline profile; a card must prove every decoded page lands in the output. Codec-unblocked, not shipped. `34 §3.3`. |
| HEIC → PDF | Supported | 📋 Planned | Needs an owned HEIF/HEVC decoder (none in tree) plus licence/platform review; WebP coder not reusable. `34 §3.3`. |
| DOCX → PDF | High-fidelity Word rendering | 🟡 Partial | `selis-pdf-convert::docx`: headings, bold/italic runs, list levels, real tables. **Not** carried: page setup, alignment, spacing, colour, fonts, images, text boxes, footnotes, fields, tracked changes. Structure, not laid-out Word, and the UI says so. `35` Phase 3. |
| XLSX → PDF | Spreadsheet rendering | 🟡 Partial | `selis-pdf-convert::xlsx`: one page per sheet, used range as a table, shared/inline strings, formula cached values, booleans. **Not** carried: cell formats (dates print as serials), formula evaluation, merged cells, charts, column widths. `35` Phase 3. |
| PPTX → PDF | Slide rendering | 🟡 Partial | `selis-pdf-convert::pptx`: one page per slide, title/subtitle placeholders, bold/italic runs, bulleted/numbered bodies, DrawingML tables. **Not** carried: slide size, theme, fonts, images, charts, SmartArt, speaker notes. `35` Phase 3. |
| HTML → PDF | Full HTML/CSS layout | 🟡 Partial | Owned HTML subset with pagination, GFM/HTML tables, links, and `page-break-before/after` + `break-before/after` CSS. **Not** carried: `@media print`, external stylesheets, a full CSS cascade. `35` Phase 3 (page-break row implemented+tested; browser-check open). |
| Markdown → PDF | Supported | ✅ Shipped | Documented subset through the convert chunk. `35` Phase 1A.1. |
| CSV → PDF | Supported | ✅ Shipped | Owned table dump: quoted fields, even columns, repeated header row, page breaks. Not a spreadsheet renderer, and the UI says so. `35` Phase 1A.1. |
| EPUB → PDF | Supported | 🟡 Partial | `selis-pdf-convert::epub`: spine XHTML through the HTML subset, paginated. No CSS, fonts, images, SVG, math, or ToC. `35` Phase 3. |
| LaTeX / TeX → PDF | TeX typesetting | 🟡 Partial | `selis-pdf-convert::latex`: sections, lists, verbatim, styled runs; every other command dropped and the docs say so. No math, `tabular`, figures, or package macros. Never shells out to a TeX distribution. `35` Phase 3. |
| SVG → PDF | Vector rendering | 🟡 Partial | `selis-pdf-convert::svg`: documented subset (shapes, paths M/L/H/V/Z/C/S/Q/T, text in standard-14, transforms, dashes, fill-rule). Arcs (`A`), gradients, patterns, nested canvases **refused** by name. `35` Phase 3. |
| XPS / PostScript / PCL → PDF | Supported | 📋 Planned | `selis-pdf-convert` is specced to carry PS/PCL **import** (`01 §3`), but no converter ships. XPS is not on the roadmap. |
| XML (Aspose XML schema) → PDF | Proprietary XML → PDF | ⛔ Out of scope | Vendor-specific schema; no analogue, not a standard. |

### 2.2 Out of PDF

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| PDF → PNG | Rasterize each page | ✅ Shipped | Owned renderer + owned PNG encoder + owned zlib, read-back pixel comparison, DPI 1–600, `/Rotate` honoured; multi-page as deterministic ZIP. `35` Phase 2. |
| PDF → JPG | Supported | ✅ Shipped | Same raster chunk; owned baseline-sequential DCT encoder; verified by decode-and-compare with honest lossy bounds. `35` Phase 2. |
| PDF → TIFF (multi-page) | Supported | ✅ Shipped | Owned baseline TIFF 6.0 writer+reader, one multi-page container, sample-for-sample read-back, no quality knob (lossless). `35` Phase 2. |
| PDF → BMP | Supported | ⛔ Out of scope | Legacy uncompressed raster; PNG/JPG/TIFF/WebP cover the need. Not on the roadmap. |
| PDF → WebP | Supported | ✅ Shipped | Owned VP8L coder (no libwebp), per-channel prefix codes, subtract-green + per-block predictor, alpha preserved; verified against Chromium's own decoder vs the PNG of the same render. `35` Phase 2. |
| PDF → SVG | Vector export | 📋 Planned | Selis exports **positioned HTML** (text as positioned spans) but not vector SVG; the display-list IR could retarget to SVG (`01 §8`) but no exporter ships. |
| PDF → EMF / XPS / PS / PCL | Metafile / print-stream export | 📋 Planned | The display list is explicitly designed to retarget to PS/PCL for print (`01 §8`), but no exporter ships; EMF/XPS are not on the roadmap. |
| PDF → DOCX | Word reconstruction | ✅ Shipped | `selis-pdf-text::docx` frame facsimile: every recovered line an absolutely positioned frame on a page-sized section, words land where the PDF drew them, file opens in Word. Position + words only — no images, fills, vectors, exact faces, or tables-as-tables. `35` Phase 3. |
| PDF → XLSX | Excel reconstruction | ✅ Shipped | `selis-pdf-text::xlsx`: one worksheet per reconstructed table + a Report sheet with per-table confidence. `35` Phase 3. |
| PDF → PPTX | PowerPoint reconstruction | ✅ Shipped | `selis-pdf-text::pptx`: one slide per page, page rendered full-bleed behind editable text boxes. No SmartArt, charts, notes, theme. `35` Phase 3. |
| PDF → HTML | Supported | ✅ Shipped (positioned) | `selis-pdf-text::html`: one `<section>` per page on the page's own canvas, one `<p>` per recovered span at its measured box, emitter owns its strict reader + verifier (no external refs can pass). Reflow/colour/faces/images unclaimed. `35` Phase 3. |
| PDF → HTML (reflow / reading-order) | Reflowable HTML | ✅ Shipped | `selis-pdf-text::reflow`: reading-order article, headings by type size, links as `<a rel="nofollow noopener">`, detected tables as real `<table>`. Table renders at end of its page section, link *annotations* not read. Implemented+tested; browser-check open. `35` Phase 3. |
| PDF → EPUB | Supported | ✅ Shipped | `selis-pdf-text::epub`: real EPUB 3 package, one XHTML chapter per page, verified by re-opening the ZIP. No CSS/fonts/images/nav. `35` Phase 3. |
| PDF → Markdown | Supported | ✅ Shipped | A Markdown export format is part of the text ladder (the low-confidence marker rides it alongside text/JSON/HTML). `35` Phase 3. |
| PDF → plain text | Text extraction | ✅ Shipped | `selis-pdf-text::quality` + extraction ladder: page separators with true page numbers, per-page extraction-quality report with honest confidence verdict. `35` Phase 3. |
| PDF → PDF/A | Archival conversion | 📋 Planned | ISO 19005; Phase 9, after spec procurement. veraPDF is the CI oracle. `35` Phase 9; `00 §3` (SL-0.LEAD.05b). |

**Reading:** Selis's *outbound* conversion (PDF → images/Office/HTML/EPUB/text) is now
broad and shipped, each with a read-back verifier. Its *inbound* conversion (X → PDF) is a
collection of honest structural subsets, not fidelity renderers — the deliberate inverse of
Aspose, which prioritises inbound fidelity. The gap to Aspose here is fidelity, not surface.

---

## 3. Rendering / rasterization

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Render pages to raster at a chosen DPI | Yes, via the rendering engine | ✅ Shipped | `Session::render_page_rgba`, DPI 1–600, `/Rotate` honoured, composited over opaque white. `35` Phase 2. |
| Deterministic / reproducible rasterization | Not a documented guarantee | ✅ Shipped (and a differentiator) | Byte-identical output across platforms and runs is an *architectural law* (ADR-P0012), enforced by a cross-platform hash-equality CI job. This is a correctness guarantee Aspose does not publish. |
| Transparency groups, soft masks, blend modes, shadings 1–7 | Yes | 📋 Planned | Specced in `selis-raster` (`01 §3`) and gated by G2 (`00 §2`); the raster path has shipped PNG/JPG/TIFF/WebP but the full graphics-state render corpus (G2 "Render parity") is not yet passed. |
| Antialiasing control | Yes | 📋 Planned | Part of the `selis-raster` surface; not independently exposed yet. |

---

## 4. Text — extract, search, replace, add

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Extract text (with layout/position) | Full text absorber with position | ✅ Shipped | Extraction ladder with reading-order inference (structure-tree-first, geometric fallback), visible-region gate, Unicode recovery, per-span measured boxes (surfaced in the HTML export). `01 §3` (`selis-pdf-text`); `35` Phase 3. |
| Extraction confidence / quality report | Not a documented feature | ✅ Shipped (and a differentiator) | `selis-extract-quality/1` report: per-page char/line counts, low-confidence flag, `ok`/`low-confidence`/`empty` verdict. Honest confidence is itself the wedge (`26 §4`). |
| Search text | Yes | ✅ Shipped | Search index + selection geometry in `selis-pdf-text` (`01 §3`); split-by-text scans recovered lines case-insensitively. `35` Phase 3. |
| Add / overlay text | Yes | ✅ Shipped | Page numbers, text watermark, headers/footers, Bates numbering — standard-14 overlays with placement/opacity/selection. `35` Phase 1A.1. |
| Replace / edit existing text | Find-and-replace, text edit | 📋 Planned | Deliberately deferred to Phase 5 editing, which reconstructs paragraphs and re-lays-out rather than editing raw operators in place, with surfaced confidence (ADR-P0024). No Phase 1A tool claims to reflow text (`34 §1`). `35` Phase 5. |

---

## 5. Fonts — embed / subset / replace

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Parse embedded fonts (Type1/CFF/TrueType/OpenType/CID/Type3) | Yes | ✅ Shipped | `selis-font` owns all program types + encodings/cmaps/CMaps/ToUnicode (`01 §3`); required for the shipped extraction and raster paths. |
| Standard-14 metric-compatible substitution | Yes | ✅ Shipped | Metric-compatible substitution table in `selis-font`; fallback fonts (Liberation/TeX Gyre/Noto) procured (`00 §3`, SL-0.LEAD.07 ticked). |
| Subset / re-embed fonts | Font subsetting | 📋 Planned | `selis-font` is specced to own subsetting + re-embedding (needed for ADR-P0024 text edit and G3), but it is not yet a user-facing operation. `00 §2` (G3). |
| Replace / swap fonts | Yes | 📋 Planned | No font-replacement operation ships; depends on the subset/re-embed path above. |

---

## 6. Tables

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Generate tables in new PDFs | Rich table builder | 🟡 Partial | The convert layout engine emits real tables (header row, rules, per-column wrapping) from DOCX/HTML/Markdown/PPTX, but there is no standalone table-authoring API. `35` Phase 3. |
| Extract tables from PDFs | Table recognition | ✅ Shipped | `selis-pdf-text::table` infers columns from word starts, reports per-table confidence, writes self-describing CSV + a `selis-tables/1` report. No borders/merged/nested cells. `35` Phase 3. |

---

## 7. Images — add / extract / replace / compress

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Add images to a page | Yes | ✅ Shipped | Image → PDF and image watermark both embed a Flate `DeviceRGB` XObject via `selis-image`. `35` Phase 1A.1. |
| Extract images from a PDF | Yes | 🟡 Partial | The fingerprint report hashes and enumerates every image stream (width/height/filter); a dedicated "extract images to files" card is not listed. `35` fingerprint row. |
| Replace an image | Yes | 📋 Planned | No image-replace operation ships; it belongs to the Phase 5 edit model. |
| Compress / downsample images (lossy) | Yes | ✅ Shipped | Lossy compress with Less/Recommended/Extreme + target-size search: eligible image XObjects re-encoded as JPEG and box-downsampled, lossless floor, never-larger rule, measured result, "over target" flag. `35` Phase 2. |

---

## 8. Forms — AcroForm fill & flatten, XFA

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Read AcroForm fields | Yes | ✅ Shipped | `selis-pdf-form` walks `/AcroForm /Fields`, resolves inherited `/FT`/`/Ff`/`/DA`, associates widgets, reports per-field page. `35` Phase 4. |
| Fill AcroForm fields | Yes | ✅ Shipped | Writes `/V` (+`/I` for choices), sets `/NeedAppearances`, re-reads every value from the written bytes (`ReadBack` refusal). Encrypted/XFA/signed/read-only/multi-select/duplicate-name refused by type. `35` Phase 4. |
| Generate field appearance streams | Yes | ✅ Shipped (text) | `fill_form` with `synthesize_appearances` writes a real `/AP /N` Form XObject per text field (ISO 32000-1 §12.7.3.3 variable text: `/DA` font/size/colour via `/DR`, `/Q` quadding, multiline, auto-size), so the value shows without `/NeedAppearances`. Choice/listbox synthesis is the remaining slice; buttons already carry appearances. `35` Phase 4/flatten rows. |
| Flatten forms | Yes | ✅ Shipped (core+CLI) | `selis-pdf-tools::flatten_forms` bakes each widget's `/AP /N` into page content (§12.5.5), drops the widgets and `/AcroForm`. Combined with text appearance synthesis, a filled form flattens to permanent marks. CLI `selis flatten-forms`. Web card + a pixel render-check are the deferred half. `35` Phase 2 flatten rows. |
| Import/export FDF/XFDF | Yes | 📋 Planned | `selis-pdf-form` is specced to own FDF/XFDF import/export (`01 §3`); not yet exposed. |
| XFA forms | Read + render | 🟡 Partial / ⛔ authoring out of scope | Reading/static render of XFA is specced; **XFA authoring is explicitly never built** (`01 §13`; `26 §5`) — a deliberate decision, as XFA is a dead format. |

---

## 9. Digital signatures — sign / verify / timestamp / PAdES

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Sign with a digital certificate | Yes | ✅ Shipped | `selis-pdf-sign` appends an incremental revision with a detached CMS (`adbe.pkcs7.detached`) over `/ByteRange`. RSA PKCS#1 v1.5 + SHA-256 only, 2048-bit minimum; EC keys refused by name. Keys read in the page, never written to the PDF or a request. `35` Phase 4. |
| Visual signature stamp | Yes | ✅ Shipped | Typed or freehand visual stamp through the same chunk. `35` Phase 4. |
| Verify signatures | Yes | ✅ Shipped | Report-only card: each signature verified against the exact `/ByteRange` bytes; X.509 chain walked to caller-supplied PEM anchors (no built-in root store); names, signatures, validity windows, CA/path-length constraints checked. `35` Phase 4. |
| Timestamp (RFC 3161 / TSA) | Yes | 📋 Planned | Not claimed. `selis-crypto` owns RFC 3161 primitives (`01 §3`), but no timestamping ships; it is a long-lead item (TSA contract, SL-0.LEAD.08). `35` Phase 4 sign row. |
| PAdES / CAdES / LTV / MDP | Yes | 📋 Planned | Explicitly not claimed — "the result says what it is." Blocked on the signature trust-chain long lead (SL-0.LEAD.08), not on code. `35` Phase 4 sign row. |
| Revocation checking (OCSP/CRL) | Yes | 📋 Planned | Always reported "not checked — this build has no network," never guessed. A network-enabled surface would be a later, consent-gated addition. `35` certificate-validation row. |

Signatures are an `owner: HUMAN` safety-critical area (`01 §3`; `00 §0`); the four crypto
rows in `35` carried explicit human sign-off (2026-11-04) against pinned PDFium and qpdf.

---

## 10. Security — encrypt / decrypt, permissions, passwords

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Decrypt / remove password | Yes | ✅ Shipped | AESV3/R6 + RC4/AESV2 legacy + qpdf files open with the user password; a wrong password is refused and nothing written. Human-reviewed, interop-verified both directions. `35` Phase 1A.1. |
| Encrypt / protect with password | Yes | ✅ Shipped | AESV3/R6 only (ADR-P0019 — read legacy, write only modern), separate user/owner passwords, keys injected by the page so no RNG import reaches the guest. `35` Phase 1A.1. |
| Set / clear permissions | Yes | ✅ Shipped | Set permissions (owner password, no open password) and clear permissions (owner-password override, incremental revision, reports what was lifted). `35` Phase 1A.1. |
| Legacy RC4 *writing* | Yes | ⛔ Out of scope (by policy) | Selis reads RC4/AESV2 but **writes AESV3/R6 only** — "continuing to write RC4 in 2026 makes us complicit" (ADR-P0019). A deliberate correctness/security stance, not a gap. |

---

## 11. Annotations — types + flatten

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Create annotations (all subtypes) | Full annotation set | 📋 Planned | `selis-pdf-annot` owns all subtypes + appearance generation (`01 §3`), and annotations are written as spec-conformant PDF annotations, not a private sidecar (ADR-P0026) — but interactive annotation authoring is Phase 5 (`26 §1`). |
| Flatten annotations | Yes | ✅ Shipped | `selis-pdf-tools::flatten_annotations` bakes each non-widget normal appearance (§12.5.5 placement) into page content and drops it; widgets/hidden/no-appearance kept. CLI render-check proves the mark is in content, not an annotation. Engine+CLI shipped; web card deferred. `35` Phase 2. |
| Read annotation inventory | Yes | ✅ Shipped | The privacy scanner enumerates annotation-borne external actions (URI/Launch/GoToR/SubmitForm/ImportData) per page. `35` privacy row. |

---

## 12. Stamps / watermarks — text / image / page

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Text watermark | Yes | ✅ Shipped | Centered (diagonal/horizontal) + tiled staggered grid, opacity via `ExtGState`, 1-based selection. `35` Phase 1A.1. |
| Image watermark | Yes | ✅ Shipped | `selis-pdf-tools::add_image_watermark`: one Flate `DeviceRGB` XObject, per-stamp placement matrix, opacity, tiled option, rotation-aware offsets. `35` Phase 1A.1. |
| Page / PDF-as-stamp | Overlay one PDF onto another | 📋 Planned | N-up composes pages via Form XObjects, and merge/overlay primitives exist, but "stamp page A onto page B" is not a standalone card. |
| Page numbers / Bates | Yes | ✅ Shipped | Page numbers (corner/edge, formats, start) and Bates numbering (prefix/suffix/pad/placement). `35` Phase 1A.1. |

---

## 13. Bookmarks / outlines + TOC

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Read / add / edit / delete outline entries | Yes | ✅ Shipped | `selis-pdf-tools::set_outline` replaces the whole tree from a caller list (title/page/level) — one rewrite covers add/rename/retarget/reorder/delete; drops the old unreachable tree. `35` Phase 1A.1. |
| Generate a TOC page | Build a table-of-contents page | 📋 Planned | Outline *editing* ships; synthesising a rendered TOC page is not a card. (Split-by-bookmarks consumes the outline.) |

---

## 14. Page operations — merge / split / insert / delete / rotate / crop / move / N-up

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Merge | Yes | ✅ Shipped | Multi-file, ordered, with annotation/form/OCG/outline reconciliation. `35` Phase 1A.0. |
| Split (range / every-N / half / size / bookmarks / text) | Range + page-based split | ✅ Shipped | All six split modes ship, each verified; split-by-text and split-by-size exceed Aspose's typical split set. `35` Phase 1A.1 + Phase 3. |
| Extract pages | Yes | ✅ Shipped | One PDF, selected pages in document order. `35` Phase 1A.1. |
| Insert / delete / duplicate / blank pages | Yes | ✅ Shipped | Delete (refuses deleting all), duplicate (content+resources+annots travel), insert-blank (copies neighbour media box/rotation). `35` Phase 1A.1. |
| Rotate | Yes | ✅ Shipped | 90/180/270, all or 1-based selection. `35` Phase 1A.0. |
| Crop | Yes | ✅ Shipped | Visual-edge margins → new `/MediaBox`+`/CropBox`, clipped not rasterized. `35` Phase 1A.1. |
| Resize / scale to page size | Yes | ✅ Shipped | A4/A5/Letter/Legal/A3 presets or custom points, fit or stretch, normalized rotation. `35` Phase 1A.1. |
| Move / reorder pages | Yes | ✅ Shipped | Organize workspace (full permutation) + drag-to-reorder as an accessible enhancement. `35` Phase 1A.0/1A.1. |
| N-up (2/4/6/9) | Yes | ✅ Shipped | Grid via Form XObjects on the first page's displayed sheet, aspect ratio kept, outlines/labels/attachments/layers reconciled. `35` Phase 1A.1. |
| Flip / mirror | Yes | ✅ Shipped | Display-relative content mirror; boxes/rotation/annots unchanged. `35` Phase 1A.1. |
| Mix / interleave pages | Yes | ✅ Shipped | Round-robin interleave with per-output-page label materialisation. `35` Phase 1A.1. |

Page operations are the single area where Selis is at or beyond full parity.

---

## 15. Optimization — downsample / subset / linearize

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Lossless compress / object GC | Yes | ✅ Shipped | Reachable-object GC, Flate re-encode, byte-identical-object dedup, inherited page attributes, trailer `/Info` preserved; encrypted refused not silently unlocked. `35` Phase 1A.1. |
| Image downsample (lossy optimize) | Yes | ✅ Shipped | See §7 — level + target-size search with measured result. `35` Phase 2. |
| Font subsetting as an optimize step | Yes | 📋 Planned | `selis-pdf-optimize` is specced to orchestrate font subsetting (`01 §3`); not yet exposed (depends on §5). |
| Linearize / "fast web view" | Yes | 📋 Planned | `selis-pdf-optimize` owns a linearisation writer in the spec (`01 §3`); Selis *reads* linearised PDFs for fast first-paint (HttpRangeSource, `01 §5`) but does not yet *write* a linearised output. |

---

## 16. PDF/A, PDF/UA, PDF/X — validate + convert

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| PDF/A validate + convert | Yes | 📋 Planned | ISO 19005; Phase 9 after spec procurement. veraPDF + Isartor are the CI oracles; the free Matterhorn/veraPDF path defers the paid ISO purchase (`00 §3`, SL-0.LEAD.05b). `35` Phase 9. |
| PDF/UA validate + remediate | Yes | 📋 Planned | Phase 9; **but** the structure tree is first-class in the document model from Phase 1 (ADR-P0031), so Selis is better positioned to add this than a renderer built without tagging. `35` Phase 9. |
| PDF/X (prepress) | Yes | 📋 Planned (optional) | Prepress/PDF/X is a Phase 9 optional SKU (`26 §1`); no implementation. |

---

## 17. Redaction

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Redact regions / text | Yes | 📋 Planned | `selis-pdf-redact` exists in the engine spec as an `owner: HUMAN` crate; the browser needs page rendering for the rectangle picker + a removal-verification pass (Phase 5). `34 §3.2`. |
| Auto-redact PII | Yes | 📋 Planned | Detection may be text/regex, but committing must be *removal* with a render preview, never an overlay. Phase 5. `34 §3.2`. |
| Proof that content is gone | Not a documented guarantee | ✅ (design) / 📋 (ship) — differentiator | ADR-P0023: redaction re-opens the output and runs an independent extraction+render pass; if anything survives in the region, `save()` errors and produces no file. "It never draws a black box and calls it done" (`00 §5`, Rule 5). This proof obligation is the headline differentiator; the feature itself is Phase 5. |

---

## 18. OCR

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| OCR a scanned PDF / image | Yes (Aspose.OCR) | 📋 Planned | Owned OCR path via `selis-ocr` (Tesseract-WASM, ADR-P0018), lazy language packs, confidence reported; Phase 2 exit gate. `34 §3.5`. |
| Searchable PDF export (OCR layer) | Yes | 📋 Planned | OCR text layer + original image, verify extracted text. Phase 2. `34 §3.5`. |
| Scan-to-PDF (camera, deskew) | Mobile capture | 📋 Planned | Feature-scoped camera, edge detect, deskew; Android-first (iOS PWA is viewer/annotator per ADR-P0022). Phase 7. `34 §3.5`. |

---

## 19. Metadata / XMP

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Read Info + XMP | Yes | ✅ Shipped | `metadata_report`: header version, revision count, page count, first `/MediaBox`, every `/Info` key, the XMP packet (filter decoded or named), structural counts; one shared export shape for CLI and web. `35` Phase 1A.1. |
| Edit Info dictionary | Yes | ✅ Shipped | Title/Author/Subject/Keywords. `35` Phase 1A.0. |
| Strip all metadata | Yes | ✅ Shipped | Drops `/Info` + catalog XMP, keeps everything structural; warns PDF/A + signatures may invalidate. `35` Phase 1A.1. |
| Write / edit XMP packet | XMP read/write | 📋 Planned | XMP *reading* ships; XMP *writing* is explicitly not exposed yet. `34 §2` (edit-metadata row), `35` Phase 1A.0. |

---

## 20. Attachments / portfolios

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Embed files | Yes | ✅ Shipped | Flate `/EmbeddedFile` + `/Filespec` in `/Names/EmbeddedFiles`, same-named replaced, verified rewrite, 64-file/256 MiB limits. `35` Phase 1A.1. |
| Extract attachments | Yes | ✅ Shipped | Reads `/Names/EmbeddedFiles`, decodes the filter chain, downloads each (+ZIP for ≥2). `35` Phase 1A.1. |
| Remove attachments | Yes | ✅ Shipped | Drops the name tree and prunes superseded objects so bytes don't linger. `35` Phase 1A.1. |
| Portfolios / collections (packages) | Yes | ⛔ Out of scope | PDF portfolio UI is not on the roadmap; embedded-file primitives cover the data case. |

---

## 21. Optional-content layers (OCG)

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Read / reconcile OCG layers | Yes | 🟡 Partial | Layers are reconciled through merge/N-up/mix, and the privacy scanner flags hidden OC layers; `selis-pdf-doc` owns OCG/OCMD (`01 §3`). But there is no "toggle/add/delete layer" editing card. `35` merge/privacy rows. |

---

## 22. Links & actions

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Read link / action inventory | Yes | ✅ Shipped | Privacy scanner enumerates URI/Launch/GoToR/SubmitForm/ImportData in the open action + every page annotation. `35` privacy row. |
| Create / edit links & actions | Yes | 📋 Planned | No link-authoring card; belongs to Phase 5 editing. Internal destinations are written when outlines are set, but link-annotation authoring is not exposed. |
| Execute actions by default | Yes | ⛔ Out of scope (by policy) | Launch/URI/GoToR/SubmitForm and remote references are **off by default** and sandboxed when enabled (ADR-P0020, Rule 4). A deliberate safety stance. |

---

## 23. JavaScript

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Read / detect document JavaScript | Yes | ✅ Shipped (detection) | Privacy scanner flags JS via open action / name-tree scripts / catalog additional actions / XFA. `35` privacy row. |
| Add / edit / execute JavaScript | Yes | 🟡 Partial / by policy | `selis-pdf-js` is a budgeted, whitelisted Acrobat-JS *subset* interpreter, **off by default**, no filesystem/network, form-calculation only — not `app.launchURL` or `exportDataObject` (ADR-P0020). Authoring/injecting JS is not a goal. |

---

## 24. Barcodes

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Generate / recognize barcodes | Yes (Aspose.BarCode integration) | ⛔ Out of scope | Not a PDF-engine primitive and not on any Selis roadmap file. A separate product domain for Aspose too. |

---

## 25. LaTeX / MathML

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| LaTeX → PDF | TeX math typesetting | 🟡 Partial | Structure only (see §2.1) — **no math rendering**. Aspose typesets math; Selis drops math commands and says so. `35` Phase 3. |
| MathML rendering | Yes | ⛔ Out of scope | No MathML support anywhere in the plan. |

---

## 26. 3D / rich media

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| 3D annotations (U3D/PRC), rich media, embedded video/audio | Yes | ⛔ Out of scope | Not on any roadmap file. Rich-media/3D is outside the "document platform" surface Selis targets (`26`). |

---

## 27. Document comparison

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Compare two PDFs (text + visual) | Yes | ✅ Shipped (CLI) | `selis-pdf-tools::compare_pdfs`: renders both at the same DPI and compares sample-for-sample (deterministic), plus whole-document text comparison; `selis-compare/1` report with per-page `present_in`/`pixels_identical`/`diff_fraction`. Engine+CLI shipped; web card needs a two-input render+text chunk (named deferral). `35` Phase 2. |

---

## 28. Print

| Capability | What Aspose.PDF does | Selis status | Notes / plan ref |
|---|---|---|---|
| Print to a physical/virtual printer | Yes | 📋 Planned | Browser-native print is available through the viewer today; a native print pipeline (Win32/CUPS/NSPrintOperation) and a virtual printer are Phase 6 desktop work. `26 §1`; `01 §9`. |
| Print-stream (PS/PCL) generation | Yes | 📋 Planned | The display list is designed to retarget to PS/PCL (`01 §8`), and `selis-pdf-convert` is specced for PS/PCL import, but no print-stream exporter ships. |

---

## 29. Synthesis

### Where Selis wins

- **Privacy as a mechanism, not a promise.** Document bytes never leave the device: the
  browser engine has zero network imports, enforced at build time, and a CI test fails if
  any engine crate below the binding layer can even *name* a network API (ADR-P0016). Aspose
  runs on your server or its cloud; "local" depends on how *you* deploy it. For a user
  handling a contract, a medical record, or a court filing, Selis's guarantee is structural.
- **Provable per-feature correctness.** Every shipped Selis operation decodes/re-opens its
  own output and verifies it before handing it over — pixels compared after raster, values
  re-read after form fill, the envelope checked after decrypt, spans re-walked after HTML
  export. "Verified" means the round-trip held, not that a function returned `Ok`. Aspose
  does not publish a comparable per-operation verification contract.
- **Deterministic rasterization** (ADR-P0012) — byte-identical output across platforms and
  runs — which Aspose does not guarantee.
- **Redaction as removal with a proof obligation** (ADR-P0023) — when it ships, it is
  categorically safer than overlay "redaction"; the proof pass is the design, not an add-on.
- **Honest confidence.** Extraction and table recognition report a confidence verdict and
  refuse to overstate; conversions name exactly what they do and do not carry.
- **Own engine, no copyleft/CVE-prone C in the client core** (ADR-P0008, ADR-P0001) — the
  historical PDF CVE class (memory-safety in codecs/font/xref parsers) is removed by
  construction.
- **Page operations** are at or beyond full parity, including split-by-text and
  split-by-size that Aspose's typical split set does not offer.
- **Free, unlimited, no account** for individuals — the web app *and* the desktop app, not just
  the tools surface (`26 §2`; `27 §4A`) — against incumbents who charge individuals. Selis instead
  charges only commercial/enterprise use (SDK, Server, fleet, add-ons): the Aspose per-developer /
  per-deployment model inverted on the consumer side, which is a different economic model entirely.

### Where Aspose wins

- **Breadth and maturity of conversion fidelity.** Aspose renders DOCX/XLSX/PPTX/HTML/LaTeX
  with high fidelity (fonts, images, layout, charts, math). Selis converts *structure*, not
  layout — honest subsets, not renderers. This is the largest real gap.
- **A shipping, documented SDK today** across five language ecosystems. Selis's SDK is
  Phase 9; the comparison is premature until then.
- **Full annotation, link, and JavaScript *authoring*** — Aspose creates them
  programmatically now; Selis reads/detects them and defers authoring to Phase 5.
- **PDF/A, PDF/UA, PDF/X** validate + convert ship in Aspose; they are Phase 9 for Selis.
- **OCR** ships in Aspose; it is Phase 2-gated and unbuilt in Selis.
- **Signature maturity** — TSA timestamping, PAdES/CAdES/LTV. Selis signs and verifies but
  explicitly claims none of the advanced profiles yet (long-lead blocked).
- **Breadth beyond the document core** — barcodes, 3D/rich media, MathML, portfolios,
  BMP/EMF/XPS — which Selis deliberately leaves out of scope.
- **Font subsetting, linearization, image replace, TOC generation, XMP writing** — all
  specced in Selis but not yet exposed; Aspose ships them.

### Priority gaps to close next

Ordered by leverage against Aspose's surface, grounded in the plan's own next-slice list
(`35` "Next slices, in order" + the Phase 2 exit gate):

1. **Close the Phase 2 raster exit gate — OCR + searchable-PDF export, plus the full
   render corpus (G2).** OCR is the single most-cited capability Aspose has and Selis does
   not, and the raster path (PNG/JPG/TIFF/WebP, compare, invert, flatten) is already landed,
   so the chunk and verification doctrine exist. This also unblocks redaction preview and
   scan-to-PDF. `34` Phase 2; `35` OCR/scanning rows.
2. **Ship redaction (Phase 5), leading with the ADR-P0023 proof pass.** It is a genuine
   differentiator *and* a capability Aspose has — closing a gap and widening the moat in one
   move. Legal review of the claim (SL-0.LEAD.12) is the gating long lead, not the code.
3. **Raise inbound conversion fidelity and/or ship the font subset + appearance-stream
   path.** The honest-subset converters are the biggest visible fidelity gap to Aspose; and
   font subsetting (§5) + field-appearance synthesis (§8) together unblock font replace,
   linearization, and form flatten — several Aspose rows at once from one engine investment.

---

*Grounding: every Selis status above is derived from `35-PARITY-CHECKLIST.md`,
`34-TOOLS-SITE-ROADMAP.md`, `26-PRODUCT-SURFACE.md`, `01-ARCHITECTURE.md`, and `02-ADRS.md`.
The Aspose.PDF capability column is authored from general knowledge of the product and is
descriptive, not a quotation of Aspose documentation. No Selis capability here is claimed
beyond what those plan files record; rows whose checkbox is unticked but whose note states
"implemented + tested" with only the Edge/WASM browser-check outstanding are marked ✅ with
that caveat, per the ✅-threshold note in the legend section.*
