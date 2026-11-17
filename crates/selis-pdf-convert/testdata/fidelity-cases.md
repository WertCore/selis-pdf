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

## What this corpus does *not* claim

Font embedding/subsetting, full CSS layout (cascade, rulesheets, selectors,
relative units, floats/positioning), image/chart/math/SVG-in-HTML rendering,
and a true fidelity renderer remain out of scope. These cases only narrow the
inbound-fidelity gap; they do not make Selis Aspose-equivalent.
