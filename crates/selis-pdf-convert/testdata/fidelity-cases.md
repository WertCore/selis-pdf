# Inbound conversion fidelity — before / after corpus

A small, human-readable corpus of the cases closed by the `conv-fidelity` slice.
Each case is exercised as an inline fixture by the read-back tests in
`src/lib.rs` (`mod fidelity`): the test converts the input, **re-opens the PDF
with the engine's own reader**, and asserts the observable structure listed
below. Nothing here is verified by string-matching the converter source.

## 1. Link annotations (HTML and Markdown)

Before — URL painted in parentheses, no annotation:

```markdown
Read [the docs](https://selis.example/docs?q=1&r=2) today.
```

After (observable in the re-opened PDF):

- the label `the docs` is painted as text; the URL is **not** painted;
- exactly one `/Subtype /Link` annotation with
  `/A << /S /URI /URI (https://selis.example/docs?q=1&r=2) >>`;
- the annotation's `/Rect` is non-degenerate and lies on the page.

HTML equivalent:

```html
<p>Visit <a href="https://example.org/a(b)">our site</a>.</p>
```

The `(` `)` in the URL also regression-guards PDF string-literal escaping:
the re-parsed URI must read back as `https://example.org/a(b)`.

Multiple links (`[one](https://a.test) … [two](https://b.test)`) must produce
distinct annotations (one per link box).

## 2. Inline colour and font size

Before — all text black at 11 pt; `style=` dropped.

After — input:

```html
<p>before <span style="color:#ff0000; font-size:20pt">RED BIG</span> after</p>
```

Observable in the re-opened content stream:

- an `1 0 0 rg` fill operator for the red run and `0 0 0 rg` for the body;
- a `/F1 20 Tf` size for the styled run and `/F1 11 Tf` for the body.

Narrow colour forms: `color:blue`, `color:rgb(0,128,0)` → `0 0 1 rg`,
`0 0.502 0 rg`. Units: `font-size:16px` → `/F1 12 Tf` (96 dpi).

Unknown declarations (`color:rebeccapurple`, `font-size:2em`,
`text-align:justify`, `background:yellow`) are ignored: the run keeps the
default black 11 pt style, with no new colour operator.

## 3. Text alignment (paragraphs and table columns)

Before — alignment dropped; all text flush left.

After — paragraph input:

```html
<p style="text-align:left">LEFT</p>
<p style="text-align:center">CENTER</p>
<p style="text-align:right">RIGHT</p>
```

Observable text-matrix x-positions: `LEFT < CENTER < RIGHT`, with `LEFT` at
the 54 pt margin and `RIGHT` near the right margin.

GFM column alignment — A/B two tables with identical cells, one with plain
`---` delimiters and one with `:---` / `:---:` / `---:`:

```markdown
| Long Header One | Long Header Two | Long Header Three |
| :---            | :---:           | ---:              |
| x               | y               | z                 |
```

Observable: the left column's body x is unchanged, the centred and right
columns' body x values move right versus the plain table.

HTML cell alignment `<td align="right">x</td>` likewise shifts the cell's
text x versus the same cell without the attribute.

## 4. Embedded JPEG and PNG images (HTML and Markdown)

Before — `<img>` was dropped and `![alt](…)` was skipped; no image XObject,
no pixels on the page:

```html
<img src="data:image/jpeg;base64,/9j/4AAQSkZJRg…" width="100" height="50">
<img src="data:image/png;base64,iVBORw0KGgo…">
```

```markdown
![a red square](data:image/jpeg;base64,/9j/4AAQSkZJRg…)
![a red square](data:image/png;base64,iVBORw0KGgo…)
```

After — the base64 payload is decoded without fetching anything. A JPEG's SOF
marker supplies the true geometry and its bytes are embedded verbatim behind a
`DCTDecode` XObject. A PNG's IHDR supplies the geometry; its IDAT is decoded
through the engine's PNG reader (zlib + scanline filters), alpha (if any) is
flattened over white, and the raw samples are Flate-re-encoded behind a
`FlateDecode` XObject. Observable in the re-opened PDF:

- exactly one `/Subtype /Image` stream with `Width`/`Height` from the source,
  `/Filter /DCTDecode` (JPEG) or `/FlateDecode` (PNG), `/BitsPerComponent 8`,
  and `/ColorSpace` from the SOF component count or the PNG IHDR (`/DeviceGray`
  for grey, `/DeviceRGB` for colour). JPEG stream bytes are byte-identical to
  the source; PNG stream bytes decode back to the unfiltered RGB/gray samples;
- the page's `/Resources` carries an `/XObject << /Im1 … >>` entry;
- the content stream draws it with a `cm` matrix whose rectangle is the
  measured box — intrinsic pixels × 0.75 pt (96 dpi), or the explicit
  `width`/`height` attributes when given, clamped to the 504 pt text column —
  followed by `/Im1 Do`;
- rasterising the re-opened document at 72 dpi shows the image's ink inside
  that rectangle (a solid-red 32×32 JPEG **or PNG** paints red pixels there);
- a 2×1 grey PNG reads back as `/DeviceGray` with samples `[0x20, 0xE0]`; an
  RGBA PNG with one opaque red and one fully transparent pixel reads back as
  `/DeviceRGB` samples `[255,0,0, 255,255,255]` (alpha flattened over white).

The refusal path is part of the case: `https://…`, `file://…`, GIF/WebP/SVG
data URIs, malformed/truncated JPEGs and PNGs, and PNGs outside the supported
subset (palette, interlaced, non-8-bit) are refused by name with typed image
errors, produce **no** XObject and **no** `Do` operator, and are never fetched.

## 5. Inline `background-image` data URIs

Before — `style="background-image: …"` was dropped; no pixels behind the text.

```html
<p style="background-image: url('data:image/png;base64,iVBORw0KGgo…')">
  Text over a background
</p>
```

After — a single `url(...)` token with a base64 JPEG/PNG data URI is decoded
through the same image path as `<img src>` and drawn as an image XObject
**before** the element's text runs in the content stream, so the image sits
behind the glyphs. Observable in the re-opened PDF:

- a `/Subtype /Image` XObject with the same geometry/filter/colour-space rules
  as case 4 (`/FlateDecode` for a PNG background, `/DCTDecode` for JPEG);
- the `/Im1 Do` operator appears before the styled text's `Tj` operator.

This is a deliberately narrow CSS subset: remote URLs, gradients, multiple
images, other declarations and rulesheets are ignored or refused by name, and
no network access occurs.

## 6. Embedded SVG data URIs (HTML and Markdown)

Before — SVG `data:` URIs were refused by name; `<img>` and `![alt]()` produced
no XObject and no ink.

```html
<img src="data:image/svg+xml;base64,PHN2ZyB3aWR0aD0iNDAiIGhlaWdodD0iMjAiIHZpZXdCb3g9IjAgMCA0MCAyMCI+PHJlY3Qgd2lkdGg9IjQwIiBoZWlnaHQ9IjIwIiBmaWxsPSIjZmYwMDAwIi8+PC9zdmc+">
```

```markdown
![a red square](data:image/svg+xml,%3Csvg%20width%3D%2240%22...)
```

After — both the base64 and percent-encoded forms are decoded (with a 256 KiB
cap), and the SVG text is handed to the **existing** `svg.rs` subset parser and
emitter — no new SVG engine and no rasterisation. Observable in the re-opened
PDF:

- exactly one `/Subtype /Form` XObject (not an `Image`): `/BBox [0 0 30 15]`
  for the 40×20 px canvas, `/Resources << /Font … >>` so SVG `<text>` can use
  the standard-14 faces;
- the form stream begins with the y-flip `1 0 0 -1 0 15 cm` and contains the
  shape operators (`re`-equivalent path, `rg`, `f`);
- the page draws it as `/Im1 Do` under a `cm` whose scale divides by the form's
  natural size — identity when the box is intrinsic (`1 0 0 1 54 723 cm`), and
  `3.333 0 0 3.333 54 688 cm` with `width="100" height="50"`;
- rasterising at 72 dpi shows red ink inside that box.

The refusal path is part of this case: a remote URL, `<script>`, `<use>`,
`<foreignObject>`, `<image>`, an event-handler attribute, a non-`#fragment`
`href`, a non-SVG root, malformed XML and an unsupported arc all produce **no**
Form XObject and **no** `Do`, and nothing is fetched. Markdown remote/unsafe
SVGs are refused through the same source path.

## 7. CSS `background-size`

Before — `background-size` was ignored; a background always stretched to the
run's text box.

```html
<p style="background-image:url(data:image/png;base64,iVBORw0KGgo…);background-size:8pt 4pt">Hello</p>
```

After — the declaration is read from inline `style=` (or the rulesheet) and
resolves against the element box through the same layout clamping as images:

- `auto` uses the intrinsic image size, `contain` fits it within the box,
  `cover` fills the box (preserving aspect, **not** clipped), and absolute
  lengths (`pt`/`px`/bare number) set the size — `8pt 4pt` reads back as the
  `cm` rectangle `8 0 0 4 …`; a single length derives the other axis;
- `cover` reads back measurably wider than `contain` for a short, wide text box;
- absent `background-size` keeps the legacy stretch-to-box behaviour.

Percentages, keywords other than `auto`/`contain`/`cover`, relative units,
`background-position`, `background-repeat` and multiple layers are ignored.

## 8. Narrow `<style>` rulesheet and two-level cascade

Before — `<style>` was dropped wholesale; only inline `style=` had any effect.

```html
<style>
  p { color: #ff0000; font-size: 20pt; text-align: center }
  h1 { color: blue; font-size: 20pt }
</style>
<p>RED</p>
<h1>HEAD</h1>
<p style="color:#00ff00;text-align:left">GREEN</p>
```

After — a **single** `<style>` block contributes rules for bare element-type
selectors `p`, `h1`–`h6`, `li`, `td`. Observable in the re-opened stream:

- the rulesheet fills appear (`1 0 0 rg` for `p`, `0 0 1 rg` for `h1`) and the
  sizes take effect (`/F1 20 Tf`, `/F2 20 Tf`);
- the inline declarations win **per property**: `GREEN` paints `0 1 0 rg` and
  its `text-align:left` keeps it at the margin while the rulesheet-centred
  `RED` shifts right;
- rulesheet `background-image`/`background-size` work and an inline
  `background-size` overrides just that property.

Ignored rather than guessed: `.cls`, `#id`, `p.special`, `div p`, `*`, unknown
element names, `@media` and every other at-rule (with their nested rules),
second `<style>` blocks and unknown declarations. A negative control keeps the
default black fill for a paragraph whose only rules are ignored.

## What this corpus does *not* claim

Font embedding/subsetting, full CSS layout (general selectors/cascade,
relative units, floats/positioning, flex/grid), chart/math rendering, and a
true fidelity renderer remain out of scope. Cases 4–6 cover JPEG/PNG/SVG
data-URI images; cases 7–8 cover a narrow `background-size` and a two-level
inline-over-rulesheet subset. GIF/WebP data URIs, remote images, arbitrary CSS
backgrounds (gradients, multiple layers, position/repeat), at-rules, images in
the Office/EPUB readers, and inline images inside a Markdown paragraph are
still not embedded. SVG support is exactly the standalone `svg` subset — no
arcs, gradients, `use`, scripting or external references. These cases only
narrow the inbound-fidelity gap; they do not make Selis Aspose-equivalent.
