# The virtualised page list (SL-4.UI.02)

The continuous, windowed page list the viewer scrolls — and the primitive
UI.03 (canvas compositor), UI.04 (text layer), UI.05 (search), UI.06
(navigation) and EXT.06 (the extension reuses this verbatim) build on.

It is a **controller plus pure geometry**, not a widget framework. There is no
DOM in this package and no rendering library: the controller hands the shell one
immutable `PageListState` and the shell paints it. That is what lets the same
code run in the web app, the extension and the desktop shell (ADR-P0022), and
what lets the whole thing be tested in Node — the repo ships no jsdom, by
ADR-P0021's zero-dependency rule.

## The pieces

| File | Owns |
|---|---|
| `layout.ts` | Page boxes: fit-page / fit-width / spread, zoom, gaps, padding, and the row index. Pure. |
| `windowing.ts` | Which rows exist for a scroll offset. Binary search, overscan band, hard row cap. Pure. |
| `anchoring.ts` | Keeping the reader on the same page and the same place in it across a relayout. Pure. |
| `tile-ladder.ts` | The placeholder → low-res → full-res ladder, the render scheduler (bounded concurrency, cancellation, LRU cache). |
| `keyboard.ts` | The key map and the labels assistive tech reads. Pure. |
| `page-list.ts` | The controller: owns mode, zoom, scroll offset, window, current page, and the scheduler. |
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
   delivered to `onState` already assumes the shell applied it.

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
- `tileAt(page)` returns the cached bitmap for the compositor (UI.03).

## Accessibility

The list is a list (`role="list"` / `listitem`), not a listbox: pages are not
options. The topmost page in view is the current page — a function of the scroll
offset alone, so the toolbar, `aria-current` and the live region cannot disagree.
Arrow keys move one page (one spread in two-up mode), PageUp/PageDown move a
screen, Home/End jump to the ends, and `handleKey` returns `null` for keys that
are not the list's so the shell can keep them. The full axe/screen-reader pass is
SL-4.UI.07; this is UI.02's share of it.

## The two DoD clauses, and where they are proved

- *"60 fps sustained scroll on a 2 000-page document."* The window is bounded by
  rows, the ladder is bounded per frame, and a relayout is memoised so scrolling
  never triggers one. `page-list.test.ts` walks a full 2 000-page document and
  asserts the per-frame cost against the 16 ms budget, logging the measured
  number. Real frames-per-second needs a browser and lands with UI.03's
  compositor; this is the headless half of the claim.
- *"No layout shift when a tile resolves."* `layout.ts` cannot see tile state, so
  a page's box is identical at all three rungs. The tests compare the boxes and
  the content height before and after a tile lands.

## Extending it

- **UI.03** (compositor): subscribe via `tileAt`/`onTile`; do not add a second
  source of truth for what is on screen.
- **UI.04** (text layer): the text layer goes inside the tile box, absolutely
  positioned against the same geometry — `.selis-text-layer` already exists in
  `base.css`.
- **UI.05** (search): page numbers from `state.tiles` map straight onto
  `SearchMatch.page`.
- **UI.06** (navigation): `scrollTopFor(page)` and `goToPage(page)` are the only
  two ways to move; do not compute offsets anywhere else.
- **Thumbnails** (UI.02's sibling in UI.06's scope): a thumbnail rail is the same
  ladder at a fixed width — reuse `planLadder` with a viewport one page wide
  rather than writing a second renderer.
