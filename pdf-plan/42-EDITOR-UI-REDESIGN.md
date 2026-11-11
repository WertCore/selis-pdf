# 42 — The editor's face: what the workspace looks like, and why

**Status:** shipped. **Owner:** AI+ · **Companion to:** `39-EDIT-PDF-WORKSPACE.md`,
`40-EDIT-PDF-CONTENT-TOOLS.md`, `41-EDITOR-ROADMAP.md`.

**Why this file exists:** the workspace worked and looked like a form with a
preview next to it. The features were real; the *surface* was a list of
`<details>` panels in a sidebar, and a person who has used Acrobat or Foxit
would not recognise it as an editor. This records the design rules the new
face follows, so the next change to it is made against a reason rather than
against a taste.

---

## 1. The shape

```
┌──────────────────────────────────────────────────────────────────────────┐
│ the app bar: what is open · find · undo/redo · open · download · no upload │
├──────────────────────────────────────────────────────────────────────────┤
│ the tabs: Home · Edit · Organize · Document · Protect                       │
├──────────────────────────────────────────────────────────────────────────┤
│ the ribbon: the commands, in groups, with the group's name under it        │
├────────────┬──────────────────────────────────────┬───────────────────────┤
│ the rail:  │ the canvas: the document on a        │ the inspector: the    │
│ the pages, │ neutral surface, with the page        │ document's own        │
│ as         │ lifting off it                       │ panels                │
│ thumbnails ├──────────────────────────────────────┴───────────────────────┤
│            │ the status bar: where you are, and how big                   │
└────────────┴──────────────────────────────────────────────────────────────┘
```

That shape is not an invention. It is the shape every established editor
settled on — Acrobat's "All Tools" view, Foxit's ribbon, PDFelement's
workspace — because it falls out of the work rather than out of a sketch:

- The **ribbon** is the set of commands. A ribbon's tabs are the answer to
  "there are too many commands for one bar", and they are what Foxit calls a
  "familiar ribbon-style interface" and what Acrobat's tool panes are.
- The **rail** is the document's shape. A list of page *numbers* is a list; a
  list of *pages* is a map, and the map is what a reader navigates by.
- The **inspector** is where a document's own properties live: its form
  fields, its bookmarks, its page size, its metadata. A ribbon command that
  changes the document's structure opens the panel that holds it — the command
  and its options are never in two different places.
- The **canvas** is the document, on a neutral surface.
- The **status bar** says where you are and how big the page is, which is the
  one thing that is always true and always wanted.

## 2. The rules

1. **The page is the only bright thing on screen.** The canvas is
   `--selis-color-bg-inset` — one step off the chrome — and the page carries
   the only shadow in the workspace. A reader's eye goes to the document, not
   to the furniture.
2. **Chrome is quiet.** Hairlines, not shadows. One accent colour, used for
   the selected tab, the primary button, the current page and the current
   match. Everything else is grey.
3. **One thing per band.** The bar is the file, the ribbon is the commands,
   the inspector is the options.
4. **The icons are one set.** Twenty-seven glyphs on a 24-unit grid with one
   stroke weight, drawn inline as SVG (`src/editor/icons.ts`) so they inherit
   `currentColor`, need no request, and cannot drift apart. The markup names
   the icon (`data-icon="undo"`); the paths live in one file.
5. **Nothing is hidden behind a gesture.** Every command is a button with a
   name. The only keyboard-only things are the shortcuts a reader already
   expects (Enter on a line to edit it).

## 3. What changed, and what it cost

| | Before | After |
|---|---|---|
| The surface | a header bar and four stacked `<details>` panels | an app bar, a tab strip, a ribbon, a rail, a canvas, an inspector, a status bar |
| The pages | a list of page numbers | real thumbnails, drawn from the page's own raster |
| Finding | not possible | a find box in the app bar, with the matches highlighted and a next/previous |
| Zoom | not possible | zoom in/out, fit width, fit page, in the status bar |
| Undo | two buttons | two buttons **and** a history panel that lists every change and returns the document to any of them |
| The document's panels | all open at once, in one column | collapsible panels in the inspector, opened by the ribbon command that fills them |

## 4. The three decisions that were not obvious

### 4.1 The zoom is a change of resolution, not a change of scale

The page is **rendered again** at a higher or lower DPI rather than being
scaled by the browser. A CSS scale would leave the glyphs soft and — worse —
would make the canvas's box disagree with the geometry the placements are
computed from, so a click would land in the wrong place. Rendering again keeps
the raster and the transform in step, and `placement.ts` needs no change.

The cost is a re-render per zoom step. For a large document that is real work,
so the steps are a ladder (25%, 50%, 75%, 100%, 125%, 150%, 200%, 300%, 400%)
rather than a continuous slider.

### 4.2 The page is never scaled to fit the canvas

The canvas's stylesheet used to carry `max-width: 100%` on the page's canvas,
which quietly shrank a wide page to fit the surface — and broke every
placement on it, because the geometry knew the page's real width and the
browser was drawing it at another. The rule now is: **the page is drawn at
its size, the canvas scrolls, and the fit commands change the zoom.**

### 4.3 The rail's thumbnails are drawn out of band

A thumbnail is a copy of the page's raster at a fraction of the size — one
scaled blit, not a second render. It is produced with `canvas.toBlob`, which
is asynchronous, so a document with fifty pages does not block the workspace
while they are drawn. The object URLs are revoked when the document is drawn
again, so a long session does not hold fifty dead blobs.

The thumbnails are `<img>` elements, not `<canvas>`: the browser check finds
the document's canvas with `querySelector("canvas")`, and a canvas in the rail
would be that one.

## 5. What is still missing from the face

- **A theme.** The design system's tokens carry a dark theme, but
  `assets-src/viewer-shell.css` remaps them onto the site's warm palette in
  `:root` and is loaded last — so the dark tokens lose, site-wide. A dark
  workspace is a **site-wide** decision, not an editor one, and the editor
  follows the product's palette rather than being a different product.
- **A customisable ribbon.** Acrobat lets the reader hide the groups they do
  not use. It is a nice-to-have, and it is a real accessibility win for a
  small screen.
- **Touch.** The drag is a pointer drag with pointer capture; a finger works,
  but there is no pinch-zoom and no long-press menu.
- **A command palette.** Every command is a button now. With the ribbon's
  five tabs and the inspector's ten panels, a "type a command" box would get
  busy work done faster than clicking.

---

## 6. Where the files are

| | |
|---|---|
| The shell | `apps/web/host/public/edit-pdf/index.html` |
| The stylesheet | `apps/web/host/assets-src/editor.css` (copied to `public/assets/editor.css`) |
| The controller | `apps/web/host/src/editor/editor-page.ts` |
| The icons | `apps/web/host/src/editor/icons.ts` |

**The stylesheet's digest is pinned** in the page's `integrity` attribute, and
`sw-ship.test.ts` fails if it drifts. Recompute it after every CSS change:

```sh
node -e "const {createHash}=require('node:crypto');const fs=require('node:fs');const b=fs.readFileSync('apps/web/host/assets-src/editor.css');console.log('sha384-'+createHash('sha384').update(b).digest('base64'))"
```

**The service worker serves stale modules.** When a change "does not take",
clear the caches and unregister the workers:

```js
for (const r of await navigator.serviceWorker.getRegistrations()) await r.unregister();
for (const n of await caches.keys()) await caches.delete(n);
```

## 7. The evidence

The editor's browser leg grew a `shell` leg that checks the face against the
**document**, not against the DOM alone:

- the rail drew a thumbnail of the page (a blob URL, so it is a real raster);
- the ribbon's tabs swapped the commands under them;
- the find box found the text in the document and counted the matches;
- the zoom changed the size of the page on the canvas;
- the history panel listed the changes that had been made;
- a **watermark** was applied and read back out of the produced bytes — a
  watermark that is on the screen but not in the file is not a watermark.

`xtask/src/browser.rs`'s `verdict_editor` fails the check if any of those is
false.
