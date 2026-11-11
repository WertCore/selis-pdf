# 41 — The editor: what is shipped, and what is next

**Status:** living document. **Owner:** AI+ · **Companion to:** `39-EDIT-PDF-WORKSPACE.md`,
`40-EDIT-PDF-CONTENT-TOOLS.md`.

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
| (the next one) | **Slice 4** — the document's bookmarks, and the page size as a named size. Plus the redaction's transform fix (see §2). |

Slice 4 is in the working tree, uncommitted — see §5.

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

## 5. The TODO list

### Slice 3 — the document's own properties (IN THE WORKING TREE, uncommitted)

- [x] **C1 — The form's fields.** Read them (`form_fields`), show one input each, fill them in
      (`fill_forms`). The read-only, signature and push-button fields are shown but not editable —
      the engine refuses the whole operation if you send one.
      *Files:* `src/editor/editor-page.ts`, `public/edit-pdf/index.html`, `assets-src/editor.css`.
- [x] **C2 — The page's size.** Crop the selected page by its margins (`crop`).
- [x] **C3 — The document's properties.** Title, author, subject, keywords (`metadata`).
- [x] **C4 — The browser leg.** `xtask/browser/editor/index.html` grows three legs (crop,
      properties, form) and `xtask/src/browser.rs`'s `verdict_editor` checks them.
- [x] **C5 — Rebuild and commit.** See §7.

### Slice 4 — the document's bookmarks, and the page size as a size (IN THE WORKING TREE, uncommitted)

- [x] **E1 — The bookmarks.** The workspace reads the document's outline with the core
      engine's `outline` op, shows one row per bookmark (title, page, nesting depth), and
      writes the whole list back with the tools chunk's `outline` op.
      *Watch:* the reader's page numbers are **zero-based** and the writer's are
      **one-based**; the editor converts. A bookmark that points at a **named
      destination** cannot be written back (the writer takes a page), so the save is
      refused and the reader is told which bookmark to remove — rather than quietly
      repointing it at page 1.
- [x] **E2 — The page size as a size.** The page-size panel offers the named sizes the
      tools page offers (A3, A4, A5, Letter, Legal) alongside the crop margins, and
      resizes with the tools chunk's `resize` op.
- [x] **E3 — The redaction's own transform.** The redaction's region is in the page's
      own space, but a content stream's text lives in the content's space — and a
      resize scales the content, so the two differ. The rewriter in `selis-pdf-edit`
      now tracks the content's transform (`q`, `Q`, `cm`) and compares the text's
      position in the **page's** space, and it hands back the inverse of a trailing
      transform so the black box is drawn where the reader pointed. This was a real
      bug in the CLI's `redact` too.
- [ ] **E4 — Rebuild and commit.** See §7.

### Slice 5 — what is next, in the order it is worth doing

- [ ] **F1 — Undo that reads as a history.** The undo stack is a stack of bytes with
      labels; a panel that lists them ("Rotated page 2", "Added a text box") and jumps
      to any of them would make the stack visible. The stack is already there
      (`DocumentHistory`) — it only needs a reader and a way to select a state.
- [ ] **F2 — The document's own structure, more of it.** The outline op can also
      retarget a bookmark at a different page; the editor's bookmark rows take a page
      number already, so this is a UI change rather than an engine one.
- [ ] **F3 — Select and move.** **Blocked on the engine.** There is no operation that
      addresses an existing object, so "move the box you drew" cannot be built without
      `SL-5.ANNOT.*`. Do not fake it with a delete-and-redraw: that loses the object's
      identity and the document's structure.
- [ ] **F4 — A redaction that removes more than text.** Also blocked: the redaction
      strips text and covers the rest. Removing an image under a region means
      rewriting the page's XObjects (`SL-5.REDACT.*`). Until then the page must keep
      saying that only text is removed.
- [ ] **F5 — The last of the document-level operations.** The tools chunk's `privacy`
      and `fingerprint` reports, and the `security` chunk's unlock/protect, are
      document-level rather than page-level: they belong in the same panel as the
      properties, and they are worth having. The security chunk is a **separate WASM
      chunk** with its own protocol (`src/tools/security-protocol.ts`) — the editor
      would need a client for it.

### Things that are NOT worth doing

- Re-opening the mutation journal (`mutate`/`save`): that is the spec's model, and it is a
  multi-month project, not an editor feature.
- Freehand drawing: the stamp primitive carries strokes, so it is cheap — but it belongs with
  signing, which is a document-level act.
- A "select mode" that turns off click-to-edit: the tool row already does that.

---

## 6. How to build and check it

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

## 7. The traps

These have all cost time in this session. They are not obvious.

1. **A stale WASM chunk is a silent, total failure.** The build copies the chunks from
   `target/wasm32-unknown-unknown/release/*.opt.wasm`; a chunk built before a source change is
   simply the old program. The raster chunk was stale for weeks and the tools page's invert card
   failed with "unknown image format code 4" — a message that reads like a bug in the caller and
   is really a stale binary. **After changing any Rust that a chunk links, run
   `cargo xtask size-check`** (it rebuilds and strips them) and then rebuild the host.
2. **The service worker serves stale assets.** The page's modules are in the runtime cache, so a
   rebuilt module can still be the old one. When a change "does not take", clear the caches:
   `for (const n of await caches.keys()) await caches.delete(n)` and unregister the workers.
3. **The stylesheet's digest is pinned in the page.** `public/edit-pdf/index.html` carries
   `integrity="sha384-…"`, and `sw-ship.test.ts` fails if it drifts. Recompute it after every CSS
   change:
   `node -e "const {createHash}=require('node:crypto');const fs=require('node:fs');const b=fs.readFileSync('apps/web/host/assets-src/editor.css');console.log('sha384-'+createHash('sha384').update(b).digest('base64'))"`
4. **`CACHE_VERSION` is the service worker's namespace.** Bump it in `src/sw-policy.ts` when the
   precache set changes, or an existing install keeps serving the old set.
5. **The wasm-tools op codes are a wire contract.** The `ToolOp` union in
   `src/tools/protocol.ts` and `OP_CODE` must agree with `crates/selis-pdf-wasm-tools/src/lib.rs`'s
   `Op::from_wire`. Two tests probe for "unknown tool operation" with a code **above** the highest
   op — when a new op is added, that probe moves up or the test fails for the wrong reason.
6. **The page numbers differ between the chunks, deliberately.** `edit_text` and `redact` take a
   **zero-based** page; the stamp and the image overlay take a **one-based** page; the crop and the
   overlay's page selection are one-based. Each is documented where it is defined.
7. **A workspace test build unifies `native-fs` into the browser crates.** The browser chunks
   must activate the in-memory session (`selis_pdf_tools::MemorySession::activate()`) or their
   tools read `in/0` from the filesystem. `selis-pdf-wasm-text` forgot this and its tests failed
   under `cargo test --workspace` while passing on their own.
8. **`cargo xtask lint` ends at `cargo deny check`.** Four RustSec advisories (three wasmtime, one
   constant-time crypto crate) are against the pinned versions. That is a security decision — a
   version bump or an accepted-risk entry with a written rationale — and it is not a drive-by.
9. **The registry is 47 cards and must stay 47.** `src/tool-registry.test.ts` and
   `xtask/src/browser.rs`'s `TOOLS_PAGE_CARDS` both pin the count. The editor is deliberately NOT a
   card: a `ToolDef` has one verb, one input set and one output, and an editor has none of those.
10. **`apps/web/host/tools/_tp_head.ts` is scratch.** It is not ours and is not committed.

---

## 8. The honest limits, in the words the page uses

If a new capability changes one of these, change the page's copy in the same commit.

> The text is replaced where it is drawn — the line does not re-flow, so a much longer replacement
> runs over what follows it. Not every line can be changed: a line drawn as several separate runs
> can only have one of them replaced, and a line whose words the engine cannot address is left
> alone.

> New text and images are drawn on top of the page. A redaction removes the text under the region
> and covers it with a black box; anything else under it — an image, a line of art — is covered, not
> erased.
