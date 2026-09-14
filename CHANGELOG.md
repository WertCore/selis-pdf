# Changelog

All notable user-visible changes to `selis` are documented here.
The format is a plain reverse-chronological list of unreleased/released
sections; every entry states what changed and what it means for the user.

## [Unreleased]

### Added

- CJK fallback no longer needs the 100 MB payload (SL-3.FONT.10, engine
  side): the viewer ships a subsetted CJK core, fetches the remaining
  Unicode ranges as separately-loadable chunk files on demand, and through
  them turns `.notdef` boxes into real glyphs on the repaint — never
  blocking a render on a font fetch. `Session::render_page_cjk` reports
  which chunks the page needs and carries the revision counter the shell
  repaints on; `xtask cjk-build` produces the payload
  (`cjk/core.ttf`, `cjk/<id>.ttf`, size-pinned `cjk/manifest.json`). The
  web-side download and repaint wiring arrives with the WASM shell
  (SL-4.WASM.07); the contract is drafted in ADR-P0043 pending sign-off.
- A page whose display list drew glyphs but from which no character could be
  recovered now reports a visible low-confidence marker in every `selis extract`
  format (text, JSON — via `low_confidence` in the `selis-extract/1` schema —,
  Markdown, and HTML), instead of a silent empty string indistinguishable from
  a blank page (SL-3.TEXT.10).
- The pinned oracle-container images for PDFium and pdf.js additionally
  accept `--text <out.txt>` — each driver extracts a page's Unicode text
  as UTF-8 alongside its existing `--dpi`/`<out.png>` render mode. Used by
  the SL-3.CONF.01 CI text legs of the `render-conf` scheduled job; the
  MuPDF text leg needs no driver change (`mutool draw -F txt` already
  extracts). Selis's own behaviour is unaffected — an oracle contract
  change, not a product change; the images' digests must be re-pinned
  once the oracle-images job has rebuilt them after this lands.
- The WASM binding speaks the versioned Worker protocol (ADR-P0042): a JS
  shell drives document open, page metadata, tiled page render, text
  extraction, search, and document close over one message boundary, with
  typed error codes (including `CANCELLED` and the budget family) and
  per-document budget profiles crossing it. Cancellation has two channels —
  pre-cancel messages and an out-of-band cancel slot for shared-memory
  hosts — and progress is published to an exported slot at every stage
  boundary. Editing is not wired yet: `mutate`/`save` answer a typed
  "unsupported" until Phase 5.
- Every tool operation now ends with a compact verification line on stderr —
  pages, annotations, form fields, OCGs, outline entries, and embedded files
  (in→out), byte sizes, and the structural-check verdict — built from the
  verification pass every output already goes through. Tool commands accept
  `--json` to print the machine-readable twin (`{"verification": {…}}`) on
  stdout for scripts and UIs.
- `selis` installs a Ctrl-C hook: cancelling a long operation now stops it
  cleanly with a typed cancellation ("no partial file was written") and exit
  code 130, instead of killing the process. Large outputs report byte
  progress while being written.
- Budget exhaustion is reported honestly: which budget tripped (memory,
  objects, nesting depth, time, pixels), the measured usage when available,
  and what to do (split the file). `selis batch` report.json carries the same
  fields per failed file and the `verification` object per successful one.

- `xtask oracle text-sweep --only-pair A+B` (repeatable) runs a pair-only
  oracle-vs-oracle text calibration — the SL-0.ORACLE.04 baseline mode: the
  named oracle legs only, no selis, no golden renders, no font inventories;
  the rows land in the same pair-verdict JSONL the CONF.01 artifacts use.
  `xtask oracle compare-text` gained `--tool <mutool|pdfium|pdfjs>` and now
  runs the exact page-1 leg the sweep runs, scored through the one shared
  normalisation policy (`xtask/src/text_norm.rs`). Selis's behaviour is
  unaffected — an oracle-harness change.

### Fixed

- The PDFium and pdf.js oracle drivers print their page-extraction banners
  to stderr now, and the harness runs local text legs with stdout to a side
  log — on local legs the banner was interleaved into the tools' `--text`
  output files (`xtask oracle text-sweep`, `xtask oracle compare-text`; the
  container legs read mounted files and were unaffected).
- Text extraction now decodes characters instead of emitting raw encoded
  bytes. Simple-font codes are resolved through the SL-3.TEXT.02 recovery chain
  (`/ToUnicode` first, then the encoding's glyph name, the Adobe Glyph List and
  the `uniXXXX` convention) before the text layer sees them, so é, °, and CJK
  reach `text`, `json`, `md`, and `html` output as UTF-8 rather than as control
  characters or the per-glyph octal-escape garbage the sweep reported across
  1,428 files (SL-3.TEXT.08).
- Word-gap inference no longer splits runs into single letters. The extractor
  previously treated a gap of roughly one half-em as a space, which is the
  *normal* distance between glyphs, fragmenting text like "Selis oracle smoke
  test" into "S e lis o ra cle sm o ke te st". Words now split only at real
  space glyphs or at an advance gap exceeding half the font's actual space width
  (SL-3.TEXT.09); the full-corpus sweep's exact-match band rises accordingly.
- Text shown in a `BT` block whose font was set by an earlier text object —
  the shape TCPDF and many form generators emit (`BT /F1 12 Tf ET` then a
  separate `BT … Tj ET`) — is no longer silently dropped from **render and**
  extraction. `BT` resets only the text and line matrices (§9.4.1); the font,
  size, and text state correctly persist (SL-3.TEXT.10 root cause).
- Indic and other complex scripts (Devanagari, Tamil, Bengali, …) now shape
  correctly: reordered matras, conjuncts, `reph`, and split vowels render as
  the font intends. Previously every script tag fell back to Latin shaping,
  so complex-script text rendered unshaped (no reordering, no conjuncts).
- Embedded composite fonts (Type0/Identity-H, the standard carrier for
  complex scripts) now render and extract: `/W` widths, `/CIDToGIDMap`, and
  `/ToUnicode` recovery are honoured; such text was previously dropped from
  output.
- Page operations (`split`, `delete`, `rotate`, `reorder`, `set-metadata`,
  `redact`) no longer silently drop optional-content groups
  (`/OCProperties`): layers survive every rewrite, and the verification line
  asserts it.
- Page selection (`split`, `delete`) now computes its annotation expectation
  from the surviving original pages; previously the expectation could be
  computed against the wrong objects, mis-failing or mis-asserting the
  operation on annotated documents.
