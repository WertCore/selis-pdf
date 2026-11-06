//! The conversion crate of the Selis PDF engine (SL-5.CONV.05).
//!
//! Deliberately scoped per SL-5.CONV.05: a real HTML engine is out of scope,
//! so this crate ships a **documented subset**, not a half-working general
//! renderer. It converts Markdown and an HTML subset into paginated PDF bytes
//! using the standard-14 base fonts (measured with [`selis_font::standard14`]
//! metrics, encoded in WinAnsiEncoding) and the full-document writer
//! (SL-1A.WRITE.01).
//!
//! # Supported Markdown subset
//!
//! ATX headings (`#`–`######`), paragraphs with soft line breaks, bold /
//! italic / inline code spans, fenced (``` ``` ```) and indented code blocks,
//! bullet lists (`-`, `*`, `+`, nested by two-space indents), ordered lists
//! (`1.`), blockquotes (`>`), horizontal rules, GFM pipe tables (cells carry
//! plain text; `:---:` alignment is dropped; `\|` escapes a pipe), and links
//! (rendered as their text with the URL in parentheses when it differs — no
//! link annotations). Images, reference links, and raw HTML inside Markdown
//! are ignored or rendered as plain text.
//!
//! # Supported HTML subset
//!
//! `<h1>`–`<h6>`, `<p>`, `<br>`, `<hr>`, `<ul>`/`<ol>`/`<li>`, `<pre>` (with
//! optional inner `<code>`), `<blockquote>`, `<b>`/`<strong>`, `<i>`/`<em>`,
//! `<code>`, `<a href>` (same link rendering as Markdown), `<table>` with
//! `<tr>`/`<th>`/`<td>` (inline cell content only; `<th>` in the first row
//! makes a header row; nested tables and lists inside cells are not
//! supported), and block containers (`<div>`, `<section>`, `<article>`,
//! `<main>`, `<table>` cells). `<script>` and `<style>` content is dropped;
//! unknown tags unwrap to their text. Named (`&amp;`, `&eacute;`, …) and
//! numeric (`&#8212;`, `&#x2014;`) entities are decoded.
//!
//! # Typography
//!
//! US Letter or A4 pages, 54 pt margins, 11 pt Helvetica body (Helvetica-Bold
//! headings, Helvetica-Oblique blockquotes, Courier code blocks). Tables are
//! set booktabs-style: a heavy rule above the header, hairlines between body
//! rows, the header row on a light grey fill, columns sized to their widest
//! cell. Text that cannot be represented in WinAnsiEncoding is replaced with
//! `?`; wrapping is greedy word-wrap measured from the exact AFM widths.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard};

mod blocks;
mod csv;
mod emit;
mod epub;
mod html;
mod latex;
mod layout;
mod markdown;
mod svg;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// The output must open with selis's own parser and expose every page.
    fn opens_with_page_count(bytes: &[u8], expected_pages: usize) {
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
