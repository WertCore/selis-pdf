# Changelog

All notable user-visible changes to `selis` are documented here.
The format is a plain reverse-chronological list of unreleased/released
sections; every entry states what changed and what it means for the user.

## [Unreleased]

### Added

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
