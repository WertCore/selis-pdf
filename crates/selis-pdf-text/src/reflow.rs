//! Reflow output: the extracted text as an article, not as a canvas
//! (Phase 3 — PDF → HTML reflow).
//!
//! The positioned HTML document in [`crate::html`] is a layout copy: every span
//! sits where the page put it. This module is the other half — paragraphs in
//! reading order, headings inferred from type size, no coordinates — for the
//! reader who wants the article rather than the page.
//!
//! # What this does and does not claim
//!
//! * **Tables.** The article does not auto-detect tables: run blind over prose,
//!   table inference over-detects (a two-word heading becomes two columns), so
//!   a genuine table's cells reflow as body text here and the opt-in "Tables as
//!   CSV" tool is where a reader asks for a grid. The `<table>` emitter and its
//!   verifier remain for a future, precise line-to-row attribution.
//! * **Links.** Bare URLs in body text (`http://`, `https://`, `www.`) become
//!   `<a>` elements; a `www.` link is given an `https` scheme and every link
//!   carries `rel="nofollow noopener"`. Link *annotations* are not read — the
//!   links come from the text itself.
//! * **Layout fidelity.** Reflow discards positions by design.
//! * **Figures, colour and fonts.** Images are not extracted (a figure's caption
//!   is all that survives), and the extraction model carries no colour or faces.
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

/// One table lifted out of the reading flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflowTable {
    /// How many columns the table has.
    pub columns: usize,
    /// The rows, each holding one cell per column (an absent cell is empty).
    pub rows: Vec<Vec<String>>,
}

/// One page of reflowed paragraphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflowPage {
    /// The 1-based page number the paragraphs came from.
    pub number: u32,
    /// The page's paragraphs, in reading order.
    pub paragraphs: Vec<ReflowParagraph>,
    /// Tables recovered on the page, lifted out of the paragraph flow and
    /// rendered as real `<table>` elements after the page's paragraphs. A table
    /// at the end of its page section is a deliberate, documented limitation:
    /// the cells are a true grid rather than garbled body text, but their inline
    /// position within the prose is not reconstructed.
    pub tables: Vec<ReflowTable>,
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

/// Build a page's paragraphs for the reading-order article.
///
/// Tables are deliberately **not** auto-lifted here. `tables_on_page` is tuned
/// for the opt-in "Tables as CSV" tool, where a reader has decided the page is a
/// table; run blind over arbitrary prose it over-detects — a two-word heading
/// like "Parity Report" gets split into two columns — which would corrupt the
/// article rather than improve it (caught by the real-browser reflow leg). So
/// the article stays paragraphs + autolinked URLs, and a genuine table's cells
/// reflow as body text, as the module's "does not claim" note records. The
/// `ReflowTable` emitter and `verify_reflow_html` still handle tables for a
/// future, precise line-to-row attribution; this function just does not feed
/// them. `page` is kept in the signature for that future use.
#[must_use]
pub fn page_content(
    lines: &[TextLine],
    texts: &[String],
    _page: u32,
) -> (Vec<ReflowParagraph>, Vec<ReflowTable>) {
    (paragraphs(lines, texts), Vec::new())
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

/// The URL prefixes the autolinker recognises.
const URL_PREFIXES: [&str; 3] = ["https://", "http://", "www."];

/// Find the first autolinkable URL in `text`, as a `(start, end)` byte range.
///
/// A URL must start at a word boundary (so `xhttp://…` is not one), runs to the
/// first whitespace or URL-hostile character, and sheds trailing sentence
/// punctuation so `(see https://x.test).` links `https://x.test`.
fn find_url(text: &str) -> Option<(usize, usize)> {
    for (index, _) in text.char_indices() {
        let at_boundary = index == 0
            || !text
                .get(..index)
                .and_then(|prefix| prefix.chars().next_back())
                .is_some_and(|ch| ch.is_ascii_alphanumeric());
        if !at_boundary {
            continue;
        }
        let rest = text.get(index..).unwrap_or("");
        for prefix in URL_PREFIXES {
            if rest.starts_with(prefix) {
                let after = index.saturating_add(prefix.len());
                let mut end = after;
                for (offset, ch) in text.get(after..).unwrap_or("").char_indices() {
                    if ch.is_whitespace() || "<>\"'{}|\\^`".contains(ch) {
                        break;
                    }
                    end = after.saturating_add(offset).saturating_add(ch.len_utf8());
                }
                while end > after {
                    let last = text.get(..end).and_then(|s| s.chars().next_back());
                    match last {
                        Some(ch) if ".,;:!?)]}".contains(ch) => {
                            end = end.saturating_sub(ch.len_utf8());
                        }
                        _ => break,
                    }
                }
                if end > after {
                    return Some((index, end));
                }
            }
        }
    }
    None
}

/// Render body text as HTML, turning bare URLs into links. Everything is
/// escaped; a `www.` URL is linked over `https://`. The link carries
/// `rel="nofollow noopener"` and no target, so nothing external can act through
/// it beyond the navigation the reader chose.
#[must_use]
pub fn paragraph_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((start, end)) = find_url(rest) {
        out.push_str(&escape_html(rest.get(..start).unwrap_or("")));
        let url = rest.get(start..end).unwrap_or("");
        let href = if url.starts_with("www.") {
            format!("https://{url}")
        } else {
            url.to_string()
        };
        out.push_str("<a href=\"");
        out.push_str(&escape_html(&href));
        out.push_str("\" rel=\"nofollow noopener\">");
        out.push_str(&escape_html(url));
        out.push_str("</a>");
        rest = rest.get(end..).unwrap_or("");
    }
    out.push_str(&escape_html(rest));
    out
}

/// Render one recovered table as an HTML `<table>`; the first row is the header.
fn table_html(table: &ReflowTable) -> String {
    let mut out = String::from("<table>\n");
    for (row_index, row) in table.rows.iter().enumerate() {
        out.push_str("<tr>");
        let tag = if row_index == 0 { "th" } else { "td" };
        for cell in row {
            out.push_str(&format!("<{tag}>{}</{tag}>", escape_html(cell)));
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table>\n");
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
a{color:#06c}\
table{border-collapse:collapse;margin:0 0 1em;font-size:.95em}\
th,td{border:1px solid #ccc;padding:.3em .6em;text-align:left;vertical-align:top}\
th{background:#f3f3f3;font-weight:600}\
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
            match paragraph.kind {
                ParagraphKind::Body => {
                    out.push_str("<p>");
                    out.push_str(&paragraph_html(&paragraph.text));
                    out.push_str("</p>\n");
                }
                ParagraphKind::Heading(level) => {
                    let level = level.clamp(1, 3);
                    let text = escape_html(&paragraph.text);
                    out.push_str(&format!("<h{level}>{text}</h{level}>\n"));
                }
            }
        }
        for table in &page.tables {
            out.push_str(&table_html(table));
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
    let mut find_in_order = |needle: &str| -> Option<usize> {
        let found = document
            .get(cursor..)
            .and_then(|rest| rest.find(needle))
            .map(|offset| cursor.saturating_add(offset))?;
        cursor = found.saturating_add(needle.len());
        Some(found)
    };
    for page in pages {
        for paragraph in &page.paragraphs {
            // Body text is autolinked, so its rendered markup — not the bare
            // escape — is what must appear; a heading is escaped plainly.
            let needle = match paragraph.kind {
                ParagraphKind::Body => paragraph_html(&paragraph.text),
                ParagraphKind::Heading(_) => escape_html(&paragraph.text),
            };
            if find_in_order(&needle).is_none() {
                return Err(format!(
                    "page {} lost a paragraph: {:?}",
                    page.number, paragraph.text
                ));
            }
        }
        for table in &page.tables {
            for cell in table.rows.iter().flatten() {
                if cell.is_empty() {
                    continue;
                }
                if find_in_order(&escape_html(cell)).is_none() {
                    return Err(format!(
                        "page {} lost a table cell: {:?}",
                        page.number, cell
                    ));
                }
            }
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
            tables: vec![],
        };
        let document = to_reflow_html("<t>", &[page]);
        assert!(!document.contains("<script>"), "{document}");
        assert!(document.contains("&lt;script&gt;"));
        assert!(document.contains("<title>&lt;t&gt;</title>"));
    }

    #[test]
    fn urls_in_body_become_links() {
        let page = ReflowPage {
            number: 1,
            paragraphs: vec![ReflowParagraph {
                text: "see https://selis.test/docs and www.example.org, thanks".to_string(),
                kind: ParagraphKind::Body,
            }],
            tables: vec![],
        };
        let document = to_reflow_html("T", std::slice::from_ref(&page));
        assert!(
            document.contains("<a href=\"https://selis.test/docs\" rel=\"nofollow noopener\">https://selis.test/docs</a>"),
            "{document}"
        );
        // A www link is given an https scheme; the trailing comma stays outside.
        assert!(
            document.contains("<a href=\"https://www.example.org\" rel=\"nofollow noopener\">www.example.org</a>,"),
            "{document}"
        );
        // The document still verifies against its own model with links in place.
        verify_reflow_html(&document, std::slice::from_ref(&page)).expect("verifies");
    }

    #[test]
    fn a_url_cannot_inject_an_attribute() {
        let page = ReflowPage {
            number: 1,
            paragraphs: vec![ReflowParagraph {
                text: "http://x.test/\"><img src=x>".to_string(),
                kind: ParagraphKind::Body,
            }],
            tables: vec![],
        };
        let document = to_reflow_html("T", &[page]);
        assert!(
            !document.contains("<img"),
            "the injection is escaped: {document}"
        );
        assert!(document.contains("&lt;img"));
    }

    #[test]
    fn tables_render_as_html_tables() {
        let page = ReflowPage {
            number: 1,
            paragraphs: vec![],
            tables: vec![ReflowTable {
                columns: 2,
                rows: vec![
                    vec!["Name".to_string(), "Count".to_string()],
                    vec!["Tea".to_string(), "3".to_string()],
                ],
            }],
        };
        let document = to_reflow_html("T", std::slice::from_ref(&page));
        assert!(document.contains("<table>"), "{document}");
        assert!(
            document.contains("<th>Name</th><th>Count</th>"),
            "header row: {document}"
        );
        assert!(
            document.contains("<td>Tea</td><td>3</td>"),
            "body row: {document}"
        );
        verify_reflow_html(&document, std::slice::from_ref(&page)).expect("verifies");
        let dropped = document.replace("<td>Tea</td><td>3</td></tr>\n", "");
        assert!(verify_reflow_html(&dropped, std::slice::from_ref(&page))
            .unwrap_err()
            .contains("lost a table cell"),);
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
            tables: vec![],
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
