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
//! the label is not automatically underlined or recoloured. Images, reference
//! links, and raw HTML inside Markdown are ignored or rendered as plain text.
//!
//! # Supported HTML subset
//!
//! `<h1>`–`<h6>`, `<p>`, `<br>`, `<hr>`, `<ul>`/`<ol>`/`<li>`, `<pre>` (with
//! optional inner `<code>`), `<blockquote>`, `<b>`/`<strong>`, `<i>`/`<em>`,
//! `<code>`, `<span>`, `<a href>` (link label plus a real URI annotation, as
//! in Markdown), `<table>` with `<tr>`/`<th>`/`<td>` (inline cell content only;
//! `<th>` in the first row makes a header row; nested tables and lists inside
//! cells are not supported), and block containers (`<div>`, `<section>`,
//! `<article>`, `<main>`, `<table>` cells). `<script>` and `<style>` content is
//! dropped; unknown tags unwrap to their text. Named (`&amp;`, `&eacute;`, …)
//! and numeric (`&#8212;`, `&#x2014;`) entities are decoded.
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
//! No font embedding or subsetting (only the standard-14 base fonts), no image
//! embedding, no charts, math or SVG-in-HTML, no full CSS layout (floats,
//! positioning, flexible boxes, real table layout, page margins from CSS), and
//! no fidelity renderer. This subset produces honest, structurally useful
//! PDFs with clickable links and a little inline styling — not an
//! Aspose-equivalent conversion.

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
/// Markdown never fails to parse — every input produces a document. Returns
/// `BUDGET_*` on budget exhaustion.
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
/// degrade to text. Returns `BUDGET_*` on budget exhaustion.
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
}
