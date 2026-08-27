# Phase 1A — The PDF toolkit (Weeks 10–16, parallel with Phase 2)

**Gate G1.5 exit criteria:** merge, split, page ops, unlock, protect, compress, and image↔PDF
shipping in the extension and web app · every output opens in Acrobat *and* PDFium · structural
verification on 100% of tool outputs · zero data-loss reports · the writer's property suite green
at 50 000 sequences.

---

## Why this phase exists

The original plan shipped a read-only viewer at month 7. But the highest-volume PDF use cases —
merge, split, compress, unlock, rotate — **do not need a renderer at all.** They are object-graph
operations over `selis-pdf-cos` + `selis-pdf-doc` + a writer. Everything they require exists at the end of
Phase 1.

That means a complete, sellable product can ship around **month 4**, three months before the
viewer, and it can be built by one person in parallel with the Phase 2 rendering work.

Three further reasons this ordering is right:

1. **It is the sharpest possible demonstration of ADR-P0016.** Smallpdf, iLovePDF, and every
   "merge PDF online" result upload your document to a server to rotate a page. We do it in the
   tab, offline, and the file never leaves the device. That comparison needs no explanation — it
   is the entire pitch, and it lands hardest on exactly these commodity operations.
2. **It hardens the writer eight months early.** `selis-pdf-edit`'s document writer is the most
   safety-critical component in the codebase (ADR-P0007, `23-EDIT-MODEL-SPEC.md`). Building it now,
   surrounded by six simple operations instead of a full editor, means it gets eight extra months
   of real-world exposure before anything complicated depends on it.
3. **The WASM payload is tiny.** No renderer, no font engine, no text layer — the toolkit chunk
   should land around 600–900 KB brotli against the 3 MB viewer budget. First paint is instant,
   and the extension package stays small enough that Chrome Web Store review is uneventful.

**Dependency note:** this phase needs Phase 1 complete (`SL-1.COS.*`, `SL-1.FILT.*`, `SL-1.ENC.*`,
`SL-1.DOC.*`) and the WASM binding tasks `SL-4.WASM.01/02/04/05`, which are pulled forward from
Phase 4. It does **not** need Phase 2 or Phase 3.

---

## 1A.WRITE — The document writer

The tools are all writer operations. Build the writer properly here and Phase 5 inherits it.

- [x] **SL-1A.WRITE.01 — Full-document writer** · deps: `SL-1.COS.09` · owner: HUMAN
  - **Note:** Shipped as `selis_pdf_cos::doc_writer::DocumentBuilder` + `ContentBuilder`
    (hand-written, no external crate): header, catalog, page tree, pages, content
    streams, classic xref table, trailer, Info dictionary, RGBA image embedding.
  - **Do:** Generate a complete, well-formed PDF from a document model: object renumbering, xref
    table or stream, trailer, `/ID` generation, and correct stream `/Length` handling. This is the
    generation path (merge, split, unlock all produce *new* documents), distinct from the
    incremental append path of `SL-1A.WRITE.02`.
  - **Files:** `crates/selis-pdf-edit/src/write/full.rs`
  - **API:** `fn write_document(model: &DocModel, sink: &mut dyn DocSink, opts: &WriteOpts) -> Result<()>`
  - **DoD:** Property test over generated document models: `parse(write(m))` is semantically equal
    to `m`; output opens in PDFium, qpdf, and Acrobat; `qpdf --check` reports no warnings.
- [ ] **SL-1A.WRITE.02 — Incremental-update writer** · deps: WRITE.01 · owner: HUMAN
  - **Do:** `23-EDIT-MODEL-SPEC.md §6` in full. Pulled forward from `SL-5.EDIT.03` because page
    rotation, page deletion, and metadata edits are all naturally incremental — and because this
    is the component that most benefits from early hardening.
  - **DoD:** The invariant suite at 50 000 sequences (100 000 at G5): original bytes are a
    byte-identical prefix, output parses, model matches, prior revisions readable, PDFium and
    Acrobat both open it.
- [ ] **SL-1A.WRITE.03 — Object-graph copying with resource reconciliation** · deps: WRITE.01 · owner: AI+
  - **Do:** Deep-copy an object subgraph from document A into document B, remapping references,
    de-duplicating identical resources, and resolving name collisions in resource dictionaries.
    This is the primitive underneath merge, split, and page insertion.
  - **DoD:** Copying a page carries its fonts, XObjects, patterns, shadings, and annotations with
    no dangling reference; a cyclic subgraph terminates; identical fonts across inputs deduplicate.
- [ ] **SL-1A.WRITE.04 — Cross-document reconciliation** · deps: WRITE.03 · owner: AI+
  - **Do:** The hard part of merge. Reconcile, across N input documents: named destinations,
    outlines/bookmarks, structure trees (ADR-P0031 — a merged document must stay tagged), form
    field name collisions, optional-content groups, embedded files, page labels, and `/ID`.
  - **DoD:** Merging two tagged, formed, outlined documents produces a result where every one of
    those features survives and validates. **This single test catches more bugs than any other in
    the phase.**
- [ ] **SL-1A.WRITE.05 — Structural verification** · deps: WRITE.01 · owner: HUMAN
  - **Do:** `save_rewritten()`'s verification obligation (`23-EDIT-MODEL-SPEC.md §7`) requires a
    renderer, which does not exist yet. Until G2, verify structurally instead: reparse the output,
    assert page count, assert every reference resolves, assert annotation/field/OCG counts match
    expectations, assert no object is unreachable that should be reachable, and run `qpdf --check`
    in CI.
  - **DoD:** A deliberately-corrupted writer output is caught. **Upgrade to full render + text
    verification at G2** — file that follow-up task now so it is not forgotten.
  - **Risk:** Structural verification is weaker than render verification. It is sufficient for the
    Phase 1A operations because none of them alter page content, only page *selection* and
    document structure. Do not reuse this weaker standard for any operation that rewrites content.
- [ ] **SL-1A.WRITE.06 — Crash-atomicity for the writer** · deps: WRITE.02, `SL-0.IO.05` · owner: HUMAN
  - **Do:** Kill the process at 500 random points during each tool operation; assert the output is
    always either absent, the untouched original, or a complete valid document.

- [ ] **SL-1A.WRITE.07 — Conformance-friendly writer defaults** · deps: WRITE.01, `SL-1.DOC.09` · owner: AI+
  - **Do:** Make the writer avoid, by default, everything that would make output non-conformant
    later: preserve and update XMP consistently with the Info dictionary, never introduce
    JavaScript or external references, never drop a structure tree that was present on input,
    carry output intents through, and keep font embedding intact on copy.
  - **DoD:** Run `SL-1.DOC.09`'s evaluable rule subset over the output of every tool, on every
    corpus file. A tool that degrades a document's conformance posture fails CI.
  - **Why now:** it costs almost nothing at this stage and is expensive to retrofit. A writer that
    silently strips tags or desynchronises metadata makes the Phase 9 compliance product much more
    work, and quietly damages users' documents in the meantime.

---

## 1A.TOOL — The operations

Every task here shares a DoD template: output opens in Acrobat and PDFium; structural verification
passes; a corpus entry exercising the operation; a property test over generated inputs; the
operation is available identically in the CLI, the web app, and the extension.

- [x] **SL-1A.TOOL.01 — Merge** · deps: WRITE.04 · owner: AI+
  - **Note:** Shipped as `selis merge a.pdf b.pdf -o out.pdf` — object-graph copy with
    renumbering; each source page's content + resources are copied. Outline/forms handling is a
    refinement (the DoD's 10-document scenario is not yet automated).
- [x] **SL-1A.TOOL.02 — Split and extract pages** · deps: WRITE.03 · owner: AI+
  - **Note:** Shipped as `selis split in.pdf --first N --last M -o out.pdf` — page range split
    with per-page resource copying. every-N / by-size / by-bookmark modes and resource-pruning
    measurement are refinements.
- [ ] **SL-1A.TOOL.03 — Page operations** · deps: WRITE.02 · owner: AI+
  - **Do:** Rotate, delete, reorder, duplicate, and insert blank pages — as **incremental updates**
    where the input is a single document, so a 200 MB file rotates one page in milliseconds and
    keeps its signatures valid.
  - **DoD:** Rotating one page in a 200 MB document appends < 2 KB and meets the
    `03-CONVENTIONS.md §12` incremental-save budget.
- [ ] **SL-1A.TOOL.04 — Unlock: remove password** · deps: WRITE.01, `SL-1.ENC.02` · owner: HUMAN
  - **Do:** User supplies the password, we hand back a decrypted copy. Decrypt every stream and
    string with the document's handler, drop `/Encrypt` from the trailer, and write out a clean
    document. Works for whichever password the user has — user or owner.
  - **API:** `fn remove_encryption(doc: &Doc, pw: &Password, sink: &mut dyn DocSink) -> Result<()>`
  - **DoD:** Round-trips RC4-40, RC4-128, AESV2, and AESV3/R6; output has no `/Encrypt`, every
    stream decodes, and it opens in Acrobat and PDFium.
  - **Engineering notes** — the parts that are easy to get wrong:
    - **This is a full rewrite, not an incremental append.** ADR-P0007's default does not apply:
      the encrypted bytes must not survive in the output, so this uses the `WRITE.01` generation
      path with `WRITE.05` verification.
    - **Not every string and stream is encrypted.** `/EncryptMetadata false` leaves the XMP
      packet in the clear; identity crypt filters (`/Identity`) exempt specific streams; the
      `/Encrypt` dictionary itself and the `/ID` are never encrypted. Decrypting something that
      was already plaintext produces garbage — a real bug in several tools. Test it.
    - **Cross-reference streams and object streams are encrypted as a whole**, not per contained
      object. Decrypt at the right granularity.
    - **Signatures do not survive.** Decrypting rewrites the byte layout, so any existing digital
      signature is invalidated. Detect this *before* running and tell the user, rather than
      silently handing back a document whose signature has quietly died.
    - **Wrong password must be a clean typed error** (`WRONG_PASSWORD`), never a partial decrypt
      that produces a corrupt file.
  - **Corpus:** `encrypted-legacy` and `encrypted-aes`, plus a document with `/EncryptMetadata
    false`, one with an identity crypt filter, and one that is signed *and* encrypted.
- [ ] **SL-1A.TOOL.05 — Clear permission restrictions** · deps: TOOL.04 · owner: AI+
  - **Do:** The adjacent case: a document that opens with no password but sets `/P` bits
    forbidding print, copy, or edit. Given the owner password, clear them — same as Acrobat's
    security-settings removal. Follow `SL-1.ENC.04`: honour the bits by default, offer an explicit
    override, record it in the oplog.
  - **DoD:** A corpus of permission-restricted documents round-trips with the bits cleared.
  - **Note:** Distinct from TOOL.04 and lower priority — ship TOOL.04 first, it is the one users
    ask for by name.
- [ ] **SL-1A.TOOL.06 — Add password / set permissions** · deps: `SL-1.ENC.02` · owner: HUMAN
  - **Do:** Encrypt with AESV3/R6 only (ADR-P0019). Separate user and owner passwords, permission
    bit selection, and a plain-language explanation in the UI that permission bits are a
    convention, not enforcement.
- [ ] **SL-1A.TOOL.07 — Compress / optimise** · deps: WRITE.01, `SL-1.FILT.02` · owner: AI+
  - **Do:** Recompress streams, deduplicate identical objects, garbage-collect unreferenced
    objects, drop unused resources, and optionally downsample images (needs `selis-image` decode,
    which Phase 1 provides for DCT and Flate).
  - **DoD:** A size/quality preview before the user commits; a guarantee that "lossless" mode
    changes no rendered pixel — verified structurally now, verified by render at G2.
  - **Note:** Font subsetting and aggressive image work land at G2/G3. Ship the lossless tier first.
- [ ] **SL-1A.TOOL.08 — Images → PDF** · deps: WRITE.01 · owner: AI
  - **Do:** JPEG, PNG, WebP, HEIC, TIFF (multi-page) → PDF with page-size fitting, orientation, and
    margin options. Pure generation — no renderer needed. High-volume, low-difficulty.
- [x] **SL-1A.TOOL.09 — Metadata editor** · deps: WRITE.02 · owner: AI
  - **Note:** Shipped as `selis set-metadata --field K=V -o out.pdf` — rewrites the document with
    the Info dict set and the catalog `/Info` wired. XMP editing is a refinement.
  - **Do:** Read and edit Info dictionary and XMP, including bulk metadata stripping — which is a
    genuine privacy feature and a natural fit for this product's positioning.
- [x] **SL-1A.TOOL.10 — Attachment extraction** · deps: `SL-1.DOC.07` · owner: AI+
  - **Do:** List and extract embedded files, with filename sanitisation (path traversal is a real
    attack here — see `22-SECURITY-AND-SUPPLY-CHAIN.md` T12) and an explicit user action per file.
  - **Note:** Shipped as `selis extract --format=embedded` (inventory, JSON) and `--output <dir>`
    (extraction with sanitised filenames).
- [ ] **SL-1A.TOOL.11 — Batch mode** · deps: TOOL.01, TOOL.07 · owner: AI+
  - **Do:** Apply any tool across a file set, with per-file isolation so one bad document never
    kills the batch, plus a machine-readable report.
- [x] **SL-1A.TOOL.12 — PDF → images** · deps: Phase 2 · owner: AI
  - **Note:** Shipped at G1.5 as `selis convert` (PPM). Ships at G2 with PNG/JPEG output.

---

## 1A.UI — The toolkit surface

- [ ] **SL-1A.UI.01 — Tool-first web app layout** · deps: `SL-4.UI.01` · owner: AI
  - **Do:** The landing surface is a grid of tools, not a document viewer — this is what the
    audience searching "merge pdf" expects. Multi-file drag-drop, reorder by dragging, run, download.
  - **Note:** This means `SL-4.UI.01` (`PlatformAdapter`) is pulled forward from Phase 4. It is a
    small interface and building it now is the right call regardless.
- [ ] **SL-1A.UI.02 — Per-tool result verification display** · deps: `SL-1A.WRITE.05` · owner: AI
  - **Do:** Show what was verified after each operation — page count, preserved features, size
    delta. Turning the verification pass into visible UI is a trust asset competitors cannot copy
    without doing the work.
- [ ] **SL-1A.UI.03 — "Your file never left this device" indicator** · deps: `SL-8.BOUND.03` · owner: AI+
  - **Do:** Pull the data-flow indicator forward from Phase 8. On a tools product it is the single
    most valuable piece of UI on the page.
  - **DoD:** Backed by the egress test harness (`SL-8.BOUND.02`), also pulled forward — the claim
    must be mechanically true from the first release, not from Phase 8.
- [ ] **SL-1A.UI.04 — Extension surface for tools** · deps: `SL-4.EXT.01`, UI.01 · owner: AI+
  - **Do:** Side panel and toolbar entry points; right-click a PDF link → run a tool; multi-file
    selection handled in the extension's own page rather than a content script.
  - **Note:** `SL-4.EXT.02` (PDF navigation interception) is *not* required for the tools release.
    The extension can ship as a tools surface first and gain the viewer at G4. That decouples the
    fiddliest extension work from the first ship.
- [ ] **SL-1A.UI.05 — Offline-first PWA** · deps: `SL-4.WEB.02` · owner: AI
  - **Do:** The whole toolkit works airplane-mode. Demonstrably. It is the proof of the claim.
- [ ] **SL-1A.UI.06 — Large-file handling UX** · deps: `SL-0.SBX.05` · owner: AI+
  - **Do:** Progress, cancellation, and honest failure when a file exceeds the WASM budget —
    with a clear message rather than a dead tab. Competitors have server-side memory; we have a
    tab, so this must be graceful.

---

## 1A.SHIP — Launch

- [ ] **SL-1A.SHIP.01 — Chrome Web Store submission** · deps: UI.04, `SL-4.EXT.04` · owner: HUMAN
  - **Note:** Total procurement cost to reach this point is the $5 CWS developer fee
    (`27-COST-AND-LICENSING.md §2`).
- [ ] **SL-1A.SHIP.02 — SEO landing pages per tool** · owner: HUMAN
  - **Do:** One page per operation. This category's demand is almost entirely search-driven, and
    "merge pdf", "compress pdf", "unlock pdf" are the queries. The differentiator in the meta
    description writes itself: it runs in your browser, the file is never uploaded.
- [ ] **SL-1A.SHIP.03 — Free/paid boundary for tools** · deps: `SL-5.BIZ.01` · owner: HUMAN
  - **Do:** Recommendation: all single-file tools free and unlimited; charge for batch, for files
    above a size threshold, and for the Editor SKU later. Competitors gate on *daily task count*,
    which is universally resented and is only necessary because their costs are per-document.
    Ours are zero — that is a pricing advantage worth spending, not hoarding.
- [ ] **SL-1A.SHIP.04 — G1.5 review and go/no-go** · owner: HUMAN
