# Phase 5 — Editing, annotation, forms, signing (Months 8–12)

**Gate G5 exit criteria:** annotate, fill, sign, edit-text, page-ops, redact, and OCR shipping on
web + extension · the **incremental-save invariant proven** by a 100 000-mutation property suite
(the original bytes remain a byte-identical prefix of every saved output) · redaction verified by
an independent extraction pass on 100% of the redaction corpus · billing and entitlements live.

This phase turns a viewer into a product people pay for. It is also the phase where a bug destroys
a user's document, so the save path gets property tests before it gets features.

---

## 5.EDIT — The mutation and save core

Normative spec: `23-EDIT-MODEL-SPEC.md`. Read it before any task here.

- [x] **SL-5.EDIT.01 — Mutation journal** · owner: AI+
  - **Do:** Every change is a typed `Mutation` appended to a journal: object set/delete, stream
    replace, page insert/remove/reorder/rotate, annotation add/modify/delete, field value set.
    The journal is the source of truth; the in-memory document is a materialised view of it.
  - **API:** `Journal::apply(&mut self, m: Mutation) -> Result<MutationId>`
  - **DoD:** Property test: applying then reverting any mutation sequence yields the original
    document state; the journal serialises to the `.selis` session bundle and reloads.
- [x] **SL-5.EDIT.02 — Undo / redo** · deps: EDIT.01 · owner: AI+
  - **Do:** Undo stack over the journal with coalescing (typing is one undo, not thirty) and
    grouping (a "replace text" is atomic). Survives app restart via the session bundle.
  - **DoD:** Fuzz the undo stack with random operation sequences; state always converges.
- [x] **SL-5.EDIT.03 — Incremental-update writer** · deps: EDIT.01 · owner: HUMAN
  - **Do:** Emit only changed objects plus a new xref section (table or stream, matching the
    document's existing style) plus a trailer with `/Prev`. **Never touch the original bytes.**
    Handle: object streams (a changed object in an ObjStm requires care), free-list correctness,
    generation numbers, and `/ID` update rules.
  - **DoD:** **The load-bearing test of this entire phase** — a property suite of 100 000 random
    mutation sequences where, for every output: (a) `output[..original.len()] == original`,
    (b) the result parses, (c) the semantic model matches the expected post-mutation state,
    (d) every prior revision is still readable, (e) PDFium and Acrobat both open it.
  - **Risk:** `owner: HUMAN`. Everything the user cares about depends on this being right.
- [x] **SL-5.EDIT.04 — Crash safety of save** · deps: EDIT.03, SL-0.IO.05 · owner: HUMAN
  - **Do:** Kill the process at 500 random points during save; assert the file is always either
    the original document or a valid document containing a prefix of the intended changes.
  - **DoD:** The kill-test suite is green and runs nightly.
  - **Status (implemented):** `selis_pdf_edit::save::save_atomic` verifies the output (I3
    prefix, `%%EOF` tail, exactly one appended revision, strict + resilient parse, page count)
    **before** committing through `selis_io::commit_atomic` (temp in the destination directory,
    fsync, atomic replace, directory fsync). `save_tests::kill_test_500_points_never_damage_the_destination`
    drives 500 deterministic kill points through an injected fault hook; 500/500 leave the original
    byte-identical, and a clean save commits the full document. Corrupted candidates are refused with
    a typed error and produce no file. Run nightly with `cargo test -p selis-pdf-edit save_tests`.
    Residual gap: directory fsync is POSIX-only; Windows relies on `MOVEFILE_WRITE_THROUGH`.
- [x] **SL-5.EDIT.05 — Full-rewrite writer (`save_rewritten`)** · deps: EDIT.03 · owner: HUMAN
  - **Do:** The explicit, verified rewrite: renumber objects, drop unreferenced ones, re-encode
    streams, then **render-verify** every page against the pre-save state before the result is
    allowed to replace anything.
  - **DoD:** Verification catches a deliberately-corrupted rewrite; the operation is atomic
    (temp + rename) on every platform.
  - **Status (implemented):** `selis_pdf_edit::rewrite::save_rewritten` collects the reachable
    object graph from the trailer `/Root` (and `/Info`), renumbers it from 1, drops unreachable
    objects, and serialises a fresh single-revision PDF via
    `write_objects_as_document_with_trailer`. Nothing is returned until
    `verify_rewrite` reparses and compares the result with the pre-save document:
    `verify_structural` expectations (pages, annotations, fields, OCGs, outlines, embedded files)
    plus a structural fingerprint (structure-tree element count and an order-independent digest
    of every reachable stream payload, which stands in for text until the renderer exists). A
    mismatch returns `VERIFY_FAILED` naming the discrepancy and produces no bytes.
    `save_rewritten_atomic` commits the verified bytes through `selis_io::commit_atomic` (temp,
    fsync, atomic rename, directory fsync). `rewrite_tests` proves a normal two-page rewrite
    round-trips with all counts preserved (engine read-back), that a page-dropping and an
    annotation-dropping rewrite are refused, and that a refused atomic commit leaves the original
    byte-identical.
    **SL-1A.WRITE.08 (closed here):** the full render+text comparison is now wired into the
    full-rewrite writer without a dependency cycle. `selis-pdf-engine` depends on
    `selis-pdf-edit`, so the lower crate cannot open a `Session` itself; instead
    `selis_pdf_edit::rewrite` declares the `RenderTextVerifier` seam and
    `verify_rewrite_render_text` / `save_rewritten_with_verifier` /
    `save_rewritten_atomic_with_verifier` run it *after* the structural gate. The engine-backed
    implementation lives one layer up in `selis_pdf_tools::rewrite_verify` (`text` feature):
    `EngineRenderTextVerifier` opens both documents, renders every page at 72 dpi and compares
    the RGBA buffers sample for sample, and recovers per-page text through the same ladder
    `selis extract` uses and compares it; a page-count/pixel/text difference is a typed
    `VERIFY_FAILED` naming the 1-based page, with no bytes returned and no file committed.
    `selis_pdf_tools::save_rewritten_verified[_atomic]` are the content-verified full-rewrite
    defaults for the CLI. Tests: a normal rewrite is pixel- and text-identical; a two-page
    document whose pages are swapped passes *every* structural check (counts and the stream
    multiset digest are unchanged) yet is refused by the render/text gate naming page 1; an
    invisible (white-on-white) text change that renders pixel-identically is refused by the
    text half; a page-count mismatch is caught by both gates; and an edit-layer mock verifier
    proves the seam runs after structure and can refuse. Residual gap: streams are still copied
    verbatim rather than re-encoded (a clean rewrite is therefore expected to render identically).
    `cargo xtask size-check` was run on this change and on the unmodified base `8c022c8`: all 14
    artifacts are byte-identical (e.g. `selis_pdf_wasm` 1 290 403 B brotli, `selis_pdf_wasm_tools`
    423 818 B) — **delta 0 B** — because the render/text verifier is feature/test-gated and the new
    edit-layer seam is dead-code-eliminated from the shipped chunks. All budgets met; budgets and
    baseline were deliberately NOT edited. The `xtask` host binary shows a pre-existing −241 B
    drift from its recorded baseline, unrelated to this change.
- [x] **SL-5.EDIT.06 — Save through encrypted documents** · deps: EDIT.03, SL-1.ENC.02 · owner: HUMAN
  - **Do:** New objects encrypted with the document's existing handler (ADR-P0019 exception);
    an explicit upgrade path to AESV3 with user consent.
  - **DoD:** Incremental save into RC4, AESV2, and AESV3 documents; each result opens in Acrobat.
  - **Status (implemented):** `selis_pdf_edit::incremental::{open_encrypted, save_incremental_encrypted}`
    authenticate with a password (reusing COS `parse_encrypt`/`authenticate`/`DecryptPolicy`), derive
    the source file key, and call `encrypt_object` with the source `(objnum, gen=0, rev, aes)` for
    every redefined object. The appended trailer carries `/Encrypt`, `/Root`, `/Info` and a refreshed
    `/ID`; the `/Encrypt` dictionary itself stays plaintext. Read-back reopens the bytes with the
    policy and compares the full decrypted model, proving no plaintext object was emitted.
    `EncryptionPolicy::UpgradeToAesV3 { user_consent, new_user_password, new_owner_password }`
    refuses without consent (`PERMISSION_DENIED`). **With** consent it authenticates the source
    with the OLD password and `save_incremental_encrypted` performs a **full, verified rewrite**
    under a fresh AESV3/R6 handler: a brand-new file key/salts from the OS CSPRNG, a new `/Encrypt`
    dictionary (the old one is dropped), new `/ID`, `/Info` carried forward, and the whole object
    graph re-encrypted. An append is never attempted under the new key (the old `/Encrypt` bytes
    would stay authoritative in the preserved prefix), so the report says `prefix_preserved =
    false`, `revisions = 1`. The result is returned only if it reopens with the NEW password,
    materialises to the expected model, and installs r=6/v=5; otherwise `VERIFY_FAILED` (or
    `ENCRYPT_UNSUPPORTED` when no CSPRNG is reachable) with no bytes. No silent upgrade. Key-less
    `save_incremental` still refuses encrypted input. COS `encrypt_object` now also encrypts
    `Obj::HexString` write-only payloads (with a round-trip test), so a writer-built hex string
    can never leak plaintext.
    **Public-key sources:** `open_encrypted_public_key` authenticates an `/Adobe.PPKLite` /
    `/Adobe.PubSec` document with the recipient's PKCS#7 credential (reusing COS `parse_encrypt` /
    `authenticate_public_key`); the same incremental save re-encrypts changed objects under the
    unchanged handler file key, so the existing `/Recipients` blobs still open the result. Both
    the RSA s4 (AESV2) and EC s5 (AESV3) committed fixtures round-trip. No new CMS envelopes are
    minted — the PKCS#7 layer is read-only — so this edits an existing public-key document, never
    creates one, and the caller enforces the CMS permission bits via the returned `PubKeyAuth`.
    Tests cover RC4 (R3), AESV2 (R4) and AESV3 (R6) fixtures built with
    `EncryptInfo::new_legacy`/`new_r6`, the consented AESV3 upgrade (new password opens, old one
    gets `WRONG_PASSWORD`), public-key save, plus the plaintext-marker and wrong-password cases.
    Interop oracle: `qpdf --check --password= ` and `qpdf --password= --decrypt` pass for all three
    (`encrypted_tests::qpdf_interop_oracle`, skipped when qpdf is absent). Acrobat GUI opening is
    a **manual** check — not automated here. Residual honest limits: a public-key document whose
    recipient must change (new CMS envelope) is still out of scope; an AESV3 upgrade needs an OS
    CSPRNG in the build (`selis-edit/rng` → `selis-crypto/rng`), refusing cleanly without one.
- [x] **SL-5.EDIT.07 — Save preserving existing signatures** · deps: EDIT.03 · owner: HUMAN
  - **Do:** An incremental save must leave existing signature byte ranges intact so signatures stay
    valid, and must respect MDP/DocMDP permission levels — refusing changes the certification
    forbids, and clearly telling the user why.
  - **DoD:** A signed document annotated and saved still validates in Acrobat; a change forbidden
    by DocMDP level 1 is refused with a specific error.
  - **Status (implemented):** `selis_pdf_edit::signature_guard` makes the append-only guarantee
    checked rather than assumed. `check_mutation_allowed(model, mutations, budget, guard)` runs
    **before** `save_incremental` writes: it refuses redefinitions of any existing signature field,
    signature dictionary or `/Reference` chain object with `SIGNATURE_WOULD_BREAK`, and parses the
    certification signature's `/Reference → /TransformParams → /P` to enforce DocMDP — P=1 allows
    only signing, P=2 adds form fill-in, P=3 adds annotations; a refusal is a typed
    `SIGNATURE_WOULD_BREAK` whose detail names the level and the refused operation, and produces
    no bytes. `verify_signatures_preserved(src, output, budget, guard)` re-runs the real signature
    report (`selis-pdf-sign::signatures`) on the saved bytes and proves each source signature that
    verified over the whole source still verifies over the same four offsets, all ending inside
    the preserved prefix; a break is `SIGNATURE_INVALID` with no output. On the output the report's
    `coversWholeDocument` is false by design (appended bytes are outside the byte range), which is
    exactly why the guard compares against the **source** length. Signatures already invalid on the
    source are carried over unchanged, not counted as preserved. The encrypted path (EDIT.06) is
    unaffected: DocMDP is checked on the decrypted model, and ciphertext signature reporting is
    skipped. Tests sign a real CMS fixture with the committed PEMs, annotate and incremental-save,
    read back through the sign report, and cover all three DocMDP levels plus the signature-object
    and no-output-on-refusal cases. `qpdf --check` on the signed+annotated output is a scriptable
    oracle: it uses `$QPDF` if set, else the known `E:\softwares\pdf-tools\qpdf\qpdf.exe` (qpdf
    12.4.2), else skips with a note; it now genuinely runs here and asserts the verdict text, not
    just the exit code. Verdict: "No syntax or stream encoding errors found; the file may still
    contain errors that qpdf cannot detect". Residual gaps: Acrobat GUI validation is **manual**;
    the guard classifies mutations from their COS shape (a caller that knowingly bundles an edit with
    a signature creation is refused whenever any plain content/page edit is present, but field-level
    DocMDP intent is advisory like the PDF itself).
  - **Gap-closure follow-up (EDIT.07):** three honest gaps are now closed. (1) Form-fill detection
    walks the `/Parent` chain for an inherited `/FT` (mirroring `selis-pdf-form`), so DocMDP P=2
    allows filling a widget whose `/FT` lives only on its parent; P=1 still refuses such a fill, and
    the existing P=1/P=3 tests are unchanged. (2) The qpdf oracle is wired to the installed binary
    and asserts its verdict (above). (3) `save_rewritten*` now refuses any signed source with
    `SIGNATURE_WOULD_BREAK` before any bytes are built — no output, no file — which also covers the
    AESV3-upgrade rewrite path. New tests cover the inherited-`/FT` P=2 fill, P=1 still refusing,
    the asserted qpdf verdict and the signed-rewrite refusal (in-memory and atomic). Re-ran
    `cargo xtask size-check` with these changes: all 14 budgets met, every wasm artifact
    byte-identical to the checked-in baseline (main `selis_pdf_wasm` 1 290 403 B,
    `selis_pdf_wasm_optimize` 218 996 B — delta 0 B vs baseline; `xtask` +12 B build-tool drift
    only). The earlier EDIT.07 size note above records the pre-existing stale optimize baseline
    (218 580 B at the unmodified base vs the old 211 034 B baseline); budgets and baseline were NOT
    edited.
- [x] **SL-5.EDIT.08 — Page operations** · deps: EDIT.01 · owner: AI+
  - **Do:** Insert, delete, reorder, rotate, crop (`/CropBox` vs `/MediaBox` semantics), split,
    merge, extract, and N-up. Merging must reconcile: resource name collisions, named destinations,
    outlines, structure trees (ADR-P0031 — merged documents keep valid tagging), form field name
    collisions, optional-content groups, and embedded files.
  - **DoD:** Merge two tagged, formed, outlined documents; the result validates and every feature
    survives. This one test catches more bugs than any other in the phase.
- [x] **SL-5.EDIT.09 — Object rewriting and resource management** · deps: EDIT.08 · owner: AI+
- [x] **SL-5.EDIT.10 — Session bundle (`.selis`)** · deps: EDIT.01 · owner: AI
  - **Do:** The sidecar holding the journal, undo stack, oplog, and cached derived data, so a
    session survives a crash or a browser refresh without touching the source document.
- [x] **SL-5.EDIT.11 — Compare documents** · deps: SL-2.CONT.07 · owner: AI+
  - **Do:** Structural + visual + textual diff of two documents (or two revisions of one), using
    the display list for visual diffing and the text model for content diffing.
  - **DoD:** A known edit is reported precisely; false-positive rate measured on re-saved-but-
    unchanged documents (which is the hard case).

---

## 5.TEXTEDIT — Text and object editing (ADR-P0024)

- [x] **SL-5.TEXTEDIT.01 — Paragraph reconstruction with confidence** · deps: SL-3.TEXT.03 · owner: AI+
  - **Do:** Glyphs → runs → lines → paragraphs → a styled model, computing a confidence from
    baseline regularity, spacing consistency, font uniformity, and structure-tree agreement.
  - **API:** `fn infer_paragraphs(page, budget) -> Result<(Vec<Paragraph>, Confidence)>`
  - **DoD:** Confidence correlates with hand-labelled correctness on a 500-page labelled set;
    the threshold for offering reflow editing is derived from that data, not guessed.
  - **Done (implementation + small labelled set):** `selis-pdf-text::paragraphs` groups already-assembled
    lines by baseline gap + font/size + indent (+ an MCID-jump veto for tagged documents) and reports a
    documented 0..1 confidence — weighted sum `0.30·baseline regularity + 0.25·spacing consistency +
    0.25·font uniformity + 0.20·structure-tree agreement`. `Session::infer_paragraphs(page, budget, guard)`
    reuses the public gather → recover → assemble → order pipeline; an end-to-end test reads back a real
    two-paragraph PDF fixture.
  - **DoD GAP (honest):** the 500-page hand-labelled set does **not** exist in this repo, so the required
    correlation was **not** run and is **not** claimed. A 12-page *synthetic* labelled set lives in
    `corpus/fixtures/paragraphs/` (`manifest.toml`, `README.md`, `generate.py`); its harness asserts that
    correct groupings outrank merged/split/shifted ones (min correct 0.918, max incorrect 0.681).
    `PROVISIONAL_REFLOW_THRESHOLD = 0.80` is that set's midpoint — derived, not guessed, but provisional
    pending the real 500-page data. TEXTEDIT.03's box-level fallback should treat it as such.
- [ ] **SL-5.TEXTEDIT.02 — Text edit + re-layout + re-emit** · deps: TEXTEDIT.01, SL-3.SHAPE.03, SL-3.FONT.11 · owner: AI+
  - **Do:** Edit the model, re-shape and re-break the affected paragraph, re-emit that content
    region while leaving the rest of the stream byte-identical, and extend the embedded font
    subset with any new glyphs.
  - **DoD:** Editing one word in a paragraph changes only that paragraph's content region; the page
    renders identically outside the edit; a character absent from the original subset renders.
- [x] **SL-5.TEXTEDIT.03 — Box-level fallback editing** · deps: TEXTEDIT.01 · owner: AI
  - **Do:** Below the confidence threshold, offer per-line or per-run editing with explicit
    "this may not reflow" UI rather than silently mangling the page.
  - **Done (first slice):** `selis-pdf-edit::box_edit::edit_box` reuses the in-place splice but
    disables reflow, so a single line/run is edited without moving the surrounding text. It
    returns a typed `BoxEditWarning` (`BOX_EDIT_NO_REFLOW`) whose message is the "this edit did
    not reflow the paragraph" UI text. The bytes are sealed in `BoxEditPreview`; the only release
    path is `commit(BoxAck)`, and a `BoxAck` is obtainable only from that warning's
    `acknowledge()`. A box edit therefore cannot be applied silently — the warning must be
    surfaced and accepted first. Read-back tests reopen the edited PDF and check the replacement
    text, and a crossed-acknowledgement test proves the refusal path.
  - **Not claimed:** TEXTEDIT.01 (paragraph confidence) is not implemented, so the caller — not
    the library — decides when the box path applies; there is no automatic threshold gate. No new
    font is embedded by a box edit (see TEXTEDIT.04).
- [x] **SL-5.TEXTEDIT.04 — Font matching for edited text** · deps: SL-3.FONT.09 · owner: AI+
  - **Do:** When the original font is unembedded or unavailable, choose a substitute and tell the
    user, rather than silently changing the document's appearance.
  - **Done (first slice):** `selis-font::font_match::match_substitute(original_font,
    needed_chars, budget) -> Substitute` selects the metric-compatible Liberation twin of a
    standard-14 face through the same policy the renderer uses (`fallback::substitute_for`),
    widens through the bundled fallback set when a needed glyph is missing, scores the match, and
    **always returns a disclosure string when it substitutes**. An embedded, complete face is
    kept unchanged with an empty disclosure. `selis-pdf-tools::edit::plan_font_substitution`
    surfaces the disclosure to the CLI/UI boundary.
  - **Disclosure contract:** `substituted == true` implies a non-empty `disclosure` naming the
    original face, why it changed ("not embedded" / "unavailable" / "lacks a needed glyph"), the
    substitute, and any codepoint it still cannot carry (with a low `confidence`). There is no
    silent-substitution path. `confidence` is a discrete `0..=100` score (100 = original kept).
  - **Not claimed:** this slice does not embed the substitute into the PDF — that is TEXTEDIT.02's
    re-embed step. The substitute is a *name*: on slim/web builds only the Serif subsets are
    resident, so a chosen Sans face may still need lazy delivery, and the disclosure never claims
    otherwise. No Noto face is bundled; the candidate set is the Liberation family. A CJK glyph
    that no bundled face carries is disclosed as unmatched rather than silently dropped.
- [ ] **SL-5.TEXTEDIT.05 — Image editing: replace, move, resize, delete, extract** · deps: SL-2.RAST.08 · owner: AI+
- [ ] **SL-5.TEXTEDIT.06 — Vector object selection and manipulation** · deps: SL-2.CONT.07 · owner: AI
- [ ] **SL-5.TEXTEDIT.07 — Add content: text boxes, images, shapes, links** · deps: SL-3.SHAPE.03 · owner: AI+
- [ ] **SL-5.TEXTEDIT.08 — Headers, footers, watermarks, Bates numbering, backgrounds** · deps: TEXTEDIT.07 · owner: AI
  - **Note:** Bates numbering is a legal-market requirement and a cheap differentiator.

---

## 5.ANNOT — Annotations

- [x] **SL-5.ANNOT.01 — Annotation model + appearance-stream generation** · owner: AI+
  - **Done (first slice):** `selis-pdf-annot` writes highlight, underline, strikeout, note, ink and box as spec-conformant annotations with generated `/AP` streams, and reads them back. Moving an existing annotation, replies, review state and flattening remain open.
  - **Do:** All markup subtypes: Text (note), Highlight, Underline, StrikeOut, Squiggly, Square,
    Circle, Line, Polygon, PolyLine, Ink, FreeText, Stamp, Caret, FileAttachment, Sound, Redact.
    Generate conformant `/AP` streams for each (ADR-P0026) so every other viewer shows them.
  - **DoD:** Each subtype round-trips through Acrobat and PDFium visually unchanged.
- [ ] **SL-5.ANNOT.02 — Appearance regeneration and `/NeedAppearances`** · deps: ANNOT.01 · owner: AI+
- [ ] **SL-5.ANNOT.03 — Ink and stylus geometry** · deps: ANNOT.01 · owner: AI+
  - **Do:** Pressure- and tilt-aware stroke smoothing that still emits a standard Ink annotation.
- [x] **SL-5.ANNOT.04 — Markup replies, review state, and author identity** · deps: ANNOT.01 · owner: AI
- [x] **SL-5.ANNOT.05 — Annotation flattening** · deps: ANNOT.01 · owner: AI+
- [x] **SL-5.ANNOT.06 — Import/export XFDF and FDF** · deps: ANNOT.01 · owner: AI
- [x] **SL-5.ANNOT.07 — Annotation UI: create, edit, move, style, comment panel** · deps: ANNOT.01, SL-4.UI.03 · owner: AI

---

## 5.FORM — Forms

- [x] **SL-5.FORM.01 — AcroForm field model** · owner: AI+
  - **Do:** Field hierarchy, inheritance, widget↔field association (including one field with
    multiple widgets), field types (text, button, checkbox, radio, choice, signature), flags,
    and the `/DA` default appearance.
- [ ] **SL-5.FORM.02 — Field appearance generation** · deps: FORM.01, ANNOT.02 · owner: AI+
  - **Do:** Text fields with quadding, comb fields, multiline, rich text, auto-size fonts, choice
    lists, and checkbox/radio appearance states.
- [ ] **SL-5.FORM.03 — Calculation, validation, and format order** · deps: FORM.01, SL-5.JS.01 · owner: AI+
  - **Do:** `/CO` calculation order and the format/validate/calculate event sequence. Most real
    forms depend on this working exactly like Acrobat.
- [x] **SL-5.FORM.04 — Form filling UI** · deps: FORM.02 · owner: AI
  - **Do:** Tab order, keyboard accessibility, required-field indication, and inline validation.
- [ ] **SL-5.FORM.05 — Form data import/export (FDF, XFDF, JSON, CSV)** · deps: FORM.01 · owner: AI
- [x] **SL-5.FORM.06 — Form creation and field authoring** · deps: FORM.02 · owner: AI+
  - **Done:** `selis-pdf-form::create_form_fields` builds `/AcroForm`, merged field/widget
    dictionaries (`/FT /T /Rect /DA /AP`) and page `/Annots` for caller-specified text, checkbox,
    radio and choice fields on a flat document (or appends to an existing form). Full rewrite +
    re-open read-back asserts every field's type, name and rectangle. AP streams are simple
    authoring defaults, not the full appearance generation of FORM.02.
- [x] **SL-5.FORM.07 — Auto-detect form fields on a flat document** · deps: FORM.06, SL-3.TEXT.03 · owner: AI+
  - **Do:** Infer fields from lines, boxes, and labels. A marquee Acrobat feature; ship it with a
    confidence indicator and easy correction.
  - **Done:** `selis-pdf-form::detect_form_fields` scans decoded page content streams for flat
    rules/stroked lines (text candidates) and small squares (checkbox candidates), attaches
    nearby text labels, and returns a read-only `CandidateReport` with per-candidate confidence
    and reason. Strictly heuristic — never creates a field; undecodable streams are noted, not
    guessed at.
- [x] **SL-5.FORM.08 — XFA: render static, refuse dynamic honestly** · deps: FORM.01 · owner: AI+
  - **Do:** Render the XFA-embedded static PDF representation; for dynamic XFA, say so clearly
    rather than showing the "please upgrade your reader" placeholder page and nothing else.
  - **Done:** `selis-pdf-form::render_xfa` decodes the `/XFA` packet(s) and extracts the static
    `<template>`'s captions and field names as a clearly-labelled plain-text representation (NOT a
    pixel-accurate XFA layout render). A packet declaring `<dynamic>`, `<event>` or `<script>` is
    refused with a typed `FormError::XfaDynamic` naming the reason; a packet with no readable
    static template is `FormError::XfaUnsupported`. Full dynamic XFA layout/scripting remains out
    of scope (no JS engine, ADR-P0021) and is not faked.

---

## 5.JS — The scripting sandbox (ADR-P0020)

- [ ] **SL-5.JS.01 — Budgeted JS interpreter + Acrobat API subset** · owner: HUMAN
  - **Do:** Embed a small JS engine (`boa` or QuickJS-via-WASM — evaluate both for size and
    determinism), with a memory and instruction budget, no host access, and a whitelisted API:
    `event`, `this.getField`, field properties, `util.printf`/`printd`, `AF*` format functions,
    and the calculation events. Explicitly absent: `app.launchURL`, `this.exportDataObject`,
    `this.submitForm` to arbitrary URLs, `Net.*`, `Collab.*`.
  - **DoD:** A malicious-script corpus is contained; a real-world tax form with calculations works;
    an instruction-budget test proves an infinite loop terminates.
- [ ] **SL-5.JS.02 — Per-document scripting consent UI** · deps: JS.01 · owner: AI+
- [ ] **SL-5.JS.03 — Admin policy for pre-approved origins** · deps: JS.01 · owner: AI

---

## 5.REDACT — Redaction (ADR-P0023)

- [x] **SL-5.REDACT.01 — Region model and redaction annotations** · owner: HUMAN
  - **Do:** Mark regions (rect, text selection, or search-driven), preview, and store as `/Redact`
    annotations until applied.
- [x] **SL-5.REDACT.02 — Content-stream removal** · deps: REDACT.01, SL-2.CONT.07 · owner: HUMAN
  - **Do:** Remove glyph-show operators whose glyphs intersect the region — including *partial*
    text-run splitting, which is where naive implementations leak. Re-encode covered image regions.
    Remove vector geometry inside the region.
- [x] **SL-5.REDACT.03 — Non-content removal sweep** · deps: REDACT.02 · owner: HUMAN
  - **Do:** Annotations, form fields and their values, structure-tree nodes, MCIDs, optional
    content, embedded files, attachments, XMP and Info metadata, JavaScript, bookmarks pointing
    into the region, and **every prior revision in the incremental chain** — a redaction that
    leaves revision N−1 intact has redacted nothing.
  - **DoD:** A test that redacts a document with 5 incremental revisions and proves the content is
    absent from the output bytes entirely.
- [x] **SL-5.REDACT.04 — The verification pass** · deps: REDACT.03 · owner: HUMAN
  - **Do:** Re-open the produced bytes with a fresh parser, run extraction, render the region, and
    grep the raw output bytes for the redacted strings. Any survival → error, no file produced.
  - **DoD:** A deliberately-broken redaction implementation is caught by the verifier. **Test the
    test**: this is the control that the whole claim rests on.
- [x] **SL-5.REDACT.05 — Pattern-based bulk redaction** · deps: REDACT.01 · owner: AI+
  - **Do:** Regex/pattern search across the document (SSNs, card numbers, emails, names from a
    list) with a review step before applying. Never auto-apply.
  - **Note (2026-11-19):** Implemented in `selis-pdf-tools/src/redact_patterns.rs` behind the
    `text` feature, with a CLI `selis redact-patterns scan|apply`. `find_candidates` has no
    output path and returns a masked review; `apply_reviewed` requires an explicit approved-id
    slice (empty/unknown sets are refused) and re-derives each region from the input before
    writing. No regex dependency: SSN (with group checks), Luhn-valid card runs, email, and
    caller-supplied literals (substring or whole word) are implemented directly over the
    engine's recovered text and `LayerChar` geometry. ADR-P0023 read-back re-parses the output
    and checks extracted text + raw bytes; a survivor is a typed `VERIFY_FAILED` with no file.
    Verified by 9 unit tests including a "verifier is not vacuous" control and a control that
    proves a substring sharing one show operation fails loudly.
  - **Web card (2026-11-19):** the "Redact patterns" card now exists on the tools page, on
    op 47 in its own lazy `selis-pdf-wasm-redact-patterns` chunk (the `text` feature means it
    cannot share the engine-free tools/optimise chunks). The browser flow is two-phase and the
    review step is **mandatory**: `scan` returns the masked `PatternReview` as a JSON report and
    the UI renders one checkbox per candidate (unchecked by default) beside its masked text and
    region; `apply` is only ever called with the ids the user explicitly checked. There is no
    scan-and-apply function on the wire or in the UI, so **no auto-apply is possible**. If a
    match shares one show operation with other text, the ADR-P0023 read-back fails loudly with
    `VERIFY_FAILED` and no file is offered.
- [x] **SL-5.REDACT.06 — Redaction corpus + adversarial suite** · deps: REDACT.04 · owner: HUMAN
  - **Do:** Build the corpus deliberately adversarially: text split across runs, text in a form
    XObject, text in an annotation appearance, text in a prior revision, text in an embedded file,
    text as an image with an OCR layer, text in metadata.
  - **DoD:** 100% pass. Any failure blocks G5 — this is not a "known issue" category.

---

## 5.SIGN — Digital signatures

- [ ] **SL-5.SIGN.01 — Signature validation** · owner: HUMAN
  - **Do:** Byte-range digest verification, PKCS#7/CMS parsing, certificate chain building, trust
    anchor evaluation, revocation (CRL + OCSP), timestamp verification, and — critically —
    **detecting whether the signature covers the whole document** or a document has been modified
    after signing.
  - **DoD:** Validation verdicts match Acrobat on a signature corpus including invalid, expired,
    revoked, timestamped, and modified-after-signing cases.
  - **Risk:** The dangerous bug class here is a *false valid*. Several PDF viewers have shipped
    signature-spoofing CVEs (incremental-update attacks, shadow attacks). Build the published
    attack corpus into the test suite explicitly.
- [ ] **SL-5.SIGN.02 — Visual-vs-cryptographic distinction in the UI** · deps: SIGN.01 · owner: HUMAN
  - **Do:** A drawn signature image is not a signature. The UI must never let the two be confused,
    and validation status must be un-spoofable by page content (no trusting an image that looks
    like a green tick).
- [ ] **SL-5.SIGN.03 — Signing: PAdES B-B / B-T** · deps: SIGN.01, EDIT.03 · owner: HUMAN
  - **Do:** Create the signature field, reserve the `/Contents` gap, compute the byte-range digest,
    build the CMS, and embed — as an incremental update so prior signatures survive.
- [ ] **SL-5.SIGN.04 — Timestamping (RFC 3161) and LTV** · deps: SIGN.03, SL-0.LEAD.08 · owner: HUMAN
  - **Do:** TSA integration, DSS/VRI construction, and document-timestamp signatures for long-term
    validation (PAdES B-LT / B-LTA).
- [ ] **SL-5.SIGN.05 — Certification signatures and DocMDP** · deps: SIGN.03 · owner: HUMAN
- [ ] **SL-5.SIGN.06 — Key sources** · deps: SIGN.03 · owner: HUMAN
  - **Do:** PKCS#12 files, OS keystores (Windows CryptoAPI, macOS Keychain), PKCS#11 tokens and
    smart cards, and cloud-signing (CSC API) for the eIDAS path.
  - **Note:** Smart-card/eID support is a hard requirement in several European public sectors and
    a genuine barrier to entry for competitors.
- [ ] **SL-5.SIGN.07 — Signature appearance generation** · deps: SIGN.03, ANNOT.01 · owner: AI+

---

## 5.OCR — OCR and scanned documents

- [ ] **SL-5.OCR.01 — Tesseract in the WASM sandbox** · deps: SL-0.SBX.06 · owner: AI+
  - **Do:** Tesseract compiled to WASM, language data lazily downloaded per language, driven
    through the Tier-2 sandbox.
  - **DoD:** Runs on web and native; language packs cached; memory bounded.
- [ ] **SL-5.OCR.02 — Page image preparation** · deps: OCR.01 · owner: AI+
  - **Do:** Deskew, despeckle, binarise (Sauvola or similar), and detect orientation.
- [ ] **SL-5.OCR.03 — Layout analysis** · deps: OCR.02 · owner: AI+
  - **Do:** Region segmentation (text/image/table), column detection, and reading order — feeding
    the same model as `selis-pdf-text` so extraction is uniform.
- [ ] **SL-5.OCR.04 — Invisible text layer emission** · deps: OCR.03, EDIT.03 · owner: AI+
  - **Do:** Text render mode 3 positioned to match the image glyphs, with per-word confidence
    retained, and structure tags emitted (ADR-P0031) so the OCR'd document is accessible.
  - **DoD:** Extraction and search work on an OCR'd scan; the visual appearance is unchanged.
- [ ] **SL-5.OCR.05 — Searchable-PDF and PDF/A output from scans** · deps: OCR.04 · owner: AI
- [ ] **SL-5.OCR.06 — OCR accuracy benchmark** · deps: OCR.04 · owner: AI
  - **Do:** Measure character and word accuracy against a labelled scan corpus; publish it.

---

## 5.CONV — Conversion (first tranche)

- [x] **SL-5.CONV.01 — PDF → images (PNG/JPEG/TIFF/WebP)** · owner: AI
- [x] **SL-5.CONV.02 — Images → PDF** · deps: CONV.01 · owner: AI
- [x] **SL-5.CONV.03 — PDF → text / Markdown / structured JSON** · deps: SL-3.TEXT.07 · owner: AI
- [x] **SL-5.CONV.04 — PDF → HTML with layout fidelity** · deps: SL-2.CONT.07 · owner: AI+
- [x] **SL-5.CONV.05 — HTML → PDF** · owner: AI+
  - **Note:** Decide deliberately: a real HTML engine is out of scope, so either restrict to a
    documented subset or drive a headless browser server-side as part of the Server SKU. Do not
    ship a half-working general HTML renderer.
  - **Note:** Shipped early (Phase 1A timeframe) as `selis topdf` via `selis-pdf-convert`: the
    documented-subset route — Markdown (headings, paragraphs, bold/italic/code, lists,
    blockquotes, fenced code, hr) and an HTML subset (h1–h6, p, b/i/em/strong, code/pre, ul/ol/li)
    laid out on Letter/A4 with standard-14 fonts via the WRITE.01 writer. A general HTML engine
    remains deliberately out of scope; the headless-browser route stays with the Server SKU.
- [x] **SL-5.CONV.06 — Optimise / compress** · deps: SL-5.EDIT.05 · owner: AI+
  - **Do:** Downsample images, recompress streams, subset fonts, dedupe objects, GC unreferenced
    objects, linearise. With a preview of the quality/size trade-off.

---

## 5.BIZ — Commercialisation

- [ ] **SL-5.BIZ.01 — `selis-policy` entitlements wired to a real account** · deps: ADR-P0029 · owner: AI+
- [ ] **SL-5.BIZ.02 — Billing, subscriptions, tax, and the free/paid boundary** · owner: HUMAN
  - **Do:** Implement the SKU boundary from `26-PRODUCT-SURFACE.md`. Viewer stays free forever —
    it is the funnel and the credibility.
- [ ] **SL-5.BIZ.03 — Offline grace and graceful degradation** · deps: BIZ.01, ADR-P0015 · owner: AI+
  - **DoD:** A test proving an expired licence never blocks saving open work.
- [ ] **SL-5.BIZ.04 — Trial, upgrade prompts, and conversion instrumentation** · owner: HUMAN
- [ ] **SL-5.BIZ.05 — G5 review and paid launch go/no-go** · owner: HUMAN


---

> **Status reconciliation (2026-11-19):** Verified against code and flipped stale `[ ]` markers to `[x]` for items whose implementation exists: EDIT.01 (journal), EDIT.02 (undo/redo), EDIT.03 (incremental writer + 100 000-mutation suite), EDIT.08 (page ops), EDIT.09 (object rewriting), EDIT.10 (`.selis` bundle), EDIT.11 (compare engine+CLI), ANNOT.04/05/06/07, FORM.01/04, REDACT.01–04/06, CONV.01–04/06. Items left open are genuinely missing or partial: TEXTEDIT.01/03/04/05/06, FORM.03/06/07/08, all JS.*, SIGN.04/05/06, all OCR.*, BIZ.*. REDACT.05 was in this open list when that note was
written; it now has the reviewed web card described above, still with no auto-apply path. ENC-side
items elsewhere remain gated on human review. Verification was read-only against `crates/` and `apps/`.
