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
