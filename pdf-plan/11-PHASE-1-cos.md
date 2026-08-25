# Phase 1 — COS, structure, filters (Weeks 4–9)

**Gate G1 exit criteria:** `selis inspect --json` structurally matches `qpdf --json` on 100% of the
clean corpus · ≥99% of the 10k wild corpus opens or fails with a *typed* error (no panic, no hang,
no OOM) · 24 h fuzz on `selis-pdf-cos` and `selis-pdf-filter` with zero crashes · all encryption revisions
round-trip · every filter has a round-trip property test.

**This phase writes no pixels.** It builds the object layer, and — more importantly — it establishes
that we can survive the real world's PDFs. A renderer on top of a fragile parser is worthless.

---

## 1.COS — The object layer

- [x] **SL-1.COS.01 — Lexer** · owner: AI+
  - **Do:** Tokenise the COS syntax: numbers (including the `--5` and `6.` malformations real files
    contain), names with `#xx` escapes, literal and hex strings (including unbalanced parens and
    odd-length hex), arrays, dicts, `stream`/`endstream`, comments, and the `R`/`obj`/`endobj`
    keywords. Budget-charged per token.
  - **Files:** `crates/selis-pdf-cos/src/lex.rs`
  - **API:** `fn next_token(&mut self, g: &mut BudgetGuard) -> Result<Option<Token>>`
  - **DoD:** Fuzz target `cos_lex` green for 4 h; property test that any byte sequence either
    tokenises or errors, never panics; a table-driven test of the 30 known real-world malformations
    from `docs/specs/MALFORMATIONS.md` (start that file here).
  - **Risk:** The temptation is to be strict. Be *permissive in what you accept, explicit about
    what you accepted* — record every deviation in a `Vec<Deviation>` on the document so the UI can
    say "this file is malformed in these ways" and so the corpus can assert on it.

- [x] **SL-1.COS.02 — Object model** · deps: COS.01 · owner: AI+
  - **Do:** `Obj` = Null | Bool | Int | Real | String(raw bytes) | Name | Array | Dict | Stream |
    Ref. Zero-copy against the source where possible (`selis-bytes::Bytes`). Strings stay **raw
    bytes** — text decoding is a separate, explicit step (PDFDocEncoding vs UTF-16BE vs UTF-8 in
    PDF 2.0), because guessing here corrupts non-Latin metadata.
  - **DoD:** `size_of::<Obj>()` recorded and budgeted; property test for the text-string decoder
    across all three encodings.

- [x] **SL-1.COS.03 — Classic xref table + trailer** · deps: COS.02 · owner: AI+
  - **Do:** Parse `startxref`, the xref table, the trailer, and `/Prev` chains. Tolerate: wrong
    subsection counts, off-by-N object offsets, missing `endobj`, and a `/Prev` cycle (depth- and
    visited-set-guarded).
  - **DoD:** Corpus tags `xref-classic` and `xref-damaged` pass; a cyclic `/Prev` terminates with
    a typed error.

- [x] **SL-1.COS.04 — Cross-reference streams + object streams** · deps: COS.03 · owner: AI+
  - **Do:** PDF 1.5+ xref streams (`/W`, `/Index`, field widths including the 0-width default case)
    and compressed object streams (`/ObjStm`), including the rule that an object stream cannot
    itself be in an object stream.
  - **DoD:** Corpus tag `xref-stream`; a test for the hybrid-reference file case (`/XRefStm`),
    which is the one everybody gets wrong.

- [x] **SL-1.COS.05 — Incremental-update chain as first-class revisions** · deps: COS.03 · owner: AI+
  - **Do:** Build `Vec<Revision>` (byte range + xref + trailer per revision, oldest first) rather
    than a flattened view. Newest wins for resolution, but every revision stays addressable.
  - **API:** `Doc::revisions()`, `Doc::at_revision(n) -> Doc<'_>`
  - **DoD:** A file with 5 incremental updates exposes 5 revisions; rendering revision 3 matches
    what that revision's author saw. **This is the foundation of ADR-P0007 — do not shortcut it.**

- [x] **SL-1.COS.06 — Damaged-file reconstruction** · deps: COS.04 · owner: AI+
  - **Do:** When the xref is unusable, scan the whole file for `N G obj` patterns, rebuild an
    index, recover the trailer by finding a `/Root`, and reconstruct page-tree order. Bounded by
    the budget; reports a `Reconstructed` deviation.
  - **DoD:** Every file in corpus tag `damaged` either opens with a reconstruction note or fails
    with `XREF_UNRECOVERABLE`; **the reconstruction never writes to the user's file** (ADR-P0007,
    `01-ARCHITECTURE.md §13`).
  - **Risk:** This is where competitors differentiate on "opens files Acrobat rejects". Worth doing
    genuinely well; also the single most fuzz-sensitive code in the codebase.

- [x] **SL-1.COS.07 — Resumable parsing over partial sources** · deps: COS.04, SL-0.IO.04 · owner: AI+
  - **Do:** Every parse step tolerates `Availability::Pending`, unwinding with the ranges it needs
    instead of blocking. Implemented as an explicit state machine, not recursion.
  - **API:** `enum ParseStep { Done(T), Need(RangeSet) }`
  - **DoD:** A test that opens a 200 MB linearised file over a simulated network having fetched
    <1% of the bytes; `FaultSource` out-of-order arrival tests pass.
  - **Note:** This task is why the web product feels fast. It is also the task most likely to be
    skipped "for now" and then be a rewrite. Do it in Phase 1.

- [x] **SL-1.COS.08 — Linearisation parsing** · deps: COS.07 · owner: AI
  - **Do:** Detect and use the linearisation dictionary and hint streams for first-page-first
    loading. Validate rather than trust — a lying hint stream must degrade, not corrupt.
  - **DoD:** Linearised corpus opens page 1 with a measured byte count under 5% of the file.

- [x] **SL-1.COS.09 — Object writer** · deps: COS.02 · owner: AI+
  - **Do:** Serialise `Obj` back to COS syntax with correct escaping, number formatting (no locale,
    no exponent notation — PDF forbids it), and stream `/Length` handling.
  - **DoD:** Property test: `parse(write(obj)) == obj` for arbitrary generated objects, including
    strings with every byte value and deeply nested containers.

- [x] **SL-1.COS.10 — `selis inspect --json`** · deps: COS.05 · owner: AI
  - **Do:** The structural dump used by the qpdf oracle: revisions, objects, page tree, streams,
    deviations found.
  - **DoD:** `SL-0.ORACLE.03` comparison passes on the clean corpus.

- [x] **SL-1.COS.11 — Deviation reporting API** · deps: COS.01 · owner: AI
  - **Do:** A typed `Deviation` list on every opened document (`BadXrefOffset`, `MissingEndobj`,
    `ReconstructedIndex`, `LengthMismatch`, `NonConformingEncoding`, …) with byte offsets.
  - **DoD:** Surfaced in `inspect --json`; the corpus asserts specific deviations on specific files.
  - **Note:** This becomes a user-visible "document health" panel and a support-cost reducer.

---

## 1.FILT — Filters and codecs

Each filter task shares a DoD template: streaming (no full-buffer requirement), budget-charged,
round-trip property test where the filter is also an encoder, fuzz target, corpus entry.

- [ ] **SL-1.FILT.01 — Filter pipeline + `/DecodeParms`** · owner: AI+
  - **Do:** Filter chains, per-filter parameters, the abbreviated names (`/Fl`, `/AHx`, …), and
    correct behaviour on a filter that is not the last in a chain producing image data.
  - **DoD:** Chain tests including the pathological "Flate then Flate then Flate" bomb, which must
    hit the budget, not memory.

- [x] **SL-1.FILT.02 — FlateDecode + predictors** · deps: FILT.01 · owner: AI+
  - **Do:** `miniz_oxide` inflate, plus PNG predictors 0–4 and the TIFF predictor. Handle the
    real-world cases: leading garbage bytes, missing zlib header (raw deflate), and truncated
    streams that should yield partial data with a deviation rather than an error.
  - **DoD:** Round-trip property test; corpus `filter-flate`; a truncated-stream test.
  - **Risk:** "Truncated Flate yields what it decoded so far" is what every other reader does and
    what users expect. Diverging here means files that "only fail in Selis".

- [x] **SL-1.FILT.03 — LZWDecode** · deps: FILT.01 · owner: AI
  - **Do:** Including `/EarlyChange` 0 and 1, and the early-code-reuse malformation.
- [x] **SL-1.FILT.04 — ASCIIHex, ASCII85, RunLength** · deps: FILT.01 · owner: AI
- [x] **SL-1.FILT.05 — DCTDecode** · deps: FILT.01 · owner: AI+
  - **Do:** `zune-jpeg`, plus the PDF-specific parts everyone gets wrong: 4-component Adobe APP14
    transform detection, inverted CMYK from Photoshop, and 12-bit samples.
  - **DoD:** Corpus `filter-dct` including a CMYK-inverted file and a 12-bit file.
- [ ] **SL-1.FILT.06 — CCITTFaxDecode** · deps: FILT.01 · owner: AI+
  - **Do:** Own implementation. Group 3 1-D, Group 3 2-D, Group 4; `/K`, `/BlackIs1`,
    `/EncodedByteAlign`, `/Columns`, `/Rows`, damaged-row recovery.
  - **DoD:** Round-trip against a generated corpus; fuzz target; decoded output matches Ghostscript
    on the fax corpus.
- [ ] **SL-1.FILT.07 — JBIG2Decode (generic region)** · deps: FILT.01 · owner: AI+
  - **Do:** Own implementation (ADR-P0018) — generic region decoding with arithmetic and MMR
    coding, plus the embedded-in-PDF stream organisation and `/JBIG2Globals`.
  - **DoD:** Matches Ghostscript on the JBIG2 corpus; fuzz target; budget-bounded.
  - **Note:** Symbol dictionary / text region / refinement land in Phase 2 (`SL-2.FILT.01`). Generic
    region alone covers the majority of scanned-document usage.
- [ ] **SL-1.FILT.08 — JPXDecode behind the WASM sandbox** · deps: FILT.01, SL-0.SBX.06 · owner: AI+
  - **Do:** OpenJPEG compiled to WASM, driven through the Tier-2 sandbox with a hard memory cap.
    Never linked natively.
  - **DoD:** A malformed-JPX corpus is contained (no host crash, no unbounded memory); output
    matches Ghostscript within tolerance on the valid set.
- [ ] **SL-1.FILT.09 — Crypt filter** · deps: FILT.01, ENC.02 · owner: AI+
  - **Do:** The `/Crypt` filter and the identity crypt filter, including per-stream crypt filter
    selection and the metadata-not-encrypted case.

---

## 1.ENC — Encryption and permissions

- [ ] **SL-1.ENC.01 — Standard security handler: revisions 2–4** · owner: HUMAN
  - **Do:** RC4 40/128 and AESV2. Key derivation (Algorithm 2), user/owner password validation,
    `/P` permission bits, `/EncryptMetadata`.
  - **DoD:** Corpus `encrypted-legacy` opens with both user and owner passwords; a test that a
    wrong password yields `WRONG_PASSWORD` and never a partial decrypt.
- [ ] **SL-1.ENC.02 — Standard security handler: revision 6 (AESV3/256)** · deps: ENC.01 · owner: HUMAN
  - **Do:** Algorithm 2.A, the hardened hash (Algorithm 2.B), `/Perms` validation, and the
    key-length rules. This is the only handler we *write* (ADR-P0019).
  - **DoD:** Round-trip encrypt→decrypt; interop test — a file we encrypt opens in PDFium and
    Acrobat, and files they encrypt open in ours.
- [ ] **SL-1.ENC.03 — Public-key (PKCS#7) handler, read-only** · deps: ENC.02 · owner: HUMAN
- [x] **SL-1.ENC.04 — Permission semantics as policy, not as a lie** · deps: ENC.01 · owner: AI+
  - **Do:** Surface `/P` bits honestly. We honour them by default and expose an explicit,
    logged override for the owner-password case. Do not pretend the bits are security.
  - **DoD:** Documented behaviour; UI copy reviewed; the override is recorded in the oplog.
  - **Note:** Decide the product posture here deliberately. Honouring permissions when the user has
    the owner password and legitimately owns the file is user-hostile; ignoring them silently is
    the thing that gets a vendor sued. Explicit, logged override is the defensible middle.

---

## 1.DOC — The document model

- [x] **SL-1.DOC.01 — Catalog, page tree, inheritance** · deps: COS.05 · owner: AI+
  - **Do:** Page-tree walk with `/Count` validation (never trust it), inherited attributes
    (`Resources`, `MediaBox`, `CropBox`, `Rotate`), and recovery when the tree is a cyclic or
    malformed graph.
  - **DoD:** Page count matches qpdf on 100% of the clean corpus; a cyclic page tree terminates.
- [x] **SL-1.DOC.02 — Cycle-safe resolution with a visited set** · deps: DOC.01 · owner: AI+
- [x] **SL-1.DOC.03 — Name trees, number trees, destinations, outlines** · deps: DOC.01 · owner: AI
- [x] **SL-1.DOC.04 — Optional content (OCG/OCMD)** · deps: DOC.01 · owner: AI
  - **Do:** The OC model and visibility evaluation, including usage-application dictionaries.
    Needed early because it affects both rendering and redaction correctness.
- [x] **SL-1.DOC.05 — Metadata: Info dictionary + XMP** · deps: DOC.01 · owner: AI
  - **Do:** Read both, reconcile conflicts, and expose which one won. XMP parsing is budget-bounded
    XML — treat it as hostile input (billion-laughs, external entities: **disabled**).
  - **DoD:** An XXE/entity-expansion test suite passes; no network fetch is ever attempted.
- [x] **SL-1.DOC.06 — Structure tree (tagged PDF), read** · deps: DOC.01 · owner: AI+
  - **Do:** Parse `/StructTreeRoot`, the element hierarchy, `/K` kids including MCID references and
    OBJR, role maps, and attribute dictionaries. Per ADR-P0031 this is Phase 1 work, not a
    Phase 9 bolt-on.
  - **DoD:** Corpus `tagged` produces a structure tree matching the file's declared hierarchy;
    exposed via `inspect --json`.
- [x] **SL-1.DOC.07 — Embedded files + attachments (inventory only)** · deps: DOC.01 · owner: AI
  - **Do:** Enumerate without extracting; extraction is gated by policy (ADR-P0020).
- [x] **SL-1.DOC.09 — Conformance rule registry + the Phase-1-checkable subset** · deps: DOC.05, DOC.06 · owner: AI+
  - **Do:** Implement the registry of `20-CONFORMANCE-PROGRAM.md §4a`. Each rule declares the
    engine area and ladder level it needs; rules whose dependency is unmet return `Unevaluated`,
    never `Pass`. Populate the structural and structure-tree classes now — encryption, JavaScript,
    `/Launch`, external references, embedded files, XMP/Info consistency, `pdfaid` claim validity,
    tag hierarchy, reading order, alt text, language marking.
  - **Sources:** the Matterhorn Protocol (free, enumerates every PDF/UA failure condition) and the
    veraPDF rule set (free, open source). The paid ISO texts are needed to *claim* certified
    conformance, not to write these rules — see `27-COST-AND-LICENSING.md §2.3`.
  - **API:** `fn evaluate(doc: &Doc, profile: Profile, budget) -> Vec<RuleResult>` where
    `RuleResult = Pass | Fail{detail} | Unevaluated{needs: (Area, Level)}`
  - **DoD:** Runs on the Isartor corpus; every rule the registry claims to evaluate agrees with
    veraPDF; every rule it cannot yet evaluate is reported as `Unevaluated` with the blocking area
    named. `xtask conformance report` shows the evaluable percentage, and it rises automatically as
    Phase 2/3 land.
  - **Note:** This is also the engine behind the **Privacy Risk Scanner** (`SL-1A.TOOL.09`) — the
    same structural checks that find a PDF/A violation find embedded JavaScript, phone-home
    references, and leftover content in prior revisions. Build once, ship twice.

- [x] **SL-1.DOC.08 — Page labels, article threads, viewer preferences** · deps: DOC.01 · owner: AI

---

## 1.ROB — Robustness campaign (the real G1 work)

- [ ] **SL-1.ROB.01 — Wild-corpus open sweep** · deps: COS.06, SL-0.CORP.05 · owner: AI+
  - **Do:** Open all 10k wild files under the Viewer budget. Classify every outcome. Target: ≥99%
    open-or-typed-error, 0 panics, 0 hangs, 0 OOMs.
  - **DoD:** A report grouping failures by root cause; each root cause is a filed task; the
    residual <1% is enumerated and understood, not hand-waved.
- [ ] **SL-1.ROB.02 — 24-hour fuzz campaign, all Phase-1 targets** · deps: SL-0.SEC.02 · owner: AI
  - **DoD:** Zero crashes; coverage report per target; new corpus entries minted from interesting
    inputs found.
- [x] **SL-1.ROB.03 — Budget-exhaustion test suite** · deps: SL-0.SBX.01 · owner: AI+
  - **Do:** A crafted set: xref bomb, `/Prev` chain of 10 000, object stream referencing itself,
    2 GB `/Length`, 10 000-deep array nesting, a Flate bomb, a name with a 100 MB escape sequence.
  - **DoD:** Each terminates with `BudgetExceeded` in bounded memory and bounded time. **These
    files go in the corpus permanently** — they are the regression suite for the sandbox.
- [ ] **SL-1.ROB.04 — Memory-ceiling test on the huge corpus** · owner: AI
  - **Do:** Prove peak RSS stays under the viewer profile on 500 MB+ documents.
- [ ] **SL-1.ROB.05 — Malformation catalogue** · deps: COS.11 · owner: AI+
  - **Do:** Finish `docs/specs/MALFORMATIONS.md`: every real-world deviation encountered, how we
    handle it, and which competitor does what. This document is a genuine competitive asset and
    the onboarding text for every future parser engineer.

---

## 1.OSS — Open-sourcing the parser crates (ADR-P0030)

- [ ] **SL-1.OSS.01 — Split `selis-pdf-cos`, `selis-pdf-filter` for publication** · owner: AI+
  - **Do:** Confirm they depend only on L0/L1 and carry no proprietary code; add Apache-2.0
    headers, a public README, and an API-stability statement.
  - **DoD:** `cargo publish --dry-run` clean; `check-layers` proves no proprietary dependency.
- [ ] **SL-1.OSS.02 — Public repo, CI, and issue triage rota** · deps: OSS.01 · owner: HUMAN
- [ ] **SL-1.OSS.03 — Submit to OSS-Fuzz** · deps: OSS.02, SL-0.SEC.03 · owner: HUMAN
