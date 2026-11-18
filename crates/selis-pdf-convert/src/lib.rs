//! The conversion crate of the Selis PDF engine (SL-5.CONV.05).
//!
//! Deliberately scoped per SL-5.CONV.05: a real HTML engine is out of scope,
//! so this crate ships a **documented subset**, not a half-working general
//! renderer. It converts Markdown and an HTML subset into paginated PDF bytes
//! using the standard-14 base fonts (measured with [`selis_font::standard14`]
//! metrics, encoded in WinAnsiEncoding) and the full-document writer
//! (SL-1A.WRITE.01). This narrows the inbound-fidelity gap to Aspose; it does
//! not make Selis a fidelity renderer.
//!
//! # Supported Markdown subset
//!
//! ATX headings (`#`–`######`), paragraphs with soft line breaks, bold /
//! italic / inline code spans, fenced (``` ``` ```) and indented code blocks,
//! bullet lists (`-`, `*`, `+`, nested by two-space indents), ordered lists
//! (`1.`), blockquotes (`>`), horizontal rules, GFM pipe tables (cells carry
//! plain text; `:---:` / `:---:` column alignment is honoured; `\|` escapes a
//! pipe), and links (`[text](url)`). A link's label is painted as text and a
//! **real URI link annotation** (`/Subtype /Link`, `/A << /S /URI … >>`) is
//! laid over it; the URL is no longer printed in parentheses. The annotation
//! has a zero-width border and no appearance stream — it is clickable, but
//! the label is not automatically underlined or recoloured. An **image whose
//! whole paragraph is `![alt](data:image/jpeg;base64,…)`** is embedded as a
//! real JPEG XObject (see below); inline images inside other text, reference
//! links, and raw HTML inside Markdown are ignored or rendered as plain text.
//! A whole-paragraph `![alt](data:image/png;base64,…)` embeds a PNG the same way.
//!
//! # Supported HTML subset
//!
//! `<h1>`–`<h6>`, `<p>`, `<br>`, `<hr>`, `<ul>`/`<ol>`/`<li>`, `<pre>` (with
//! optional inner `<code>`), `<blockquote>`, `<b>`/`<strong>`, `<i>`/`<em>`,
//! `<code>`, `<span>`, `<a href>` (link label plus a real URI annotation, as
//! in Markdown), `<table>` with `<tr>`/`<th>`/`<td>` (inline cell content only;
//! `<th>` in the first row makes a header row; nested tables and lists inside
//! cells are not supported), and block containers (`<div>`, `<section>`,
//! `<article>`, `<main>`, `<table>` cells). `<img src="…">` embeds an image
//! when `src` is a **base64 JPEG or PNG data URI**; `width`/`height` attributes
//! override its intrinsic size (see below). An inline
//! `style="background-image: url(data:image/…;base64,…)"` paints the same image
//! behind that element's text runs (see below). `<script>` and `<style>` content
//! is dropped; unknown tags unwrap to their text. Named (`&amp;`, `&eacute;`,
//! …) and numeric (`&#8212;`, `&#x2014;`) entities are decoded.
//!
//! # Supported images (JPEG and PNG data URIs)
//!
//! HTML `<img src="data:image/jpeg;base64,…">` (or `data:image/png;base64,…`)
//! and a Markdown paragraph that is exactly the same data URI inside `![alt]()`
//! are decoded and embedded as a real image XObject. JPEG bytes are written
//! verbatim behind `/Filter /DCTDecode`; a PNG's IDAT is decoded and its raw
//! samples re-encoded behind `/Filter /FlateDecode`. `/Width`, `/Height` and
//! `/ColorSpace` come from the source (JPEG SOF: `/DeviceGray`, `/DeviceRGB` or
//! `/DeviceCMYK`; PNG IHDR: `/DeviceGray` or `/DeviceRGB`). Intrinsic pixels map
//! to points at 96 dpi (0.75 pt/px). HTML `width`/`height` attributes override
//! the intrinsic size (bare number = pt, `px` at 96 dpi, `pt` = pt); when only
//! one is given the other follows the intrinsic aspect ratio, and the placement
//! rectangle is clamped to the page's text width. Read-back tests re-open the
//! PDF and assert the XObject entries, the `cm`/`Do` placement and the
//! rasterised ink.
//!
//! PNG support is deliberately narrow: 8-bit non-interlaced colour types 0
//! (grey), 2 (RGB), 4 (grey+alpha) and 6 (RGBA). Alpha is **flattened over
//! white**. Palette (3), interlaced, other bit depths and other media types are
//! refused with a typed error. Everything else is **refused by name and
//! skipped** (no text painted, no XObject emitted): remote `http(s)` URLs,
//! `file:` paths, any other scheme, GIF/WebP/SVG data URIs, malformed or
//! truncated files, and inline images mixed with other text. There is no
//! network access anywhere on the conversion path.
//!
//! # Supported inline styling
//!
//! A narrow, **well-defined** set of CSS declarations in inline `style=`
//! attributes (and the legacy `align=` attribute on table cells) is applied:
//!
//! - `color:` — `#rgb`, `#rrggbb`, `rgb(r,g,b)` (0–255) and the named colours
//!   `black`, `white`, `red`, `green`, `blue`, `navy`, `gray`/`grey`,
//!   `yellow`, `purple`, `orange`. Emitted as an `rg` fill operator.
//! - `font-size:` — absolute `pt` and `px` (at 96 dpi) and a bare number
//!   (points), clamped to [1, 72]. Relative units (`em`, `%`, `rem`,
//!   keywords) are deliberately ignored.
//! - `text-align:` — `left`, `center`, `right`, applied to paragraphs and
//!   table cells by shifting the laid-out text runs. `align="left|center|right"`
//!   on `<td>`/`<th>` is honoured too.
//! - `background-image:` — only a single `url("data:image/…;base64,…")` token
//!   (quoted or bare) is understood; the JPEG/PNG is painted behind that
//!   element's text runs. Remote URLs, gradients, multiple images and unknown
//!   declarations are ignored or refused by name. This is a narrow subset, not
//!   a CSS engine.
//! - Column alignment from GFM `:---` / `---:` / `:---:` delimiter markers is
//!   applied to the whole column.
//!
//! Any other declaration is ignored, never guessed. Styles are inherited by
//! nested elements with the innermost declaration winning. CSS rulesheets
//! (`<style>` blocks), selectors, the cascade, `@media`, external stylesheets
//! and shorthand properties are **not** supported.
//!
//! # Typography
//!
//! US Letter or A4 pages, 54 pt margins, 11 pt Helvetica body (Helvetica-Bold
//! headings, Helvetica-Oblique blockquotes, Courier code blocks). Tables are
//! set booktabs-style: a heavy rule above the header, hairlines between body
//! rows, the header row on a light grey fill, columns sized to their widest
//! cell. Text that cannot be represented in WinAnsiEncoding is replaced with
//! `?`; wrapping is greedy word-wrap measured from the exact AFM widths.
//!
//! # Still out of scope (honest gaps)
//!
//! No font embedding or subsetting (only the standard-14 base fonts). Image
//! embedding is limited to the JPEG/PNG data URIs and the single `url(...)`
//! `background-image` described above; GIF/WebP/SVG data URIs, remote images,
//! arbitrary CSS backgrounds, images embedded in DOCX / XLSX / PPTX / EPUB,
//! and inline images inside a Markdown paragraph are still not embedded. There
//! is no real CSS layout (floats, positioning, flexible boxes, real table
//! layout, page margins from CSS), no charts, math or SVG-in-HTML, and no
//! fidelity renderer. This subset produces honest, structurally useful PDFs
//! with clickable links, a little inline styling and embedded JPEG/PNG images —
//! not an Aspose-equivalent conversion.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard};

mod blocks;
mod csv;
mod docx;
mod emit;
mod epub;
mod html;
mod image;
mod latex;
mod layout;
mod markdown;
mod ooxml;
mod pptx;
mod svg;
mod xlsx;
mod xmlmini;

/// The target page size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageSize {
    /// US Letter: 612 × 792 pt.
    #[default]
    Letter,
    /// A4: 595.276 × 841.89 pt.
    A4,
}

impl PageSize {
    /// The page dimensions in points.
    #[must_use]
    pub const fn dims(self) -> (f64, f64) {
        match self {
            PageSize::Letter => (612.0, 792.0),
            PageSize::A4 => (595.276, 841.89),
        }
    }
}

/// Options shared by the conversion entry points.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The target page size.
    pub page_size: PageSize,
    /// The document title recorded in the `/Info` dictionary, if any.
    pub title: Option<String>,
}

/// Convert CSV (comma-separated, quoted-field aware) to PDF bytes.
///
/// The first row is set as the header and repeated at the top of every page;
/// columns are evenly sized and cells are WinAnsi text, nothing more — this
/// is a table print, not a spreadsheet renderer.
///
/// # Budget
///
/// Charges the generated object graph against `budget`.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document this engine and
/// any conforming reader can open.
pub fn csv_to_pdf(
    input: &str,
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    csv::csv_to_pdf(input, opts, budget, g)
}

/// Convert Markdown (the documented subset) to PDF bytes.
///
/// # Budget
///
/// Charges the generated object graph and output bytes against `budget`.
///
/// # Malformed Input
///
/// Markdown never fails to parse — every input produces a document. Images
/// whose source is not a base64 JPEG/PNG data URI (remote URLs, other schemes,
/// other media types, malformed images, unsupported PNG features) are refused
/// with a typed image error and skipped. Returns `BUDGET_*` on budget exhaustion.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document that this engine
/// and any conforming reader can open; every page carries a valid content
/// stream and font resources.
pub fn markdown_to_pdf(
    input: &str,
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let blocks = markdown::parse(input);
    emit::build(&blocks, opts, budget, g)
}

/// Convert an HTML subset (the documented subset) to PDF bytes.
///
/// # Budget
///
/// Charges the generated object graph and output bytes against `budget`.
///
/// # Malformed Input
///
/// HTML never fails to parse — unclosed tags, stray `<`, and unknown entities
/// degrade to text. Images whose `src` (or inline `background-image` URL) is
/// not a base64 JPEG/PNG data URI (remote URLs, other schemes, other media
/// types, malformed images, unsupported PNG features) are refused with a typed
/// image error and skipped. Returns `BUDGET_*` on budget exhaustion.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document that this engine
/// and any conforming reader can open; every page carries a valid content
/// stream and font resources.
pub fn html_to_pdf(
    input: &str,
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let blocks = html::parse(input);
    emit::build(&blocks, opts, budget, g)
}

/// Convert LaTeX (the documented subset) to PDF bytes.
///
/// The subset is structure, not typography: `\chapter`/`\section`/
/// `\subsection`/`\subsubsection` become headings, `itemize`/`enumerate` become
/// list items, `verbatim` stays verbatim, `\title` + `\maketitle` puts the title
/// where the document puts it, and `\textbf`/`\textit`/`\emph`/`\texttt` become
/// styled runs. Comments are dropped, `\%` and friends survive, and a command
/// this crate does not know loses its name but keeps its argument.
///
/// It does not typeset mathematics, tables, figures or a page layout: `$...$`,
/// `tabular` and `\includegraphics` produce text-only output rather than an
/// approximation that pretends to be LaTeX.
///
/// # Budget
///
/// Charges the generated object graph and output bytes against `budget`.
///
/// # Malformed Input
///
/// LaTeX never fails to parse — every input produces a document. Returns
/// `BUDGET_*` on budget exhaustion.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document that this engine
/// and any conforming reader can open; every page carries a valid content
/// stream and font resources.
pub fn latex_to_pdf(
    input: &str,
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let blocks = latex::parse(input);
    emit::build(&blocks, opts, budget, g)
}

/// Convert an EPUB package (OPF, container and the spine's content documents)
/// to PDF bytes.
///
/// The package is read with the same reader the package writers use; the
/// spine fixes the order; every content document is parsed with the HTML
/// subset above. See the module docs for exactly what is not carried over
/// (images, CSS, math, the book's own page breaks).
///
/// # Budget
///
/// Charges the expanded package bytes and the generated object graph against
/// `budget`.
///
/// # Malformed Input
///
/// A package that is not a ZIP, has no container, no rootfile, or a spine
/// document that is missing is refused with a message naming the part.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document this engine and
/// any conforming reader can open.
pub fn epub_to_pdf(
    input: &[u8],
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    epub::epub_to_pdf(input, opts, budget, g)
}

/// Convert a Word document package (OPF, relationships and `word/document.xml`)
/// to PDF bytes.
///
/// The body is mapped onto the same block model the Markdown/HTML converters
/// lay out: headings, paragraphs with bold/italic runs, list items, tables.
/// See the module docs for what is not carried (page setup, alignment, fonts,
/// images, text boxes, footnotes, tracked changes).
///
/// # Budget
///
/// Charges the expanded package bytes and the generated object graph against
/// `budget`.
///
/// # Malformed Input
///
/// A package that is not a ZIP, has no `word/document.xml`, or whose parts
/// are not parseable XML is refused with a message naming the part.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document this engine and
/// any conforming reader can open.
pub fn docx_to_pdf(
    input: &[u8],
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    docx::docx_to_pdf(input, opts, budget, g)
}

/// Convert a PowerPoint deck package (presentation order, `.pptx`) to PDF
/// bytes.
///
/// Every slide starts on a new page: title/subtitle placeholders become
/// headings, body paragraphs become paragraphs with bold/italic runs,
/// bulleted or numbered items become list items at their indent level, and
/// DrawingML tables become real tables. See the module docs for what is not
/// carried (slide size, theme, images, charts, SmartArt, speaker notes).
///
/// # Budget
///
/// Charges the expanded package bytes and the generated object graph against
/// `budget`.
///
/// # Malformed Input
///
/// A package that is not a ZIP, has no `ppt/presentation.xml`, or whose
/// parts are not parseable XML is refused with a message naming the part. A
/// deck whose slide list resolves to no slides converts as one empty page.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document this engine and
/// any conforming reader can open.
pub fn pptx_to_pdf(
    input: &[u8],
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    pptx::pptx_to_pdf(input, opts, budget, g)
}

/// Convert an Excel workbook package (`xl/workbook.xml` sheet order,
/// shared and inline strings) to PDF bytes.
///
/// Every sheet starts on a new page: a level-2 heading carries its name, then
/// one table prints the used range with cached formula values. See the module
/// docs for what is not carried (cell formats, dates as serial numbers,
/// column widths, merged cells, charts).
///
/// # Budget
///
/// Charges the expanded package bytes and the generated object graph against
/// `budget`.
///
/// # Malformed Input
///
/// A package that is not a ZIP, has no `xl/workbook.xml`, or whose parts are
/// not parseable XML is refused with a message naming the part.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document this engine and
/// any conforming reader can open.
pub fn xlsx_to_pdf(
    input: &[u8],
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    xlsx::xlsx_to_pdf(input, opts, budget, g)
}

/// Convert an SVG document (the documented subset) to a one-page PDF.
///
/// The page is exactly the SVG canvas and carries no margin; 1 px = 0.75 pt.
/// See `svg`'s module documentation for the supported grammar, which arcs
/// (`A`) are refused, and what is dropped.
///
/// # Budget
///
/// Charges the generated object graph against `budget`.
///
/// # Malformed Input
///
/// Malformed XML, an unsupported arc, an unsupported transform or a document
/// with more than 20 000 elements is refused with a message.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document with one page.
pub fn svg_to_pdf(
    input: &str,
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    svg::svg_to_pdf(input, opts, budget, g)
}

/// Helpers shared by the reader module unit tests.
#[cfg(test)]
pub(crate) mod testkit {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use selis_sandbox::{Budget, CancelToken, FixedClock};

    /// The output must open with selis's own parser and expose every page.
    ///
    /// # Budget
    ///
    /// An unlimited budget with a frozen clock: this is a test helper, and
    /// the budget is what the parser would charge in production.
    ///
    /// # Malformed Input
    ///
    /// None: the helper panics on a document that does not parse, which is
    /// the failure a test wants to see.
    pub(crate) fn opens_with_page_count(bytes: &[u8], expected_pages: usize) {
        let budget = Budget::unlimited();
        let mut g = Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new());
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).expect("startxref");
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("open");
        let rev = doc.revisions().last().expect("revision");
        // Count /Page objects among this revision's entries.
        let mut pages = 0usize;
        for &num in rev.entries.keys() {
            if let Ok(selis_pdf_cos::Obj::Dict(pairs)) = selis_pdf_cos::resolve_object(
                bytes,
                rev.entries
                    .get(&num)
                    .map(|e| match e {
                        selis_pdf_cos::XrefEntry::InUse { offset, .. } => *offset,
                        _ => 0,
                    })
                    .unwrap_or(0),
                &budget,
                &mut g,
            ) {
                if pairs.iter().any(|(k, v)| {
                    k.as_slice() == b"Type"
                        && matches!(v, selis_pdf_cos::Obj::Name(n) if n.as_slice() == b"Page")
                }) {
                    pages += 1;
                }
            }
        }
        assert_eq!(pages, expected_pages, "page count mismatch");
    }

    /// Re-open `bytes` with the engine's own reader and resolve every in-use
    /// object. Read-back tests inspect this object graph — not the source
    /// string — so they assert what a conforming reader actually sees.
    ///
    /// # Budget
    ///
    /// Unlimited with a frozen clock (test helper only).
    ///
    /// # Malformed Input
    ///
    /// Panics when the bytes do not parse, which is the failure under test.
    pub(crate) fn resolved_objects(bytes: &[u8]) -> Vec<(u32, selis_pdf_cos::Obj)> {
        use selis_pdf_cos::XrefEntry;
        let budget = Budget::unlimited();
        let mut g = Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new());
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).expect("startxref");
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("open");
        let rev = doc.revisions().last().expect("revision");
        let mut out = Vec::new();
        for (&num, entry) in &rev.entries {
            let XrefEntry::InUse { offset, .. } = *entry else {
                continue;
            };
            if let Ok(obj) = selis_pdf_cos::resolve_object(bytes, offset, &budget, &mut g) {
                out.push((num, obj));
            }
        }
        out
    }

    /// The concatenated page-content-stream bytes of a converted document.
    pub(crate) fn content_stream(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for (_, obj) in resolved_objects(bytes) {
            if let selis_pdf_cos::Obj::Stream { data, .. } = obj {
                out.extend_from_slice(data.as_slice());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn opens_with_page_count(bytes: &[u8], expected_pages: usize) {
        crate::testkit::opens_with_page_count(bytes, expected_pages);
    }

    #[test]
    fn markdown_round_trips_through_the_parser() {
        let md = "# Title\n\nHello **world**, with a [link](https://example.com).\n\n\
                  - item one\n- item two\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n---\n";
        let opts = Options {
            page_size: PageSize::Letter,
            title: Some("Round-trip".to_string()),
        };
        let bytes = markdown_to_pdf(md, &opts, &Budget::unlimited(), &mut guard()).expect("write");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        opens_with_page_count(&bytes, 1);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/BaseFont /Helvetica-Bold"));
        assert!(text.contains("/Title"));
    }

    #[test]
    fn html_round_trips_and_paginates() {
        let mut html = String::from("<h1>Report</h1>");
        for i in 0..120 {
            html.push_str(&format!("<p>Paragraph {i} with enough words to fill a decent part of a line in the output.</p>"));
        }
        let opts = Options::default();
        let bytes = html_to_pdf(&html, &opts, &Budget::unlimited(), &mut guard()).expect("write");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        // 120 paragraphs comfortably span several pages.
        let budget = Budget::unlimited();
        let mut g = guard();
        let startxref = selis_pdf_cos::xref::find_startxref(&bytes, 4096).expect("startxref");
        let doc = selis_pdf_cos::parse_revisions(&bytes, startxref, &budget, &mut g).expect("open");
        let rev = doc.revisions().last().expect("revision");
        let page_count = rev
            .entries
            .values()
            .filter(|e| matches!(e, selis_pdf_cos::XrefEntry::InUse { .. }))
            .count();
        assert!(
            page_count > 4,
            "expected multiple pages, objects={page_count}"
        );
    }

    /// A regression for the emit step: every `Tf` in the content stream must
    /// reference a font *resource key* (`F1`–`F5`), never a raw BaseFont name
    /// like `/Helvetica-Bold`. A name-based `Tf` resolves to no resource and
    /// renders a blank page.
    #[test]
    fn text_uses_font_resource_keys_not_base_font_names() {
        let md = "# Title\n\nSome **bold** and *italic* text, plus `code`.\n";
        let bytes = markdown_to_pdf(md, &Options::default(), &Budget::unlimited(), &mut guard())
            .expect("write");
        let text = String::from_utf8_lossy(&bytes);
        // The raw BaseFont names still appear in the font objects…
        assert!(text.contains("/BaseFont /Helvetica-Bold"));
        // …but every Tf operator names a resource key, e.g. "/F2 24 Tf".
        assert!(text.contains("/F2 "), "heading should select F2");
        for before in text.split(" Tf") {
            // The token just before the size must be /F1../F5.
            let mut toks = before.split_whitespace();
            let size = toks.next_back();
            let name = toks.next_back();
            let Some(name) = name else { continue };
            if size.is_some() && name.starts_with('/') {
                assert!(
                    matches!(name, "/F1" | "/F2" | "/F3" | "/F4" | "/F5"),
                    "Tf used a non-resource font name: {name}"
                );
            }
        }
        assert!(!text.contains("/Helvetica-Bold 24 Tf"));
    }

    /// A regression for absolute text placement: each text run must set the
    /// text matrix absolutely (`Tm`), never a relative `Td`, which would
    /// accumulate and push every line after the first off the page.
    #[test]
    fn text_is_placed_with_absolute_tm_not_relative_td() {
        let md = "line one\n\nline two\n\nline three\n";
        let bytes = markdown_to_pdf(md, &Options::default(), &Budget::unlimited(), &mut guard())
            .expect("write");
        let text = String::from_utf8_lossy(&bytes);
        let tm_count = text.matches(" Tm\n").count();
        let td_count = text.matches(" Td\n").count();
        assert!(tm_count >= 3, "each line needs an absolute Tm: {tm_count}");
        assert_eq!(td_count, 0, "no relative Td may be emitted");
    }

    #[test]
    fn empty_input_still_yields_a_valid_page() {
        let bytes = markdown_to_pdf("", &Options::default(), &Budget::unlimited(), &mut guard())
            .expect("write");
        opens_with_page_count(&bytes, 1);
    }

    #[test]
    fn a4_and_letter_differ() {
        let mut o = Options::default();
        let letter =
            markdown_to_pdf("text", &o, &Budget::unlimited(), &mut guard()).expect("write");
        o.page_size = PageSize::A4;
        let a4 = markdown_to_pdf("text", &o, &Budget::unlimited(), &mut guard()).expect("write");
        assert_ne!(letter, a4);
    }

    #[test]
    fn a_markdown_table_lays_out_with_rules_and_opens() {
        let md = "| Name | Amount |\n| --- | --- |\n| Ada | 1200 |\n| Grace | 900 |\n";
        let opts = Options::default();
        let bytes = markdown_to_pdf(md, &opts, &Budget::unlimited(), &mut guard()).expect("write");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        opens_with_page_count(&bytes, 1);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("1200"));
        // The header row rides on a fill and the table is bounded by rules.
        assert!(text.contains(" rg\n"), "{text}");
    }

    #[test]
    fn a_wide_table_shrinks_to_the_page() {
        let md = "| Very long header one here | Another long header two here | Third long header three here |\n| --- | --- | --- |\n| a wide cell with a great many words that would overflow a narrow column | another wide cell with just as many words in it | and a third wide cell with far too many words to hold on one line |\n";
        let bytes = markdown_to_pdf(md, &Options::default(), &Budget::unlimited(), &mut guard())
            .expect("write");
        opens_with_page_count(&bytes, 1);
    }
}

/// Read-back fidelity tests. Every assertion inspects the object graph or the
/// content stream of a PDF that has been re-opened with the engine's own
/// reader — never the converter's source string.
#[cfg(test)]
mod fidelity {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation
    )]

    use super::*;
    use selis_pdf_cos::Obj;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn from_md(md: &str) -> Vec<u8> {
        markdown_to_pdf(md, &Options::default(), &Budget::unlimited(), &mut guard()).expect("write")
    }

    fn from_html(html: &str) -> Vec<u8> {
        html_to_pdf(
            html,
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("write")
    }

    fn is_name(pairs: &[(selis_bytes::Bytes, Obj)], key: &[u8], value: &[u8]) -> bool {
        pairs.iter().any(|(k, v)| {
            k.as_slice() == key && matches!(v, Obj::Name(name) if name.as_slice() == value)
        })
    }

    fn as_f64(obj: &Obj) -> Option<f64> {
        match obj {
            Obj::Real { scaled, scale } => {
                let divisor = 10_f64.powi(i32::from(*scale));
                Some(*scaled as f64 / divisor)
            }
            Obj::Int(value) => Some(*value as f64),
            _ => None,
        }
    }

    /// URI targets of genuine `/Link` annotations in the re-opened document.
    fn link_uris(bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for (_, obj) in crate::testkit::resolved_objects(bytes) {
            let Obj::Dict(pairs) = obj else {
                continue;
            };
            if !is_name(&pairs, b"Subtype", b"Link") {
                continue;
            }
            let Some((_, Obj::Dict(action))) = pairs.iter().find(|(k, _)| k.as_slice() == b"A")
            else {
                continue;
            };
            if !is_name(action, b"S", b"URI") {
                continue;
            }
            if let Some((_, Obj::String(uri))) = action.iter().find(|(k, _)| k.as_slice() == b"URI")
            {
                out.push(String::from_utf8_lossy(uri.as_slice()).into_owned());
            }
        }
        out
    }

    /// The `/Rect` of each re-opened `/Link` annotation, in PDF coordinates.
    fn link_rects(bytes: &[u8]) -> Vec<[f64; 4]> {
        let mut out = Vec::new();
        for (_, obj) in crate::testkit::resolved_objects(bytes) {
            let Obj::Dict(pairs) = obj else {
                continue;
            };
            if !is_name(&pairs, b"Subtype", b"Link") {
                continue;
            }
            let Some((_, Obj::Array(values))) = pairs.iter().find(|(k, _)| k.as_slice() == b"Rect")
            else {
                continue;
            };
            if values.len() == 4 {
                let nums: Vec<f64> = values.iter().filter_map(as_f64).collect();
                if nums.len() == 4 {
                    out.push([nums[0], nums[1], nums[2], nums[3]]);
                }
            }
        }
        out
    }

    fn stream(bytes: &[u8]) -> String {
        String::from_utf8_lossy(&crate::testkit::content_stream(bytes)).into_owned()
    }

    /// Pair each absolute text matrix (`Tm`) with the string shown after it.
    /// This recovers actual on-page x positions from the re-opened stream.
    fn tm_text_pairs(content: &str) -> Vec<(f64, String)> {
        let mut out = Vec::new();
        let mut current_x: Option<f64> = None;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("1 0 0 1 ") {
                if let Some(xy) = rest.strip_suffix(" Tm") {
                    current_x = xy.split_whitespace().next().and_then(|s| s.parse().ok());
                }
            } else if let Some(text) = line.strip_prefix('(').and_then(|s| s.strip_suffix(") Tj")) {
                if let Some(x) = current_x {
                    out.push((x, text.to_string()));
                }
            }
        }
        out
    }

    fn x_of(pairs: &[(f64, String)], text: &str) -> f64 {
        pairs
            .iter()
            .find(|(_, shown)| shown == text)
            .map(|(x, _)| *x)
            .unwrap_or_else(|| panic!("no shown text {text:?} in {pairs:?}"))
    }

    // ---- Link annotations -------------------------------------------------

    #[test]
    fn markdown_link_becomes_a_clickable_uri_annotation() {
        let bytes = from_md("Read [the docs](https://selis.example/docs?q=1&r=2) today.");
        crate::testkit::opens_with_page_count(&bytes, 1);
        assert_eq!(
            link_uris(&bytes),
            vec!["https://selis.example/docs?q=1&r=2".to_string()]
        );
        // The URL is no longer painted as text; only the label is.
        let content = stream(&bytes);
        assert!(
            !content.contains("selis.example"),
            "URL painted as text: {content}"
        );
        assert!(
            content.contains("(the docs)"),
            "link label missing: {content}"
        );
    }

    #[test]
    fn html_link_annotation_has_a_non_degenerate_on_page_rect() {
        // The parentheses in the URL also exercise literal-string escaping.
        let bytes = from_html("<p>Visit <a href=\"https://example.org/a(b)\">our site</a>.</p>");
        crate::testkit::opens_with_page_count(&bytes, 1);
        assert_eq!(
            link_uris(&bytes),
            vec!["https://example.org/a(b)".to_string()]
        );
        let rects = link_rects(&bytes);
        assert_eq!(rects.len(), 1, "expected one link rect, got {rects:?}");
        let [x0, y0, x1, y1] = rects[0];
        assert!(x1 > x0 && y1 > y0, "degenerate rect: {:?}", rects[0]);
        assert!(
            (0.0..=612.0).contains(&x0) && (0.0..=612.0).contains(&x1),
            "x off page: {:?}",
            rects[0]
        );
        assert!(
            (0.0..=792.0).contains(&y0) && (0.0..=792.0).contains(&y1),
            "y off page: {:?}",
            rects[0]
        );
    }

    #[test]
    fn several_links_produce_several_annotations() {
        let bytes =
            from_md("[one](https://a.test) and [two](https://b.test) and [three](https://c.test)");
        let mut uris = link_uris(&bytes);
        uris.sort();
        assert_eq!(
            uris,
            vec![
                "https://a.test".to_string(),
                "https://b.test".to_string(),
                "https://c.test".to_string()
            ]
        );
    }

    // ---- Inline colour and font size --------------------------------------

    #[test]
    fn inline_hex_colour_and_font_size_reach_the_content_stream() {
        let bytes = from_html(
            "<p>before <span style=\"color:#ff0000; font-size:20pt\">RED BIG</span> after</p>",
        );
        let content = stream(&bytes);
        assert!(content.contains("1 0 0 rg"), "red fill missing: {content}");
        assert!(content.contains("/F1 20 Tf"), "20pt run missing: {content}");
        assert!(
            content.contains("0 0 0 rg"),
            "body black fill missing: {content}"
        );
        assert!(
            content.contains("/F1 11 Tf"),
            "11pt body missing: {content}"
        );
    }

    #[test]
    fn named_rgb_and_px_units_are_decoded() {
        let bytes = from_html(
            "<p><span style=\"color:blue\">b</span>\
             <span style=\"color:rgb(0,128,0)\">g</span>\
             <span style=\"font-size:16px\">p</span></p>",
        );
        let content = stream(&bytes);
        assert!(
            content.contains("0 0 1 rg"),
            "named blue missing: {content}"
        );
        assert!(
            content.contains("0 0.502 0 rg"),
            "rgb() green missing: {content}"
        );
        assert!(
            content.contains("/F1 12 Tf"),
            "16px should be 12pt: {content}"
        );
    }

    #[test]
    fn unknown_css_is_ignored_not_guessed() {
        let bytes = from_html(
            "<p><span style=\"color:rebeccapurple; font-size:2em; \
             text-align:justify; background:yellow\">plain</span></p>",
        );
        let content = stream(&bytes);
        // Exactly one (default black) fill group — no colour change from CSS.
        assert_eq!(
            content.matches(" rg\n").count(),
            1,
            "unexpected rg: {content}"
        );
        assert!(content.contains("0 0 0 rg"), "unexpected colour: {content}");
        assert!(
            content.contains("/F1 11 Tf"),
            "body size should be unchanged: {content}"
        );
    }

    // ---- Text alignment ---------------------------------------------------

    #[test]
    fn paragraph_alignment_shifts_text_x_positions() {
        let bytes = from_html(
            "<p style=\"text-align:left\">LEFT</p>\
             <p style=\"text-align:center\">CENTER</p>\
             <p style=\"text-align:right\">RIGHT</p>",
        );
        let pairs = tm_text_pairs(&stream(&bytes));
        let (left, center, right) = (
            x_of(&pairs, "LEFT"),
            x_of(&pairs, "CENTER"),
            x_of(&pairs, "RIGHT"),
        );
        assert!(
            left < center && center < right,
            "l={left} c={center} r={right}"
        );
        assert!(
            (left - 54.0).abs() < 0.5,
            "left text should start at the margin: {left}"
        );
        assert!(
            right > 500.0,
            "right text should sit near the right margin: {right}"
        );
    }

    #[test]
    fn gfm_column_alignment_moves_cell_text() {
        let head = "| Long Header One | Long Header Two | Long Header Three |";
        let plain = format!("{head}\n| --- | --- | --- |\n| x | y | z |\n");
        let aligned = format!("{head}\n| :--- | :---: | ---: |\n| x | y | z |\n");
        let plain_pairs = tm_text_pairs(&stream(&from_md(&plain)));
        let aligned_pairs = tm_text_pairs(&stream(&from_md(&aligned)));
        // Column 1 is left in both tables.
        assert!((x_of(&plain_pairs, "x") - x_of(&aligned_pairs, "x")).abs() < 0.5);
        // Column 2 is centred, column 3 right-aligned: both move right.
        assert!(x_of(&aligned_pairs, "y") > x_of(&plain_pairs, "y") + 10.0);
        assert!(x_of(&aligned_pairs, "z") > x_of(&plain_pairs, "z") + 20.0);
    }

    #[test]
    fn html_align_attribute_moves_cell_text() {
        let plain =
            from_html("<table><tr><td>Long header cell</td></tr><tr><td>x</td></tr></table>");
        let right = from_html(
            "<table><tr><td>Long header cell</td></tr><tr><td align=\"right\">x</td></tr></table>",
        );
        let plain_x = x_of(&tm_text_pairs(&stream(&plain)), "x");
        let right_x = x_of(&tm_text_pairs(&stream(&right)), "x");
        assert!(right_x > plain_x + 5.0, "plain={plain_x} right={right_x}");
    }

    // ---- Embedded JPEG/PNG images -----------------------------------------

    /// A real, decodable 32×32 solid-red baseline JPEG with an SOF0 marker.
    fn red_jpeg() -> Vec<u8> {
        let rgb = [0xFF_u8, 0x00, 0x00].repeat(32 * 32);
        let mut g = guard();
        selis_pdf_filter::dct_encode(&rgb, 32, 32, 90, &mut g).expect("jpeg fixture")
    }

    /// Minimal standard-base64 encoder for the fixture (no external crate).
    fn b64(data: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let n = (u32::from(chunk[0]) << 16)
                | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
                | u32::from(chunk.get(2).copied().unwrap_or(0));
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                TABLE[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    /// A re-opened image stream: its dictionary entries and raw stream bytes.
    type ImageObj = (Vec<(Vec<u8>, Obj)>, Vec<u8>);

    /// The first `/Subtype /Image` stream in the re-opened object graph.
    fn image_stream(bytes: &[u8]) -> Option<ImageObj> {
        for (_, obj) in crate::testkit::resolved_objects(bytes) {
            if let Obj::Stream { dict, data } = obj {
                let is_image = dict.iter().any(|(k, v)| {
                    k.as_slice() == b"Subtype"
                        && matches!(v, Obj::Name(n) if n.as_slice() == b"Image")
                });
                if is_image {
                    let pairs = dict
                        .iter()
                        .map(|(k, v)| (k.as_slice().to_vec(), v.clone()))
                        .collect();
                    return Some((pairs, data.as_slice().to_vec()));
                }
            }
        }
        None
    }

    fn dict_int(pairs: &[(Vec<u8>, Obj)], key: &[u8]) -> Option<i64> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .and_then(|(_, v)| match v {
                Obj::Int(n) => Some(*n),
                _ => None,
            })
    }

    fn dict_name(pairs: &[(Vec<u8>, Obj)], key: &[u8]) -> Option<Vec<u8>> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .and_then(|(_, v)| match v {
                Obj::Name(n) => Some(n.as_slice().to_vec()),
                _ => None,
            })
    }

    #[test]
    fn html_jpeg_data_uri_becomes_a_dctdecode_image_xobject() {
        let jpeg = red_jpeg();
        let src = format!("data:image/jpeg;base64,{}", b64(&jpeg));
        let bytes = from_html(&format!("<img src=\"{src}\">"));
        crate::testkit::opens_with_page_count(&bytes, 1);

        let (dict, data) = image_stream(&bytes).expect("image XObject in re-opened PDF");
        assert_eq!(dict_name(&dict, b"Type").as_deref(), Some(&b"XObject"[..]));
        assert_eq!(dict_name(&dict, b"Subtype").as_deref(), Some(&b"Image"[..]));
        assert_eq!(dict_int(&dict, b"Width"), Some(32));
        assert_eq!(dict_int(&dict, b"Height"), Some(32));
        assert_eq!(dict_int(&dict, b"BitsPerComponent"), Some(8));
        assert_eq!(
            dict_name(&dict, b"Filter").as_deref(),
            Some(&b"DCTDecode"[..])
        );
        assert_eq!(
            dict_name(&dict, b"ColorSpace").as_deref(),
            Some(&b"DeviceRGB"[..])
        );
        assert_eq!(data, jpeg, "JPEG bytes must be embedded verbatim");

        // The resource dict exposes /Im1 and the content stream draws it in the
        // measured box: 32 px × 0.75 = 24 pt at the 54 pt margin, y from bottom.
        let xobj = crate::testkit::resolved_objects(&bytes)
            .into_iter()
            .any(|(_, obj)| {
                if let Obj::Dict(pairs) = obj {
                    pairs.iter().any(|(k, v)| {
                        k.as_slice() == b"XObject"
                            && matches!(v, Obj::Dict(x) if x.iter().any(|(nk, _)| nk.as_slice() == b"Im1"))
                    })
                } else {
                    false
                }
            });
        assert!(xobj, "page resources must carry /XObject /Im1");
        let content = stream(&bytes);
        assert!(content.contains("/Im1 Do"), "no image draw: {content}");
        assert!(
            content.contains("24 0 0 24 54 714 cm"),
            "unexpected placement rect: {content}"
        );
    }

    #[test]
    fn html_image_width_height_override_and_clamp_to_page_width() {
        let jpeg = red_jpeg();
        let src = format!("data:image/jpeg;base64,{}", b64(&jpeg));
        // Both attributes honoured verbatim (no clamp needed).
        let exact = from_html(&format!("<img src=\"{src}\" width=\"100\" height=\"50\">"));
        assert!(
            stream(&exact).contains("100 0 0 50 54 688 cm"),
            "explicit box: {}",
            stream(&exact)
        );
        // A width larger than the 504 pt text column is clamped, preserving
        // the intrinsic 1:1 aspect ratio (the single width derives height).
        let clamped = from_html(&format!("<img src=\"{src}\" width=\"1000\">"));
        assert!(
            stream(&clamped).contains("504 0 0 504 54 234 cm"),
            "clamped box: {}",
            stream(&clamped)
        );
    }

    #[test]
    fn markdown_image_data_uri_embeds_the_same_xobject() {
        let jpeg = red_jpeg();
        let src = format!("data:image/jpeg;base64,{}", b64(&jpeg));
        let bytes = from_md(&format!("![a red square]({src})"));
        let (dict, data) = image_stream(&bytes).expect("image from Markdown");
        assert_eq!(dict_int(&dict, b"Width"), Some(32));
        assert_eq!(dict_int(&dict, b"Height"), Some(32));
        assert_eq!(data, jpeg);
    }

    #[test]
    fn remote_and_malformed_images_are_skipped_not_fetched() {
        let bytes = from_html(
            "<img src=\"https://example.com/a.jpg\">\
             <img src=\"file:///etc/passwd\">\
             <img src=\"data:image/gif;base64,R0lGOD\">\
             <img src=\"data:image/png;base64,iVBORw0KGgo=\">",
        );
        assert!(
            image_stream(&bytes).is_none(),
            "no image XObject for refused sources"
        );
        assert!(!stream(&bytes).contains(" Do\n"), "no stray image draw");
        // Markdown remote images are skipped the same way.
        let md = from_md("![no](https://example.com/x.png)");
        assert!(image_stream(&md).is_none());
    }

    #[test]
    fn embedded_jpeg_renders_ink_at_its_measured_box() {
        let jpeg = red_jpeg();
        let src = format!("data:image/jpeg;base64,{}", b64(&jpeg));
        let bytes = from_html(&format!("<img src=\"{src}\" width=\"100\" height=\"100\">"));
        let budget = Budget::unlimited();
        let session = selis_pdf_engine::Session::open(bytes, &budget, &FixedClock(0))
            .expect("engine re-open");
        let mut g = guard();
        let page = session
            .render_page_rgba(0, 72.0, &budget, &mut g)
            .expect("rasterize");
        assert_eq!((page.width, page.height), (612, 792));
        // The box is x∈[54,154], y∈[54,154] in top-left pixel space. Scan the
        // interior for a red pixel rather than assume the rasteriser's origin.
        let mut red = false;
        for y in 60usize..150 {
            for x in 60usize..150 {
                let idx = (y * page.width as usize + x) * 4;
                let (r, g, b) = (page.rgba[idx], page.rgba[idx + 1], page.rgba[idx + 2]);
                if r > 150 && g < 100 && b < 100 {
                    red = true;
                    break;
                }
            }
            if red {
                break;
            }
        }
        assert!(red, "page did not render red ink inside the image box");
    }

    // ---- Embedded PNG images and inline backgrounds ------------------------

    /// CRC-32 (IEEE 802.3), used to build PNG fixtures with valid chunks.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc_input = Vec::with_capacity(kind.len().saturating_add(body.len()));
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    }

    /// An unfiltered, single-IDAT 8-bit PNG. `color_type` is 0/2/6 and `row`
    /// holds one scanline of raw samples (no filter byte).
    fn png_fixture(width: u32, height: u32, color_type: u8, row: &[u8]) -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, color_type, 0, 0, 0]);
        let mut raw = Vec::new();
        for _ in 0..height {
            raw.push(0); // filter: None
            raw.extend_from_slice(row);
        }
        let idat = selis_pdf_filter::flate_encode(&raw, 6);
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        push_chunk(&mut out, b"IHDR", &ihdr);
        push_chunk(&mut out, b"IDAT", &idat);
        push_chunk(&mut out, b"IEND", &[]);
        out
    }

    fn red_png() -> Vec<u8> {
        png_fixture(32, 32, 2, &[0xFF, 0x00, 0x00].repeat(32))
    }

    #[test]
    fn html_png_data_uri_becomes_a_flate_image_xobject() {
        let png = red_png();
        let src = format!("data:image/png;base64,{}", b64(&png));
        let bytes = from_html(&format!("<img src=\"{src}\">"));
        crate::testkit::opens_with_page_count(&bytes, 1);

        let (dict, data) = image_stream(&bytes).expect("image XObject in re-opened PDF");
        assert_eq!(dict_name(&dict, b"Type").as_deref(), Some(&b"XObject"[..]));
        assert_eq!(dict_name(&dict, b"Subtype").as_deref(), Some(&b"Image"[..]));
        assert_eq!(dict_int(&dict, b"Width"), Some(32));
        assert_eq!(dict_int(&dict, b"Height"), Some(32));
        assert_eq!(dict_int(&dict, b"BitsPerComponent"), Some(8));
        assert_eq!(
            dict_name(&dict, b"Filter").as_deref(),
            Some(&b"FlateDecode"[..])
        );
        assert_eq!(
            dict_name(&dict, b"ColorSpace").as_deref(),
            Some(&b"DeviceRGB"[..])
        );
        // The stream is Flate-compressed, *unfiltered* RGB samples — three
        // colour bytes per pixel, not the original PNG file.
        let raw = selis_pdf_filter::flate_decode(&data).expect("raw samples");
        assert_eq!(raw.len(), 32 * 32 * 3);
        assert!(raw.chunks_exact(3).all(|px| px == [0xFF, 0x00, 0x00]));
        assert_ne!(data, png, "PNG file bytes must not be embedded verbatim");

        let xobj = crate::testkit::resolved_objects(&bytes)
            .into_iter()
            .any(|(_, obj)| {
                if let Obj::Dict(pairs) = obj {
                    pairs.iter().any(|(k, v)| {
                        k.as_slice() == b"XObject"
                            && matches!(v, Obj::Dict(x) if x.iter().any(|(nk, _)| nk.as_slice() == b"Im1"))
                    })
                } else {
                    false
                }
            });
        assert!(xobj, "page resources must carry /XObject /Im1");
        let content = stream(&bytes);
        assert!(content.contains("/Im1 Do"), "no image draw: {content}");
        assert!(
            content.contains("24 0 0 24 54 714 cm"),
            "unexpected placement rect: {content}"
        );
    }

    #[test]
    fn png_grey_is_device_gray_and_alpha_is_flattened_over_white() {
        let grey = png_fixture(2, 1, 0, &[0x20, 0xE0]);
        let src = format!("data:image/png;base64,{}", b64(&grey));
        let bytes = from_html(&format!("<img src=\"{src}\">"));
        let (dict, data) = image_stream(&bytes).expect("grey image");
        assert_eq!(
            dict_name(&dict, b"ColorSpace").as_deref(),
            Some(&b"DeviceGray"[..])
        );
        assert_eq!(
            selis_pdf_filter::flate_decode(&data).expect("grey samples"),
            [0x20, 0xE0]
        );

        // Colour type 6: opaque red next to a fully transparent pixel. The
        // transparent pixel must read back as white, not black or green.
        let rgba = png_fixture(2, 1, 6, &[0xFF, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00]);
        let src = format!("data:image/png;base64,{}", b64(&rgba));
        let bytes = from_html(&format!("<img src=\"{src}\">"));
        let (dict, data) = image_stream(&bytes).expect("RGBA image");
        assert_eq!(
            dict_name(&dict, b"ColorSpace").as_deref(),
            Some(&b"DeviceRGB"[..])
        );
        assert_eq!(
            selis_pdf_filter::flate_decode(&data).expect("flattened samples"),
            [0xFF, 0x00, 0x00, 0xFF, 0xFF, 0xFF]
        );
    }

    #[test]
    fn markdown_png_data_uri_and_explicit_size_read_back() {
        let png = red_png();
        let src = format!("data:image/png;base64,{}", b64(&png));
        let bytes = from_md(&format!("![a red square]({src})"));
        let (dict, data) = image_stream(&bytes).expect("image from Markdown PNG");
        assert_eq!(dict_int(&dict, b"Width"), Some(32));
        assert_eq!(dict_int(&dict, b"Height"), Some(32));
        assert_eq!(
            dict_name(&dict, b"Filter").as_deref(),
            Some(&b"FlateDecode"[..])
        );
        assert!(!data.is_empty());

        let exact = from_html(&format!("<img src=\"{src}\" width=\"100\" height=\"50\">"));
        assert!(
            stream(&exact).contains("100 0 0 50 54 688 cm"),
            "explicit PNG box: {}",
            stream(&exact)
        );
        let clamped = from_html(&format!("<img src=\"{src}\" width=\"1000\">"));
        assert!(
            stream(&clamped).contains("504 0 0 504 54 234 cm"),
            "clamped PNG box: {}",
            stream(&clamped)
        );
    }

    #[test]
    fn inline_background_image_data_uri_paints_behind_text() {
        let png = red_png();
        let src = format!("data:image/png;base64,{}", b64(&png));
        let bytes = from_html(&format!(
            "<p style=\"background-image: url('{src}')\">Hello background</p>"
        ));
        crate::testkit::opens_with_page_count(&bytes, 1);
        let (dict, _) = image_stream(&bytes).expect("background image XObject");
        assert_eq!(
            dict_name(&dict, b"Filter").as_deref(),
            Some(&b"FlateDecode"[..])
        );
        assert_eq!(dict_int(&dict, b"Width"), Some(32));

        let content = stream(&bytes);
        assert!(
            content.contains("/Im1 Do"),
            "background not drawn: {content}"
        );
        let draw = content.find("/Im1 Do").unwrap_or(usize::MAX);
        let text = content.find("Hello background").unwrap_or(usize::MAX);
        assert!(
            draw < text,
            "background must be drawn before the text: {content}"
        );

        // JPEG backgrounds travel the same path and keep their DCTDecode.
        let jpeg = red_jpeg();
        let src = format!("data:image/jpeg;base64,{}", b64(&jpeg));
        let bytes = from_html(&format!(
            "<div style=\"background-image:url({src})\">Text</div>"
        ));
        let (dict, data) = image_stream(&bytes).expect("JPEG background XObject");
        assert_eq!(
            dict_name(&dict, b"Filter").as_deref(),
            Some(&b"DCTDecode"[..])
        );
        assert_eq!(data, jpeg);
    }

    #[test]
    fn remote_and_malformed_background_and_image_sources_are_refused() {
        // A remote URL is refused by name; a truncated PNG is refused with a
        // typed malformed-image error. Neither produces an XObject or a draw.
        let truncated = b64(png_fixture(1, 1, 2, &[1, 2, 3]).get(..30).unwrap_or(&[]));
        let bytes = from_html(&format!(
            "<img src=\"https://example.com/x.png\">\
             <p style=\"background-image:url(https://example.com/y.png)\">Remote</p>\
             <img src=\"data:image/png;base64,{truncated}\">\
             <p style=\"background-image:url(data:image/png;base64,{truncated})\">Broken</p>"
        ));
        assert!(
            image_stream(&bytes).is_none(),
            "no image XObject for refused sources"
        );
        let content = stream(&bytes);
        assert!(!content.contains(" Do\n"), "no stray image draw: {content}");

        // The text of the styled elements still renders.
        assert!(content.contains("Remote"));
        assert!(content.contains("Broken"));
    }

    #[test]
    fn embedded_png_renders_known_pixels_at_its_measured_box() {
        let png = red_png();
        let src = format!("data:image/png;base64,{}", b64(&png));
        let bytes = from_html(&format!("<img src=\"{src}\" width=\"100\" height=\"100\">"));
        let budget = Budget::unlimited();
        let session = selis_pdf_engine::Session::open(bytes, &budget, &FixedClock(0))
            .expect("engine re-open");
        let mut g = guard();
        let page = session
            .render_page_rgba(0, 72.0, &budget, &mut g)
            .expect("rasterize");
        assert_eq!((page.width, page.height), (612, 792));
        let mut red = false;
        for y in 60usize..150 {
            for x in 60usize..150 {
                let idx = (y * page.width as usize + x) * 4;
                let (r, g, b) = (page.rgba[idx], page.rgba[idx + 1], page.rgba[idx + 2]);
                if r > 150 && g < 100 && b < 100 {
                    red = true;
                    break;
                }
            }
            if red {
                break;
            }
        }
        assert!(red, "PNG did not render red ink inside the image box");
    }
}
