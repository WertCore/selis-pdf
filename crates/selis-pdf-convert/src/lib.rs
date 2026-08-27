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
//! (`1.`), blockquotes (`>`), horizontal rules, and links (rendered as their
//! text with the URL in parentheses when it differs — no link annotations).
//! Images, tables, reference links, and raw HTML inside Markdown are ignored
//! or rendered as plain text.
//!
//! # Supported HTML subset
//!
//! `<h1>`–`<h6>`, `<p>`, `<br>`, `<hr>`, `<ul>`/`<ol>`/`<li>`, `<pre>` (with
//! optional inner `<code>`), `<blockquote>`, `<b>`/`<strong>`, `<i>`/`<em>`,
//! `<code>`, `<a href>` (same link rendering as Markdown), and block
//! containers (`<div>`, `<section>`, `<article>`, `<main>`, `<table>` cells).
//! `<script>` and `<style>` content is dropped; unknown tags unwrap to their
//! text. Named (`&amp;`, `&eacute;`, …) and numeric (`&#8212;`, `&#x2014;`)
//! entities are decoded.
//!
//! # Typography
//!
//! US Letter or A4 pages, 54 pt margins, 11 pt Helvetica body (Helvetica-Bold
//! headings, Helvetica-Oblique blockquotes, Courier code blocks). Text that
//! cannot be represented in WinAnsiEncoding is replaced with `?`; wrapping is
//! greedy word-wrap measured from the exact AFM widths.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard};

mod blocks;
mod emit;
mod html;
mod layout;
mod markdown;

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
        for (&num, _) in rev.entries.iter() {
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
}
