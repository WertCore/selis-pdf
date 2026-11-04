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
- [x] `cargo xtask size-check --strict` (tools chunk under its `xtask/size-budgets.toml` budget)
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
- [x] **Bookmarks / outline editing** — add, rename, retarget, delete outline entries; keep the
      existing reader/writer; verification counts entries in/out. No drag UI required.
      *Shipped:* `selis-pdf-tools::set_outline` replaces the whole tree from a caller-supplied
      `entries` list (`title`, 1-based `page`, 0-based `level`), so one rewrite covers add,
      rename, retarget, reorder, and delete; an empty list removes every bookmark. The builder
      links `/First`/`/Last`/`/Prev`/`/Next`/`/Parent` with `/Count` for roots and for items with
      children, points each `/Dest` at the copied page as `[page /Fit]`, and writes ASCII titles as
      literal strings and non-ASCII titles as UTF-16BE with a BOM. `retain_reachable` drops the
      input's old tree so a replaced bookmark does not linger as unreachable bytes. Empty titles,
      0/out-of-range pages, and level jumps are refused before any write. WASM op 26 `Outline`;
      web card takes `Title | page | level` lines. Evidence: 5 engine tests (nested tree,
      three-level subtree counts, replacement, clearing, refusals), the wasm dispatch test
      (3 entries, out-of-range error), and the browser legs — downloaded bytes carry
      `/Type /Outlines`, `(Chapter 1)`, and `(Section)` with `Outline entries 0 → 2`, online and
      offline with zero foreign requests. Tools chunk 288 706 B brotli of 300 000.
- [x] **Drag-to-reorder** — pointer/keyboard reordering for the file list and organize order,
      with the existing buttons kept as the accessible path. UI enhancement, not correctness.
- [x] **General PDF → ZIP** — a file-picker ZIP over any produced outputs (not only split parts
      and attachments), deterministic entry order and timestamps.
      *Shipped:* WASM op 24 `Zip` bundles the selected files by their user-visible names through
      `selis_pdf_tools::bundle_zip`. This slice also had to repair the tools chunk, which had not
      compiled since it landed: the `Zip` arm referenced an out-of-scope `files` binding and the
      private `verify_report` module. The chunk now collects the parsed inputs and uses the
      re-exported `Verification`.
- [x] **PDF → metadata JSON** — read-only export of Info/XMP and structural facts; no mutation.
      *Shipped:* `selis-pdf-tools::metadata_report` opens the newest revision and reports what the
      document says about itself — header version, revision count, leaf page count, first
      `/MediaBox`, every `/Info` key as text (a non-string value is named by its object kind rather
      than guessed), the catalog's XMP `/Metadata` packet with its `/Filter` decoded when it can be
      and named when it cannot, and the six structural counts the verification gate already walks.
      The export shape is owned by `MetadataReport::to_json`, so a CLI and the browser cannot drift.
      An encrypted document reports `status: "encrypted"` and nothing else: with no file key its
      dictionaries are still ciphertext, and printing ciphertext as a title would be a lie. WASM op
      27 `MetadataReport` returns one output of the new `report` kind — never a PDF, so no preview
      is offered — with verdict `read` (or `encrypted`) and `bytes_out == bytes_in`. The CLI twin is
      `selis metadata-report [-o report.json]`: the same export, printed to stdout when no output is
      named. The web card "Read metadata" downloads `<name>-metadata.json` and shows the packet
      described, not printed, with `/Info` fields capped at 10 rows and values at 120 characters so a
      hostile dictionary cannot grow the page. Evidence: 7 engine tests (Info + packet, nothing
      invented, Flate decode, undecodable filter named, oversized packet capped, encrypted, a
      negative fraction that keeps its sign outside the decimal point), 2 wasm dispatch tests, 3 new
      protocol tests, a CLI test that the read leaves the input byte-identical, and the browser legs —
      the same 726-byte fixture the strip leg rewrites reads back with `PDF version 1.4`,
      `First page 200 × 200 pt`, `Title = Secret Title`, `XMP packet = none`, `Pages 3 → 3`,
      `Verification = read` and `%PDF-` absent from the downloaded JSON, online and offline with zero
      foreign requests. Tools chunk 300 192 B brotli, baseline refreshed: this slice moved the budget
      from 300 000 to 320 000 in `xtask/size-budgets.toml` rather than cut the feature — compact JSON
      was 458 B cheaper and the report is meant to be read.
- [x] **Batch queue** — multi-file queue with per-file isolation, per-file results, one failure
      never cancels another, and no cross-file document access. Acceptance: 3 files in, 3
      independent verified results, one deliberate failure leaves the other two intact.
      *Shipped:* `selis-pdf-tools::batch::run_batch` opens, processes and closes one document at a
      time — it holds no second document open, so a row cannot borrow a neighbour's page count —
      over `BatchTool::{Compress, StripMetadata, Repair}`. A file the engine refuses becomes a row
      carrying its own typed message and nothing else: no output, no verification, no file on disk.
      Results keep queue order and are named after the file they came from (`<stem>-compressed.pdf`,
      `-clean`, `-repaired`, de-duplicated with `-2`/`-3`), and several of them arrive together in
      `<first-stem>-batch.zip`. The aggregate verification saturates instead of wrapping and its
      verdict is `ok` or `partial`; a queue where *nothing* worked is an error, not an empty success.
      Up to 64 files (`MAX_BATCH_FILES`), refused before the first document opens. WASM op 28
      `Batch`, capability mask now 28 bits. The 29th web card is "Batch queue"; the page reads its
      rows from the guest's own per-file report, so the Queue line, each file line and the ZIP are
      the engine's words, not the page's. The CLI twin is `selis batch compress|strip-metadata|
      repair|protect --outdir …`, whose `report.json` gained the same ok/failed rows.
      `protect` stays CLI-only and `BatchTool::from_name` refuses it by name: crypto is human-owned.
      Evidence: 5 engine tests (isolation across 2/3-page documents, damaged-index repair queue,
      refusals, name dedup, exact tool-name matching), the wasm dispatch test (two named outputs +
      a ZIP, a failed row naming `in/2` with no download), 23 protocol tests (a row must name its
      own download and its own result, or it is rejected), a CLI test that the middle bad file
      leaves its two survivors on disk and claims no output, and the browser legs — 3 files in,
      `compress · 2 of 3 files · 1 not processed`, `alpha-compressed.pdf · 3 pages · ok`,
      `beta-compressed.pdf · 1 page · ok`, `broken.pdf` as
      `not processed — in/1: [E1101] This PDF's index is damaged. …`, `Pages
      4 → 4`, `Verification = partial`, and a bundle whose bytes hold both documents and neither
      marker, online and offline with zero foreign requests. The bad file carries a real `%PDF-`
      header because that is the page's own rule: a non-PDF is refused when it is picked, so the
      browser can only prove isolation against a PDF that fails inside the engine. Tools chunk
      304 703 B brotli of 320 000, baseline refreshed from 300 192 (+4 511 B for the queue).
- [x] **Unlock / remove password** — crypto-enabled lazy chunk; AESV3/R6; never silently claim
      an unencrypted output when the password was wrong. **Signed off (HUMAN review 2026-11-04,
      approved).** *Shipped* in `selis-pdf-wasm-security`, a sixth lazy chunk with **0 imports**:
      "keys stay on device" is structural, because the guest has nowhere to send a document. The
      page injects the 64 bytes of key material (`crypto.getRandomValues`) and zeroes the request
      buffer once the call returns, since a guest with no entropy import must not be allowed to
      invent a file key. Interop, both directions: a Selis-encrypted file opened in pinned PDFium
      demands the password (a wrong or empty one fails with `pdfium_err=4`) and reports
      `handler_rev=6`; qpdf prints `R = 6`, `P = -44` with AESv3 for stream, string and file and
      `--decrypt`s it. Conversely three qpdf-written R6 files unlock on this device to a document
      the engine re-opens (3 pages), while a wrong password returns
      `locked.pdf: wrong password (WRONG_PASSWORD)` and writes nothing. Decryption is pixel-exact:
      the page-1 render digest `3363023653717085315` is identical for the plain fixture, the
      Selis-encrypted file opened with its password, and the qpdf-encrypted file after Selis
      unlock. Browser legs, online and offline, assert the envelope is gone from the download
      (`/Encrypt`, `/Filter /Standard`, `/AESV3` absent) while `/Type /Pages` and `/Count 3`
      remain, and the refusal sentence names the document the reader picked up, not the chunk's
      internal session key.
- [x] **Protect with password** — AESV3/R6, separate user/owner passwords, permission copy
      reviewed; keys stay on device. **Signed off (HUMAN review 2026-11-04, approved).**
      *Shipped* on the same chunk: AES-256 revision 6 only, separate open and owner passwords (the
      owner defaults to the open password, Acrobat-style), and permission bits written and then read
      back out of `/P`. Selis → PDFium: a print+copy file opens with its open password at
      `handler_rev=6` and PDFium derives `perm_print=1 perm_modify=0 perm_copy=1 perm_annotate=0`;
      qpdf reports `P = -44`, AESv3 for the content and the file key, and `--check`/`--decrypt`
      clean. The page's own loop closes in a browser leg: the file the protect card writes is handed
      straight to the unlock card and opens with the password the user typed. The card says in
      words that permission bits are a convention rather than enforcement, and never implies a
      reader that ignores them can be stopped. Key material is drawn in the page, so the guest links
      no RNG. Browser needles on the download: `/Encrypt`, `/Filter /Standard`, `/CFM /AESV3`,
      `/V 5`, `/R 6`, `/P -44`, with the plaintext content marker absent, online and offline.
- [x] **Clear permissions** — owner-password override only; report what changed. **Signed off
      (HUMAN review 2026-11-04, approved).** *Shipped*: the output is an incremental revision whose
      `/P` and `/Perms` change while the content stays encrypted under the unchanged file key, so
      the bytes underneath are not rewritten. The user password is refused here by name — clearing
      is the owner's decision — with `locked.pdf: owner password required (WRONG_OWNER_PASSWORD)`
      and no output; a successful run names what it actually lifted, read back from the flag it
      wrote (`modify, copy, annotate` for the qpdf fixture), and leaves the old `/P -3388` beside
      the new `/P -1` and the `/Prev` link. The cleared output renders pixel-identical to the
      original (digest `3363023653717085315`). Browser legs check `/Encrypt`, `/P -1` and `/Prev`
      present, the refusal producing no download row, online and offline.
- [x] **Permissions / encrypt** — same surface as protect; no "remove all restrictions" claim
      without the owner password. **Signed off (HUMAN review 2026-11-04, approved).** *Shipped* as
      the "Set permissions" card, which is the protect writer with an owner password and no open
      password: the document still opens without one and carries the flags (`/P -28` for
      print-annotate, `/P -4` for all four). It cannot lift a restriction — that is Clear
      permissions, and the card says so and points there — and it refuses a document that already
      carries a password rather than editing under one. Verified by the card re-opening its own
      output and by qpdf/PDFium reading `/P` back, online and offline.

### Phase 1A.1 exit checks

- [x] Every remaining box above is either ticked or explicitly deferred with a reason.
      Nothing from the parity list is outstanding in 1A.1. The four crypto-enabled security rows —
      unlock, protect, clear permissions, permissions/encrypt — carried *Human review required* and
      are now ticked: the review was signed off 2026-11-04 against independent readers (pinned
      PDFium and qpdf 12.4.2), in both directions, on the CLI and in the real browser. The Phase 2
      raster rows sit behind their own exit gate.
- [x] Tools chunk still under its `xtask/size-budgets.toml` budget with every new card lazy-loaded.
      `cargo xtask size-check --strict` now measures `selis_pdf_wasm_tools` at 321 372 B brotli
      against a 340 000 B budget — 18 628 B of headroom. The rise from 304 703 B is the AcroForm
      engine landing in `selis-pdf-tools` (the tools chunk carries it) and the crypto API the write
      path already linked; the budget moved to the next round number in the same pass rather than a
      feature being cut, and the 2% baseline rule still gates every later slice. The password
      operations do **not** weigh on this chunk: `pub mod security` is feature-gated, so the crypto
      code rides only the dedicated `selis_pdf_wasm_security` chunk (144 270 B brotli of 200 000),
      which the page loads lazily on the first password card.
- [x] Browser check drives every card online and after an offline reload.
      The gate is not "the page renders a card": an unclicked card can break without the check
      noticing. The driver now names every id in the grid (`cardIds`) and records the ids it
      actually opened in each phase (`clicked.online`, `clicked.offline`), and
      `xtask/src/browser.rs::check_card_coverage` refuses unless both click lists cover the grid
      and the grid equals `TOOLS_PAGE_CARDS`. The four cards that had no leg — rotate, delete
      pages, edit metadata, ZIP — now have one, each read from the downloaded Blob: `report-rotated.pdf`
      with `/Rotate 90` three times and Pages `3 → 3`; `report-trimmed.pdf` with `/Type /Pages`
      and `/Count 2` present, `/Count 3` gone, Pages `3 → 2`; `metadata-metadata.pdf` with
      `Browser Rewritten Title` and `Browser Author` present and `Secret Title` gone; `one.zip`
      as kind `zip` with a `PK\x03\x04\x14` header, no preview offered, and counts `[1, 2, 2]`
      over two picked files. The live run measures `cardIds 29`, `online 29`, `offline 29` with
      zero foreign requests in both phases, inside the committed 30 000 ms per-check budget. (That
      was the grid at this pass; the raster, HTML, AcroForm, sign/validate/approve and four password
      cards have since grown it to 42, and the same coverage rule refuses any run whose click lists
      do not cover the whole grid.)
      Eight negatives withdraw one witness each: a rotate that turned one page, a delete that
      kept the page, a metadata save that still carries the old title, an archive offered as a
      PDF preview, an online-only ZIP, an online pass that never opened rotate, an offline pass
      that clicked three cards, and a grid that rendered fewer cards than the check covers.
      The password cards add ten more, each withdrawing a claim: a protect download that carries no
      `/Encrypt`, a grant the flag does not hold, a card that speaks with another card's sentence, a
      clear run whose verification is not `ok`, an unlocked output that still carries `/Encrypt`, a
      refusal that produced a download, a refusal that does not name `WRONG_PASSWORD`, a refusal
      that repeats the password it was handed, an online-only security block, and an offline pass
      whose clear leg never ran.
- [x] `34-TOOLS-SITE-ROADMAP.md` "Remaining in 1A.1" is empty or lists only named deferrals.
      It now names one deferral only — the incremental writer for page edits (which the verified
      full-rewrite path covers today). The crypto-enabled security tools, the batch queue, the
      AcroForm, sign, validate and approve cards, and the four remaining tool cards are all recorded
      as Live.

---

## Phase 2 — raster and OCR

Exit gate: render hash determinism, pixel/text verification for every output, OCR confidence
and extraction tests, and a lazy raster chunk under budget. No raster claim before this gate.

### Rendering and image outputs

- [x] **PDF → JPG** — render every page, quality control, ZIP for multi-page.
      *Shipped:* the JPEG lives in the same lazy, import-free `selis-pdf-wasm-raster` chunk as PNG —
      its ABI took a `format` code and a `quality` argument rather than the tree growing a fifth
      chunk. The coder is owned from the transform up: `selis_pdf_filter::dct_encode` writes a
      baseline sequential SOF0, 8-bit, 4:4:4, with a JFIF APP0, Annex K quantisation tables scaled by
      the requested quality and an integer Q15 DCT, and `selis_image::encode_jpeg` frames it. The page
      is the same render the PNG card uses (`Session::render_page_rgba`, `/Rotate` honoured, straight
      RGBA over opaque white), so the two cards differ only in what happens after the pixels exist.
      Quality is 1–100 (default 90, both knobs rejected outside the range) and DPI stays 1–600.
      Verification is still "decode what you wrote and compare it to the page", with the lossy part
      stated as bounds instead of equality: the chunk re-opens its own JPEG and refuses a file that
      does not measure the page, then refuses unless the mean per-channel error is ≤ 96 and, from
      quality 75 upward, no sampled pixel is off by more than 160. Below quality 75 the local bound is
      dropped rather than curve-fitted — a coarse subsample legitimately moves an edge — while the
      mean bound, the size check and the dimension check keep holding. A page rendering wider or
      taller than 16384 px is refused before any file is written, because that is the limit of the
      decoder doing the read-back: a JPEG this device cannot re-open is not a verified output, and the
      card says so in its own note. One page downloads as a bare `.jpg`; several come back as one
      deterministic store-method ZIP of `page-N.jpg` entries padded to the digit width of the page
      count, never previewed. The CLI twin is `selis render --format jpg --quality N` (`jpeg` accepted
      as an alias, PPM still the default, an unknown format naming the three that exist). The web card
      is the 31st in `/tools.html` and reports `kind: "image"`.
      Evidence: 61 `selis-pdf-filter` and 20 `selis-image` tests (the coder's own round-trips and
      guards, JPEG framing determinism), 25 raster-chunk tests — the JPEG round-trip, byte-identical
      re-render, `page-N.jpg` naming, PNG ignoring the quality knob, an honest page and a megapixel
      text page clearing the bounds, refusal of a file of the wrong size, of a file that is not the
      page, of a few badly wrong pixels, of a damaged scan, ringing *not* being refused at the
      coarsest quality, and the oversized page refused before any file is written — 6 CLI tests in
      `render_jpg.rs` (canvas at 72 and 144 DPI, worst-pixel bound, quality moving the byte count on
      a text page, determinism, the `jpeg` alias, refused quality values, the refusal naming the
      formats) with `render_png.rs`'s 3 still green, and 178 `xtask` tests including the real Edge
      run. The browser legs read the downloaded bytes: `report.jpg` must re-parse as a complete JPEG —
      SOI, `JFIF` APP0, a **baseline** SOF0 measuring 200×200 for the fixture page at 72 DPI, EOI in
      the last two bytes — carry no `/Type /Page`, be previewed and downloaded from Blob URLs, with
      Pages `1` and Verification `ok`; `jpgCoarse` at quality 1 must come back smaller than the
      quality-90 leg; `report-pages.zip` must start `PK\x03\x04\x14`, hold `page-1.jpg`, `page-2.jpg`
      and `page-3.jpg`, report Pages `3` and offer **no** preview — online, and again after the origin
      is cut with the chunk served from cache and zero foreign requests. Seven verdict negatives
      withdraw one witness each: a truncated JPEG, a progressive frame, a zero canvas, a bundle with
      two of three pages, a quality knob that produced identical bytes, and the two offline legs.
      Raster chunk 1 093 374 B brotli of its own 1 300 000 budget — 4 926 B (+0.45%) over the
      PNG-only baseline, recorded as a deliberate baseline edit rather than drift; the tools chunk is
      not touched by this slice (304 640 B brotli measured against its 320 000 budget).
      Not claimed: WebP (since shipped — see the **PDF → WebP** row), TIFF (since shipped — see the
      **PDF → TIFF** row), and the images-ZIP parity row across every image format (since ticked — see
      below); the Phase 2 exit gate — corpus render-hash determinism and OCR — still open it.
- [x] **PDF → PNG** — same, lossless.
      *Shipped:* `selis-image::encode_png` writes colour type 6, filter 0, zlib through the owned
      `selis_pdf_filter::flate_encode` — no image library in the tree. The engine renders a page to
      straight RGBA over opaque white (`Session::render_page_rgba`, DPI-scaled, honouring `/Rotate`),
      and the new lazy chunk `selis-pdf-wasm-raster` owns the web path: it takes the document, a
      1-based page selection and a DPI bounded to 1–600, renders each page, encodes it, then
      **decodes its own PNG and compares the pixels** before returning anything — a page that does
      not round-trip is refused rather than shipped with a "verified" label on top. One page comes
      back as a bare PNG; several come back as one store-method ZIP whose entries are `page-N.png`,
      zero-padded to the digit width of the page count (3 pages → `page-1.png`, 12 → `page-01.png`),
      because the page must not fire a download burst. The CLI twin is `selis render --format
      ppm|png`, default still PPM. The web card is the 30th in `/tools.html`, fetching
      `/wasm/selis_pdf_wasm_raster.wasm` on first use with `kind: "image"` outputs and a real
      preview.
      Evidence: 14 `selis-image` tests (round-trip, byte determinism, 1×1, mismatched buffer,
      zero dimensions), 2 engine tests (opaque paper at every pixel, two renders identical),
      7 raster-chunk tests (chunk id, PNG decodes with the centre pixel black, byte-identical
      re-render, 1-based ordered selection, DPI bounds, multi-page ZIP, padding width), 3 CLI tests
      (`render_png.rs`: DPI dimensions, determinism, default/unknown format), and the browser legs —
      `report.png` 802 B whose bytes start `\x89PNG`, carry `IHDR` and `IDAT` once each, contain no
      `/Type /Page`, measure 200×200 at 72 DPI and are offered as both a preview and a download, and
      `report-pages.zip` with a `PK\x03\x04\x14` header, `page-1.png`/`page-2.png`/`page-3.png` each
      present twice (local header plus central directory) and **no** preview — online and after the
      origin is cut, with the chunk served from cache and zero foreign requests. Eight verdict
      negatives withdraw one witness each. Raster chunk 1 088 448 B brotli of its own 1 300 000
      budget; the tools chunk is unchanged at 304 701 B of 320 000.
      Not claimed at the time: JPG (since shipped — see the row above), WebP (since shipped — see the
      **PDF → WebP** row) and TIFF (since shipped — see the **PDF → TIFF** row), the PDF → images ZIP
      parity row (since ticked — see below), and the Phase 2 exit gate — render-hash determinism across
      the corpus and OCR still open it.
- [x] **PDF → WebP** — one file per page, lossless, through a VP8L coder the tree owns.
      *Shipped:* `selis-image::webp` writes `RIFF`/`VP8L` and reads it back — no image library and no
      libwebp in the tree. This row used to read "needs an owned VP8/VP8L coder", and that was the
      honest blocker rather than a task: a third-party codec is a supply-chain decision that belongs to
      the human-owned `cargo vet` gate, not a `Cargo.toml` edit. `encode_webp` packs the page's RGBA as
      the ARGB pixels VP8L names, gives each channel its own `PrefixCode` (the green alphabet carries
      the 24 LZ77 length codes on top of 256 literals; red, blue and alpha are plain 256-symbol),
      codes backward references through the 40 distance prefix codes, decorrelates a document's
      achromatic ink with subtract-green and lays a predictor transform whose mode is chosen per block.
      A skewed histogram that would push a code past the 15 bits the format allows gets a floor under
      its counts, a one-symbol code spends no bits, an odd payload gets the pad byte the chunk size does
      not count and the enclosing RIFF size does, and the same pixels always produce the same file.
      `decode_webp` reproduces those samples exactly and names what it will not read — a color cache, a
      meta prefix code, a color transform, a color-indexing transform, one of the 120 close-range
      distance codes, or a chunk that is not `VP8L`, is `IMAGE_UNSUPPORTED`; a broken container, an
      incomplete tree, a reference reaching past the frame or a stream that stops early is
      `IMAGE_MALFORMED`.
      Verification is the PNG's standard, not the JPEG's bounds: the chunk decodes its own WebP and
      compares every sample, alpha included — VP8L carries an alpha plane, so this is the lossless
      output that keeps what the renderer produced instead of compositing it over white like the TIFF
      profile must. A page rasterising past 16384 px on an edge is refused before a byte is written,
      because both dimensions are 14-bit minus-one fields and a truncated one would let the header lie
      about its own page; the refusal names the canvas, the limit and PDF to PNG as the per-page way
      out. DPI stays 1–600, there is no quality knob, and a nonsense quality value changes nothing byte
      for byte. One page downloads as a bare `.webp`; several come back in the same deterministic
      store-method ZIP the other per-page formats use, as `page-N.webp` padded to the digit width of the
      page count, never previewed. The CLI twin is `selis render --format webp`, and the refusal for an
      unknown format now names the five that exist (ppm, png, jpg, tif, webp). The web card is the 34th
      in `/tools.html`: it reports `kind: "image"` and previews, because unlike a TIFF a WebP is
      something Chromium draws.
      Evidence: 61 `selis-image` tests, 26 of them this module (the container being a RIFF wrapping one
      `VP8L` chunk, a document page round-tripping and not being a size liability, a flat page costing
      its headers and little else, a varying page and a page of noise round-tripping channel for channel,
      alpha surviving and saying so in the header, greyscale using the green subtraction, subtract-green
      and add-green being inverses, every prediction mode defined and bounded, LZ77 prefix coding
      covering its whole range, a long run copying itself in blocks of 4096, an overlapping reference
      rebuilding the pixels it repeats, a long zero run shrinking to the two repeat codes, the
      two-symbol and one-symbol code forms, a deep tree getting its floor, determinism, a budget too
      small for the working set refusing the page, zero-sized and oversized images refused, and every
      construct the writer does not produce refused by name); 44 raster-chunk tests, 9 of them WebP (the
      page reading back its own pixels with the centre pixel black, `page-N.webp` naming, byte-identical
      re-render, quality ignored, a document page coming back *smaller* than the same page as a PNG, the
      16384 limit holding on either side of it, a 20000×40 page refused before any file exists, and
      `verify_webp` refusing a file that is not the page and a damaged bitstream); 4 CLI tests in
      `render_webp.rs` (canvas at the requested DPI, a text page coming back exactly as the PNG leg sees
      it *and* smaller, determinism, quality not moving a lossless file) with `render_tiff.rs`'s 5,
      `render_jpg.rs`'s 6 and `render_png.rs`'s 3 still green — and `render_jpg.rs`'s unknown-format
      witness now asserting the message names WebP instead of refusing it — and 178 `xtask` tests
      including the real Edge run. The browser legs read the downloaded bytes: `report.webp` must walk
      as RIFF with a `WEBP` form type, a declared RIFF length that agrees with the file the harness was
      handed, a `VP8L` chunk whose 0x2F signature opens it and whose two 14-bit minus-one fields name
      200×200 at 72 DPI with version bits 0; `found` the `VP8L` fourcc once, carry no `/Type /Page`, be
      previewed and downloaded from Blob URLs with Pages `1` and Verification `ok`; `report-pages.zip`
      must hold `page-1.webp`, `page-2.webp` and `page-3.webp` and offer **no** preview — online, and
      again after the origin is cut with the chunk served from cache and zero foreign requests. Then the
      one raster proof that is not the writer grading itself: the harness paints each displayable file
      through `createImageBitmap` and an `OffscreenCanvas` and digests the samples with FNV-1a, and the
      PNG and the WebP of the same render must come out the **same picture** — 3 627 224 773 over
      160 000 samples on a 200×200 canvas, in both passes. Measured on that fixture page: `report.webp`
      152 B against `report.png` 802 B, and the three-page bundle 790 B against 2 712 B. Six verdict
      negatives withdraw one witness each: a stream that is not VP8L, a zero canvas, a WebP Chromium
      paints differently from the PNG, a bundle with two of three pages, and the two offline legs.
      Raster chunk 1 115 272 B brotli of its own 1 300 000 budget — 17 641 B (+1.6%) over the TIFF
      baseline, recorded as a deliberate baseline edit rather than drift; the coder needed no fifth
      chunk and no raised ceiling. The tools chunk is untouched by this slice (304 734 B brotli against
      its 320 000 budget).
      Not claimed: WebP → PDF (the reader refuses every construct libwebp writes, so a stranger's WebP
      is not an input this tool accepts — the inbound card stays roadmap), HEIC, and the Phase 2 exit
      gate — corpus render-hash determinism and OCR still open it.
- [x] **PDF → TIFF** — one multi-page container; the page order is the 1-based selection, the DPI is the tag.
      *Shipped:* `selis-image::tiff` writes the container and reads it back — no image library in the
      tree. `tiff_strip` deflates one rendered page through the owned
      `selis_pdf_filter::flate_encode` (level 6, `Compression` 8), `encode_tiff` lays the strips as an
      IFD chain (15 entries a page, 186 bytes of structure, the values block after `NextIFD`, every
      block padded to an even offset) and `tiff_page_infos` / `tiff_strip_rgb` / `decode_tiff` walk it.
      The writer emits only the profile the reader accepts — 8-bit contiguous RGB,
      `PhotometricInterpretation` 2, one strip per page with `RowsPerStrip` equal to the page height,
      `XResolution`/`YResolution` carrying the requested DPI in inches, alpha composited over opaque
      white because this profile has no transparency — and it says which rule a foreign file broke:
      `PlanarConfiguration` 2, a `BitsPerSample` that is not three 8-bit samples, a compression we do
      not write or a multi-strip page is `IMAGE_UNSUPPORTED`; a looping or off-the-end chain, a
      truncated file or a strip that will not inflate to its declared size is `IMAGE_MALFORMED`.
      Verification is the PNG's standard, not the JPEG's bounds: the chunk walks its own file, refuses a
      chain that does not hold the pages it rendered, and re-inflates every page to compare it sample
      for sample with the render, so a damaged strip comes back as a failure instead of a download.
      Because the container answers the whole selection, this is the one raster card that never makes a
      ZIP: one `.tif`, its IFDs in selection order, a repeated page written once as on the other cards.
      A page rasterising past 65535 px on an edge is refused before a byte is written — those dimensions
      are `SHORT` tags, so a truncated one would make the file lie about its own page — and the refusal
      names the canvas, the limit and PDF to PNG as the per-page way out. DPI stays 1–600; there is no
      quality knob, and a nonsense quality value changes nothing byte for byte. The CLI twin is
      `selis render --format tif` (`tiff` accepted as an alias; the CLI writes the one-page container).
      The web card is the 32nd in `/tools.html`: it reports `kind: "image"` and offers a download with
      **no** preview link, because Chromium does not display TIFF inline.
      Evidence: 35 `selis-image` tests, 15 of them this module (a page round-tripping to its own
      samples, several pages staying in order in one file, byte determinism, the chain reported without
      decoding it, a transparent sample landing on white, an odd-length strip leaving the next IFD
      aligned, refusal of pages that cannot be described or of a strip laid out by another caller,
      refusal of non-TIFF bytes / a looping or off-end chain / a strip that lies about its size / a
      multi-strip page, the profile named on refusal, an uncompressed page reading back exactly, and a
      big-endian file reading as well as a little-endian one); 35 raster-chunk tests, 10 of them TIFF
      (header `II\x2a\x00`, the same canvas the PNG leg reports sample for sample, two pages in one
      container with no `PK` prefix, a repeated page written once, an out-of-range page named, quality
      ignored with byte-identical output, the DPI bounds, the 65535 limit holding on either side of it,
      an 8000 pt page at 600 DPI refused as `66667x17` before any file exists, and `verify_tiff`
      refusing a short chain, a wrong size and a damaged strip); 5 CLI tests in `render_tiff.rs` (canvas
      at 72 and 144 DPI, a text page coming back exactly as the PNG leg sees it, determinism, the
      `tiff` alias, quality not moving a lossless file) with `render_jpg.rs`'s 6 and `render_png.rs`'s 3
      still green, and 178 `xtask` tests including the real Edge run. The browser legs walk the
      downloaded bytes independently: `tiffChain` demands the `II`/`MM` mark, version 42, a strictly
      forward IFD chain that terminates, and every page carrying photometric 2, three samples,
      `PlanarConfiguration` 1, one strip and `RowsPerStrip` = height; `report.tif` is one page measuring
      200×200 with no `/Type /Page` inside, and the three-page leg is **one** file whose chain counts 3
      with Pages `3` and Verification `ok` — neither leg may show a preview link — both repeating after
      the origin is cut, the chunk served from cache with zero foreign requests. Raster chunk
      1 097 631 B brotli of its 1 300 000 budget — 4 257 B (+0.39%) over the JPEG baseline, recorded as
      a deliberate baseline edit rather than drift; the tools chunk is not touched by this slice.
      Not claimed: WebP (since shipped — see the **PDF → WebP** row; the coder arrived after this row
      was written, which is why the TIFF card went out with the container's own limits already named),
      TIFF → PDF (the reader exists in the tree; the card does not), and the Phase 2 exit gate — corpus
      render-hash determinism and OCR still open it.
- [x] **PDF → images ZIP** — deterministic bundle of the above.
      *Shipped:* `selis-pdf-tools::bundle_zip` writes the store-method archive deterministically, and
      the three cards that produce one file per page hand several pages to it as `page-N.png` /
      `page-N.jpg` / `page-N.webp`, zero-padded to the digit width of the page count, each entry name
      browser-verified twice over (local header plus central directory) with **no** preview offered for
      the archive — online and offline. The bundle exists because the page must not fire a download
      burst, not because the formats need one. TIFF deliberately stays out of it: it is already a
      container, so its card returns one `.tif` whose IFD chain counts the selection.
      Evidence: the entry names are asserted per format by the `pngZip` / `jpgZip` / `webpZip` legs of
      the committed tools-page check, and seven verdict negatives show those witnesses are load-bearing:
      a bundle holding two of three rendered pages for each of the three formats, a PNG archive handed to
      the viewer as a preview, and an online-only bundle for each format.
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
- [x] **PDF → HTML** — positioned spans first, reflow later.
      *Shipped (the positioned half):* `selis-pdf-text::html` writes, reads and verifies one
      profile — `to_positioned_html`, `read_positioned_html`, `verify_positioned_html`. The
      shape it replaced (one `<p data-bbox>` per line, no canvas, a page range concatenated
      into several `<html>` roots in one file) is text wearing HTML clothes; this writes what
      the raster cards paint: one `<section class="page">` per selected page, sized by the
      caller's own `Session::page_view` at 72 DPI — one CSS pixel per PDF unit, `/Rotate`
      folded into the transform, so a span lands where the PDF-to-PNG card puts the ink — and
      one absolutely positioned `<p>` per *recovered* span, with the measured box emitted as
      `data-box` beside the `left`/`top`/`font-size` derived from it. A page that drew text
      and recovered none carries `LOW_CONFIDENCE_MARKER` exactly as the text, JSON and Markdown
      formats do; a zero-area page is refused before a byte is written; runaway matrices
      saturate at `MAX_COORDINATE_PX` and keep the word; a control character inside a span
      becomes U+FFFD, because a line-oriented profile has no newline to put it in.
      The reader is deliberately not an HTML parser: it accepts the emitter's tag and
      attribute whitelist and refuses anything else with the line and the reason, and the
      `<style>` block is compared line for line against a constant, so `@import` and `url()`
      cannot appear in a document that passes — "nothing external can reach whoever opens
      this file" is a checked property, not a hope. `verify_positioned_html` re-reads the
      bytes against the model they came from, span for span.
      The fifth lazy chunk is `crates/selis-pdf-wasm-html`: pointer ABI
      (`selis_html_run(bytes, pages, title, out_len, out_pages)`), **no declared imports**, so
      the page instantiates it with an empty import object — the mechanical form of "this
      module cannot call out". Page selection is the 1-based `1-3,5` grammar the other cards
      use, `""` meaning every page; a page named twice is written once; two runs on one input
      are byte-identical; the document is verified before it is handed over, and a failure is
      reported as a failure instead of a download. Measured raw 742 184 B → 228 325 B brotli
      against its own 300 000 budget, with the other nine artifacts unmoved.
      The CLI twin is `selis extract --format html`: one document for the whole selection, to
      stdout or `--output`, titled from the file stem, verified before the file is created.
      The web card is the 33rd in `/tools.html` (the card between Render TIFF and Create ZIP): it reports
      `kind: "html"` and offers a download with **no** preview, because a `blob:` document opens
      in this page's own origin — the check asserts that absence.
      Evidence: 15 `html.rs` tests inside `selis-pdf-text`'s 77 (one root per selection, a span
      where the PNG card paints it, rotation through the raster transform, DPI scaling both
      axes, escaping and U+FFFD, the marker once per page, a zero-area refusal, saturating
      coordinates that keep the text, a plain paragraph dump refused as not-this-profile, an
      injected element refused, a mutated box/text/page failing verification, numbers only in
      the emitter's own spelling, an empty selection as a valid empty document, a title that
      cannot open a tag, escapes as the only entities the reader knows); 14 chunk tests (the
      chunk's words surviving a real page, sections numbered as the user selected them, a
      text-less page as a section with no spans and no marker, byte determinism, the canvas
      being the page's own box with every span inside it, title markup/truncation/control
      characters, selection parsing, an out-of-range page refused before the document is read,
      a zero-area page refused, a non-PDF refused, the ABI moving bytes through guest slots);
      6 `extract_html.rs` CLI tests (a range is one document of sections on their own canvases
      with every box inside its page, a selected page numbered as itself, `--output` holding
      exactly what stdout showed, a zero-area page refusing to write any file, the marker
      riding its own page, and the per-page text/md/json formats byte-unchanged) plus the
      reworded `extract_fidelity` html assertions; 178 `xtask` tests including the real Edge
      run, whose four HTML legs (all pages and page 1, before and after the origin is cut) do
      not trust the writer: the downloaded bytes are re-walked by the browser's own HTML parser
      — `main > section.page`, each section's `data-page`/`data-width`/`data-height` agreed
      with its own `style`, every span's box inside its canvas, no span outside — while the
      doctype/root/`<main>` counts are taken from the raw bytes, because DOMParser *repairs* a
      concatenated document and the repaired tree would hide exactly the old broken shape.
      Not claimed: reflow (this is a layout copy, spans sit in extraction order inside the
      page), colour (the model carries no resolved fill), font faces (one CSS fallback family),
      a separator between word spans (the gap is geometry, as in the JSON format), images or
      annotations in the output. The reflow line below stays open.
- [ ] **PDF → HTML reflow** — reading-order article output, tables and links; no layout claim
      above measured fidelity.
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

- [x] **Fill forms** — AcroForm field editing; flatten is a separate raster/forms task.
      *Shipped:* a new core crate, `selis-pdf-form`, owns the form model: it walks the catalog
      `/AcroForm /Fields`, resolves inherited `/FT`/`/Ff`/`/DA`, associates each field with its own
      widget or its kids' widgets, and reports the page each one paints on. Values decode from
      ASCII literal strings and UTF-16BE with a BOM, and encode back the same way, so a name typed
      with an accent survives the round trip. `fill_form` writes `/V` for text and buttons, `/V` plus
      `/I` for a choice, and sets `/NeedAppearances true` so a viewer rebuilds the appearances, then
      re-reads every written value from the bytes it produced and refuses with `ReadBack` if one did
      not stick. It refuses by type rather than degrading: encrypted documents, XFA, a document that
      is already signed, read-only fields, push buttons, multi-select choices, unknown `/FT` values and
      duplicate qualified names. The crate is bytes-in/bytes-out with a caller-supplied `Budget` and
      `#![forbid(unsafe_code)]` — no filesystem, no network — so the desktop app reuses it unchanged.
      `selis-pdf-tools::fill_forms` is a thin wrapper that adds file I/O, the signature warning and the
      WRITE.05 gate. WASM ops 29 `form_fields` (a read-only JSON report, kind `form`) and 30
      `fill_forms`, with the capability mask now 30 ones. CLI twins: `selis form-fields` prints the
      report and `selis fill-forms --set NAME=VALUE` writes the filled file; the WRITE.07 conformance
      leg fills the fixture's first fillable field on every release build. The web card reads the
      fields as soon as a PDF lands and renders one control per field — text input (honouring
      `/MaxLen`), a button-state select built from the widget's own appearance states, a choice select
      that shows parallel `/Opt` labels but submits export values and keeps a stored value that is no
      longer an option — with signature and read-only rows rendered but disabled, and the signature
      invalidation warning shown before the run button enables. Evidence: 8 core tests (field kinds and
      values, UTF-16 round trip, unchecking writes `/Off`, a second fill replaces the first, every
      refusal named), 103 `selis-pdf-tools` tests, 37 tools-chunk tests (dispatch pair plus a refusal
      for an unknown field), 4 CLI end-to-end tests, 23 protocol tests, and the browser legs — online
      and offline the fixture renders 7 rows, the signature warning is visible, the signature and
      read-only controls are disabled, and the downloaded Blob carries `(Browser Ada Lovelace)` and
      `/NeedAppearances true` with zero foreign requests. 39 cards. Tools chunk 321 569 B brotli of
      340 000 (the budget moved one round step with a note in `xtask/size-budgets.toml`).
      Still open: appearance-stream synthesis, so this relies on `/NeedAppearances` and a viewer that
      honours it; flattening remains its own task.
- [x] **Sign PDF** — certificate/signature UI, local keys, invalidation warnings. Shipped as the core
      crate `selis-pdf-sign` (27 tests: visual-stamp geometry and WinAnsi refusal, incremental digital
      signature, `/ByteRange` patching, CMS verification and re-read of the field) plus `selis-crypto`
      DER/CMS/X.509 additions (70 unit + 8 integration tests), a seventh lazy import-free WASM chunk
      (`selis-pdf-wasm-sign`, 6 tests, capability mask `0b111`, protocol ops 1 stamp / 2 sign / 3
      report, `SLSG`/`SLRG`), `selis-pdf-tools::sign` wrappers behind a `sign` feature with
      `PRESERVE_ON_APPEND`, `selis stamp | sign | signatures`, and 21 `sign-protocol` vitest cases.
      RSA PKCS#1 v1.5 with SHA-256 only, 2048-bit minimum, EC keys refused by name; timestamp, CAdES,
      PAdES and LTV are not claimed. Browser legs online and offline stamp page 2, sign `report.pdf`
      with the committed test key and check the downloaded bytes for `/SubFilter
      /adbe.pkcs7.detached`, `/ByteRange` and the typed `/Name`. Sign chunk 220 308 B brotli of the
      400 000 B budget (725 507 B raw with custom sections stripped, measured by `size-check`); the
      grid now renders 42 cards, up from 39.
- [x] **Certificate validation** — trust store and revocation policy; no validity claim
      without a chain. Shipped as a report-only card over the signing chunk. Anchors are PEM files
      the reader selects (no built-in root store); the chain walk checks names, signatures, validity
      windows, CA and path-length constraints at the report's `now`, and lists every link. Revocation
      is always "not checked - this build has no network", never a guessed status, and an unverified
      signature is listed with its reason instead of hidden. The online browser leg validates the
      file the signing leg had just written against the test root and asserts `Signature 1:
      verified`, `Trust 1: trusted`, `Coverage 1: the whole document`; the offline leg repeats it with
      the origin cut. No download is offered, because the report is the result.
- [ ] **Permissions / encryption UI** — completes the 1A.1 crypto tools with a reviewed flow.
- [x] **Workflow (approve / route)** — local workflow state first; sharing is P2P later. Shipped as a
      local-only approval card: decision, approver, date and optional note are appended as a visible
      block through the stamp operation, and the result says no queue, account or server holds a
      copy. A later decision adds a block and leaves the earlier one readable. No routing,
      notification or P2P sharing ships yet, and none is claimed. Online and offline browser legs
      assert `(APPROVED) Tj` in the downloaded bytes and the result row "recorded in the appended
      block; no server holds a copy".

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

1. Crypto-enabled security chunk (unlock, protect, clear permissions) — human review.
2. Phase 2 raster: the exit gate's corpus render hashes + OCR, and the render-based rows still unticked
   in that block (target-size compression, invert colours, flatten annotations and forms, compare two
   PDFs). The WebP codec and the images-ZIP row have since shipped.

(Metadata → JSON export, the batch queue and the raster chunk's PNG, JPG, TIFF and WebP — with the
bundle the per-page formats share — have all shipped, so they left this list. TIFF left it as a
container: one `.tif`, never a bundle.)
