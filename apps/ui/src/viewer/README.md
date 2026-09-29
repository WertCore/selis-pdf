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
Node — the repo ships no jsdom, and SL-4.UI.01's `platform-globals` gate is the
half of that which is mechanically enforced rather than merely agreed.

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
| `page-labels.ts` | The document's own page numbering: `/PageLabels` ranges, styles, prefixes, and the bijective-alphabetic and roman renderers. Pure. |
| `outline.ts` | The bookmark tree as a flat, navigable list: path ids, expansion, the row cap, and the treeview key movement. Pure. |
| `links.ts` | **ADR-P0020**: what activating a link does — `internal`, `external` (with the full destination) or `blocked` (with a reason). Pure. |
| `navigation.ts` | The controller: the outline, the labels, the named destinations, the roving focus, and the external-link handshake. |
| `thumbnails.ts` | The rail: the tile ladder at a fixed width, windowed and bounded. |
| `navigation.css` | Presentation for the outline panel and the rail, token-driven. |
| `text-layer.ts` | Where every character is, in CSS pixels inside a page box. Pure projection from the engine's user-space quads. |
| `selection.ts` | Hit testing, caret movement, selection rectangles, and copy. Pure functions of a `TextLayerFrame`. |
| `search.ts` | The search controller (SL-4.UI.05): consumes `EnginePort.search`'s batches, owns the query, the matches and the current one, and publishes highlights. |

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

## The text layer, selection, and copy (SL-4.UI.04)

`text-layer.ts` turns the engine's `PageTextLayer` (PDF user space: points, y up,
origin at the MediaBox's bottom-left) into a `TextLayerFrame` of CSS-pixel boxes
inside one page element. `selection.ts` answers every selection question as a pure
function of that frame. Neither paints; the host renders the frame as ordinary
DOM, which is what keeps `../platform/platform-globals.test.ts` green and lets the
whole thing be asserted in Node (the repo ships no jsdom, and that gate is what
stops a platform global creeping back in).

### Four decisions worth knowing before you touch this

1. **Geometry comes from the engine's quads, never from the compositor's tiles.**
   A tile has pixels and no characters. Deriving the layer from tiles would couple
   selection to the render ladder, and the ladder deliberately presents a
   *previous* scale's bitmap during a zoom (`DrawOp.provisional`) -- so selection
   would be wrong exactly when the reader is zooming, which is the one moment it
   has to survive. The quads are resolution-independent, so one fetch serves every
   zoom level.

2. **The only multiplier is `PlacedPage.scale` (CSS px per PDF point).** Neither
   `SurfaceSize.scale` (the real backing-store factor, `deviceWidth / cssWidth`) nor
   `SurfaceSize.devicePixelRatio` (the nominal ratio) belongs here. The text layer
   is DOM positioned in CSS pixels, so the browser applies the device factor itself,
   once, when it rasterises. Multiplying by a device ratio would place the layer at
   1.25x its box on a 125% display -- and the drift would be proportional, so it
   would still look plausible. `text-layer.test.ts` asserts this explicitly.

3. **A caret is a `(line, utf16Offset)` pair, so zoom survival is structural.**
   Nothing in the frame is a pixel; the quads are re-projected on every zoom. A
   selection stored as pixel rectangles would have to be rescaled by hand, and any
   disagreement with the new layout is a selection that is subtly off at some zoom
   levels and not others -- the bug this shape makes unrepresentable.

4. **Copy is a slice, so it cannot drift from `selis extract`.** The engine builds
   `chars` index-aligned with each line's `text` and pins the page's `text` to be
   byte-identical to `to_text`, which is what `apps/cli/src/extract.rs` prints.
   `selectedText` is therefore `line.text.slice(a, b)`: no re-extraction, no
   re-joining, no second code path. A whole-page selection copies exactly
   `frame.text`, and a whole document is `pages.join("\n")` -- one separator,
   because `extract.rs` prints one between pages and `to_text` emits no trailing
   newline.

### Accessibility, and not regressing UI.02

The text layer is the accessible surface, not a decoration over one: it carries the
real text in reading order, so a screen reader and the browser's own find-in-page and
selection all work against it. Three rules for the host, all of which protect the
keyboard and announcement support UI.02 built:

- **The page tile keeps its `listitem` role and its `aria-label`.** The text layer
  goes *inside* the page element; it does not replace it. UI.02's roving tab order
  and "Page 7 of 312" announcements are unchanged because nothing about the tile's
  role or label changes.
- **Do not put the text layer in the roving tab order.** It is reachable by the
  caret commands in `selection.ts`, which the page tile forwards. Adding every line
  as a tab stop would make a 2 000-page document untabbable and would fight the
  arrow-key model the layer already implements.
- **The canvas stays `aria-hidden="true"` and the layer stays real text.** The
  canvas is a painting; the text is the content. This is the same split as UI.03's
  and it is why the layer is DOM rather than drawn into the surface.

### Known limits, recorded rather than papered over

- **One direction per line.** `direction` is read from the sign of the step between
  a line's first two inked quads -- a mirrored run's quads descend in x, and that
  descent is what a mirrored caret has to mirror. It is deliberately *not* a UAX #9
  resolution. A mixed-direction line (an Arabic word inside an English sentence) is
  therefore resolved against a single per-line answer, and a visual move on such a
  line may be off within the embedded run. Fixing it properly needs a real bidi
  pass, which is a shaper/extractor question, not a viewer one.
- **Paragraph base direction is not applied to the layer.** It is a different
  question, asked by `selis-shape`'s first-strong, and importing it here would be
  both a new layer edge and the wrong answer for a *visual* layer.
- **No live-region announcements for caret moves.** Caret position is a visual
  concept; announcing every arrow press would be noise. UI.02's page-level
  announcements are the ones that matter and they are unaffected.

## Search (SL-4.UI.05)

`createSearch` is a second controller beside the page list, and deliberately
shaped like it: it owns a slice, reports facts, hands back one immutable
`SearchState`, and paints nothing. The slice is the query, the modifiers, the
matches and which one is current. The viewport is not in it — the shell reports
what is on screen through `update({ currentPage, pages })`, the same way it
reports geometry to the page list.

```ts
const search = createSearch({
	adapter, doc,
	// The only geometry input. `.scale` is CSS pixels per PDF point; nothing
	// here knows about tiles, rungs or device pixels.
	placePage: (page) => list.layout().pages.find((entry) => entry.page === page) ?? null,
});

search.update({ currentPage: list.state().currentPage, pages: list.state().tiles.map((t) => t.page) });
search.setQuery("selis");            // one call per input event
search.setModifiers({ caseSensitive: true });

// Painting: `highlights[]` are boxes inside a page element, and `announcement`
// is the polite live region. Scrolling is the shell's, and it is the page list's
// job — `goToPage` and `scrollTopForPage` are the only two ways to move.
if (state.currentMatch && state.currentMatch.page !== state.visiblePage) {
	list.goToPage(state.currentMatch.page, "centre");
}
```

### Four decisions, and the reasons are in `search.ts`

1. **The state is the controller's, and that is a temporary answer.** ADR-P0035
   puts search match state in `selis-viewmodel`, and that crate does not exist
   yet. So the state is a plain object republished whole, and every question
   about it is a pure function over plain data — the shape that makes moving it
   across the language boundary a matter of calling these from Rust. The
   alternative, a richer local store, is more code today and nothing to migrate
   tomorrow.
2. **Highlights come from the text layer, not from `SearchMatch.rects` and never
   from the render ladder.** A match is a character range; the rectangles are
   `selectionRects` over the same `TextLayerFrame` the text selection uses, so a
   match highlight and a text selection cannot disagree, and a zoom re-projects
   from the engine's scale-free points while the tile under it is still a
   provisional bitmap from the previous scale.
3. **A superseded search cannot repaint.** Every run has a monotonic id and its
   own `AbortController`, and every mutation — batches applied, text layers
   cached — sits behind the id check. `EnginePort.search` is an `AsyncIterable`,
   so cancellation is only a promise the transport makes; the test proves the
   guard against a transport that ignores the signal and keeps yielding.
4. **The work is bounded by the window, not the document.** The count is
   document-wide, but text layers are fetched only for pages the reader can see,
   and only for pages that have matches. Per-batch cost is O(window) on a
   2 000-page document.

### Accessibility

`Enter` and `F3` step to the next match, their shifted forms to the previous,
and `Escape` closes search; `resolveSearchKey` returns `null` for everything
else, and for *everything* when there is no query, so it never swallows a key the
shell needs. The live-region sentence is chosen so a scan in progress never says
"no matches" — it says how many so far — and the current match is announced only
when it changes, so a 2 000-page scan does not interrupt itself once per page.
The page in that sentence is worded by the *page list's* catalogue, passed in, so
one page cannot be worded two ways.

### Known limits, recorded rather than papered over

- **`SearchMatch.rects` is deliberately unread.** It is optional, so no transport
  must supply it, and the engine's own `SearchMatch` carries the **line's**
  bounding rect rather than a per-match one — a transport that mapped it naively
  would highlight the whole line. A second geometry source that can be absent and
  that disagrees with the text layer when present is worse than none.
- **A range that does not index the page's text has no rectangle.** The engine
  normalises before searching (ligatures, soft hyphens, NFD), so an offset can
  come back that does not address the page's own string. The match still counts
  and still navigates; it simply has no box. Making the offsets authoritative
  means changing the engine's search to report positions in the *original* text,
  which is a TEXT.06 question, not a viewer one.
- **No browser pass yet.** Every clause of the DoD is asserted as data — the
  count, the boxes, the key map, the announcement — because the repo ships no
  jsdom. Nothing here has been looked at on a screen.
- **`state.highlights` is windowed, so a host that wants a per-page badge for
  every page in the document needs UI.06's page model.** The breakdown the
  viewer needs — a badge on each visible page — is complete; the breakdown a
  search-results sidebar would need is not, and a sidebar is UI.06's.

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
`../platform/platform-globals.test.ts` set for the host seam.

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
- **UI.05** (search): shipped. `createSearch` beside the page list; the shell
  wires `placePage` to the page list's layout and calls `goToPage` for the
  current match. Highlights are DOM boxes inside the page element, positioned by
  `--match-x`/`-y`/`-w`/`-h` exactly as a placed tile is, and they sit under the
  text layer so the glyphs stay legible and the text stays selectable.
- **UI.06** (navigation): shipped. `scrollTopFor(page)` and `goToPage(page)` are
  still the only two ways to move; navigation does not add a third. It
  **publishes a target** (`NavigationState.target`) and the shell hands it to the
  page list, so a bookmark and a search hit move the document the same way.
- **UI.06** (links): `decideLinkAction` in `links.ts` is the whole of ADR-P0020
  in this package. `internal` moves inside the document, `external` publishes a
  **prompt and opens nothing**, and `blocked` carries a reason the live region
  says out loud. `openExternal` is called from exactly one place in
  `navigation.ts`, behind a confirmation. `/Launch`, `/GoToR`, `/SubmitForm`,
  `/ImportData` and document JavaScript are refused *before* any field of the
  action is examined, so a `/Launch` carrying a URI-shaped string is still a
  launch. `links.test.ts` drives all of them through the real controller and
  asserts the mock host recorded no URL.
- **UI.06** (page labels): a page's own label, in `page-labels.ts`. It is data,
  not prose, so it is never in the catalogue; the sentences around it are, and
  they take the page-list's catalogue so "Page 7 of 900" cannot be worded two
  ways. A label a style cannot express (roman above 3 999) degrades to the page
  number rather than to an empty string — a recorded fidelity limit.
- **UI.06** (thumbnails): a thumbnail rail is the same ladder at a fixed width —
  `planLadder` per page, into its own `TileScheduler`. Two instances of one class,
  not a second implementation. The rail's one adaptation is the request `hint`,
  which it rewrites to `thumbnail`: the engine picks its budget profile from the
  hint, and every page in a rail is a preview. The cache key does not include the
  hint, so nothing else changes.
- **UI.08** (print): `TileSurface` is not the print path. Print renders at print
  resolution to its own target (ADR-P0030's colour question and the
  `/PrintScaling` requirement both outrank reusing a screen compositor).

## What UI.06 does *not* prove, and what is owed a manual pass

Honest limits, in the spirit of the search section above:

- **The engine has no outline walk and no link reader yet.** `selis-pdf-doc`
  exports `page_labels` and `parse_destination` but nothing for `/Outlines` or
  annotations, and the WASM.01 protocol has no op for either. So
  `PlatformAdapter.navigation` is an **optional** port, and `apps/web/host` and
  the extension both report it absent today; the viewer then says *"this host
  cannot read the document's outline"*, which is a different sentence from
  *"this document has no outline"* and is the truth. Everything above is proven
  against the mock, which serves the port from a declarative spec.
- **A mid-page `/XYZ` destination shows the top of the page.** The point is
  reduced to a page and an alignment because `goToPage` is the only way to move;
  scrolling to a position inside a page is a UI.07 question with a real answer.
- **Nothing has been looked at on a screen.** The outline's treeview roles, the
  dialog's chrome, the rail's appearance and the prompt's readability are all
  owed a browser pass. What *is* asserted headlessly is the prompt's **content**
  (the full destination, verbatim, plus the host), because that is the part this
  code owns and the part ADR-P0020 is about.
