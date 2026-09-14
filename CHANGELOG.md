# Changelog

All notable user-visible changes to `selis` are documented here.
The format is a plain reverse-chronological list of unreleased/released
sections; every entry states what changed and what it means for the user.

## [Unreleased]

### Added

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

### Fixed

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
