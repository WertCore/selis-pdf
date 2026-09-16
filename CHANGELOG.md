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
- Public-key (PKCS#7) documents now open by **certificate identity**
  (SL-1.ENC.07): a supplied X.509 chain is matched against each recipient's
  CMS `RecipientIdentifier` — `issuerAndSerialNumber` or
  `subjectKeyIdentifier` (RFC 5652 §6) — *before* the unwrap-decrypt, like
  Acrobat/Foxit/PDFium/qpdf/PDFBox select. A `match_by` knob
  (`auto | first_valid | certificate`, default `auto`) makes the policy
  explicit; a bare private key keeps the existing structural behavior, and
  a chain that matches nothing fails typed `RECIPIENT_NO_MATCH` — never a
  silently different recipient. (Shipped, pending human line-by-line
  review; `Session` receipts now carry `matched_by`/`recipient_index` so
  shells can prove who opened.)
- The per-recipient PKCS#7 permission bits are **enforced as policy**
  (SL-1.ENC.09): the active recipient's 4-byte block is intersected with the
  document `/P`, and every content-touching operation (render-to-print, text
  copy/extract — which already gates `embedded_file_data` — annotate,
  redact/edit, form fill) fails typed with the new code 1808
  `PERMISSION_DENIED_BY_CMS` when the block forbids it. A weaker CMS grant
  never raises the PDF-level grant, and each recipient is bound to *its own*
  bits (Bob cannot borrow Alice's); the owner-password / standard-handler
  path keeps the SL-1.ENC.04 `/P`-only semantics. Viewer-UI greying is
  Phase 4 — this is the API gate. (Shipped pending review.)
- Public-key documents from the **RC4 era now open on the read side**
  (SL-1.ENC.08): `adbe.pkcs7.s3` (any `/V ≤ 3`) envelopes whose content is
  RC4-40/RC4-128, 3DES-CBC (2- or 3-key) or RC2-CBC decrypt with the
  recipient key, the CMS `[0]` *implicit* content shape that OpenSSL/Adobe
  actually write is accepted, and RSAES-OAEP key transports (SHA-1 and
  SHA-256 label/MGF1 pairs) unwrap modern CMS libraries' envelopes. The
  wrong-key contract is unchanged — `RECIPIENT_NO_MATCH` is typed for every
  cipher, RC4's no-padding case included, because the key transport unwraps
  before a file key can be derived (design note §5, risk R19 notes). Writing
  is untouched (ADR-P0019): Selis still emits AES only. The remaining
  refusals are the genuinely-broken subset (`aes192`-wrapped ECDH KEKs,
  OAEP with a non-empty label or a non-SHA mask, detached/PBES2/other
  recipient infos, PKCS#12 keystores). Fixtures/DoD are in-repo deterministic
  (`xtask pubkey-fixtures` RC4/TDEA/RC2/OAEP PDFs) — the local corpus holds no
  public-key PDFs at all, so the "three real 2010-era files" clause of the
  task stays open as a human/corpus step, not a claim (design note §6.7).
  (Shipped, pending HUMAN line-by-line review.)
- The pinned oracle-container images for PDFium and pdf.js additionally
  accept `--text <out.txt>` — each driver extracts a page's Unicode text
  as UTF-8 alongside its existing `--dpi`/`<out.png>` render mode. Used by
  the SL-3.CONF.01 CI text legs of the `render-conf` scheduled job; the
  MuPDF text leg needs no driver change (`mutool draw -F txt` already
  extracts). Selis's own behaviour is unaffected — an oracle contract
  change, not a product change; the images' digests must be re-pinned
  once the oracle-images job has rebuilt them after this lands — done in
  SL-3.CONF.02: all five re-recorded from the 2026-09-14 push (run
  34820871908), and the CI legs themselves turned out to be blocked by our
  own harness — the container output bind (`-v` of an unwritten host file
  becomes a directory → `cannot write /out.img`) and the text plan's missing
  `mutool`→`mupdf` pin alias — both fixed in this wave with regression tests,
  so the first post-merge scheduled run is the confirmation.
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

### Changed

- The published conformance ladder (`conformance/REPORT.md`, SL-3.CONF.02,
  re-measured post-SL-3.TEXT.11 by SL-3.CONF.05) is updated: **Text and
  fonts** climbs None → Parse — extraction is now measured across the whole
  3,862-file corpus against MuPDF (970 files at ≥0.99 similarity, 979 at
  ≥0.98 of 1,747 comparable = 56.04%, median 1.000; every pre-merge count
  reproduced within one file) — with Render withheld (the text-bearing slice
  itself still fails the calibrated render bar, 89.9% ≤25% / 37.9% ≤2% at
  150 DPI on the ≥50-char-cohort, and the two-oracle CI legs stay confined to a
  pre-re-pin merge tree that never finished) and Extract withheld (56.04%
  ≥0.98 against a G3 wording SL-0.ORACLE.04 shows sits under the
  oracle-vs-oracle ceiling), and the **Rendering** row now states the post-fix
  reality: Bar A passes (97.2% ≤25% @150), Bar B is inside the oracle-pair
  envelope on the MuPDF leg (≤2% 80.9 vs best pair 82.3), the
  size_skew/blank_selis clusters are closed (17 → 0, 25 → 2, both the typed
  /Redact deviations), and the Render rung waits only on the CI
  two-independent-oracle confirmation — which the fixed output bind and the
  re-pinned oracle images make achievable from the next scheduled run.

### Fixed

- The pinned-container oracle legs (`xtask oracle sweep` / `text-sweep`) finally
  bind their output the way the smoke contract does: an absolute *directory*
  mount (`<parent>:/out`) with the tool writing `/out/<name>`, not a `-v` of a
  host file that does not exist yet — docker turns such a bind into a
  *directory* and the tools died with `cannot write /out.img` / `EISDIR` /
  `Device or resource busy`, which is why every CI `render-conf` leg (runs
  34806315351, 34946893403) reported zero comparable pages after the earlier
  absolutise fix. The text-sweep plan also routed `mutool` through the missing
  `[tool.mutool]` pin instead of `[tool.mupdf]` and mounted its output
  relative; both are resolved through the same `pin_id()`/bind path the render
  legs already use. `oracle check` was already correct; the *sweep* legs were
  not. Tests pin both behaviours, so a leg that silently stops producing
  comparable pages is a diff, not a green row.
- The nightly `fuzz-soak` CI job (red on every target since 2026-09-11) builds
  again: `generic-array` 0.14.8+ marks its crate `#[deprecated]`, and under the
  fuzz job's `RUSTFLAGS='-D warnings'` every `aes::cipher::Block`/digest-`Output`
  *inherent* call in `selis-crypto` (`clone_from_slice`, `as_slice`) became a hard
  error. Those call sites now go through non-deprecated coercions (no behavioural
  change), and `#![deny(deprecated)]` in `selis-crypto` keeps a deprecated call
  from sneaking back in (SL-1.ENC.08's companion repair; SL-1.ROB.06's CI note
   and design note §9 R19 carry the regression note).

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
- Text shown under a **non-identity text matrix** now lays out where every
  oracle lays it. The interpreter composed `show_string`/`Td`/`TD`/`T*` pen
  steps as a raw add into the text matrix's `e`/`f` (`Tm × Translate`); PDF
  §9.4.3 instead **pre-multiplies** the translation (`Translate × Tm`), so a
  step of `adv` moves the user-space origin by `adv × (a, b)` — through the
  matrix's linear part. Under a 90° matrix `0 1 -1 0 x y Tm` the run now reads
  along user-space +y (was +x); the common generator scale-trick
  `12 0 0 12 … Tm /F 1 Tf` (bug1057544, the veraPDF "Hello world" files) lays
  glyphs at 12× the text-space advance, not one-twelfth. Extracted glyph
  positions and the recorded `advance`/`space` (SL-3.TEXT.09 word-gap) metrics
  move together, so word inference stays scale-invariant. Identity-`Tm`
  documents are byte-for-byte unchanged (SL-3.TEXT.11).
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
