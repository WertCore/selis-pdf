//! Word (`.docx`) → PDF: the document's structure, not its page.
//!
//! # What this does
//!
//! The package's main part (`word/document.xml`) is parsed and its body is
//! mapped onto the same block model the Markdown/HTML converters lay out:
//! paragraphs become flow text with bold/italic runs, the built-in heading
//! styles (`Heading1`–`Heading6`, `Title`, `Subtitle`) become headings with the
//! same levels, numbered/bulleted paragraphs become list items at the
//! paragraph's indent level, and tables become real tables (a first row marked
//! `tblheader` is the header row).
//!
//! # What this does not do (said plainly)
//!
//! It is not a Word layout engine. Page size and margins come from the caller's
//! options, not from `sectPr`; paragraph indents, spacing, alignment, colour,
//! fonts, headers/footers, footnotes, fields, images, shapes, text boxes and
//! tracked changes are not carried. A `.docx` that stores its whole layout in
//! floating frames or images converts as the text those parts contain, not as
//! the page Word draws. An encrypted Office file is refused by the archive
//! reader itself (it is not a ZIP), with the same message as any other
//! malformed package.

#![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::blocks::{Block, Span, Style};
use crate::ooxml::{attr, child, children, deep_text, flag, malformed, parse_part, part};
use crate::xmlmini::Element;
use crate::{emit, Options};

/// The main document part's path.
const MAIN_PART: &str = "word/document.xml";

/// Convert a Word document to PDF bytes.
///
/// # Budget
///
/// Charges the input bytes, the expanded package bytes and the generated object
/// graph against `budget`.
///
/// # Malformed Input
///
/// A package that is not a ZIP, has no `word/document.xml`, or whose main part
/// is not parseable XML is refused with a message naming the part.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document this engine and
/// any conforming reader can open.
pub(crate) fn docx_to_pdf(
    input: &[u8],
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    g.charge(
        Resource::Bytes,
        u64::try_from(input.len()).unwrap_or(u64::MAX),
    )?;
    let entries = selis_archive::read_zip(input)
        .map_err(|message| malformed("docx", format!("not a Word document: {message}")))?;
    let expanded = entries.iter().fold(0u64, |total, (_, data)| {
        total.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX))
    });
    g.charge(Resource::Bytes, expanded)?;
    let main = part(&entries, MAIN_PART).ok_or_else(|| {
        malformed(
            "docx",
            "the package has no word/document.xml (is this a Word file?)".to_string(),
        )
    })?;
    let root = parse_part(main, "docx", MAIN_PART)?;
    let body = root
        .find_first("body")
        .ok_or_else(|| malformed("docx", "word/document.xml has no body".to_string()))?;
    let blocks = body_blocks(body);
    emit::build(&blocks, opts, budget, g)
}

/// Map the body's block children onto layout blocks, in document order.
fn body_blocks(body: &Element) -> Vec<Block> {
    let mut out = Vec::new();
    push_body_children(body, &mut out);
    if out.is_empty() {
        // A Word file with no body content still converts to one blank page
        // rather than failing: an empty document is a document.
        out.push(Block::Paragraph(Vec::new()));
    }
    out
}

fn push_body_children(node: &Element, out: &mut Vec<Block>) {
    for item in &node.children {
        match item.tag.as_str() {
            "p" => {
                if let Some(block) = paragraph_block(item) {
                    out.push(block);
                }
            }
            "tbl" => {
                if let Some(block) = table_block(item) {
                    out.push(block);
                }
            }
            // Content controls and custom XML wrappers hold the same block
            // children one level down.
            "sdt" | "sdtcontent" | "customxml" => push_body_children(item, out),
            _ => {}
        }
    }
}

/// One paragraph as a block: heading, list item or body paragraph.
fn paragraph_block(paragraph: &Element) -> Option<Block> {
    let spans = paragraph_spans(paragraph);
    if let Some(props) = child(paragraph, "ppr") {
        if let Some(level) = heading_level(props) {
            return Some(Block::Heading { level, spans });
        }
        if let Some(depth) = list_level(props) {
            return Some(Block::ListItem {
                ordered: false,
                ordinal: 1,
                depth,
                spans,
            });
        }
    }
    Some(Block::Paragraph(spans))
}

/// The heading level a paragraph style names, if any.
fn heading_level(props: &Element) -> Option<u8> {
    let style = child(props, "pstyle")?;
    let value = attr(style, "val")?.trim().to_ascii_lowercase();
    if value == "title" {
        return Some(1);
    }
    if value == "subtitle" {
        return Some(2);
    }
    let digits = value
        .strip_prefix("heading")
        .map(str::trim)
        .and_then(|rest| rest.parse::<u8>().ok())?;
    Some(digits.clamp(1, 6))
}

/// The list indent level a paragraph declares, if it is a list item.
fn list_level(props: &Element) -> Option<usize> {
    let numbering = child(props, "numpr")?;
    // A `numPr` without an `ilvl` is still a list item: level 0.
    let level = child(numbering, "ilvl")
        .and_then(|node| attr(node, "val"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    Some(level.min(8))
}

/// The styled runs of one paragraph, with adjacent equal styles merged.
fn paragraph_spans(paragraph: &Element) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for run in paragraph.find_all("r") {
        let (style, raw) = run_text(run);
        if raw.is_empty() {
            continue;
        }
        match spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(&raw),
            _ => spans.push(Span { style, text: raw }),
        }
    }
    spans
}

/// One run's style and text (tabs and breaks included).
fn run_text(run: &Element) -> (Style, String) {
    let mut style = Style::Regular;
    if let Some(props) = child(run, "rpr") {
        if child(props, "b").is_some_and(flag) {
            style = style.with_bold();
        }
        if child(props, "i").is_some_and(flag) {
            style = style.with_italic();
        }
        if child(props, "rstyle")
            .and_then(|node| attr(node, "val"))
            .is_some_and(|value| value.eq_ignore_ascii_case("code"))
        {
            style = Style::Code;
        }
    }
    let mut text = String::new();
    for node in &run.children {
        match node.tag.as_str() {
            "t" => text.push_str(&deep_text(node)),
            "tab" => text.push('\t'),
            "br" | "cr" => text.push('\n'),
            "nobreakhyphen" => text.push('-'),
            _ => {}
        }
    }
    (style, text)
}

/// One table as a layout table: an optional header row, then the body rows.
fn table_block(table: &Element) -> Option<Block> {
    let mut head: Vec<Vec<Span>> = Vec::new();
    let mut rows: Vec<Vec<Vec<Span>>> = Vec::new();
    for (index, row) in children(table, "tr").into_iter().enumerate() {
        let cells: Vec<Vec<Span>> = children(row, "tc").into_iter().map(cell_spans).collect();
        if index == 0 && is_header_row(row) {
            head = cells;
        } else {
            rows.push(cells);
        }
    }
    if head.is_empty() && rows.is_empty() {
        return None;
    }
    Some(Block::Table { head, rows })
}

/// Whether a row is marked as a repeating header (`w:tblHeader`).
fn is_header_row(row: &Element) -> bool {
    child(row, "trpr")
        .and_then(|props| child(props, "tblheader"))
        .is_some_and(flag)
}

/// One cell's text: its paragraphs joined with a space.
fn cell_spans(cell: &Element) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for paragraph in children(cell, "p") {
        let part = paragraph_spans(paragraph);
        if part.is_empty() {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span {
                style: Style::Regular,
                text: " ".to_string(),
            });
        }
        spans.extend(part);
    }
    spans
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;
    use crate::{testkit::opens_with_page_count, Options};
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn docx(body: &str) -> Vec<u8> {
        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
{body}
  </w:body>
</w:document>"#
        );
        selis_archive::store_zip(&[("word/document.xml", document.as_bytes())])
    }

    #[test]
    fn a_documented_docx_converts_all_blocks() {
        let body = r#"<w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Quarterly plan</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Section heading</w:t></w:r></w:p>
<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold lead. </w:t></w:r><w:r><w:t>Plain tail.</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First item</w:t></w:r></w:p>
<w:tbl>
  <w:tr><w:trPr><w:tblHeader/></w:trPr>
    <w:tc><w:p><w:r><w:t>Quarter</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>Amount</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:tc><w:p><w:r><w:t>Q1</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>100</w:t></w:r></w:p></w:tc>
  </w:tr>
</w:tbl>"#;
        let bytes = docx_to_pdf(
            &docx(body),
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("convert");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        let text = String::from_utf8_lossy(&bytes);
        for word in [
            "Quarterly plan",
            "Bold lead",
            "First item",
            "Quarter",
            "Q1",
            "100",
        ] {
            assert!(text.contains(word), "missing {word}");
        }
        opens_with_page_count(&bytes, 1);
    }

    #[test]
    fn a_package_without_the_main_part_is_named() {
        let broken = selis_archive::store_zip(&[("word/other.xml", b"<x/>")]);
        let Err(error) = docx_to_pdf(
            &broken,
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("expected a refusal");
        };
        assert_eq!(error.ctx().during, Some("docx"));
        assert!(error.to_string().contains("document.xml"));
    }

    #[test]
    fn a_malformed_zip_is_refused() {
        let Err(_) = docx_to_pdf(
            b"not a zip",
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("expected a refusal");
        };
    }

    #[test]
    fn an_empty_document_still_produces_a_page() {
        let bytes = docx_to_pdf(
            &docx(""),
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("convert");
        opens_with_page_count(&bytes, 1);
    }
}
