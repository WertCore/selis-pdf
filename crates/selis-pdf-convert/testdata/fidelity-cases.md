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

## 4. Embedded JPEG images (HTML and Markdown)

Before — `<img>` was dropped and `![alt](…)` was skipped; no image XObject,
no pixels on the page:

```html
<img src="data:image/jpeg;base64,/9j/4AAQSkZJRg…" width="100" height="50">
```

```markdown
![a red square](data:image/jpeg;base64,/9j/4AAQSkZJRg…)
```

After — the base64 JPEG is decoded without fetching anything, its SOF marker
supplies the true geometry, and the bytes are embedded verbatim behind a
`DCTDecode` image XObject. Observable in the re-opened PDF:

- exactly one `/Subtype /Image` stream with `Width`/`Height` from the SOF,
  `/Filter /DCTDecode`, `/BitsPerComponent 8`, a `/ColorSpace` derived from the
  SOF component count, and stream bytes byte-identical to the source JPEG;
- the page's `/Resources` carries an `/XObject << /Im1 … >>` entry;
- the content stream draws it with a `cm` matrix whose rectangle is the
  measured box — intrinsic pixels × 0.75 pt (96 dpi), or the explicit
  `width`/`height` attributes when given, clamped to the 504 pt text column —
  followed by `/Im1 Do`;
- rasterising the re-opened document at 72 dpi shows the image's ink inside
  that rectangle (a solid-red 32×32 JPEG paints red pixels there).

The refusal path is part of the case: `https://…`, `file://…` and
`data:image/png;base64,…` sources are refused by name (typed image error),
produce **no** XObject and **no** `Do` operator, and are never fetched.

## What this corpus does *not* claim

Font embedding/subsetting, full CSS layout (cascade, rulesheets, selectors,
relative units, floats/positioning), chart/math/SVG-in-HTML rendering, and a
true fidelity renderer remain out of scope. Case 4 covers only JPEG data-URI
images: PNG/GIF/WebP data URIs, remote images, images in the Office/EPUB
readers, and CSS background images are still not embedded. These cases only
narrow the inbound-fidelity gap; they do not make Selis Aspose-equivalent.
