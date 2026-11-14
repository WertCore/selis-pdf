# 45 — Annotations: the marks a reader leaves on a document

**Status:** the first slice shipped. **Owner:** AI+ · **Companion to:** `41-EDITOR-ROADMAP.md`,
`42-EDITOR-UI-REDESIGN.md`, `44-EDITOR-NATIVE-EDITING.md`, `23-EDIT-MODEL-SPEC.md`.

**Why this file exists:** the roadmap's biggest single gap between us and the
market's feature list was "Comment & Annotate" — highlight, underline,
strikeout, a sticky note, a freehand mark, and a reply. None of it existed,
because the engine could read a document's annotations but not **write** one.

---

## 1. The crate

`crates/selis-pdf-annot` is the crate the architecture already named
(`01-ARCHITECTURE.md` §3) and the layers already reserved
(`selis-pdf-annot = 2`). It holds three things, deliberately separate:

| | |
|---|---|
| the **model** | `AnnotKind`, `AnnotSpec`, `PageAnnotation` — what a markup is, in the terms the PDF specification uses |
| the **appearance generator** | the `/AP` stream each kind draws, so a viewer that does not implement the annotation's own appearance still shows the mark |
| the **writer and the reader** | `annotate`, `remove_annotation` and `list_annotations` |

### The honesty rules it is built around

- Every write is an **incremental revision**: the bytes before the edit are
  preserved verbatim, so a signature over an earlier revision stays
  verifiable.
- An annotation is written **with** its appearance. A mark that only some
  viewers draw is a mark that is not in the document.
- The reader reports what the document says. An annotation with no
  `/Contents` is an annotation with no text, not an empty comment.

### What it writes today

`Highlight`, `Underline`, `StrikeOut`, `Text` (the note), `Ink` (the drawn
mark) and `Square`. Each carries its `/Type /Annot`, its `/Subtype`, its
`/Rect`, its `/C`, its `/F 4` (print), its `/P` (the page), its `/QuadPoints`
where it covers text, and an `/AP` form XObject with a `/BBox` of its own.

The highlight's appearance is **translucent** (`/ca 0.55`), because a
highlight that hides the words under it is not a highlight.

### What it does NOT do yet

- **Moving or editing an existing annotation.** The editor can add and it can
  list; it cannot yet address one ("the third highlight on page 2") and move
  it. That is the next slice, and it needs the same addressing model the
  roadmap's `SL-5.ANNOT.07` names.
- **Replies, review state and author identity** (`SL-5.ANNOT.04`).
- **Flattening** (`SL-5.ANNOT.05`) — the tools crate already flattens
  annotations, but it does not do it through this crate's model.
- **XFDF and FDF** (`SL-5.ANNOT.06`).

---

## 2. The wire

Two operations on the tools chunk, which the editor already loads:

| Op | Number | What it does |
|---|---|---|
| `annotate` | 35 | Writes one annotation and returns the document. |
| `annotations` | 36 | **Read-only**: the annotations a page carries, as a JSON report. |

The editor's markup is a drag: the ribbon's **Markup** group carries
Highlight, Underline, Strike out and Note; the drag over the words is the
region the mark covers; a note is a **click**, and the comment is written in
the inline editor **where the note will go**.

---

## 3. The bug the annotations exposed

The markup leg failed the redaction leg, and the reason was a real one.

**A redaction of a page that carries an annotation left the redacted text in
the file.**

An annotation's `/P` names the page it is on. When the redaction rewrote the
page, the annotation was copied with its `/P` still pointing at the **old**
page — and copying it dragged that old page into the output, and with it the
page's old content stream. The text the redaction was there to remove was
still readable in the file, in a page object no reader would ever draw.

That is the failure ADR-P0023 is about: *"Redaction is a removal operation
with a proof obligation."* The proof obligation was not being met whenever
the page carried an annotation.

**The fix:** the redaction's page copy drops the annotations' `/P`. `/P` is
**optional** (ISO 32000-1 §12.5.3), and the page's own `/Annots` array says
which page each annotation is on. The annotation itself is kept — a redaction
is not a way to lose a reader's comments — and the old page no longer
travels with it.

**The rule, which is worth more than the fix:** *a rewrite must not carry
the old page along by the back door.* Every tool that rewrites a page has to
ask what still points at the page it replaced.

The proof has its own test now
(`a_redaction_of_an_annotated_page_removes_the_text_from_the_file`), because
a proof obligation without a test is a hope.

---

## 3b. The finding the markup work turned up

Writing the markup leg found a defect that has nothing to do with
annotations:

> **After a text box is added, the OTHER text on the page loses its measured
> width.**

The text layer reports each line's glyph box, and the editor draws its
clickable overlay from that box. On a page whose text was measured before the
text box was added, the same line comes back with a box a fraction of its
real width — a few pixels instead of hundreds. The line's overlay is scaled
to that width, so the line becomes **invisible and unclickable**: the reader
cannot click a line they could click a moment earlier.

The text itself is still in the document, and the page still draws it (the
raster comes from the engine's renderer, not from the text layer). It is the
MEASUREMENT that is wrong.

This is in the sign chunk's page rewrite — the text box is a **visual stamp**,
and the stamp rewrites the page — or in the text layer's reading of the
rewritten page. What is known:

- it needs **both** an edited page and a stamp. A stamp on a page whose text
  was never edited measures fine (488px, then 426px for the box itself), and
  an edit on its own measures fine (516px). The edit followed by the stamp
  gives 27px.
- it is not the undo, the redo, the split, the attach or the signature: each
  was tried on its own, and none of them did it.

### 3b-i. The cause: the stamp numbered its objects from the wrong revision

The stamp writes an **incremental update**: the original bytes are kept and
the changed objects are appended. The object numbers it uses must not be
numbers the document already uses.

They were:

```rust
doc.revisions().last()          // the NEWEST revision only
    .map(|revision| revision.entries.keys().copied().max().unwrap_or(0) + 1)
```

An incremental update writes **only the objects it changes**, so the newest
revision's table holds ONE entry — the object the editor's text edit
rewrote. The max of that table is the edited object's number, and the stamp
handed out the number after it.

In the check's fixture the page's font is object 5 and the edited content
stream is object 4, so the stamp's own content stream was numbered 5 — the
font's number. The stamp **overwrote the page's font with its content
stream**:

```
3 0 obj
<</Type /Page/Parent 2 0 R/MediaBox [0 0 300 200]
  /Contents [4 0 R 5 0 R]/Resources 6 0 R>>
endobj
5 0 obj
<</Length 102>>
stream
q 0.05 0.05 0.12 rg ... (A stamp) Tj ET Q
endstream
endobj
6 0 obj
<</Font <</F1 5 0 R ...>>>>     <-- /F1 now points at the stamp's stream
endobj
```

So `/F1` resolved to a stream instead of a font dictionary. The page still
DREW its text — the renderer substitutes a face when a font cannot be
read — but the font's `/Widths` were gone, so the text layer's advance
table had no widths and every line measured a fraction of its real width.

The fix is one line and it is the same rule the annotation crate already
used: **every revision counts**, because a document's object numbers are
the highest across all of them.

```rust
let mut highest = 0u32;
for revision in doc.revisions() {
    for number in revision.entries.keys() {
        highest = highest.max(*number);
    }
}
highest.saturating_add(1)
```

### 3b-ii. The proof

The sign crate has a test that reproduces the collision
(`a_stamp_after_an_edit_does_not_overwrite_the_page_font`): it edits a
document, stamps it, and asks whether object 5 is still a font. It fails
without the fix and passes with it.

And the editor's own check measures what the reader can click, before the
stamp and after it:

```json
{"widthsBefore": [{"text": "Invoice total: 1200 USD", "width": 247}],
 "widthsAfter":  [{"text": "Invoice total: 1200 USD", "width": 247},
                  {"text": "Added by the editor", "width": 442}]}
```

Without the fix the same check says:

```
the line "Invoice total: 1200 USD" measured 247px before the stamp and 0px
after it; the stamp took the page's font away
```

---

## 4. The evidence

The browser check's `markup` leg:

```json
{"marked": "Added a mark on page 1",
 "noted": "Added a note on page 1",
 "comments": "HighlightNote — A comment from the check",
 "carriesTheAnnotation": true,
 "carriesTheAppearance": true}
```

- the mark is **in the file** (`/Subtype /Highlight`) ✓
- it carries an **appearance** (`/Subtype /Form`) ✓
- the document's own report lists **both** the highlight and the note ✓

And the redaction leg, which used to fail:

```json
{"stillCarriesText": false, "coversText": true}
```

---

## 5. Where the files are

| | |
|---|---|
| The crate | `crates/selis-pdf-annot/src/lib.rs` |
| Its tests | `crates/selis-pdf-annot/src/tests.rs` |
| The tools plumbing | `crates/selis-pdf-tools/src/annot.rs` |
| The wire | `crates/selis-pdf-wasm-tools/src/lib.rs` (ops 35 and 36) |
| The editor | `apps/web/host/src/editor/markup.ts`, `editor-page.ts` |
