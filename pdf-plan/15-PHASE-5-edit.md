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

- [ ] **SL-5.EDIT.01 — Mutation journal** · owner: AI+
  - **Do:** Every change is a typed `Mutation` appended to a journal: object set/delete, stream
    replace, page insert/remove/reorder/rotate, annotation add/modify/delete, field value set.
    The journal is the source of truth; the in-memory document is a materialised view of it.
  - **API:** `Journal::apply(&mut self, m: Mutation) -> Result<MutationId>`
  - **DoD:** Property test: applying then reverting any mutation sequence yields the original
    document state; the journal serialises to the `.selis` session bundle and reloads.
- [ ] **SL-5.EDIT.02 — Undo / redo** · deps: EDIT.01 · owner: AI+
  - **Do:** Undo stack over the journal with coalescing (typing is one undo, not thirty) and
    grouping (a "replace text" is atomic). Survives app restart via the session bundle.
  - **DoD:** Fuzz the undo stack with random operation sequences; state always converges.
- [ ] **SL-5.EDIT.03 — Incremental-update writer** · deps: EDIT.01 · owner: HUMAN
  - **Do:** Emit only changed objects plus a new xref section (table or stream, matching the
    document's existing style) plus a trailer with `/Prev`. **Never touch the original bytes.**
    Handle: object streams (a changed object in an ObjStm requires care), free-list correctness,
    generation numbers, and `/ID` update rules.
  - **DoD:** **The load-bearing test of this entire phase** — a property suite of 100 000 random
    mutation sequences where, for every output: (a) `output[..original.len()] == original`,
    (b) the result parses, (c) the semantic model matches the expected post-mutation state,
    (d) every prior revision is still readable, (e) PDFium and Acrobat both open it.
  - **Risk:** `owner: HUMAN`. Everything the user cares about depends on this being right.
- [ ] **SL-5.EDIT.04 — Crash safety of save** · deps: EDIT.03, SL-0.IO.05 · owner: HUMAN
  - **Do:** Kill the process at 500 random points during save; assert the file is always either
    the original document or a valid document containing a prefix of the intended changes.
  - **DoD:** The kill-test suite is green and runs nightly.
- [ ] **SL-5.EDIT.05 — Full-rewrite writer (`save_rewritten`)** · deps: EDIT.03 · owner: HUMAN
  - **Do:** The explicit, verified rewrite: renumber objects, drop unreferenced ones, re-encode
    streams, then **render-verify** every page against the pre-save state before the result is
    allowed to replace anything.
  - **DoD:** Verification catches a deliberately-corrupted rewrite; the operation is atomic
    (temp + rename) on every platform.
- [ ] **SL-5.EDIT.06 — Save through encrypted documents** · deps: EDIT.03, SL-1.ENC.02 · owner: HUMAN
  - **Do:** New objects encrypted with the document's existing handler (ADR-P0019 exception);
    an explicit upgrade path to AESV3 with user consent.
  - **DoD:** Incremental save into RC4, AESV2, and AESV3 documents; each result opens in Acrobat.
- [ ] **SL-5.EDIT.07 — Save preserving existing signatures** · deps: EDIT.03 · owner: HUMAN
  - **Do:** An incremental save must leave existing signature byte ranges intact so signatures stay
    valid, and must respect MDP/DocMDP permission levels — refusing changes the certification
    forbids, and clearly telling the user why.
  - **DoD:** A signed document annotated and saved still validates in Acrobat; a change forbidden
    by DocMDP level 1 is refused with a specific error.
- [ ] **SL-5.EDIT.08 — Page operations** · deps: EDIT.01 · owner: AI+
  - **Do:** Insert, delete, reorder, rotate, crop (`/CropBox` vs `/MediaBox` semantics), split,
    merge, extract, and N-up. Merging must reconcile: resource name collisions, named destinations,
    outlines, structure trees (ADR-P0031 — merged documents keep valid tagging), form field name
    collisions, optional-content groups, and embedded files.
  - **DoD:** Merge two tagged, formed, outlined documents; the result validates and every feature
    survives. This one test catches more bugs than any other in the phase.
- [ ] **SL-5.EDIT.09 — Object rewriting and resource management** · deps: EDIT.08 · owner: AI+
- [ ] **SL-5.EDIT.10 — Session bundle (`.selis`)** · deps: EDIT.01 · owner: AI
  - **Do:** The sidecar holding the journal, undo stack, oplog, and cached derived data, so a
    session survives a crash or a browser refresh without touching the source document.
- [ ] **SL-5.EDIT.11 — Compare documents** · deps: SL-2.CONT.07 · owner: AI+
  - **Do:** Structural + visual + textual diff of two documents (or two revisions of one), using
    the display list for visual diffing and the text model for content diffing.
  - **DoD:** A known edit is reported precisely; false-positive rate measured on re-saved-but-
    unchanged documents (which is the hard case).

---

## 5.TEXTEDIT — Text and object editing (ADR-P0024)

- [ ] **SL-5.TEXTEDIT.01 — Paragraph reconstruction with confidence** · deps: SL-3.TEXT.03 · owner: AI+
  - **Do:** Glyphs → runs → lines → paragraphs → a styled model, computing a confidence from
    baseline regularity, spacing consistency, font uniformity, and structure-tree agreement.
  - **API:** `fn infer_paragraphs(page, budget) -> Result<(Vec<Paragraph>, Confidence)>`
  - **DoD:** Confidence correlates with hand-labelled correctness on a 500-page labelled set;
    the threshold for offering reflow editing is derived from that data, not guessed.
- [ ] **SL-5.TEXTEDIT.02 — Text edit + re-layout + re-emit** · deps: TEXTEDIT.01, SL-3.SHAPE.03, SL-3.FONT.11 · owner: AI+
  - **Do:** Edit the model, re-shape and re-break the affected paragraph, re-emit that content
    region while leaving the rest of the stream byte-identical, and extend the embedded font
    subset with any new glyphs.
  - **DoD:** Editing one word in a paragraph changes only that paragraph's content region; the page
    renders identically outside the edit; a character absent from the original subset renders.
- [ ] **SL-5.TEXTEDIT.03 — Box-level fallback editing** · deps: TEXTEDIT.01 · owner: AI
  - **Do:** Below the confidence threshold, offer per-line or per-run editing with explicit
    "this may not reflow" UI rather than silently mangling the page.
- [ ] **SL-5.TEXTEDIT.04 — Font matching for edited text** · deps: SL-3.FONT.09 · owner: AI+
  - **Do:** When the original font is unembedded or unavailable, choose a substitute and tell the
    user, rather than silently changing the document's appearance.
- [ ] **SL-5.TEXTEDIT.05 — Image editing: replace, move, resize, delete, extract** · deps: SL-2.RAST.08 · owner: AI+
- [ ] **SL-5.TEXTEDIT.06 — Vector object selection and manipulation** · deps: SL-2.CONT.07 · owner: AI
- [ ] **SL-5.TEXTEDIT.07 — Add content: text boxes, images, shapes, links** · deps: SL-3.SHAPE.03 · owner: AI+
- [ ] **SL-5.TEXTEDIT.08 — Headers, footers, watermarks, Bates numbering, backgrounds** · deps: TEXTEDIT.07 · owner: AI
  - **Note:** Bates numbering is a legal-market requirement and a cheap differentiator.

---

## 5.ANNOT — Annotations

- [ ] **SL-5.ANNOT.01 — Annotation model + appearance-stream generation** · owner: AI+
  - **Do:** All markup subtypes: Text (note), Highlight, Underline, StrikeOut, Squiggly, Square,
    Circle, Line, Polygon, PolyLine, Ink, FreeText, Stamp, Caret, FileAttachment, Sound, Redact.
    Generate conformant `/AP` streams for each (ADR-P0026) so every other viewer shows them.
  - **DoD:** Each subtype round-trips through Acrobat and PDFium visually unchanged.
- [ ] **SL-5.ANNOT.02 — Appearance regeneration and `/NeedAppearances`** · deps: ANNOT.01 · owner: AI+
- [ ] **SL-5.ANNOT.03 — Ink and stylus geometry** · deps: ANNOT.01 · owner: AI+
  - **Do:** Pressure- and tilt-aware stroke smoothing that still emits a standard Ink annotation.
- [ ] **SL-5.ANNOT.04 — Markup replies, review state, and author identity** · deps: ANNOT.01 · owner: AI
- [ ] **SL-5.ANNOT.05 — Annotation flattening** · deps: ANNOT.01 · owner: AI+
- [ ] **SL-5.ANNOT.06 — Import/export XFDF and FDF** · deps: ANNOT.01 · owner: AI
- [ ] **SL-5.ANNOT.07 — Annotation UI: create, edit, move, style, comment panel** · deps: ANNOT.01, SL-4.UI.03 · owner: AI

---

## 5.FORM — Forms

- [ ] **SL-5.FORM.01 — AcroForm field model** · owner: AI+
  - **Do:** Field hierarchy, inheritance, widget↔field association (including one field with
    multiple widgets), field types (text, button, checkbox, radio, choice, signature), flags,
    and the `/DA` default appearance.
- [ ] **SL-5.FORM.02 — Field appearance generation** · deps: FORM.01, ANNOT.02 · owner: AI+
  - **Do:** Text fields with quadding, comb fields, multiline, rich text, auto-size fonts, choice
    lists, and checkbox/radio appearance states.
- [ ] **SL-5.FORM.03 — Calculation, validation, and format order** · deps: FORM.01, SL-5.JS.01 · owner: AI+
  - **Do:** `/CO` calculation order and the format/validate/calculate event sequence. Most real
    forms depend on this working exactly like Acrobat.
- [ ] **SL-5.FORM.04 — Form filling UI** · deps: FORM.02 · owner: AI
  - **Do:** Tab order, keyboard accessibility, required-field indication, and inline validation.
- [ ] **SL-5.FORM.05 — Form data import/export (FDF, XFDF, JSON, CSV)** · deps: FORM.01 · owner: AI
- [ ] **SL-5.FORM.06 — Form creation and field authoring** · deps: FORM.02 · owner: AI+
- [ ] **SL-5.FORM.07 — Auto-detect form fields on a flat document** · deps: FORM.06, SL-3.TEXT.03 · owner: AI+
  - **Do:** Infer fields from lines, boxes, and labels. A marquee Acrobat feature; ship it with a
    confidence indicator and easy correction.
- [ ] **SL-5.FORM.08 — XFA: render static, refuse dynamic honestly** · deps: FORM.01 · owner: AI+
  - **Do:** Render the XFA-embedded static PDF representation; for dynamic XFA, say so clearly
    rather than showing the "please upgrade your reader" placeholder page and nothing else.

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

- [ ] **SL-5.REDACT.01 — Region model and redaction annotations** · owner: HUMAN
  - **Do:** Mark regions (rect, text selection, or search-driven), preview, and store as `/Redact`
    annotations until applied.
- [ ] **SL-5.REDACT.02 — Content-stream removal** · deps: REDACT.01, SL-2.CONT.07 · owner: HUMAN
  - **Do:** Remove glyph-show operators whose glyphs intersect the region — including *partial*
    text-run splitting, which is where naive implementations leak. Re-encode covered image regions.
    Remove vector geometry inside the region.
- [ ] **SL-5.REDACT.03 — Non-content removal sweep** · deps: REDACT.02 · owner: HUMAN
  - **Do:** Annotations, form fields and their values, structure-tree nodes, MCIDs, optional
    content, embedded files, attachments, XMP and Info metadata, JavaScript, bookmarks pointing
    into the region, and **every prior revision in the incremental chain** — a redaction that
    leaves revision N−1 intact has redacted nothing.
  - **DoD:** A test that redacts a document with 5 incremental revisions and proves the content is
    absent from the output bytes entirely.
- [ ] **SL-5.REDACT.04 — The verification pass** · deps: REDACT.03 · owner: HUMAN
  - **Do:** Re-open the produced bytes with a fresh parser, run extraction, render the region, and
    grep the raw output bytes for the redacted strings. Any survival → error, no file produced.
  - **DoD:** A deliberately-broken redaction implementation is caught by the verifier. **Test the
    test**: this is the control that the whole claim rests on.
- [ ] **SL-5.REDACT.05 — Pattern-based bulk redaction** · deps: REDACT.01 · owner: AI+
  - **Do:** Regex/pattern search across the document (SSNs, card numbers, emails, names from a
    list) with a review step before applying. Never auto-apply.
- [ ] **SL-5.REDACT.06 — Redaction corpus + adversarial suite** · deps: REDACT.04 · owner: HUMAN
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

- [ ] **SL-5.CONV.01 — PDF → images (PNG/JPEG/TIFF/WebP)** · owner: AI
- [ ] **SL-5.CONV.02 — Images → PDF** · deps: CONV.01 · owner: AI
- [ ] **SL-5.CONV.03 — PDF → text / Markdown / structured JSON** · deps: SL-3.TEXT.07 · owner: AI
- [ ] **SL-5.CONV.04 — PDF → HTML with layout fidelity** · deps: SL-2.CONT.07 · owner: AI+
- [x] **SL-5.CONV.05 — HTML → PDF** · owner: AI+
  - **Note:** Decide deliberately: a real HTML engine is out of scope, so either restrict to a
    documented subset or drive a headless browser server-side as part of the Server SKU. Do not
    ship a half-working general HTML renderer.
  - **Note:** Shipped early (Phase 1A timeframe) as `selis topdf` via `selis-pdf-convert`: the
    documented-subset route — Markdown (headings, paragraphs, bold/italic/code, lists,
    blockquotes, fenced code, hr) and an HTML subset (h1–h6, p, b/i/em/strong, code/pre, ul/ol/li)
    laid out on Letter/A4 with standard-14 fonts via the WRITE.01 writer. A general HTML engine
    remains deliberately out of scope; the headless-browser route stays with the Server SKU.
- [ ] **SL-5.CONV.06 — Optimise / compress** · deps: SL-5.EDIT.05 · owner: AI+
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
