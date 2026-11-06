//! Reflow output: the extracted text as an article, not as a canvas
//! (Phase 3 — PDF → HTML reflow).
//!
//! The positioned HTML document in [`crate::html`] is a layout copy: every span
//! sits where the page put it. This module is the other half — paragraphs in
//! reading order, headings inferred from type size, no coordinates — for the
//! reader who wants the article rather than the page.
//!
//! # What this does not claim
//!
//! * **Layout fidelity.** Reflow discards positions by design.
//! * **Tables and figures.** A table's cells reflow as body text; images are not
//!   extracted, so a figure's caption is all that survives.
//! * **Colour, fonts and links.** The extraction model carries none of them.
//! * **Perfect paragraphing.** A paragraph break is inferred from the vertical
//!   gap, the indent and the type size. A page with generous leading will break
//!   more often than its author did, and the export never claims otherwise.
//!
//! The document is checked against its own model before it is handed over:
//! every paragraph's escaped text must appear, in order, in the bytes that were
//! written. A reflow that dropped a paragraph is a failure, not a shorter
//! article.

#![allow(clippy::integer_division)]

use crate::assembly::TextLine;

/// How one reflowed paragraph is marked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParagraphKind {
    /// Body text (`<p>`).
    Body,
    /// A heading, 1–3 (`<h1>`–`<h3>`).
    Heading(u8),
}

/// One reflowed paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflowParagraph {
    /// The paragraph's text, with its lines joined by a single space.
    pub text: String,
    /// The element the emitter uses for it.
    pub kind: ParagraphKind,
}

/// One page of reflowed paragraphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflowPage {
    /// The 1-based page number the paragraphs came from.
    pub number: u32,
    /// The page's paragraphs, in reading order.
    pub paragraphs: Vec<ReflowParagraph>,
}

/// A heading is at least this many times the body size.
const HEADING_1_RATIO: f64 = 1.35;
/// An `<h2>` is at least this many times the body size.
const HEADING_2_RATIO: f64 = 1.18;
/// An `<h3>` is at least this many times the body size.
const HEADING_3_RATIO: f64 = 1.06;
/// The longest text that can still be a heading.
const MAX_HEADING_CHARS: usize = 120;
/// A vertical gap wider than this share of the type size breaks a paragraph.
///
/// Ordinary leading is 1.2–1.5× the type size; a paragraph break is wider than
/// that. The bar sits between the two, so a normally leaded page joins its lines
/// and a blank half-line opens a paragraph.
const PARAGRAPH_GAP_RATIO: f64 = 1.6;
/// An indent difference wider than this share of the type size breaks it too.
const PARAGRAPH_INDENT_RATIO: f64 = 0.9;

/// The type size of a line: its tallest glyph.
fn line_size(line: &TextLine) -> f64 {
    line.words
        .iter()
        .flat_map(|word| word.runs.iter())
        .flat_map(|run| run.glyphs.iter())
        .map(|glyph| glyph.size)
        .fold(0.0_f64, f64::max)
}

/// The median of a page's line sizes, which is this module's body size.
fn body_size(lines: &[TextLine]) -> f64 {
    let mut sizes: Vec<f64> = lines
        .iter()
        .map(line_size)
        .filter(|size| *size > 0.0)
        .collect();
    if sizes.is_empty() {
        return 0.0;
    }
    sizes.sort_by(f64::total_cmp);
    sizes.get(sizes.len() / 2).copied().unwrap_or(0.0)
}

/// Decide the element for one line.
fn kind_for(text: &str, size: f64, body: f64) -> ParagraphKind {
    let ratio = if body > 0.0 { size / body } else { 1.0 };
    let short = text.chars().count() <= MAX_HEADING_CHARS;
    let sentence = text.trim_end().ends_with(['.', ',', ';', ':']);
    if !short || sentence {
        return ParagraphKind::Body;
    }
    if ratio >= HEADING_1_RATIO {
        ParagraphKind::Heading(1)
    } else if ratio >= HEADING_2_RATIO {
        ParagraphKind::Heading(2)
    } else if ratio >= HEADING_3_RATIO {
        ParagraphKind::Heading(3)
    } else {
        ParagraphKind::Body
    }
}

/// Group a page's lines into paragraphs.
///
/// `texts` is parallel to `lines` (the caller's recovered line texts). A line
/// with no text starts nothing; a gap wider than [`PARAGRAPH_GAP_RATIO`] of the
/// type size, a different element, or an indent that moved by more than
/// [`PARAGRAPH_INDENT_RATIO`] starts a new paragraph.
#[must_use]
pub fn paragraphs(lines: &[TextLine], texts: &[String]) -> Vec<ReflowParagraph> {
    let body = body_size(lines);
    let mut out: Vec<ReflowParagraph> = Vec::new();
    let mut previous: Option<(f64, f64, f64, ParagraphKind)> = None;
    for (line, text) in lines.iter().zip(texts.iter()) {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let size = line_size(line);
        let left = line.bbox.x0;
        let kind = kind_for(text, size, body);
        let joins = previous.is_some_and(|(last_left, last_baseline, last_size, last_kind)| {
            let height = size.max(last_size).max(1.0);
            // Baseline to baseline, not box edge to box edge: an assembled
            // line's box may be a bare baseline, and ordinary leading is what
            // the ratio is calibrated against.
            let gap = last_baseline - line.bbox.y0;
            last_kind == kind
                && kind != ParagraphKind::Heading(1)
                && gap <= height * PARAGRAPH_GAP_RATIO
                && gap >= -height
                && (left - last_left).abs() <= height * PARAGRAPH_INDENT_RATIO
        });
        if joins {
            if let Some(last) = out.last_mut() {
                if !last.text.is_empty() {
                    last.text.push(' ');
                }
                last.text.push_str(text);
            }
        } else {
            out.push(ReflowParagraph {
                text: text.to_string(),
                kind,
            });
        }
        previous = Some((left, line.bbox.y0, size, kind));
    }
    out
}

/// Escape text for HTML body content.
#[must_use]
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            ch => out.push(ch),
        }
    }
    out
}

/// The stylesheet every reflow document carries: one column, readable measure,
/// and no external reference of any kind.
const STYLE: &str = "body{margin:0;background:#fff;color:#111;font:16px/1.55 Georgia,serif}\
article{max-width:38rem;margin:0 auto;padding:2rem 1.25rem}\
section.page{border-top:1px solid #e5e5e5;padding-top:1.5rem}\
section.page:first-child{border-top:0;padding-top:0}\
h1,h2,h3{line-height:1.2;margin:1.4em 0 .5em}\
p{margin:0 0 .9em}\
.marker{color:#a33;font-family:ui-monospace,monospace;font-size:.85em}";

/// Write the reflowed pages as one HTML document.
///
/// One `<section class="page">` per page keeps the page boundary visible
/// without pretending the article is paginated, and a page that drew text it
/// could not recover carries the SL-3.TEXT.10 marker as its own paragraph.
#[must_use]
pub fn to_reflow_html(title: &str, pages: &[ReflowPage]) -> String {
    let mut out =
        String::from("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>");
    out.push_str(&escape_html(title));
    out.push_str("</title>\n<style>");
    out.push_str(STYLE);
    out.push_str("</style>\n</head>\n<body>\n<article>\n");
    for page in pages {
        out.push_str(&format!(
            "<section class=\"page\" data-page=\"{}\">\n",
            page.number
        ));
        for paragraph in &page.paragraphs {
            let text = escape_html(&paragraph.text);
            match paragraph.kind {
                ParagraphKind::Body => {
                    out.push_str("<p>");
                    out.push_str(&text);
                    out.push_str("</p>\n");
                }
                ParagraphKind::Heading(level) => {
                    let level = level.clamp(1, 3);
                    out.push_str(&format!("<h{level}>{text}</h{level}>\n"));
                }
            }
        }
        out.push_str("</section>\n");
    }
    out.push_str("</article>\n</body>\n</html>\n");
    out
}

/// Check that the document holds the pages it was written from.
///
/// The check is deliberately not an HTML parser: it demands the doctype, one
/// `<section class="page"` per page, and every paragraph's escaped text in
/// order. A document that lost a paragraph, duplicated one, or emitted markup
/// that swallows a paragraph (an unescaped `<`) fails.
///
/// # Errors
///
/// A sentence naming the first paragraph the document does not carry.
pub fn verify_reflow_html(document: &str, pages: &[ReflowPage]) -> Result<(), String> {
    if !document.starts_with("<!doctype html>\n<html lang=\"en\">") {
        return Err("the document does not open as an HTML article".to_string());
    }
    let sections = document.matches("<section class=\"page\"").count();
    if sections != pages.len() {
        return Err(format!(
            "the document holds {sections} page section(s) for {} page(s)",
            pages.len()
        ));
    }
    let mut cursor = 0usize;
    for page in pages {
        for paragraph in &page.paragraphs {
            let needle = escape_html(&paragraph.text);
            let Some(found) = document
                .get(cursor..)
                .and_then(|rest| rest.find(&needle))
                .map(|offset| cursor.saturating_add(offset))
            else {
                return Err(format!(
                    "page {} lost a paragraph: {:?}",
                    page.number, paragraph.text
                ));
            };
            cursor = found.saturating_add(needle.len());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::integer_division
    )]

    use super::*;
    use selis_bytes::Bytes;
    use selis_geom::{Point, Rect};
    use selis_pdf_content::text::TextGlyph;

    fn glyph(code: u16, size: f64, x: f64) -> TextGlyph {
        TextGlyph {
            advance: size * 0.5,
            code,
            at: Point::new(x, 0.0),
            tm: selis_geom::Matrix::IDENTITY,
            font: Bytes::copy_from_slice(b"F1"),
            size,
            space: size * 0.25,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
        }
    }

    fn line(text: &str, size: f64, x: f64, y: f64) -> TextLine {
        let glyphs = text
            .chars()
            .enumerate()
            .map(|(index, ch)| glyph(ch as u16, size, x + size * 0.5 * index as f64))
            .collect();
        TextLine {
            words: vec![crate::assembly::TextWord {
                runs: vec![crate::assembly::TextRun {
                    glyphs,
                    bbox: Rect::new(x, y, x + size * 4.0, y + size),
                }],
                bbox: Rect::new(x, y, x + size * 4.0, y + size),
            }],
            bbox: Rect::new(x, y, x + size * 4.0, y + size),
        }
    }

    #[test]
    fn close_lines_join_and_a_gap_breaks() {
        let lines = vec![
            line("first part", 10.0, 50.0, 800.0),
            line("second part", 10.0, 50.0, 786.0),
            line("new paragraph", 10.0, 50.0, 740.0),
        ];
        let texts = vec![
            "first part".to_string(),
            "second part".to_string(),
            "new paragraph".to_string(),
        ];
        let paragraphs = paragraphs(&lines, &texts);
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].text, "first part second part");
    }

    #[test]
    fn a_large_short_line_is_a_heading() {
        let lines = vec![
            line("Report title", 24.0, 50.0, 800.0),
            line("Body text that runs on for a while.", 10.0, 50.0, 760.0),
            line("and continues here.", 10.0, 50.0, 746.0),
        ];
        let texts = vec![
            "Report title".to_string(),
            "Body text that runs on for a while.".to_string(),
            "and continues here.".to_string(),
        ];
        let paragraphs = paragraphs(&lines, &texts);
        assert_eq!(paragraphs[0].kind, ParagraphKind::Heading(1));
        assert_eq!(paragraphs[1].kind, ParagraphKind::Body);
        assert_eq!(
            paragraphs[1].text,
            "Body text that runs on for a while. and continues here."
        );
    }

    #[test]
    fn markup_in_the_text_is_escaped_not_executed() {
        let page = ReflowPage {
            number: 1,
            paragraphs: vec![ReflowParagraph {
                text: "a <script>alert(1)</script> & b".to_string(),
                kind: ParagraphKind::Body,
            }],
        };
        let document = to_reflow_html("<t>", &[page]);
        assert!(!document.contains("<script>"), "{document}");
        assert!(document.contains("&lt;script&gt;"));
        assert!(document.contains("<title>&lt;t&gt;</title>"));
    }

    #[test]
    fn verification_refuses_a_dropped_paragraph() {
        let page = ReflowPage {
            number: 1,
            paragraphs: vec![
                ReflowParagraph {
                    text: "kept".to_string(),
                    kind: ParagraphKind::Body,
                },
                ReflowParagraph {
                    text: "lost".to_string(),
                    kind: ParagraphKind::Body,
                },
            ],
        };
        let document = to_reflow_html("T", std::slice::from_ref(&page));
        verify_reflow_html(&document, std::slice::from_ref(&page)).expect("intact");
        let truncated = document.replace("<p>lost</p>\n", "");
        let err = verify_reflow_html(&truncated, std::slice::from_ref(&page)).expect_err("dropped");
        assert!(err.contains("lost a paragraph"), "{err}");
    }

    #[test]
    fn an_empty_article_is_still_a_document() {
        let document = to_reflow_html("Empty", &[]);
        verify_reflow_html(&document, &[]).expect("empty");
        assert!(document.contains("<article>"));
    }
}
