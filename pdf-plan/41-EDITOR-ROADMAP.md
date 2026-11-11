# 41 — The editor: what is shipped, and what is next

**Status:** living document. **Owner:** AI+ · **Companion to:** `39-EDIT-PDF-WORKSPACE.md`,
`40-EDIT-PDF-CONTENT-TOOLS.md`, `42-EDITOR-UI-REDESIGN.md`, `43-THEME-EXPORT-CORE.md`.

**Why this file exists:** the editor is built across several sessions. If one ends badly, the next
agent should be able to read this file and carry on without re-deriving the architecture, the
engine's actual capabilities, or the traps. Everything below is what a reader needs to change the
editor safely.

---

## 1. Where the work is

| | |
|---|---|
| Branch | `feature/tools-site` |
| Worktree | `F:/selis-worktrees/tools-site` (the main checkout at `E:/selis/selis-pdf` is untouched) |
| The page | `apps/web/host/public/edit-pdf/index.html` — the route `/edit-pdf/` |
| The workspace's code | `apps/web/host/src/editor/` |
| The plan | this directory (`pdf-plan/`) |
| The browser check | `xtask/browser/editor/` + `xtask/src/browser.rs` (`verdict_editor`) |

### The commits, in order

| Commit | What |
|---|---|
| `1a63dbb` | **Slice 1** — the workspace itself: the route, the page, the render, click-to-edit, page tools, undo/redo, download, the no-upload gate, the browser check. |
| `24503a7` | **Slice 2** — changing what is ON the page: add a text box, add an image, redact. Plus the redaction rewriter fix in `selis-pdf-edit`. |
| `222e442` | **Slice 3** — the document's own properties: the form's fields, the page's size, the document's properties. |
| `65a3aa8` | **Slice 4** — the document's bookmarks, the page size as a named size, and the redaction's transform fix. |
| `b3187e8` | The redaction's contract sections, and this roadmap's state. |
| `8e9bec4` | **Slice 5** — the editor's face, and the features that came with it: find, zoom, thumbnails, the history panel, page labels, page numbers, a header and a footer, a watermark, the privacy report and the fingerprint. |
| (the next one) | **Slice 6** — the theme, the export, and the rest of the parities the engine already had: flip, compress, Bates numbering, several pages per sheet, and eight export formats. |

The next slice is the one below; nothing is in the working tree.

---

## 2. What the engine can actually do (and what it cannot)

This is the part worth getting right, because most of the editor's limits come from here and not
from the shell.

### The three chunks, and what each is for

| Chunk | URL | What it does |
|---|---|---|
| Core engine | `/wasm/selis_pdf_wasm.wasm` | `open`, `render`, `textLayer`, `search`, `outline`, `pageLinks`, `structure`, `health`. **Read-only.** |
| Tools | `/wasm/selis_pdf_wasm_tools.wasm` | 35 byte-in/byte-out operations over one document: rotate, delete, reorder, duplicate, insert blank, crop, resize, flip, page labels, watermark, image watermark, header/footer, Bates, page numbers, metadata, strip metadata, outline, fill forms, form fields, fingerprint, privacy, compress, **edit text**, **redact**, image-to-PDF, ZIP, attach/detach, attachments, repair, batch, mix, merge, split. |
| Signing | `/wasm/selis_pdf_wasm_sign.wasm` | The visual stamp (typed lines and strokes in a rectangle, appended to a page), certificate signatures, signature reports. |

### The primitives the editor is built on

- **Text already on the page** → the tools chunk's `edit_text` (op 33): `{page, find, replace}`.
  It replaces the **first show-text operand on the page that carries `find` as a substring**. A
  line's text is *assembled* from several operands, so the editor tries the whole line first and
  then its longest words, and afterwards reads the line back to check it changed the clicked one.
- **New text on the page** → the signing chunk's `stamp` (op 1): `{page (1-based), rect [x0,y0,x1,y1]
  in display points, lines [{text, size}], strokes, color, strokeWidth, fontSize, border}`. The
  engine **measures** the text with its own coarse width table and refuses a line that does not fit
  (`crates/selis-pdf-sign/src/visual.rs`), so the box must be sized with that same table
  (`stampTextWidth` in `src/editor/operations.ts`), not with the browser's font metrics.
- **New image on the page** → the tools chunk's `image_watermark` (op 25) with `tiled: false`:
  the PDF is file 0 and the image is file 1; `{opacity, scale, x, y, pages (1-based), tiled}`.
  `x`/`y` are the image's lower-left in display points; the writer scales the image to the box it
  is given, so the editor keeps the image's aspect ratio itself.
- **Taking text out** → the tools chunk's `redact` (op 34): `{page (0-based), rects [[x,y,w,h]] in
  user space}`. It strips the **text** inside the region from the content stream and draws an
  opaque black box over it. Anything else under the region is covered, not erased.
  The region is in the PAGE's space: the rewriter tracks the content's own transform
  (`q`, `Q`, `cm`) and compares the text's position there, so a page whose content
  has been scaled (which is what the editor's own resize does) is redacted where the
  reader pointed.
- **The page's own size** → the tools chunk's `crop` (op 14): `{left, top, right, bottom, pages}`,
  the margins in points on the page as the reader sees it; and `resize` (op 18):
  `{width, height, fit, pages}`, the size in points.
- **The document's bookmarks** — the core engine's `outline` op READS them (a tree: title,
  target, children); the tools chunk's `outline` op (26) WRITES them:
  `{entries: [{title, page (one-based), level}]}` — the whole list, in reading order.
- **The document's properties** → the tools chunk's `metadata` (op 6): `{fields: {Title, Author,
  Subject, Keywords}}` — the keys are capitalised.
- **Finding text** → the core engine's `search` op: `{query, opts: {matchCase: false,
  maxMatches}}` → `{matches: [{page, line, start, end, text, rect}], total, truncated}`.
  The search folds case by design; the engine REFUSES a case-sensitive request rather than
  answering it as if it were case-insensitive, so the editor does not ask for one.
- **The document's furniture** → four more tools-chunk operations the editor now drives:
  `page_labels` (op 16) `{style, start, prefix}`, `page_numbers` (op 7) `{position, start,
  format, font_size, color}`, `header_footer` (op 9) `{header: {left, center, right}, footer:
  {...}}` (the placeholders are `{page}` and `{pages}`; the engine REFUSES an unknown one rather
  than printing it literally), and `watermark` (op 8) `{text, orientation, opacity, font_size,
  color}`.
- **The document's privacy** → `strip_metadata` (op 17) rewrites the document without its
  metadata; `privacy` (op 32) and `fingerprint` (op 31) are READ-ONLY reports — they produce a
  JSON file, never a PDF, so nothing is committed and nothing is undone.
- **The form's fields** → `form_fields` (op 29) reports them as JSON; `fill_forms` (op 30) takes
  `{fields: [{name, value}]}`. The engine **refuses** to fill a read-only, signature or push-button
  field, and refuses the WHOLE operation if you send one — so the editor must not send them.

### What the engine cannot do (do not promise it)

- **The mutation journal.** The `mutate` and `save` ops exist on the wire and are **locked stubs**
  (`crates/selis-pdf-wasm/src/worker.rs`): "the edit surface arrives in Phase 5". `SL-5.EDIT.01` is
  unticked. The workspace's undo is a stack of document bytes, not the spec's model, and the page
  says so.
- **Addressing an existing object.** There is no operation that finds "the image at (x, y)" or "the
  text run that is 30% along". Replacing an existing image, moving an existing box, or editing a
  link's target would be `SL-5.ANNOT.*` work.
- **Re-flowing text.** The in-place edit splices a string; a longer replacement runs over what
  follows it. `SL-5.TEXTEDIT.02`.
- **A font that lacks the glyph.** The stamp and the edit both refuse a character the document's
  font cannot draw.

---

## 3. The geometry, which is the easy thing to get wrong

The text layer is in **user space** (points, y-up, origin at the `/MediaBox`'s lower-left) and the
canvas is in **device pixels** (y-down, origin top-left). The render op reports the transform it
used as `ctm` (six elements, PDF's `[a b c d e f]`) — **that is why the render op was changed in
slice 1**: a shell that assumes scale-and-flip puts the click target on the wrong pixels of a page
that is turned, or whose media box does not start at (0, 0).

`apps/web/host/src/editor/placement.ts` holds the conversions, and its tests pin the numbers:

- user points = the inverse of `ctm` applied to the canvas rectangle;
- display points = the engine's own quarter-turn table (`display_to_user` in
  `crates/selis-pdf-tools/src/overlay.rs` and `crates/selis-pdf-sign/src/lib.rs`), inverted;
- the media box's size comes from the `open` reply's `pageSizes`; the origin is **not** on the wire
  and is not needed (the overlay writers work in a media-relative space, and the editor compensates
  by mapping through the engine's table rather than through the canvas).

**When you add a placement, put it through `placement.ts`.** A rectangle computed by eye is the one
thing in this editor that is silently wrong.

---

## 4. The rules this workspace lives by

1. **Every change is undoable**, and an undo restores the document's **bytes**, not a
   reconstruction of it (`src/editor/history.ts`). The stack is capped (`HISTORY_DEPTH`).
2. **Nothing is uploaded, ever.** The page installs the same recorder the tools page does
   (`src/no-upload.ts`) and asserts after every operation that no request carried a byte of the
   document. It sets `document.documentElement.dataset.noUpload = "verified"` — the browser check
   reads that marker, so **every operation path must call `assertLocalOnly()`** or the check fails.
3. **The page says what it cannot do.** The limits above are in the page's copy, not just in the
   plan. When you add a capability, add its limit to the copy too.
4. **The engine's own arithmetic, not a second implementation of it.** The placement geometry, the
   stamp's width table and the redaction's text matrix all come from the engine.

---

## 5. What the market has, and where we are

The reference list is **Foxit PDF Editor's feature map** (Create · Edit ·
Convert · Protect · Collaborate · Intelligence, and the pages under each) with
**Adobe Acrobat Pro**'s areas folded in. Both are the shape the category
settled on; neither is a wish list. Every row says what we have, and the
verdict says what is true today.

The verdicts:

- **SHIPPED** — in the editor, and the editor's own browser leg proves it.
- **IN THE TOOLS PAGE** — the one-shot card does it, but not inside a document
  the reader is holding open. Moving one of these into the editor is usually
  a UI change, not an engine change.
- **BUILDABLE** — the engine already has the operation; the editor does not
  use it yet. This is the cheapest kind of missing.
- **BLOCKED** — the engine cannot do it. The blocker is named.
- **NOT PLANNED** — and the reason is a product decision, not a gap.

### Create

| Feature | Where it is now | Verdict |
|---|---|---|
| Create a PDF from a document | The tools page's "Convert to PDF" (Markdown, HTML, CSV, text) and "Images to PDF" (`image_to_pdf`) | **IN THE TOOLS PAGE** |
| Create a PDF from a scan (OCR) | Nowhere. The text chunk *reads* text; it does not recognise it. | **BLOCKED** — no OCR engine |
| Create a fillable form | The editor fills an **existing** form. Adding a field to a page needs an AcroForm **writer**; the engine writes field *values*. | **BLOCKED** |
| Create from a template | Nowhere. | **NOT PLANNED** — a workspace holds one document; a template library is a different product |
| Create a PDF from Office files | Nowhere. There is no office renderer in the engine, and a cloud one would break the no-upload promise. | **BLOCKED** |

### Edit

| Feature | Where it is now | Verdict |
|---|---|---|
| Edit text | Click a line, type. The text is replaced **where it is drawn**. | **SHIPPED** |
| Edit text with re-flow | Nowhere. A longer replacement runs over what follows it. | **BLOCKED** — `SL-5.TEXTEDIT.02` |
| Add a text box | The sign chunk's typed stamp. | **SHIPPED** |
| Add an image | The tools chunk's image overlay. | **SHIPPED** |
| Replace or move an existing image or box | Nowhere. There is no operation that **addresses** an existing object. | **BLOCKED** — `SL-5.ANNOT.*` |
| Draw shapes, arrows, freehand | The stamp primitive carries strokes, but they would be page content, not annotations another reader can reply to. | **BLOCKED** — `SL-5.ANNOT.*` |
| Rotate a page | 90°, 180° and 270°, on the selected page. | **SHIPPED** |
| Flip a page | The Organize tab's "Flip across" and "Flip down". | **SHIPPED** |
| Delete, duplicate, insert, reorder pages | All four. | **SHIPPED** |
| Crop and resize a page | By margins, and to a named size (A3/A4/A5/Letter/Legal). | **SHIPPED** |
| Split or extract pages | Deleting the pages you do not want is the same operation. | **BUILDABLE** — `delete`/`reorder` exist |
| Several pages on one sheet | The editor's "Several pages per sheet" panel. | **SHIPPED** |
| Header, footer, page numbers, watermark | Four panels in the inspector. | **SHIPPED** |
| Page labels (i, ii, iii / A-1) | The inspector's "Page labels" panel. | **SHIPPED** |
| Bookmarks | Read and written, with their nesting depth. | **SHIPPED** |
| Find | The app bar's find box, with the matches highlighted. | **SHIPPED** |
| Find and replace across the document | Nowhere. `edit_text` is per page, and the editor's click-to-edit is the safe way in. | **BUILDABLE** — the search op finds the page; the click-to-edit does the rest |
| Measure a distance or an angle | Nowhere. Two points on the page are already in user space. | **BUILDABLE** — the editor's own geometry |
| Measure an area | Nowhere. It needs the page's vector art, which the engine does not expose. | **BLOCKED** |
| Compare two documents | Nowhere. The engine has no compare operation. | **BLOCKED** |
| Comment, highlight, underline, strikeout, reply | Nowhere. There is no annotation writer. | **BLOCKED** — `SL-5.ANNOT.*` |
| Reduce the file size | The editor's "Reduce the file size" panel, and the tools page's "Optimise & repair". | **SHIPPED** |
| Bates numbering | The editor's "Bates numbers" panel, and the tools page's. | **SHIPPED** |
| Attach a file, detach a file | The tools page's "Attachments". | **BUILDABLE** — `attach`/`detach`/`attachments` exist |

### Convert

| Feature | Where it is now | Verdict |
|---|---|---|
| Export to Word, Excel, PowerPoint | The editor's **Export** tab. The writers have been in `crates/selis-pdf-text` all along — this row was marked BLOCKED, and that was wrong. | **SHIPPED** |
| Export to HTML | The editor's Export tab, and the tools page's "PDF to HTML". | **SHIPPED** |
| Export plain text or CSV | The editor's Export tab, and the tools page's text export. | **SHIPPED** |
| Export to an EPUB book | The editor's Export tab. | **SHIPPED** |
| Export to PNG, JPG, TIFF, WebP | The tools page's four image cards — the chunk exists. | **BUILDABLE** — the editor needs a client for it |
| Convert to PDF/A | Nowhere. | **BLOCKED** |
| Translate | Nowhere. | **NOT PLANNED** — it needs a service, and the promise is that nothing leaves the device |

### Protect

| Feature | Where it is now | Verdict |
|---|---|---|
| Redact | A region, and the **text** under it is removed from the file and covered. | **SHIPPED** — for text only |
| Redact an image or a line of art | Covered, not erased. | **BLOCKED** — `SL-5.REDACT.*` |
| Find the sensitive text for me (AI redaction) | Nowhere. | **NOT PLANNED** — it needs an inference service |
| Remove the document's metadata | The Protect tab's "Strip metadata". | **SHIPPED** |
| See what the document leaks | The privacy report and the fingerprint, in the inspector. | **SHIPPED** |
| Password protect, unlock, permissions | The tools page's four password cards — the security chunk exists. | **BUILDABLE** — the editor needs a client and a prompt |
| Sign: type or draw a signature | The sign chunk's typed stamp, which the editor already drives for the text box. | **BUILDABLE** — the same primitive, a different label |
| Sign with a certificate | The tools page's "certificate" card. | **BUILDABLE** — the editor needs a client and a key picker |
| Fill a form | Read the fields, write the values back. | **SHIPPED** |

### Collaborate

| Feature | Where it is now | Verdict |
|---|---|---|
| Comment and shared review | Nowhere. | **BLOCKED** — `SL-5.ANNOT.*` |
| Open and save in cloud storage | Nowhere. | **NOT PLANNED** — the product's promise is that nothing leaves the device |
| Version history | The editor's history panel: every state the document has been in, in this session. | **SHIPPED** — for the session, not shared |

### Intelligence

| Feature | Where it is now | Verdict |
|---|---|---|
| Chat with the document, summarise it | Nowhere. | **NOT PLANNED** — a cloud service |
| Extract data from a document | Nowhere. | **NOT PLANNED** — a cloud service |

### Accessibility

| Feature | Where it is now | Verdict |
|---|---|---|
| Tagged PDF, alt text, reading order | The engine **reads** a document's structure; there is no structure **writer**. | **BLOCKED** for writing |
| Check the document's accessibility | Nowhere. The `structure` op already reports what the document carries. | **BUILDABLE** — a report, not a rewrite |

### The product itself

| Feature | Where it is now | Verdict |
|---|---|---|
| A dark theme | The switch in the app bar, the grid's navigation and the viewer's bar. The decision lives in `@selis/ui-kit`, so the desktop app and the SDK can share it. | **SHIPPED** |
| The reader's choice is remembered | One key, `selis-theme`, across the site and across sessions. | **SHIPPED** |

### The one thing the list says that we do not argue with

Every row above that says **BLOCKED** is blocked on the *engine*, not on the
editor. The engine is further along than the editor's face: thirty-four
operations are on the wire, and the editor drives nineteen of them. **Most of
what the market has is a UI away from being here** — which is why slice 7 is
a list of panels and not a list of engine work.

---

## 6. The TODO list

### Slice 5 — the editor's face, and the features that came with it (shipped in `8e9bec4`)

- [x] **G1 — The face.** The app bar, the tab strip, the ribbon, the page rail
      with real thumbnails, the canvas on a neutral surface, the inspector, and
      the status bar. The design rules are in `42-EDITOR-UI-REDESIGN.md`.
      *Files:* `public/edit-pdf/index.html`, `assets-src/editor.css`,
      `src/editor/editor-page.ts`, `src/editor/icons.ts`.
- [x] **G2 — Find.** The app bar's find box: the engine's own search, the
      matches highlighted on the page, a count, and next/previous.
- [x] **G3 — The zoom.** Zoom in and out on a ladder, fit width, fit page, and
      the page indicator. The zoom is a change of **resolution**, so the
      glyphs stay sharp and the geometry stays exact.
- [x] **G4 — The history panel.** The undo stack, made visible: every change
      with its label, and a click that returns the document to it.
- [x] **G5 — The document's furniture.** Page labels, page numbers, a header
      and a footer, and a watermark — four panels, four operations that the
      engine already had.
- [x] **G6 — The privacy report and the fingerprint.** Two read-only reports,
      read from the tools chunk and shown in the inspector.
- [x] **G7 — Rotate left.** The last missing quarter turn.
- [x] **G8 — The evidence.** The browser leg's new `shell` leg: the rail drew a
      thumbnail, the tabs swapped, the find box found the text, the zoom
      changed the page, the history listed the changes, and a **watermark was
      read back out of the produced bytes**.
- [x] **G9 — The harness's own bug.** `waitFor` quoted a `frame` that only
      existed inside `main()`, so a leg that timed out **threw instead of
      rejecting** and the promise stayed pending forever: a timeout hung the
      whole check instead of reporting what went wrong. Every leg that times
      out from now on will say what it was waiting for.

### Slice 6 — the theme, the export, and the rest of the parities

- [x] **H1 — Flip the page.** The `flip` op, on the selected page, both ways.
- [x] **H2 — Reduce the file size.** The `compress` op, with the size before
      and after in the status line.
- [x] **H3 — Bates numbering.** The `bates` op — prefix, start, digits.
- [x] **H11 — Several pages to one sheet.** The `nup` op.
- [x] **H13 — The export.** Eight formats — Word, PowerPoint, Excel, a web
      page, an EPUB book, a reflowed article, tables as CSV, plain text — all
      written by the engine's own writers, from the document **as it is**.
      *The finding:* the writers have been in `crates/selis-pdf-text` all
      along, and the roadmap's "BLOCKED — no office writer" was wrong.
- [x] **H14 — The theme.** The switch between the light and the dark theme,
      on every surface, with the decision in `@selis/ui-kit` so the other
      products can share it. *The finding:* the dark theme had always been in
      the tokens and never took effect, because the site's remap and the
      kit's `[data-theme="dark"]` had the same specificity and the remap was
      loaded last.
- [x] **H15 — The shared export layer.** The HTML writer's client, extracted
      from the tools page, and one module that knows which chunk writes
      which format — so a format is added once.
- [x] **H16 — The evidence.** Three more browser legs: the theme switched and
      was remembered, a Word export is a ZIP container and an HTML export
      carries the text, and the Bates prefix is in the produced file.

### Slice 7 — what is next: the features the engine already has

- [ ] **H4 — Split the document.** "Keep these pages": a selection in the
      rail, then `delete`/`reorder` to leave the ones that were chosen.
- [ ] **H5 — Attach and detach.** The `attach`, `detach` and `attachments`
      ops. A file carried inside the document is a real thing an editor
      should be able to see.
- [ ] **H6 — The accessibility report.** The core engine's `structure` op:
      does the document have a tag tree, are its images tagged, is the
      reading order right. A report, not a rewrite.
- [ ] **H7 — Export to an image.** The raster chunk (PNG, JPG, TIFF, WebP).
      The HTML and text exports are done; the image one is the same shape, and
      the editor's own canvas already renders the pages.
- [ ] **H8 — Protect with a password.** The security chunk's `protect` and
      `unlock`. A prompt for the password, and the honest note that the
      browser's own memory holds the key while the tab is open.
- [ ] **H9 — Fill and sign.** A typed signature (the sign chunk's stamp, which
      the editor already drives) and a certificate signature (the key picker
      the tools page already has).
- [ ] **H10 — Measure.** The distance and the angle between two points the
      reader marks, in the page's own user space, drawn as the overlay
      writers draw.
- [ ] **H12 — A command palette.** Every command is a button; with the
      ribbon's six tabs and the inspector's panels, a box you can type into
      would be faster than clicking.

### Slice 8 — what needs the engine

These are the blockers named above, in the order they unlock the most.

- [ ] **I1 — The annotation writer (`SL-5.ANNOT.*`).** It unlocks the whole
      of "Comment & Annotate" — highlight, underline, strikeout, a sticky
      note, a freehand mark, a reply, and the selection that moves an
      existing object. It is the largest single gap between us and the
      market's feature list.
- [ ] **I2 — A redaction that removes more than text (`SL-5.REDACT.*`).**
      Today the redaction strips text and covers everything else. Removing an
      image under the region means rewriting the page's XObjects.
- [ ] **I3 — Text that re-flows (`SL-5.TEXTEDIT.02`).** The edit splices a
      string; a longer replacement runs over what follows it.
- [ ] **I4 — The mutation journal (`SL-5.EDIT.01`).** The spec's own model.
      The workspace's undo is a stack of bytes, which is exact but keeps a
      copy of the document per step.

### Things that are NOT worth doing

- **Cloud storage, shared review, AI, translation.** They all break the one
  promise the page is built on.
- **A second implementation of the engine's arithmetic.** The placement
  geometry, the stamp's width table and the redaction's text matrix all come
  from the engine. A second copy is a second set of bugs.
- **A redaction that fakes removal.** Covering an image is not erasing it,
  and the page says so.
- **A second copy of a decision.** The theme's decisions are the design
  system's, the export's writers are the engine's, and the web layer holds
  only the wire. `43-THEME-EXPORT-CORE.md` says which is which.

---

## 7. How to build and check it

```sh
# The workspace's TypeScript and the assets it serves.
pnpm --filter @selis/web-host build
pnpm --filter @selis/web-host test

# The engine's chunks. The recipe matters: fat LTO, one codegen unit,
# wasm-opt -O3, and the custom-section strip. `xtask size-check` does all
# of it AND measures the size budgets.
cargo xtask size-check          # rebuilds every chunk, then the budgets
cargo test --workspace          # the whole Rust suite
cargo xtask browser-check --only editor   # the editor's browser leg
cargo xtask browser-check       # all seven checks
cargo xtask lint                # every mechanical gate
```

The dev server is `pnpm --filter @selis/web-host serve` → <http://127.0.0.1:4173/edit-pdf/>.

---

## 8. The traps

These have all cost time. They are not obvious.

1. **A stale WASM chunk is a silent, total failure.** The build copies the
   chunks from `target/wasm32-unknown-unknown/release/*.opt.wasm`; a chunk
   built before a source change is simply the old program. The raster chunk
   was stale for weeks and the tools page's invert card failed with
   "unknown image format code 4" — a message that reads like a bug in the
   caller and is really a stale binary. **After changing any Rust that a
   chunk links, run `cargo xtask size-check`** (it rebuilds and strips them)
   and then rebuild the host.
2. **The service worker serves stale assets — including stale MODULES.** The
   page's modules are in the runtime cache, so a rebuilt module can still be
   the old one. This bites hardest on the editor, whose controller is a big
   module. **This is now fixed rather than worked around**: the cache
   namespace follows the build (`tools/place-sw.mjs` stamps a hash of the
   served bytes into the worker and into the policy module), so a deploy
   changes the worker's bytes, the browser installs it, and `activate` drops
   the stale caches. The page still has to TAKE the update, because the new
   worker waits — `takeUpdateWhenSafe` does that when a reload costs the
   reader nothing.
   When a change "does not take" in a browser you are testing by hand, check
   the cache list first: `caches.keys()` shows the namespace, and a stale
   `selis-runtime-v<old>` with entries in it is the answer.
3. **The stylesheet's digest is pinned in the page.** `public/edit-pdf/index.html`
   carries `integrity="sha384-..."`, and `sw-ship.test.ts` fails if it drifts.
   Recompute it after every CSS change:
   `node -e "const {createHash}=require('node:crypto');const fs=require('node:fs');const b=fs.readFileSync('apps/web/host/assets-src/editor.css');console.log('sha384-'+createHash('sha384').update(b).digest('base64'))"`
4. **`CACHE_VERSION` is the service worker's namespace.** Bump it in
   `src/sw-policy.ts` when the precache set changes, or an existing install
   keeps serving the old set.
5. **The wasm-tools op codes are a wire contract.** The `ToolOp` union in
   `src/tools/protocol.ts` and `OP_CODE` must agree with
   `crates/selis-pdf-wasm-tools/src/lib.rs`'s `Op::from_wire`. Two tests
   probe for "unknown tool operation" with a code **above** the highest op —
   when a new op is added, that probe moves up or the test fails for the
   wrong reason.
6. **The page numbers differ between the chunks, deliberately.** `edit_text`
   and `redact` take a **zero-based** page; the stamp and the image overlay
   take a **one-based** page; the crop and the overlay's page selection are
   one-based. Each is documented where it is defined.
7. **A workspace test build unifies `native-fs` into the browser crates.**
   The browser chunks must activate the in-memory session
   (`selis_pdf_tools::MemorySession::activate()`) or their tools read `in/0`
   from the filesystem. `selis-pdf-wasm-text` forgot this and its tests
   failed under `cargo test --workspace` while passing on their own.
8. **`cargo xtask lint` ends at `cargo deny check`.** Four RustSec advisories
   (three wasmtime, one constant-time crypto crate) are against the pinned
   versions. That is a security decision — a version bump or an accepted-risk
   entry with a written rationale — and it is not a drive-by.
9. **The registry is 47 cards and must stay 47.** `src/tool-registry.test.ts`
   and `xtask/src/browser.rs`'s `TOOLS_PAGE_CARDS` both pin the count. The
   editor is deliberately NOT a card: a `ToolDef` has one verb, one input set
   and one output, and an editor has none of those.
10. **`apps/web/host/tools/_tp_head.ts` is scratch.** It is not ours and is
    not committed.
11. **A page must not be scaled to fit its canvas.** `max-width: 100%` on a
    page's canvas silently shrinks a wide page, and every placement on it
    then lands in the wrong place: the geometry knows the page's real width
    and the browser is drawing it at another. The canvas scrolls; the fit
    commands change the zoom.
12. **The theme's selector is load-bearing.** The site's palette is a remap of
    the design system's tokens, and it used to be written in a plain `:root`
    block: the same specificity as the kit's `[data-theme="dark"]`, and
    loaded last, so the dark theme could never take effect. The remap is
    `:root:not([data-theme="dark"])` and `:root[data-theme="dark"]` now. A
    theme that "does not work" is very often one selector.
13. **A leg that times out must say so.** The browser harness's `waitFor`
    used to quote a `frame` that only existed inside `main()`, so a timeout
    **threw** instead of rejecting and the promise stayed pending forever: a
    leg that timed out hung the whole check instead of reporting what went
    wrong. The frame is module-level now, and the rejection cannot throw.
14. **A PowerShell script that contains an em-dash is read as ANSI.** The
    editor's plan files are UTF-8, and `powershell -File` decodes a script
    without a BOM as the system code page — so a here-string's `—` arrives
    as three mojibake characters and the file is written back corrupted.
    Write the content to its own UTF-8 file and have an ASCII-only script
    splice it in, or give the script a BOM.
15. **A file's LENGTH and a file's BYTES are both numbers to the careless
    eye.** `new Uint8Array(length)` builds an array of that many zeros, so
    writing one where the bytes belong produces a file of the right size and
    no content — and nothing says so. The exported file's fields are named
    `size` and `content` for exactly this reason, and the write takes the
    bytes themselves so the mistake cannot compile.
16. **A native dialog takes the page's activation with it.** Chrome's Save
    dialog is a native one: by the time the reader closes it, the page is no
    longer "fully active", and `createWritable()` is refused with "The
    request is not allowed by the user agent or the platform in the current
    context" — sometimes leaving a 0-byte file at the path the reader chose.
    The browser's own "Save as…" on a download or a link has no such
    problem, and leaves nothing behind.

---

## 9. The honest limits, in the words the page uses

If a new capability changes one of these, change the page's copy in the same
commit.

> The text is replaced where it is drawn — the line does not re-flow, so a
> much longer replacement runs over what follows it. Not every line can be
> changed: a line drawn as several separate runs can only have one of them
> replaced, and a line whose words the engine cannot address is left alone.

> New text and images are drawn on top of the page. A redaction removes the
> text under the region and covers it with a black box; anything else under
> it — an image, a line of art — is covered, not erased.
