# 44 — The editor that feels native: the click, the caret, and the page

**Status:** shipped. **Owner:** AI+ · **Companion to:** `41-EDITOR-ROADMAP.md`,
`42-EDITOR-UI-REDESIGN.md`, `43-THEME-EXPORT-CORE.md`.

**Why this file exists:** a reader opened the editor, clicked a line, and said
three things:

> the "Open a PDF" button does not open a file;
> text editing does not feel native like an editor;
> inserting a text box and an image is not working.

All three were true, and none of them was the bug it looked like. This file
records what was actually wrong, because each of them is a trap that will
bite the next person who touches the text layer.

---

## 1. The text layer was eating the page

The text layer is the scaffold of clickable lines: one absolutely-positioned
element per line of the page's text, sitting over the rendered raster. It
covers the **whole page** — it has to, because a line can be anywhere on it.

It was also taking the pointer, because nothing said otherwise. So:

- a click on **empty space** landed on the layer, never on the canvas;
- the placement tools listen on the canvas, so **every tool that puts
  something on the page was dead** unless the reader clicked exactly on a
  word.

A text box, an image, a redaction: all three were "not working", and all
three worked the moment the layer stopped taking the pointer.

```css
.selis-editor-text-layer { pointer-events: none; }
.selis-editor-line { pointer-events: auto; }
```

The layer is a scaffold, not a target; the **lines** are the targets. The
pointer passes through the layer to the canvas, and the canvas's own handlers
do the placement.

**The trap:** a transparent overlay that covers everything is a mouse trap.
If an overlay is only a hit target for its own children, it must say so.

---

## 2. The editor was a form, not a line

The inline editor was a raised box with a border and a shadow, floating near
the text it was editing. That is what a *form* looks like, and editing a
document does not feel like filling in a form.

What Acrobat, Foxit and PDF.js all do is the opposite: **the line becomes the
editor**. The same size, the same place, the same baseline. The reader is
editing the document, and the editor is invisible until they type.

So the editor was rebuilt:

| | Before | Now |
|---|---|---|
| The text's place | the top of a box | **the line's own baseline** |
| The text's size | a fixed 16px | **the line's own size** |
| The box | a border, a shadow, a raised panel | the paper and the ink |
| The chrome | always there | a hairline, and only while it has the keyboard |

### The baseline is the whole thing

The text layer draws its own overlay text at `origin.y - 0.8em` from the top
of the line's box — that is where the overlay's baseline lands. The editor
has to start its text at **the same place**, or the typed text sits a
fraction of an em below the printed text and the reader can see that it does
not belong.

The first version of the rebuilt editor used `line-height: 2` and let the
browser's half-leading decide where the baseline fell. That put the editor's
text **0.3em below** the line's — 5px at 100%, 15px when zoomed in. The
check now measures the gap between the editor's text and the line's own
overlay text, and it is **0**.

### The paper and the ink

The editor writes **on the page**, so it is the page's colours and not the
theme's: a page of paper is white in the light theme and in the dark one, and
the ink on it is dark in both. Two custom properties carry it —
`--selis-editor-paper` and `--selis-editor-ink` — so the rule is stated
once.

### The editor grows with the words

The editor's width follows the text: a hidden mirror of the words is
measured, and the box is as wide as the widest thing in it. A caret that
ends up outside the box is a caret the reader cannot see.

---

## 3. Clicking away used to throw the edit away

The reader typed a change, looked at another part of the page, and the edit
was **gone** — silently, because the next editor simply replaced the old one.

That is the worst kind of loss: a change the reader did not mean to make is
visible and can be undone, and a silent loss is neither.

Clicking away now **commits** the change, the way Acrobat does. The handler
is in the **capture** phase, so the click that ends the edit cannot also open
another editor on a page that is about to change underneath it, and the
editor is closed only once the engine has answered — the reader sees the
change land, not a box vanish and a promise appear in the status bar.

---

## 4. The engine: the text stopped being editable after a stamp

This one was not visible in the browser at all. It surfaced when the new
click-away commit was proved in the check:

> The text could not be changed: in/0: this page's content is split across
> several streams; that is not editable yet

A page's `/Contents` is normally **one** stream. But a text box, an image or
a redaction **appends its own stream** and leaves `/Contents` an array. The
in-place text editor refused any page whose content was split — so:

> add a text box → the page's text can no longer be edited.

The editor could be used exactly once. That is not a shell problem, so it was
fixed in the core — `crates/selis-pdf-edit` — where the web app, the desktop
app and the SDK all get it:

- the page's content streams are read **in order** and joined with a
  newline, which is exactly what the reader sees (PDF draws an array of
  content streams as one run of operators);
- the text is found and rewritten in that joined run;
- the result is written back as **one** stream, and the page dictionary is
  rewritten to point at it.

The original bytes are still an incremental update — a byte-identical prefix —
so a signature over the original revision stays valid.

**The rule:** a tool that puts something on the page must not take away the
ability to change what was already there. A feature that disables the feature
next to it is a trap, and the trap is in the engine, not in the shell.

---

## 5. The evidence

The browser check grew three legs, and each one measures the thing that was
broken rather than the thing that was easy to measure:

- **`placement`** — the click is dispatched at **the element the browser would
  actually hit** (`document.elementFromPoint`), not at the canvas directly.
  The old leg dispatched on the canvas, which is exactly what hid the bug: it
  bypassed the hit test a reader goes through. The leg asserts that the hit
  at empty space is the canvas, and that the text box opened.
- **`inline`** — the editor's text and the line's own text must share a
  baseline (the measured gap must be 0), the editor must be the line's own
  size, the editor must be over the line, and **clicking away must commit
  the change**.
- **`face`** — the invitation's own "Open a PDF" button must ask for a file.

```json
{"placement": {"hit": "selis-editor-page__canvas", "onCanvas": true, "opened": true},
 "inline": {"committedOnClickAway": true, "gap": 0,
            "lineSize": "23.52px", "overTheLine": true, "size": "23.52px"},
 "face": {"pickerAsked": true}}
```

---

## 6. Where the files are

| | |
|---|---|
| The controller | `apps/web/host/src/editor/editor-page.ts` |
| The stylesheet | `apps/web/host/assets-src/editor.css` |
| The core fix | `crates/selis-pdf-edit/src/lib.rs` |
| The check | `xtask/browser/editor/index.html`, `xtask/src/browser.rs` |
