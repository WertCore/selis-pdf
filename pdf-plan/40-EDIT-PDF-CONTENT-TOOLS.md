# 40 — Editing the content itself: add a text box, add an image, redact

**Status:** Slice 2 shipped. **Owner:** AI+ · **Depends on:** `39-EDIT-PDF-WORKSPACE.md`, SL-4.WASM.02,
SL-1A.TOOL.03, SL-5.TEXTEDIT.01.
**Rule inherited from 35:** a box is ticked only when the slice ships through the owned engine, has
unit/browser evidence, and is documented.

## What this slice is

Slice 1 gave the workspace a document and the operations that move pages around. It left the
**content** alone: the only thing you could do to what is on a page was replace a run of text.

This slice adds the three things a person means by "edit the PDF":

- **add a text box** — new text, placed where the reader points;
- **add an image** — a picture, placed and sized where the reader points;
- **redact** — remove what is already there, rather than merely cover it.

## Decisions

- **D1 — Every placement is a rectangle the reader drags.** The workspace already knows the
  page's display geometry (the render reports the transform it used), so a rectangle in CSS pixels
  over the canvas converts to the two spaces the engine's writers use: **display points** (the page as
  it is displayed, origin at the bottom-left) for the stamp and the image overlay, and **user points**
  (the unrotated `/MediaBox` space) for the redaction, which edits the content stream. The
  conversion is the engine's own arithmetic — the same quarter-turn table the overlay writers use —
  and not a second implementation of it.
- **D2 — A text box is a stamp.** The engine's visual stamp (`selis-pdf-wasm-sign`, op 1) draws
  typed lines inside a rectangle as an appended content stream. It is the only primitive that
  writes NEW text at a chosen position, and it is already implemented and tested. The workspace
  drives it; it does not get a new engine operation.
  *Limit, stated on the page:* the stamp's font is Helvetica with WinAnsi encoding, so a character
  outside WinAnsi is refused by the engine rather than drawn as a substitute. And the engine
  **measures** the text with its own coarse width table before it draws it, refusing a line that
  does not fit — so the box is sized with that same table, not with the browser's font metrics
  (which are narrower, and would be refused).
- **D3 — An image is a positioned overlay.** The tools chunk's image-watermark operation places one
  image at an offset with a scale, and appends it to the page's content stream — so it lands **on
  top of** the page, which is what "add an image" means. The workspace places it with the reader's
  rectangle, keeping the image's own aspect ratio and centring it in that rectangle, because the
  writer scales an image to the rectangle it is given and would otherwise stretch it.
  *Limit, stated on the page:* the image is drawn over the page; it is not placed behind the text
  and it is not an editable object.
- **D4 — Redaction is a new operation, and it is honest about what it removes.** The engine has
  `redact` in the CLI: it strips the **text** inside a rectangle from the content stream and draws
  an opaque black box over the region. It was never exposed to the browser chunk, and it applied
  the same rectangles to *every* page — which is not what an editor means by "redact this".
  So: the tools layer grows a page-scoped redaction, the browser chunk grows the operation
  (wire code 34), and the page says plainly what the operation does and does not remove.
  *The limit, stated on the page:* the **text** under the region is removed from the file. An
  image or a line of vector art under the region is **covered** by an opaque box, not erased —
  the bytes are still in the file. A redaction is only a redaction for text.
- **D5 — The tools chunk is the only chunk that grows.** The stamp and the image are existing
  operations. The redaction is the one new operation, and it is a byte-oriented operation over one
  document — exactly the shape the chunk already has, with the same input/output discipline and
  the same write gate.

## Slice 2 — what ships

- [x] **B1 — A tool mode and a rectangle.** The workspace grows a tool row, and dragging on the
      page draws a rectangle the reader can see before anything is committed.
- [x] **B2 — Add a text box.** Click where the text should start, type it, and it is stamped there.
- [x] **B3 — Add an image.** Pick a PNG or JPEG, drag a rectangle, and the image is placed in it.
- [x] **B4 — Redact.** Drag a rectangle over what should not be there, and the text under it is
      removed from the file and covered.
- [x] **B5 — Evidence.** Unit tests for the geometry (canvas → display points → user points), the
      stamp's width table and the operation builders; and a browser leg that adds a text box, adds
      an image and redacts a region, reading the produced bytes back each time.

## A bug this slice found, and fixed

The CLI's redaction rewrote the content stream with a rewriter that assumed a content stream's
**operands follow their operator** — the reverse of how a PDF reads (`20 160 Td`, not `Td 20 160`).
The result was a stream whose operators and operands were shuffled, whose show operands were
dropped, and whose `TJ` arrays lost their brackets: a redaction mangled the page it was supposed
to clean.

The rewriter now walks the stream the way a PDF reads — the pending operands are collected until
the operator arrives — and it tracks the text matrix across `BT`, `Td`, `TD`, `Tm`, `TL` and `T*`,
so a show operation is judged by the position it draws at. Everything it does not understand is
copied through byte for byte. It lives in `selis-pdf-edit`, which already owned the byte-level
content-stream rewriting, and it is pinned by three tests.

## Out of scope

- Replacing an existing image, or moving an existing object: the engine has no operation that
  addresses an existing object, and inventing one is `SL-5.ANNOT.*` work.
- Freehand drawing and signing: the stamp primitive carries strokes and the signing chunk signs,
  but a signature is a document-level act, not an edit.
- Editing a form field's appearance, or a link's target: the engine reads both and writes neither.
- Anything the mutation journal (`SL-5.EDIT.01`) would own. The `mutate` and `save` ops exist on
  the wire and are locked stubs; this slice does not open them.
