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
- [x] **SL-5.TEXTEDIT.02 — Text edit + re-layout + re-emit** · deps: TEXTEDIT.01, SL-3.SHAPE.03, SL-3.FONT.11 · owner: AI+
  - **Do:** Edit the model, re-shape and re-break the affected paragraph, re-emit that content
    region while leaving the rest of the stream byte-identical, and extend the embedded font
    subset with any new glyphs.
  - **DoD:** Editing one word in a paragraph changes only that paragraph's content region; the page
    renders identically outside the edit; a character absent from the original subset renders.
  - **Done (pipeline):** `selis_pdf_engine::reflow::reflow_paragraph` reconstructs the paragraph
    model with confidence, refuses below `MIN_REFLOW_CONFIDENCE` (0.80, the TEXTEDIT.01 provisional
    threshold) pointing at the box path, locates the paragraph's show-text byte span, re-breaks the
    replacement text with the page font's own advance table + UAX #14 opportunities
    (`selis_shape::line_breaks`), re-emits only that region, extends the embedded TrueType subset
    with `selis_font::subset::add_glyph` (widening `/FontFile2`, `/Widths` and `/LastChar`), and writes
    through `selis_pdf_edit::reflow_regions` → `incremental::save_incremental`. The low-level writer
    is independently tested in `selis-pdf-edit` (`reflow_edit_tests.rs`).
  - **DoD evidence (all four items verified):** (a) a one-word edit on the real
    `corpus/fixtures/paragraphs/two_paragraphs.pdf` changes only the reported byte span; the decoded
    stream is byte-identical before and after it and the original file is a verbatim byte prefix
    (I3). (b) `render_page_rgba` before/after, with the paragraph rectangle masked out, is
    pixel-identical everywhere else. (c) a 'C' glyph absent from the embedded mini subset (code
    0x43, advance 0) is added with a caller-supplied outline; after the edit the re-embedded font
    maps 0x43 to a 400-unit advance and the page paints strictly more glyph pixels. (d) a
    low-confidence paragraph (single-line `text.pdf`) is refused with a typed error naming
    `edit_box` (TEXTEDIT.03). Tests: `selis-pdf-engine/tests/textedit_reflow.rs` +
    `selis-pdf-edit/src/reflow_edit_tests.rs`.
  - **Not claimed (honest gaps):** only simple, single-byte fonts are re-laid-out — two-byte CID /
    `/ToUnicode` fonts are refused rather than mangled. New glyph outlines are **supplied by the
    caller** (from a matched face via TEXTEDIT.04); this slice does not itself fetch a substitute
    font program, and a face it cannot extend is disclosed in the refusal, never silently swapped.
    Horizontal, top-to-bottom paragraphs only. The 0.80 threshold is still the provisional
    synthetic-set value from TEXTEDIT.01, not the DoD's 500-page correlation. The subset extension
    covers TrueType `/FontFile2`; CFF extension is available through the shared subset writer but
    is not wired into this re-embed path.
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
- [x] **SL-5.TEXTEDIT.05 — Image editing: replace, move, resize, delete, extract** · deps: SL-2.RAST.08 · owner: AI+
  - **Done (engine slice):** `selis-pdf-edit::objects` addresses image XObjects by a stable `(resource name, content-stream op index)` id, reports each placement rect in PDF user space, and implements `list_image_objects`, `replace_image`, `move_object`, `resize_object`, `delete_object` and `extract_image`. Each mutation lowers to a COS object edit and an incremental save: the original bytes stay a verbatim prefix and every other object/operator is byte-identical (I3/I7). Read-back re-opens the output, re-enumerates, and checks the target changed while a control object is unchanged. Re-exported as library APIs from `selis-pdf-tools::object_edit`.
  - **Done (binding slice):** the engine APIs are exposed as first-class ops in the dedicated lazy `selis-pdf-wasm-content` chunk (`list_objects`/`list_vectors`/`move_object`/`resize_object`/`delete_object`/`replace_image`/`extract_image`, wire codes 56-62) and as CLI subcommands (`selis list-objects`, `selis list-vectors`, `selis move-object`, `selis resize-object`, `selis delete-object`, `selis replace-image`, `selis extract-image`). The web editor has an Objects tool with a click-to-select overlay: it asks the engine for each object's reported bounding box, converts it through the page's render transform (`canvasRect`) and hit-tests clicks against those boxes, choosing the smallest overlapping box. The op bridge (`selis-pdf-tools::content_edit`) keeps the engine crates filesystem/network-free and adds no JS imports to the zero-import tools chunk. The old `image_watermark`/stamp workarounds were removed from the editor's image placement flow.
  - **Deferred (not claimed):** only top-level page content is scanned; form XObjects and inline images are not addressed, and images with a colour space/filter outside 8-bit DeviceRGB + (none|Flate|DCT) are refused rather than guessed. `replace_image` decodes PNG/JPEG and writes a fresh Flate/DeviceRGB XObject (a normalisation, not a byte-preserving re-encode). The editor's move/resize/replace/delete actions use fixed page-derived defaults rather than a free-form resize handle.
- [x] **SL-5.TEXTEDIT.06 — Vector object selection and manipulation** · deps: SL-2.CONT.07 · owner: AI
  - **Done (engine slice):** `list_vector_objects` enumerates painted paths (`f`/`S`/`B` family) with stable ids and user-space bounding boxes; `move_object`, `resize_object` and `delete_object` rewrite the placement `cm` operands or remove the path operators. Same incremental-write, read-back and control-object-preservation gate as TEXTEDIT.05, with tests over a fixture carrying an image and vector paths.
  - **Done (binding slice):** vector paths are selected by the same click-to-select overlay as images (TEXTEDIT.05), and `list_vectors` reports their engine-measured boxes to the editor; the CLI and wasm op codes are shared with that row.
  - **Deferred (not claimed):** general free-form path geometry — resize maps the path's local bounding box onto the target rect, so a rotated or self-intersecting path is approximated by that box rather than reshaped. Clip-only paths (`W n`) are not painted objects and are skipped.
- [x] **SL-5.TEXTEDIT.07 — Add content: text boxes, images, shapes, links** · deps: SL-3.SHAPE.03 · owner: AI+
  - **Done (engine slice):** `selis-pdf-edit::add_content` adds proper new content to a leaf page: `add_text_box` (standard-14 face, WinAnsi/ASCII, with the TEXTEDIT.03/04 disclosed-substitution path — `match_substitute`'s disclosure is returned in the report and an unavailable face or non-ASCII character is a typed refusal, never a silent swap), `add_image` (caller-supplied PNG/JPEG bytes decoded in memory to an 8-bit Flate/DeviceRGB XObject, placed by a general affine matrix incl. rotation), `add_shape` (rect / inscribed ellipse / line, fill and stroke) and `add_link` (proper `/Annots` `/Link` with `/A` URI or GoTo and a generated `/AP` border appearance). Every addition is a NEW content-stream object appended to (or, for backgrounds, prepended to) the page's `/Contents`; the original content-stream object is never rewritten. Writes go through `incremental::save_incremental`, so the original bytes stay a verbatim prefix, the signature guard and read-back still apply, and untouched content is byte-identical. Tests build synthetic PDFs in-test and assert prefix preservation, byte-identical control draws, read-back of the added font/image/shape/link objects, render parity outside the declared rects, and typed refusals (bad geometry, page, text, URI, image, font). No file/network I/O; no new dependencies.
  - **Done (binding slice):** all eight add-content operations are exposed as first-class ops in the dedicated lazy `selis-pdf-wasm-content` chunk (`add_text_box`/`add_image`/`add_shape`/`add_link`/`add_header_footer`/`add_watermark`/`add_bates`/`add_background`, wire codes 48-55) and as typed CLI subcommands (`selis add-text`, `selis add-image`, `selis add-shape`, `selis add-link`, `selis add-header-footer`, `selis add-watermark`, `selis add-bates`, `selis add-background`). The web editor now places text and images with a drag-a-rectangle flow that converts canvas pixels to the page's user space and sends the first-class op; the documented stamp (`runSign op: "stamp"`) and `image_watermark` workarounds were removed from those flows, and whole-page furniture ops (watermark/Bates/header/footer/background/shape/link) are offered in the ribbon. Caller-supplied image bytes ride as a second input file, never as base64 in options. The strict zero-import tools chunk guarantee is unchanged.
  - **Deferred (not claimed):** text is printable-ASCII, unembedded standard-14 only; no font embedding/re-subsetting, no CJK. Shape authoring is limited to rect/ellipse/line, not free-form Bézier paths. Only top-level page content is targeted (no form-XObject/inline-image hosts). The drag tool sets an axis-aligned rect; rotated placement is available through the CLI's `--angle` / options but has no drag handle. The 45-degree image-placement renderer gap this item originally found is now CLOSED (`cd28fef`): `selis-pdf-content::exec` derives the XObject image AABB from all four CTM corners with a regression test, so a rotation that makes `a+c` or `b+d` zero paints correctly. No external corpus dependency — all evidence is synthetic in-test fixtures.
- [x] **SL-5.TEXTEDIT.08 — Headers, footers, watermarks, Bates numbering, backgrounds** · deps: TEXTEDIT.07 · owner: AI
  - **Done (engine slice):** built on the same `add_content` plan/incremental writer: `add_header_footer` (MediaBox-derived placement with left/centre/right alignment), `add_watermark` (rotated text plus `/ExtGState` opacity), `add_bates_numbering` (caller-supplied pattern whose last ASCII digit run is the zero-padded counter; caller-supplied start; sequential ids stamped on every page in one revision), and `add_background` (a new stream placed FIRST in `/Contents`, so the fill is beneath existing paint rather than over it). Tests read back the stamped strings and objects across two pages, verify sequential ids `CASE-000007`/`CASE-000008`, prove the background sits under a control rectangle by pixel sampling, and assert render parity outside each added rect plus typed refusals (non-finite geometry, a pattern with no digit run, unavailable font). Signature/prefix/read-back gates are inherited from the TEXTEDIT.07 writer.
  - **Done (binding slice):** the page-furniture ops are wired to the same CLI/wasm/editor seam as TEXTEDIT.07 (wire codes 52–54, `selis add-header-footer`/`add-watermark`/`add-bates`, and ribbon buttons with page-sized defaults).
  - **Deferred (not claimed):** Header/footer/Bates centre/right alignment uses an approximate standard-14 advance (~0.5 em per ASCII glyph), not measured font metrics. The Bates counter is the last digit run only; no page-range selection or extra auto-fields beyond the pattern. The editor exposes one fixed watermark text/opacity and one Bates pattern by default; richer authoring is via the CLI/options. Watermark rotation paints correctly (the renderer handles rotated text); rotated-image rendering is now correct too (the four-corner AABB fix in `cd28fef`). No external corpus dependency — synthetic fixtures only.
  - **Note:** Bates numbering is a legal-market requirement and a cheap differentiator.

---

## 5.ANNOT — Annotations

- [x] **SL-5.ANNOT.01 — Annotation model + appearance-stream generation** · owner: AI+
  - **Done (first slice):** `selis-pdf-annot` writes highlight, underline, strikeout, note, ink and box as spec-conformant annotations with generated `/AP` streams, and reads them back. Moving an existing annotation, replies, review state and flattening remain open.
  - **Do:** All markup subtypes: Text (note), Highlight, Underline, StrikeOut, Squiggly, Square,
    Circle, Line, Polygon, PolyLine, Ink, FreeText, Stamp, Caret, FileAttachment, Sound, Redact.
    Generate conformant `/AP` streams for each (ADR-P0026) so every other viewer shows them.
  - **DoD:** Each subtype round-trips through Acrobat and PDFium visually unchanged.
- [x] **SL-5.ANNOT.02 — Appearance regeneration and `/NeedAppearances`** · deps: ANNOT.01 · owner: AI+
  - **Done (this slice):** `selis_pdf_annot::regenerate_appearances` walks every page and rebuilds the `/AP` of annotations that are missing one or carry a **stale** one (unreadable `/AP /N`, empty stream, or a Form `/BBox` that no longer matches `/Rect` — the case after a move). Widgets and unknown `/Subtype`s are left untouched. Each stream is drawn by the same `appearance_stream` path the writer uses, so the bytes are deterministic; all rewrites land in **one incremental revision** whose original bytes remain a verbatim prefix, and the output is re-opened and checked annotation-by-annotation (`/AP /N`, `/BBox`, exact stream bytes) before it is returned. A failure is a typed error, never a silent "looks fine". `/QuadPoints` read-back was corrected to the writer's counterclockwise vertex order so a regenerated highlight keeps its position after a move.
  - **`/NeedAppearances`:** detected on a direct or indirect `/AcroForm`, reported in `AppearanceReport::need_appearances`, and deliberately **left set**. Widget appearance synthesis already lives in `selis-pdf-form` (`FillFormsOptions::synthesize_appearances`); clearing the flag here without writing those widget `/AP`s would claim work that was not done. This crate never sets the flag to make a viewer do markup work either — it writes real `/AP` streams. That crate was read for coordination only, not edited.
  - **Tests:** `a_current_appearance_is_not_touched`, `a_missing_appearance_is_regenerated_and_verified`, `a_stale_appearance_is_rebuilt_with_the_new_geometry` (byte-identical AP to a fresh write — render-parity proxy), `a_need_appearances_form_is_reported_and_left_intact`, `an_indirect_need_appearances_form_is_reported`, `a_markup_with_unreadable_geometry_is_a_typed_error`, plus byte-prefix assertions in each.
  - **Binding added (this slice):** `selis annot-appearances <in> --output <out> [--json]` is the native CLI surface. `apps/cli/src/annot.rs` supplies the file bytes and real clock and runs the shared WRITE.05 gate before committing; the incremental write, byte-prefix guarantee and per-annotation AP read-back stay in the engine. It prints the page/annotation/rebuilt counts and the untouched `/NeedAppearances` flag. Named test: `apps/cli/tests/annot_commands.rs::annot_appearances_rebuilds_a_missing_ap_in_an_incremental_revision`.
  - **WASM binding (this slice):** the typed-gap deferral is closed by the dedicated zero-import lazy chunk `selis-pdf-wasm-annot` (artifact `selis_pdf_wasm_annot`, op 63). It hands the supplied PDF to the same `selis_pdf_annot::regenerate_appearances` path, runs the shared WRITE.05 gate, and returns the rewritten bytes with the rebuilt count and the untouched `/NeedAppearances` flag in its typed `annotation` report. The editor fetches the chunk only when the reader chooses "Rebuild appearances"; the tools chunk (already inside its 2% gate) is unchanged, and op 35 there still binds only single-stroke `Annotate`.
  - **Honest gaps:** no widget-`/AP` synthesis or `/NeedAppearances` clearing in this crate (delegated to FORM as above); **no Acrobat/PDFium visual round-trip** — AP bytes are compared to a fresh write as the parity proxy.
- [x] **SL-5.ANNOT.03 — Ink and stylus geometry** · deps: ANNOT.01 · owner: AI+
  - **Done (this slice):** `AnnotSpec::paths` carries caller-supplied multi-stroke point sequences; each stroke becomes one array in a spec-conformant `/InkList` (array-of-arrays), written with `/BS` (solid, round cap/join, `/W` width), `/C` and a `/Rect` derived from the points (grown by half the width so nothing is clipped). Geometry is **pressure-agnostic** by design: callers supply plain x/y points, so a stroke renders identically in every viewer (no pressure/tilt fields are emitted). Typed caps: `MAX_INK_PATHS = 256` strokes, `MAX_INK_POINTS_PER_PATH = 20_000` points per stroke and `MAX_INK_POINTS = 20_000` total; empty strokes, non-finite points and oversized input are refused with `MUTATION_INVALID`. The legacy flat single-`points` form still writes one stroke and the reader accepts both flat and nested `/InkList`. `PageAnnotation::ink_points` reads each stroke back with its point count.
  - **Tests:** `ink_strokes_round_trip_with_their_point_counts`, `an_ink_rectangle_bounds_every_point_and_carries_a_border`, `the_legacy_single_point_line_still_writes_one_stroke`, `empty_non_finite_and_oversized_strokes_are_refused`.
  - **Binding added (this slice):** `selis annot-ink <in> --output <out> --stroke "x,y;..." [--stroke ...] [--page N] [--width W] [--colour r,g,b] [--contents TEXT] [--author NAME] [--json]` writes every repeatable `--stroke` as one `/InkList` array through the same incremental, WRITE.05-gated path. The rectangle is derived from the points by the engine, so a caller cannot clip its own mark. Named tests: `apps/cli/tests/annot_commands.rs::annot_ink_writes_every_stroke_as_an_inklist_array` and `a_single_point_stroke_is_refused_before_any_write`.
  - **WASM binding (this slice):** the multi-stroke `paths` capability is exposed to the browser by the dedicated zero-import lazy chunk `selis-pdf-wasm-annot` (op 64): the editor buffers drags into caller-supplied strokes and the guest builds one `AnnotSpec` with `paths` through the same incremental, WRITE.05-gated path as the CLI. The tools chunk's op 35 deliberately keeps the legacy flat `points` form rather than growing a 2%-gated chunk; the new chunk is fetched only when the reader adds a multi-stroke mark.
  - **Honest gaps:** no pressure-/tilt-aware smoothing — deliberately out of scope under the pressure-agnostic constraint (the plan's original phrasing is superseded); no stylus hardware or event-capture integration (the caller supplies the point sequences); no Acrobat/PDFium visual round-trip (read-back and AP geometry are asserted instead).
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
- [x] **SL-5.FORM.02 — Field appearance generation** · deps: FORM.01, ANNOT.02 · owner: AI+
  - **Do:** Text fields with quadding, comb fields, multiline, rich text, auto-size fonts, choice
    lists, and checkbox/radio appearance states.
  - **Done (all widget kinds):** `selis_pdf_form::regenerate_appearances(src, budget, g) ->
    (bytes, AppearanceReport)` walks the whole AcroForm and writes a real `/AP` for **every** widget
    type from the field's current `/V`: text and single/multi-select choice (reusing the shared
    variable-text generator with exact standard-14 metrics, `/Q`, multiline and auto-size —
    FORM.06's fill-time synthesis already covered these); checkbox and radio (`/AP /N` `Off` + an
    on-state with a vector checkmark / filled disc, `/AS` selected from `/V`); push button (a
    bordered `/MK /CA` caption); and signature (a bordered box drawing the signer name from an
    existing `/V /Name`). Buttons that already carry appearance states are never replaced — only
    `/AS` is selected. A signed field with no `/V`, a missing widget/box, or an unresolvable font
    is skipped with a typed `AppearanceSkip`, and `/NeedAppearances` is left/set so a conformant
    viewer rebuilds it (the ANNOT.02-compatible contract — that crate was read, not edited). Every
    font substitution is disclosed in `AppearanceReport::disclosures`; nothing changes silently.
    The result is re-opened and each generated `/AP` read back before it is returned. Exposed as
    `selis form-appearances <in> --output <out>` through
    `selis-pdf-tools::regenerate_field_appearances` (write-gate verified).
  - **Incremental save (FORM.02 gap closed 2026-11-20):** `regenerate_appearances` now appends an
    **incremental revision**: the input bytes are a byte-for-byte verbatim prefix, so a signature
    over the source keeps verifying. It materialises the live graph with ORIGINAL object numbers,
    snapshots it, and appends only the objects the synthesis actually changed (changed widgets,
    the `/AcroForm` whose `/NeedAppearances` changed, and the new Form XObjects) through the SAME
    low-level primitive `selis-pdf-edit` uses — `selis_pdf_cos::doc_writer::write_incremental_update_full`.
    The reusable primitive already lived in COS, so no dependency inversion was needed and
    `selis-pdf-form` still depends only on COS (no edit crate in the WASM form chunk). A test signs
    a document with the committed PEM fixtures, regenerates, proves the signed source is a prefix,
    and re-verifies the signature with the real report; size-check stays 16/16.
  - **Not claimed (honest gaps):** the other two `selis-pdf-form` write paths — `fill_form` and
    `create_form_fields` — still emit verified **full rewrites** (renumbered objects), not
    incremental revisions; the shared helper (`collect_objects_incremental` +
    `write_form_incremental` in `selis-pdf-form::lib`) is ready for them but wiring them would change
    their documented full-rewrite/signature-invalidation semantics and byte-level tests. The
    checkbox/radio marks are simple vector glyphs, not Acrobat's exact bundled checkmark/dingbat
    art. Push-button `/MK /BC` (border colour), `/BG` (background) and `/R` (rotation) are read but
    the generated appearance uses a fixed black border / no fill / no rotation — a visible
    simplification. A signature appearance is a *drawing of the name*, not a cryptographic visual,
    and no signing is performed. Comb fields, rich text and auto-size beyond the shared text
    generator are not separately implemented.
- [x] **SL-5.FORM.03 — Calculation, validation, and format order** · deps: FORM.01, SL-5.JS.01 · owner: AI+
  - **Do:** `/CO` calculation order and the format/validate/calculate event sequence. Most real
    forms depend on this working exactly like Acrobat.
  - **Done (event subset, consent-gated):** `scan_form_scripts` now reports a typed `ScriptManifest`
    with `/CO`-ordered `CalcScript`s **and** `/AA /V` `ValidateScript`s and `/AA /F`
    `FormatScript`s (fully-qualified names, hierarchy resolved); `FormFieldsReport` carries all
    three. `JsSandbox::run_event_sequence` evaluates them as one commit-time sequence in the
    required order — **calculate (`/CO`) → validate (`/V`) → format (`/F`)** — reusing the same
    consent gate: every action goes through `JsSandbox::run_authorized`, which refuses with
    `JS_CONSENT_REQUIRED` before any interpreter work. A validate script setting `event.rc = false`
    is a typed `JS_VALIDATE_REFUSED` naming its field and writes no file. A script that does not
    assign `event.value` no longer clears the field (the value property is an accessor that tracks
    real assignments). The CLI `fill-forms --allow-js` runs the whole sequence and merges computed
    values back; without consent a scripted form is refused with a typed error naming
    `--allow-js`/`SELIS_ALLOW_JS=1`.
  - **Not claimed — full Acrobat semantics are NOT claimed:** this is a commit-time sequence, not
    Acrobat's interactive event engine. `/AA /K` (keystroke) events, per-keystroke re-entrancy,
    Acrobat's exact recalculation trigger points, and its separate *display* value are not
    modelled: a format script's `event.value` is written to the stored `/V` because this engine
    keeps one value per field. Document-level `/JS` execution remains out of scope (ADR-P0020).
    Browser-computed values from the web prompt are still shown as read-back rows but are **not**
    written back into the filled PDF; the native CLI does write them back.
- [x] **SL-5.FORM.04 — Form filling UI** · deps: FORM.02 · owner: AI
  - **Do:** Tab order, keyboard accessibility, required-field indication, and inline validation.
- [x] **SL-5.FORM.05 — Form data import/export (FDF, XFDF, JSON, CSV)** · deps: FORM.01 · owner: AI
  - **Done:** XFDF and FDF import already existed; added a documented, versioned JSON schema
    (`selis-form-data/1`: `{"schema":...,"fields":{"name": value-or-array}}`) and a CSV shape
    (header row of field names + one value row, multi-select joined with `;` and RFC-4180 quoted)
    with `export_json`/`export_csv` and `import_json`/`import_csv` in `selis_pdf_form::formdata`.
    Both import paths produce the same `Vec<FieldEdit>` the FDF/XFDF importer feeds into
    `fill_form`, and a test proves export → import → fill → read-back round-trips. Binary and
    attachment-like fields (signature fields, push buttons, NUL-bearing or CSV-reserved values)
    are explicitly skipped with typed `SkippedField`/`SkipReason` notes, never silently dropped.
    CLI: `selis form-export <pdf> [--format json|csv]` and
    `selis form-import <pdf> <data> --output <pdf> [--format json|csv|auto]`.
  - **Not claimed:** the CSV single-value/`;` collision is refused with a typed note rather than
    escaped; there is no XFDF/FDF *export* (only import) in this slice.
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

- [x] **SL-5.JS.01 — Budgeted JS interpreter + Acrobat API subset** · owner: HUMAN
  - **Do:** Revised to a two-environment split (owner-directed 2026-11-19): **native** uses
    `rquickjs =0.11.0` (MIT, QuickJS MIT) compiled *natively only* with an interrupt handler as the
    instruction budget; **browser** uses the platform's own engine in a sandboxed `Worker` with
    `worker.terminate()` as the hard deadline — no engine compiled to WASM, no new npm dependency,
    zero wasm size delta. `boa` rejected as larger; GPL engines ruled out (ADR-P0021).
  - **Done:** `selis-pdf-js` (native) installs only `event`, `this.getField`, `util.printf`/`printd`
    and the `AF*` helpers, with a memory/stack/script-size/field budget and a tick interrupt;
    `apps/web/host/src/editor/js-worker.ts` generates a Worker bootstrap that removes `fetch`,
    `XMLHttpRequest`, `importScripts`, `WebAssembly`, `indexedDB`, `WebSocket`, `postMessage`, …
    and installs the same subset. `app.launchURL`, `this.exportDataObject`, `this.submitForm`,
    `Net.*`, `Collab.*` are refused by a static scan and absent at runtime in both slices.
  - **DoD:** met — a tax-style calculation computes `event.value`; `while(true){}` is interrupted by
    the native instruction budget and by the browser Worker deadline (the latter proven with an
    injected Worker + timers, and the realm itself proven interruptible in a Node `vm` analogue);
    a 13-script malicious corpus is contained. The static scan is a diagnostic, not the boundary;
    the boundary is that dangerous APIs are never installed and the engine has no host capability.
- [x] **SL-5.JS.02 — Per-document scripting consent** · deps: JS.01 · owner: AI+
  - **Done:** `selis-policy::ConsentPolicy` (Rust) and its TypeScript mirror
    `apps/web/host/src/editor/js-policy.ts`. Both default to deny, key consent on the document id,
    and expose grant/revoke/clear. `JsSandbox::run_authorized` and `runFormScript` call the gate
    *before* creating a runtime or Worker; tests prove no Worker is created without consent.
  - **Done (this slice — the visible prompt + wiring):**
    - `apps/web/host/src/editor/js-consent.ts` adds `ScriptConsentController`, a dark-themed
      dialog ("This document contains JavaScript. Allow it to run? [No] [Allow once for this
      document]" with a required acknowledgment checkbox), `detectFormScripts`, and
      `runFormScriptWithConsent`. The dialog is built through a typed DOM seam so it is tested
      with a jsdom-style mock; a change event reveals the Allow button, which is inert until the
      box is ticked, and an allow records a session grant for that one document id only.
    - The Fill Forms card (`tools-page.ts`) reads the typed `ScriptManifest` from the field
      report, shows the prompt after the local no-upload check, and calls `runFormScript` only
      after an explicit grant. A decline (or a missing prompt host) leaves the grant empty, so no
      Worker is created. Browser-computed values are surfaced as result rows.
    - Native CLI `fill-forms`: without consent it refuses a scripted form with a typed error naming
      `--allow-js` / `SELIS_ALLOW_JS=1`; with the explicit grant it evaluates `/CO` through
      `JsSandbox::run_authorized`. No script ever auto-runs on either surface.
    - Tests: jsdom-style prompt read-back, no-Worker-on-deny, real-realm computed value, and
      per-document scope in `js-consent.test.ts`; native refusal, gate enforcement and scope in
      `apps/cli/src/forms.rs`; script detection in `selis-pdf-form/src/scripts.rs`.
  - **Honest gap:** the editor's own form panel (`editor-page.ts`) is not yet wired to the prompt —
    the Fill Forms card on the tools page is. The browser detection is a conservative ASCII name
    token scan (`/JS`, `/AA`, `/CO`) OR-ed with the tools layer's typed manifest; it is a prompting
    signal, not a security boundary (the sandbox's forbidden-API scan and consent gate are).
    Computed values from the browser sandbox are shown but not written back into the PDF.
- [x] **SL-5.JS.03 — Admin policy for pre-approved origins** · deps: JS.01 · owner: AI
  - **Done:** `selis-policy::ApprovedOrigins` (Rust) + TS mirror: a list of pre-approved origins
    and document ids, empty = deny-all, with approve/revoke and `is_approved`. Combined in
    `ScriptAuthorization::authorize` / `authorizeScripting` as the explicit ADR-P0020 exception to
    reader consent. Default-deny, origin and per-id approval, and gate behaviour are tested.
  - **Done (this slice — explicit, scoped, auditable, decline-proof):** `selis-policy::ScriptPolicy`
    parses an administrator-supplied allow-list from caller-provided text (no file/network I/O in
    the engine): only `origin <value>` / `id <value>` lines are accepted; blank lines and `#`
    comments are ignored, and any other shape is a typed `JsPolicyMalformed` refusal (new registry
    code 4306) that enables nothing. `ConsentPolicy` now records explicit reader declines with
    `decline` / `is_declined` / `undecline`; `ScriptAuthorization::authorize` checks the decline
    FIRST and returns the new typed `JsDeclined` (4305), so an admin allow-list can never silently
    re-enable a document the reader said "No" to. Native CLI `fill-forms` gains `--script-policy
    <FILE>` (or `SELIS_SCRIPT_POLICY`) and `--deny-js`: a malformed file is a typed refusal, only the
    exact named id/origin is admitted, an audit line on stderr names the policy source when the
    admin list (not `--allow-js`) opened the gate, and `--deny-js` wins over both. The TS mirror
    (`js-policy.ts`, `js-consent.ts`) adds the same decline precedence, an explicit
    `setAdminPolicy` / `parseAdminScriptPolicy` (no guessed origins) and tests; the browser gate
    remains default-deny and the Worker runner still re-checks before creating a Worker.
  - **Evidence (named tests):** Rust `selis-policy` (`a_reader_decline_is_never_overridden_by_admin_approval`,
    `malformed_admin_policy_is_a_typed_refusal`, `an_admin_allow_list_admits_only_the_exact_scoped_origin`,
    `an_allow_list_is_auditable`), `selis-pdf-js/tests/sandbox.rs`
    (`a_reader_decline_is_never_overridden_by_admin_approval`), CLI `apps/cli/src/forms.rs`
    (`admin_policy_permits_only_the_exact_scoped_document`, `a_reader_decline_is_never_overridden_by_admin_policy`,
    `malformed_admin_policy_is_a_typed_refusal`), host vitest `js-policy.test.ts` / `js-consent.test.ts`.
  - **Not claimed:** the allow-list is a static, caller-supplied list; there is no remote policy
    service, signature over the policy file, or rotation/expiry of entries. An attacker who can edit
    the local policy file can approve origins — the file is trusted administrator configuration,
    not a trust boundary against a compromised host.

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
- [ ] **SL-5.SIGN.04 — Timestamping (RFC 3161) and LTV** · deps: SIGN.03, SL-0.LEAD.08 · owner: AI+ (consent-gated slice), HUMAN (trust membership)
  - **Do:** TSA integration, DSS/VRI construction, and document-timestamp signatures for long-term
    validation (PAdES B-LT / B-LTA).
  - **Done (partial, consent-gated slice):** the engine stays network-free. `selis-pdf-sign::embed_dss`
    is a PURE writer that takes ALREADY-FETCHED DER (RFC 3161 tokens, OCSP, CRLs, certs), writes a
    `/DSS` with `/TS /OCSP /CRL /V` as an incremental revision, parses each token with the new
    `selis-crypto::timestamp` DER reader and refuses one whose `messageImprint` is not SHA-256 of
    the named signature's `/Contents`. Read-back proves the DSS chain and that the original
    signature still verifies. The **only** socket in the workspace is the native CLI binding
    `apps/cli/src/tsa.rs`: it runs solely under `--connect-tsa` / `--connect-aia`, requires a URL
    from `SELIS_TSA_URL` / `SELIS_OCSP_URL` (no hardcoded third party), prints the exact consent
    line to stderr **before** connecting, and sends only a 32-byte hash of the signature (ADR-P0017)
    — never document bytes, keys or a request body containing them. `http://` only; HTTPS, chunked
    responses and AIA auto-discovery are refused with typed errors. E2E tests prove: default sign
    opens no socket and writes no DSS; no flag/URL is a typed refusal; a local mock TSA receives
    the hash and the returned token embeds while the signature keeps verifying.
  - **Consent wording (stderr, verbatim):** "Connecting to external time provider &lt;URL&gt; to
    timestamp this signature; a hash of the document will be sent. Continue?" The flag *is* the
    opt-in; there is no silent connection path. No document bytes leave the device without it.
  - **Done (trust anchors, "Foxit model"):** `selis-crypto::timestamp::verify_tsa_token` now
    verifies the token's OWN CMS signature over the `TSTInfo` (via `cms::verify_signer`) and walks
    the TSA signing certificate's chain to caller-supplied anchors (`--tsa-anchor` on the CLI, PEM
    bundles accepted). `embed_dss` refuses a token when anchors are supplied but the signature or
    chain fails, and reports `tsaChainTrusted`; with no anchors a token is embedded but reported
    untrusted — never silently trusted. Tests: trusted anchor, no/wrong anchor, forged TSTInfo.
  - **Remaining long-lead (NOT code):** Adobe-trusted / AATL membership and a CA-trusted TSA
    certificate are required before a timestamp is honoured by Acrobat as LTV — that is a trust /
    procurement long-lead (SL-0.LEAD.08), not something this slice can claim. Also open: an X.509
    AIA extension reader (we use a configured responder URL instead, deliberately avoiding SSRF
    from a document-controlled URL), HTTPS transport, full CRL/DSS-VRI refresh, and a separate
    document-timestamp *signature* object with its own `/ByteRange` over the revised file (PAdES
    B-LTA) — this slice records the parsed, imprint-bound token in `/DSS /TS` (PAdES B-LT data)
    and does not claim B-LTA.
- [x] **SL-5.SIGN.05 — Certification signatures and DocMDP** · deps: SIGN.03 · owner: AI+
  - **Done:** `selis-pdf-sign::sign_certification` writes the same detached CMS as SIGN.03 plus
    `/Reference → /TransformMethod /DocMDP /TransformParams /P` (levels 1/2/3), re-opens the
    output, follows the graph to read `/P` back and re-runs the real signature report before
    returning. The written shape is exactly the one `selis-pdf-edit::signature_guard` parses, so
    creation and EDIT.07 enforcement agree. Tests cover all three levels plus an edit→guard
    round-trip that proves a level-1 forbidden content edit is refused with a named error.
- [ ] **SL-5.SIGN.06 — Key sources** · deps: SIGN.03 · owner: AI+ (partial), HUMAN (PKCS#11/CSC)
  - **Do:** PKCS#12 files, OS keystores (Windows CryptoAPI, macOS Keychain), PKCS#11 tokens and
    smart cards, and cloud-signing (CSC API) for the eIDAS path.
  - **Done (partial):** `selis-pdf-sign::p12::load_pkcs12` opens a password-protected `.p12/.pfx`
    locally with `p12-keystore` 0.4.1 (MIT/Apache-2.0, pure Rust, MSRV 1.85; `rc2` is MIT for
    legacy stores), converts the PKCS#8 key and X.509 chain to the existing PEM path and refuses
    EC keys, wrong passwords and oversized blobs. Tests build a real encrypted store in-test,
    sign and verify from it, and assert private-key bytes never appear in the PDF. Native-only;
    no WASM chunk grows. `keystore.rs` is a native-only typed abstraction of Windows CNG / macOS
    Keychain / Linux secret-service; because the only available bindings are raw `unsafe` FFI,
    signing returns a precise typed `OsKeyStoreUnavailable` instead of pretending to support it.
    Keys are never uploaded (ADR-P0019).
  - **Remaining:** PKCS#11/PIV tokens and CSC (cloud) signing are NOT implemented — they need a
    safe native backend / CA integration and are a genuine barrier to entry for European public
    sectors. The typed OS-keystore gap is deliberate, not a silent fallback.
  - **Note:** Smart-card/eID support is a hard requirement in several European public sectors and
    a genuine barrier to entry for competitors.
- [x] **SL-5.SIGN.08 — Trust anchors and local self-signed identity (Foxit model)** · owner: AI+
  - **Decision:** do what Foxit does — support bring-your-own certificates (PKCS#12, SIGN.06),
    offer a local self-signed identity, and trust public CA anchors the caller supplies. Do NOT
    run our own public CA or pursue AATL membership as a code prerequisite. Owning a domain helps
    host CRL/OCSP/TSA endpoints but does NOT make a root trusted by Acrobat (AATL/EUTL is the only
    path to a default-green check, and that is a business decision — SL-0.LEAD.08).
  - **Done:** the verifier (`selis-pdf-sign::signatures` → `selis-crypto::cert::validate_chain`)
    walks the signer's chain to caller-supplied anchors; `--anchor` accepts a PEM **bundle** of
    several CERTIFICATE blocks (`cert::pem_certificates`). `TrustReport.trust_scope` is always
    `"selis-caller-anchors"`, so a local "trusted" verdict is explicitly NOT a public/Adobe one.
    `selis-crypto::identity::generate_self_signed` builds a 2048–4096-bit RSA key and a self-signed
    X.509 v3 certificate locally (SHA-256, basicConstraints/keyUsage/SKI), reads it back and
    verifies its self-signature, and uses pure-Rust deps only (no new dependency, no `unsafe`,
    ADR-P0019/P0021; rcgen was rejected for MSRV 1.88 > workspace 1.85 and its C crypto deps).
    CLI `selis generate-identity --name ... --output DIR [--bits N] [--days N] [--ca] [--force]`
    writes `key.pem`/`cert.pem`, refuses overwrite by default, and prints the honesty line "not a
    publicly trusted / AATL identity". Tests: a generated identity is trusted only when its own
    cert is supplied as an anchor; with no anchors or a wrong CA it is not; a bundle's later
    blocks are loaded; no private key bytes appear in the PDF.
  - **Remaining (external/business):** a publicly trusted (AATL/EUTL) signing certificate and CA
    membership — procurement, not code. System/OS trust-store auto-loading is deliberately NOT
    done (the browser has none; we keep trust explicit and local-only).
- [x] **SL-5.SIGN.07 — Signature appearance generation** · deps: SIGN.03, ANNOT.01 · owner: AI+
  - **Done:** `selis-pdf-sign::appearance` adds a typed `AppearanceTemplate` (signer name, reason,
    location, human-readable date, optional **caller-supplied** RGB logo) and
    `build_signature_appearance(page, rect, template, strokes)`, which reuses
    `crate::visual::content_stream` / `VisualOptions` / `VisualStroke` — the same renderer as a
    standalone stamp — to produce the widget's `/AP /N` form XObject, its `/BBox`, `/Resources`
    (font plus an image XObject for the logo) and the logo stream. The logo's bytes come from the
    caller; non-finite/oversized geometry, a stroke with fewer than two finite points and missing
    or mis-sized logo bytes are typed `SignError::Invalid` / `SignError::Limit` refusals — there is
    no fetch, file read or decoder. `DigitalOptions` carries `logo` / `strokes` and the detached-CMS
    writer (`sign_digital` and, through it, `sign_certification`) builds the widget appearance from
    the template; the CMS byte range, detached signature and verify-only-on-read-back path are
    unchanged. The incremental revision is checked to preserve the source bytes verbatim and the
    written `/AP /N` stream (plus the logo XObject) is re-opened and compared byte-for-byte on
    native builds. `DigitalReport` gains `appearance_bytes` / `has_logo`.
  - **Evidence (named tests):** `crates/selis-pdf-sign/src/appearance.rs` unit tests (typed logo
    validation, finite geometry, strokes, `/SelisSigLogo Do`); `crates/selis-pdf-sign/tests/appearance_e2e.rs`
    (`a_typed_appearance_becomes_a_readable_ap_with_a_logo` — real detached CMS over the committed
    PEM fixtures, independent `/AP` + image read-back, signature still verifies, source is a verbatim
    prefix; `missing_logo_bytes_are_a_typed_refusal_before_any_write`;
    `render_parity_holds_outside_the_appearance_rect` — engine RGBA before/after are identical
    everywhere outside the masked widget rect and the appearance paints inside it).
  - **PNG/JPEG logos accepted (native and browser, this slice):** `SignatureLogo::from_encoded(bytes, &mut guard)` decodes PNG/JPEG file bytes with the workspace's existing pure-Rust `selis-image` decoder — no new dependency, no file read, no network. Type 8 grey/RGB/palette/grey+alpha/RGBA PNGs and baseline/progressive grey/RGB/CMYK JPEGs are supported; decoder failures become a typed `SignError::Invalid` ("not a PNG or JPEG this build can decode"), oversized encoded payloads `SignError::Limit`, and decoded pixels go through the same dimension/length validation as raw RGB. The native CLI exposes it as `selis sign --logo <file>` (the file is read and decoded in the binding layer; a malformed file is a typed refusal before any output). In the browser the same constructor and the same decoder now run inside a **dedicated lazy chunk**, `selis-pdf-wasm-sign-logo` (artifact `selis_pdf_wasm_sign_logo`), which the sign card fetches only when a local logo file is selected. That chunk exists *because* a linked call site in `selis-pdf-wasm-sign` measured 255 153 B brotli versus 225 276 B for the unchanged chunk — +29 877 B, far past that chunk's 2% cap of 225 402 B (baseline 220 983 B); the lazy chunk has its own 600 000 B budget and stays import-free (ADR-P0011). The page still reads the logo in the tab, the key still arrives in the options JSON and is never echoed, and the no-upload/consent model is unchanged. Named tests: `crates/selis-pdf-sign/src/appearance.rs` (`a_png_logo_decodes_to_exact_rgb_pixels`, `a_jpeg_logo_decodes_to_opaque_rgb_pixels`, `undecodable_or_oversized_logo_bytes_are_typed_refusals`), `crates/selis-pdf-sign/tests/appearance_e2e.rs` (`a_decoded_png_logo_survives_detached_signing_and_ap_read_back`, `an_encoded_logo_signs_and_the_ap_reads_back_byte_for_byte`, `an_undecodable_encoded_logo_is_refused_before_a_write`), `apps/cli/tests/sign_logo.rs` (`a_png_logo_is_decoded_into_the_signature_appearance`, `undecodable_logo_bytes_are_refused_before_a_write`), `crates/selis-pdf-wasm-sign-logo/src/lib.rs` (four chunk-protocol tests including a real PNG sign through the guest ABI) and `apps/web/host/src/tools/sign-logo-protocol.test.ts` (14 typed-DOM/wire tests).
  - **Not claimed (honest gaps):** alpha/soft masks are still deferred — `from_encoded` flattens RGBA to RGB by **dropping alpha**, so a transparent logo keeps its colour but paints opaquely (no soft-mask XObject); text uses the
    named standard-14 Helvetica faces and refuses non-WinAnsi characters instead of embedding a
    font. The `/AP /N` byte-for-byte read-back no longer counts as a gap: the earlier attempt to put it in `selis-pdf-wasm-sign` measured 227 088 B brotli, +1 812 B over the 225 276 B chunk and past the 2% cap of 225 402 B, so instead it lives in the same lazy `selis-pdf-wasm-sign-logo` chunk and runs on every target, including wasm32. Browser logo signing still verifies the detached CMS on read-back through the unchanged `selis-pdf-wasm-sign` path, and the signature report re-checks the byte range. AATL/trust membership and timestamp trust remain the external
    long-lead (SL-0.LEAD.08 / SIGN.04) and are not touched here.

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

> **Status reconciliation (2026-11-19):** Verified against code and flipped stale `[ ]` markers to `[x]` for items whose implementation exists: EDIT.01 (journal), EDIT.02 (undo/redo), EDIT.03 (incremental writer + 100 000-mutation suite), EDIT.08 (page ops), EDIT.09 (object rewriting), EDIT.10 (`.selis` bundle), EDIT.11 (compare engine+CLI), ANNOT.04/05/06/07, FORM.01/04, REDACT.01–04/06, CONV.01–04/06. Items left open are genuinely missing or partial: TEXTEDIT.01/03/04/05/06, FORM.03/06/07/08, all JS.*, SIGN.04/06 (SIGN.05 delivered: certification signatures + DocMDP; SIGN.04 has a
consent-gated TSA/LTV slice but remains open for Adobe/AATL trust membership), all OCR.*, BIZ.*. REDACT.05 was in this open list when that note was
written; it now has the reviewed web card described above, still with no auto-apply path. ENC-side
items elsewhere remain gated on human review. Verification was read-only against `crates/` and `apps/`.
