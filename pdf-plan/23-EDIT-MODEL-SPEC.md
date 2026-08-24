# 23 — The Edit Model — Normative Specification

Implements ADR-P0007 and ADR-P0024. Read this in full before writing any of `selis-pdf-edit`.
Where this document and a task description disagree, this document wins.

---

## 1. Invariants

These hold at every point in the lifetime of an editing session. Each is a test.

**I1 — Source immutability.** The bytes of the opened document are never modified in memory or on
disk. `selis-bytes::Bytes` over the source is refcounted and immutable; there is no API that yields
a mutable view of it.

**I2 — Prefix preservation.** For every `save()` output `O` and original source `S`:
`O[0..S.len()] == S`. Byte for byte. The only operation exempt is `save_rewritten()`, which is a
distinct, explicitly-invoked function (§7) and is required for redaction (ADR-P0023).

**I3 — Revision monotonicity.** Every save appends exactly one revision. Revision `N-1` remains
parseable and renderable from the output of revision `N`.

**I4 — Journal completeness.** The in-memory document state is exactly
`materialise(source, journal)`. There is no mutation that bypasses the journal, and no state that
is not reconstructible from `(source, journal)`.

**I5 — Crash atomicity.** At any point during a save, the on-disk file is either `S`, or `S`
followed by a complete valid revision. A partially-written revision is detectable and discarded on
next open (the appended revision is only reachable via a `startxref` that is written last).

**I6 — Undo exactness.** `undo()` restores the model to a state equal to the pre-mutation state
under model equality, not under render equality. Two documents that render identically but differ
structurally are not equal.

**I7 — Non-interference.** A mutation to region A leaves every construct not reachable from A
byte-identical in the output. Editing a heading must not rewrite the form fields.

---

## 2. The mutation taxonomy

```rust
pub enum Mutation {
    // Object level
    SetObject   { id: ObjId, value: Obj },
    DeleteObject{ id: ObjId },
    ReplaceStream { id: ObjId, data: Bytes, filters: FilterChain },

    // Page level
    InsertPage  { at: PageIndex, source: PageSource },
    RemovePage  { at: PageIndex },
    MovePage    { from: PageIndex, to: PageIndex },
    RotatePage  { at: PageIndex, degrees: Rotation },
    SetPageBox  { at: PageIndex, box_: BoxKind, rect: Rect },

    // Content level
    ReplaceContentRegion { page: PageIndex, region: ContentRange, ops: DisplayOps },
    InsertContent        { page: PageIndex, at: ContentAnchor, ops: DisplayOps },

    // Annotation / form
    AddAnnotation    { page: PageIndex, annot: Annotation },
    ModifyAnnotation { id: ObjId, patch: AnnotationPatch },
    DeleteAnnotation { id: ObjId },
    SetFieldValue    { field: FieldPath, value: FieldValue },

    // Document level
    SetMetadata  { patch: MetadataPatch },
    SetStructure { patch: StructPatch },
    AddSignature { field: FieldPath, sig: SignatureRequest },
}
```

Rules:

- Every variant is **invertible**. `Mutation::inverse(&self, pre_state) -> Mutation` must exist and
  be total. `AddSignature` is the one exception and is therefore marked `Irreversible` — the
  journal refuses to place an irreversible mutation anywhere but the tail of an undo group, and the
  UI must warn.
- Every variant declares its **blast radius**: the set of object ids it can touch. `Non-interference`
  (I7) is tested by asserting the output diff is contained in the declared radius.
- Mutations are **applied**, not merged. Coalescing for undo (§5) happens at the group level, never
  by rewriting a mutation in place.

---

## 3. Journal and materialisation

```rust
pub struct Journal {
    source_id: SourceFingerprint,     // size + hash prefix + mtime; a changed source invalidates
    entries:   Vec<JournalEntry>,     // append-only
    groups:    Vec<UndoGroup>,
}

pub struct JournalEntry {
    id:        MutationId,
    mutation:  Mutation,
    inverse:   Mutation,
    at:        Nanos,                 // from the injected Clock
    actor:     ActorId,               // for collaboration and the oplog
}
```

Materialisation is a fold over the journal producing an **overlay** — a sparse map of
`ObjId -> Obj` layered over the source's `CosIndex`. Resolution checks the overlay first, then the
newest revision, then older revisions. This is the same lookup order the incremental writer will
produce on disk, which is why an in-memory session and a saved-then-reloaded session are
indistinguishable — a property test asserts exactly that.

The journal persists in the `.selis` session bundle so a crash or a browser refresh loses nothing.
On reopen, `source_id` is revalidated; a changed source is a hard error offering the user a choice,
never a silent reapply.

---

## 4. Text-edit reconstruction pipeline (ADR-P0024)

```text
glyphs (from DisplayList)
   │  cluster by graphics state + baseline + advance continuity
   ▼
runs        ── style-homogeneous glyph sequences with a font, size, colour, and matrix
   │  cluster by baseline proximity and horizontal ordering (bidi-aware)
   ▼
lines       ── ordered runs sharing a baseline, with measured inter-word gaps
   │  cluster by leading regularity, indentation, and alignment; consult the structure tree
   ▼
paragraphs  ── with an inferred alignment, leading, indent, and a Confidence
   │
   ▼
StyledText model  ── the thing the editor mutates
```

### Confidence

`Confidence` is a value in `[0,1]` computed from named, individually-testable signals:

| Signal | Weight | Reason |
|---|---|---|
| Structure-tree agreement | highest | If the document says where the paragraph is, believe it |
| Baseline regularity | high | Irregular baselines mean this is not flowed text |
| Font homogeneity | high | A "paragraph" spanning six fonts is probably a table row |
| Advance/space consistency | medium | Justified text with erratic spacing resists re-layout |
| Absence of rotation/skew in the text matrix | medium | Rotated text reflows unpredictably |
| No overlapping glyph boxes | medium | Overlap means the visual order is not the logical order |
| Column-boundary agreement | low | Cross-column merges are a classic reconstruction error |

Behaviour by band:

- `≥ 0.85` — offer full paragraph reflow editing.
- `0.55 – 0.85` — offer line-level editing; reflow available but the UI states that surrounding
  layout may shift, with a live preview.
- `< 0.55` — offer box/run-level editing only. **Do not silently reflow.** Say why.

The thresholds come from the labelled set in `SL-5.TEXTEDIT.01`, not from intuition, and are
re-derived whenever reconstruction changes.

### Re-emission

On commit, the affected paragraph is re-shaped (`selis-shape`), re-broken, and emitted as a **new
content region** that replaces exactly the byte range of the original paragraph's operators. The
rest of the content stream is copied byte-identically. New glyphs absent from the embedded subset
trigger `selis-font::extend_subset` (`SL-3.FONT.11`); if the font cannot be extended (no embedded
program, licence bits forbid it) the editor substitutes and tells the user rather than dropping
characters.

---

## 5. Undo groups

```rust
pub struct UndoGroup { label: String, entries: Range<usize>, coalescing: Coalesce }
pub enum Coalesce { Never, Typing { idle: Duration }, Dragging, Explicit }
```

- Typing coalesces into one group after an idle interval or a caret move.
- A single user gesture is one group even when it is many mutations — "delete page 3" is one undo,
  though it mutates the page tree, the outline, the structure tree, and named destinations.
- Redo is discarded on a new mutation, except that the discarded branch is retained in the oplog
  so "what did I lose?" is answerable.
- Undo across a save is allowed and produces a *new* revision that reverts — it never truncates
  the file, because I2/I3 forbid it.

---

## 6. The incremental writer

Emission order, normative:

1. Compute the changed-object set from the journal overlay.
2. For each changed object originally inside an object stream, decide: rewrite the whole object
   stream, or promote the object to a top-level indirect object. **Promote by default** — rewriting
   an object stream inflates the diff and risks disturbing unrelated objects (I7).
3. Serialise changed objects sequentially, recording offsets.
4. Emit the new xref — the same flavour the document's newest revision used (a table for a
   table-based document, a stream for a stream-based one). Mixing flavours across revisions is
   legal but confuses older readers; match, do not innovate.
5. Emit the trailer with `/Prev` pointing at the previous `startxref`, `/Size` correct, `/Root` and
   `/Info` carried forward, and `/ID` updated per ISO 32000-2 §14.4 (first element preserved,
   second element regenerated).
6. Emit `startxref` and `%%EOF` **last**. Everything before this point is unreachable by a parser,
   which is what gives I5.
7. `finish()` fsyncs.

Encryption: new objects are encrypted with the document's existing handler and the correct
per-object key (ADR-P0019). An unencrypted new object in an encrypted document is a correctness
bug that many tools ship; a test asserts we do not.

Signature preservation: existing `/ByteRange` values point into `S`, which I2 guarantees is
untouched, so signatures remain valid by construction. DocMDP levels are checked *before* the
mutation is accepted, not at save time, so the user is told immediately (`SL-5.EDIT.07`).

---

## 7. `save_rewritten()` — the exception

Required for: redaction (ADR-P0023), optimisation, PDF/A conversion, and any user-requested
"clean save".

Contract:

1. Build a fresh document from the materialised model with renumbered objects.
2. **Verify before replacing anything:** render every page of the new document and compare against
   the pre-save render (excluding regions the operation intentionally changed); compare extracted
   text; compare form field values, annotation count, structure-tree node count, and page count.
3. Only on a clean verification, write to a temporary file, fsync, and rename atomically.
4. On verification failure: produce no file, return a typed error naming the discrepancy, and keep
   the session intact so the user loses nothing.

The verification step is not optional and is not skippable by a flag. It is the reason a rewrite is
allowed to exist at all.

---

## 8. What the model deliberately does not do

- **No in-place mutation of the source, ever** — not even "just this once, for performance".
- **No lossy round-trip.** If we cannot preserve a construct through a save, the construct's
  conformance level is not `Parse` and the save must refuse or warn, not silently drop it.
- **No auto-repair on save.** A damaged document that we reconstructed in memory is saved as an
  incremental update over the *original damaged bytes*; we do not quietly hand the user a
  different file than the one they opened. An explicit "repair and rewrite" action exists and is
  a `save_rewritten()` with verification.
- **No merging of concurrent content edits.** Page-granularity last-writer-wins with an explicit
  conflict UI (ADR-P0027).
