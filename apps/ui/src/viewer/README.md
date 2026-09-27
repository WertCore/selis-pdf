# The virtualised page list and its compositor

The continuous, windowed page list the viewer scrolls (SL-4.UI.02) and the tile
compositor that presents it (SL-4.UI.03) — and the primitive UI.04 (text
layer), UI.05 (search), UI.06 (navigation) and EXT.06 (the extension reuses
this verbatim) build on.

The list is a **controller plus pure geometry**, not a widget framework, and the
compositor is a **policy plus a port**, not a canvas. Neither paints the DOM:
the controller hands the shell one immutable `PageListState`, the compositor
hands its `TileSurface` a `CompositorFrame` of plain geometry, and the host
paints both. That is what lets the same code run in the web app, the extension
and the desktop shell (ADR-P0022), and what lets the whole thing be tested in
Node — the repo ships no jsdom, by ADR-P0021's zero-dependency rule.

## The pieces

| File | Owns |
|---|---|
| `layout.ts` | Page boxes: fit-page / fit-width / spread, zoom, gaps, padding, and the row index. Pure. |
| `windowing.ts` | Which rows exist for a scroll offset. Binary search, overscan band, hard row cap. Pure. |
| `anchoring.ts` | Keeping the reader on the same page and the same place in it across a relayout. Pure. |
| `tile-ladder.ts` | The placeholder → low-res → full-res ladder, the render scheduler (bounded concurrency, cancellation, LRU cache keyed by page, rung **and scale**). |
| `keyboard.ts` | The key map and the labels assistive tech reads. Pure. |
| `strings.ts` | The message keys and the English catalogue — every user-facing string. |
| `page-list.ts` | The controller: owns mode, zoom, scroll offset, window, current page, and the scheduler. |
| `surface.ts` | The `TileSurface` and `FrameClock` **ports**: where frames go, and what a frame is. Names no platform global. |
| `compositor.ts` | Which tile to present, at which device scale, into which device-pixel box — and whether it is standing in for a sharper one. |
| `worker-surface.ts` | The `OffscreenCanvas` surface: transfers the canvas to a worker, and strips tile pixels out of the frame on the way. |
| `offscreen-compositor.ts` | The worker half: owns the canvas, draws the ops, hands the composed frame back by transfer. |
| `page-list.css` | Presentation, entirely token-driven. |

## Using it

```ts
import { createPageList } from "@selis/ui";
import "@selis/ui-kit/css/tokens.css";
import "@selis/ui-kit/css/base.css";
import "@selis/ui/css/page-list.css";

const list = createPageList({ adapter, doc, mode: "width" });

// On mount, on resize, on scroll, and on a devicePixelRatio change:
const state = list.update({
	width: scroller.clientWidth,      // the shell measures; nothing here sniffs
	height: scroller.clientHeight,
	scrollTop: scroller.scrollTop,
	devicePixelRatio: hostDpr,
});
paint(state);                        // your DOM layer
```

Two rules for the shell:

1. **Metrics are inputs, not measurements.** `width`, `height`, `scrollTop` and
   `devicePixelRatio` are passed in because they are host facts
   (`getBoundingClientRect`, `devicePixelRatio`). The same goes for the two
   density values in `PageListMetrics` — read `--selis-density-page-gap` and
   `--selis-space-6` with `getComputedStyle` and hand them over. The package
   touches no platform globals, which `src/platform/platform-globals.test.ts`
   fails the build on.
2. **`setMode`/`setZoom` return a scroll offset. Apply it.** The returned number
   is where the reader's page will be after the relayout (anchoring); the state
   delivered to `onState` already assumes the shell applied it. Feed it back
   through `compositor.update({ …viewport, scrollTop })`.

## Adding the compositor

The list works without a canvas — that is UI.02's half, and it is what the
extension's DOM mode and any host without `OffscreenCanvas` use. Adding the
compositor is three injected values and one element:

```ts
const surface = createWorkerSurface({
	post: (message, transfer) => worker.postMessage(message, transfer),
	transfer: (canvas) => canvas.transferControlToOffscreen(),
});
const compositor = createTileCompositor({
	list,
	surface,
	clock: createAnimationFrameClock({
		request: (cb) => requestAnimationFrame(cb),
		cancel: (id) => cancelAnimationFrame(id),
	}),
});
surface.attach(canvas);          // once; the canvas is the worker's from here

// Every scroll, resize or DPR change is one call. The compositor coalesces
// them onto animation frames and re-reads the list's current state, so a
// burst of scroll events costs one draw rather than one draw per event.
compositor.update({
	width: scroller.clientWidth,
	height: scroller.clientHeight,
	scrollTop: scroller.scrollTop,
	devicePixelRatio: window.devicePixelRatio,
});
```

Three things the shell must get right, and why:

1. **The canvas is viewport-sized, not document-sized.** The compositor's ops
   are in viewport coordinates and it redraws on scroll. A canvas the height of
   a 2 000-page document is far past what a backing store can hold at DPR 2, and
   reallocating it on every zoom is a stall.
2. **`aria-hidden="true"` on the canvas**, and keep the DOM tiles. The canvas is
   a *painting* of the list: every page it draws is still a labelled `listitem`
   with its place in the roving tab order, and a page with no pixels yet is
   counted as a placeholder and left alone rather than drawn as a hole.
   `.selis-page-list__surface` already sets `pointer-events: none` for the same
   reason — the canvas must not become the interactive thing.
3. **Do not clear the tile cache on zoom.** `PageList` deliberately does not
   (the cache is keyed by scale); clearing it is exactly the blank-page flash
   the zoom path exists to avoid.

A host with no worker — the Tauri desktop webview, a test — implements
`TileSurface` directly against its own 2D context. It is three methods.

## Painting a state

`PageListState` is plain data:

- `tiles[]` — the window, in document order. Each has a box
  (`x`/`y`/`width`/`height`, CSS pixels, list coordinates), the ladder `stage`
  currently available (`placeholder` | `lowres` | `full`), a `className` of
  `.selis-*` classes, an `aria` `label` ("Page 7 of 312"), a roving `tabIndex`
  and a `current` flag for `aria-current="page"`.
- The list plane is sized by `--page-list-width` / `--page-list-height`; each
  tile's box by `--tile-x` / `--tile-y` / `--tile-w` / `--tile-h`. Set them as
  inline custom properties rather than an inline `style` string, so all
  presentation stays in `page-list.css`.
- `announcement` is the text for a polite live region; `regionLabel` and
  `listLabel` are the `aria-label`s for the scrolling region and the list.
- `tileAt(page)` returns the best cached bitmap for a page; `tilesFor(page)`
  returns every cached one, at every scale. The compositor uses the second: after
  a zoom the best thing it can show is a tile at the *previous* scale, presented
  scaled, and "best" in `tileAt`'s sense (full-res or not) cannot express that.
- `subscribe(listener)` observes every published state, which is how the
  compositor learns that a tile landed without the shell having to forward it.

## Accessibility

The list is a list (`role="list"` / `listitem`), not a listbox: pages are not
options. The topmost page in view is the current page — a function of the scroll
offset alone, so the toolbar, `aria-current` and the live region cannot disagree.
Arrow keys move one page (one spread in two-up mode), PageUp/PageDown move a
screen, Home/End jump to the ends, and `handleKey` returns `null` for keys that
are not the list's so the shell can keep them. The full axe/screen-reader pass is
SL-4.UI.07; this is UI.02's share of it.

**The compositor does not spend any of it.** A canvas is a poor substitute for a
list and this one is not asked to be: the canvas is `aria-hidden`, takes no
pointer events, and every page it paints keeps its DOM `listitem`, its
localised `aria-label` and its place in the roving tab order. A page the
compositor has no pixels for is *counted* and skipped, so its box keeps the
placeholder styling and its element keeps its semantics — the canvas never
becomes the only representation of a page. `compositor.test.ts` asserts that
invariant directly: every drawn page is a labelled tile, and exactly one tile is
in the tab order.

## Strings and i18n

Every user-facing string the page list owns — the tile `aria-label`, the live
region, the two region labels and the three fit-mode names — is a **key** in
`strings.ts`, with English in a catalogue. The ADRs require this from day 1 so
a rename is a resource change rather than a code change, and a translator can
reach the text without touching the geometry layer.

Pass a catalogue to `createPageList` to localise; overrides merge over English,
so a host overrides what it has and inherits the rest:

```ts
const list = createPageList({
	adapter,
	doc,
	strings: { "pageList.page.label": "Seite {page} von {total}" },
});
```

Placeholders are substituted **by name**, so a catalogue can reorder the
sentence — which most languages need to do. `SL-4.UI.11` owns the real runtime
(English shipping, pseudo-locale in CI) and replaces the body of
`createPageListStrings` with a catalogue lookup; nothing else moves.

`strings.test.ts` is the gate: it fails the build if a user-facing English
literal reappears beside the catalogue, or if an aria label is assigned from a
literal. It is a lint-style unit test, following the idiom
`platform-globals.test.ts` set for the host seam.

## The DoD clauses, and where they are proved

- *"60 fps sustained scroll on a 2 000-page document."* Two halves, and it is
  worth being precise about which is which.
  - *The windowing half* is in `page-list.test.ts`: the window is bounded by
    rows, the ladder is bounded per frame, a relayout is memoised so scrolling
    never triggers one, and a 2 000-page walk measures the per-frame cost
    against the 16 ms budget, logging the number. No canvas, no browser.
  - *The compositor half* is `cargo xtask compositor-bench`: the **built**
    `apps/ui/dist` compositor running in a real browser, with a real
    `OffscreenCanvas` transferred to a real `Worker`, both `ImageBitmap`
    transfers real, a real animation-frame loop, and a continuous scroll of a
    2 000-page document. It reports the UI thread's per-frame cost and asserts
    it against the same 16 ms budget, and it fails if too few frames were
    measured. The engine is stood in for by the UI.01 mock — the leg measures
    the compositor, not the rasteriser — and `xtask/src/compositor_bench.rs`
    states exactly what is real and what is not. A headless browser has no
    display, so the leg reports frame *spacing* without ever calling it a
    refresh rate.
- *"No layout shift when a tile resolves."* `layout.ts` cannot see tile state, so
  a page's box is identical at all three rungs. The tests compare the boxes and
  the content height before and after a tile lands. The compositor adds nothing
  that can move: it draws into the boxes the state already published.

## Extending it

- **UI.03** (compositor): shipped. It subscribes to the state, reads its boxes
  from it, and keeps no second source of truth for what is on screen.
- **UI.04** (text layer): the text layer goes inside the tile box, absolutely
  positioned against the same geometry — `.selis-text-layer` already exists in
  `base.css`. It is a DOM layer over the canvas, which is why the canvas being
  `aria-hidden` and pointer-transparent matters: the text layer is where
  selection lives, and it is ordinary, selectable, focusable DOM.
- **UI.05** (search): page numbers from `state.tiles` map straight onto
  `SearchMatch.page`.
- **UI.06** (navigation): `scrollTopFor(page)` and `goToPage(page)` are the only
  two ways to move; do not compute offsets anywhere else.
- **UI.08** (print): `TileSurface` is not the print path. Print renders at print
  resolution to its own target (ADR-P0030's colour question and the
  `/PrintScaling` requirement both outrank reusing a screen compositor).
- **Thumbnails** (UI.02's sibling in UI.06's scope): a thumbnail rail is the same
  ladder at a fixed width — reuse `planLadder` with a viewport one page wide
  rather than writing a second renderer.
