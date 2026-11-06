//! PowerPoint (`.pptx`) → PDF: the deck's words and tables, not its art.
//!
//! # What this does
//!
//! Slides are converted in the order `presentation.xml` presents them. For
//! each slide the shapes that carry text become flow blocks: a title or
//! centred-title placeholder becomes a level-1 heading, a subtitle becomes a
//! level-2 heading, body paragraphs become paragraphs with bold/italic runs,
//! bulleted or numbered paragraphs become list items at their `lvl`, and a
//! shape carrying a DrawingML table becomes a real table. Every slide starts
//! on a new page.
//!
//! # What this does not do (said plainly)
//!
//! It is not a PowerPoint renderer. Slide size, theme, fonts, colour,
//! backgrounds, images, charts, SmartArt, shapes other than text and tables,
//! speaker notes, animations, transitions, headers/footers and master layout
//! are not carried; a deck that stores its message in pictures converts as the
//! text those parts own, not as the slides PowerPoint draws. A slide with no
//! text and no table disappears rather than producing a page.

#![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::blocks::{Block, Span, Style};
use crate::ooxml::{
    attr, bool_attr, child, children, deep_text, malformed, parse_part, part, rel_attr,
    relationships, resolve_target, split_path,
};
use crate::xmlmini::Element;
use crate::{emit, Options};

/// The presentation part's path.
const PRESENTATION: &str = "ppt/presentation.xml";
/// The fallback scan prefix for slides when the presentation part declares none.
const SLIDE_PREFIX: &str = "ppt/slides/slide";

/// Convert a PowerPoint deck to PDF bytes.
///
/// # Budget
///
/// Charges the input bytes, the expanded package bytes and the generated object
/// graph against `budget`.
///
/// # Malformed Input
///
/// A package that is not a ZIP, has no `ppt/presentation.xml`, or whose parts
/// are not parseable XML is refused with a message naming the part. A deck
/// whose slide list resolves to no slides converts as one empty page.
///
/// # Output Guarantees
///
/// The bytes are a complete single-revision PDF 1.4 document this engine and
/// any conforming reader can open.
pub(crate) fn pptx_to_pdf(
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
        .map_err(|message| malformed("pptx", format!("not a PowerPoint deck: {message}")))?;
    let expanded = entries.iter().fold(0u64, |total, (_, data)| {
        total.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX))
    });
    g.charge(Resource::Bytes, expanded)?;
    let main = part(&entries, PRESENTATION).ok_or_else(|| {
        malformed(
            "pptx",
            "the package has no ppt/presentation.xml (is this a PowerPoint file?)".to_string(),
        )
    })?;
    let root = parse_part(main, "pptx", PRESENTATION)?;
    let slides = slide_parts(&entries, &root)?;

    let mut blocks = Vec::new();
    for (index, path) in slides.iter().enumerate() {
        if index > 0 {
            blocks.push(Block::PageBreak);
        }
        let bytes = part(&entries, path)
            .ok_or_else(|| malformed("pptx", format!("{path}: the slide part is missing")))?;
        let slide = parse_part(bytes, "pptx", path)?;
        push_slide_blocks(&slide, &mut blocks);
    }
    emit::build(&blocks, opts, budget, g)
}

/// The slide part paths in presentation order.
fn slide_parts(entries: &[(String, Vec<u8>)], root: &Element) -> Result<Vec<String>> {
    let rels = relationships(entries, PRESENTATION, "pptx");
    let (dir, _) = split_path(PRESENTATION);
    let mut by_id = std::collections::BTreeMap::new();
    for rel in rels {
        by_id.insert(rel.id, resolve_target(dir, &rel.target));
    }
    let mut ordered = Vec::new();
    if let Some(list) = root.find_first("sldidlst") {
        for slide in children(list, "sldid") {
            if let Some(id) = rel_attr(slide) {
                if let Some(target) = by_id.get(id) {
                    ordered.push(target.clone());
                }
            }
        }
    }
    if !ordered.is_empty() {
        return Ok(ordered);
    }
    // A deck that declares no slide list is still a deck: enumerate the slide
    // parts that exist and order them by their numeric suffix.
    let mut scanned: Vec<(u32, String)> = Vec::new();
    for (name, _) in entries {
        let Some(rest) = name.strip_prefix(SLIDE_PREFIX) else {
            continue;
        };
        let Some(num) = rest.strip_suffix(".xml") else {
            continue;
        };
        if let Ok(num) = num.parse::<u32>() {
            scanned.push((num, name.clone()));
        }
    }
    scanned.sort_by_key(|(num, _)| *num);
    Ok(scanned.into_iter().map(|(_, name)| name).collect())
}

/// Walk a slide's shape tree in document order, collecting blocks.
fn push_slide_blocks(slide: &Element, out: &mut Vec<Block>) {
    let Some(tree) = slide.find_first("sptree") else {
        return;
    };
    walk_shape_tree(tree, out);
}

fn walk_shape_tree(node: &Element, out: &mut Vec<Block>) {
    for child in &node.children {
        match child.tag.as_str() {
            "sp" => push_shape_blocks(child, out),
            "graphicframe" => {
                if let Some(table) = child.find_first("tbl") {
                    if let Some(block) = table_block(table) {
                        out.push(block);
                    }
                }
            }
            // Grouped shapes keep their order under the group.
            "grpsp" => walk_shape_tree(child, out),
            _ => {}
        }
    }
}

/// The placeholder type of a shape, if any.
fn placeholder(shape: &Element) -> Option<String> {
    let nv_sp_pr = child(shape, "nvsppr")?;
    let nv_pr = child(nv_sp_pr, "nvpr")?;
    child(nv_pr, "ph").map(|ph| {
        attr(ph, "type")
            .unwrap_or("body")
            .trim()
            .to_ascii_lowercase()
    })
}

/// One shape's paragraphs as blocks, or a heading when it is a title.
fn push_shape_blocks(shape: &Element, out: &mut Vec<Block>) {
    let Some(text_body) = child(shape, "txbody") else {
        return;
    };
    let kind = placeholder(shape);
    for paragraph in children(text_body, "p") {
        let spans = paragraph_spans(paragraph);
        if spans.is_empty() {
            continue;
        }
        if matches!(kind.as_deref(), Some("title" | "ctrtitle")) {
            out.push(Block::Heading { level: 1, spans });
            continue;
        }
        if matches!(kind.as_deref(), Some("subtitle")) {
            out.push(Block::Heading { level: 2, spans });
            continue;
        }
        if let Some((ordered, depth)) = list_marker(paragraph) {
            out.push(Block::ListItem {
                ordered,
                ordinal: 1,
                depth,
                spans,
            });
            continue;
        }
        out.push(Block::Paragraph(spans));
    }
}

/// The slide list marker of a paragraph: ordered, and the indent depth.
fn list_marker(paragraph: &Element) -> Option<(bool, usize)> {
    let props = child(paragraph, "ppr")?;
    if child(props, "bunone").is_some() {
        return None;
    }
    let depth = attr(props, "lvl")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(8);
    if child(props, "buautonum").is_some() {
        return Some((true, depth));
    }
    if child(props, "buchar").is_some() {
        return Some((false, depth));
    }
    None
}

/// The styled runs of one paragraph, with adjacent equal styles merged.
fn paragraph_spans(paragraph: &Element) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for run in paragraph
        .children
        .iter()
        .filter(|node| matches!(node.tag.as_str(), "r" | "fld"))
    {
        let style = run_style(run);
        let mut text = String::new();
        for node in &run.children {
            if node.tag.as_str() == "t" {
                text.push_str(&deep_text(node));
            }
        }
        merge_spans(&mut spans, style, &text);
    }
    for node in &paragraph.children {
        if node.tag == "br" {
            merge_spans(&mut spans, Style::Regular, "\n");
        }
    }
    spans
}

fn merge_spans(spans: &mut Vec<Span>, style: Style, text: &str) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => spans.push(Span {
            style,
            text: text.to_string(),
        }),
    }
}

/// One run's style; DrawingML bold/italic are attributes on `rPr`.
fn run_style(run: &Element) -> Style {
    let mut style = Style::Regular;
    if let Some(props) = child(run, "rpr") {
        if bool_attr(props, "b").unwrap_or(false) {
            style = style.with_bold();
        }
        if bool_attr(props, "i").unwrap_or(false) {
            style = style.with_italic();
        }
    }
    style
}

/// One DrawingML table as a layout table.
fn table_block(table: &Element) -> Option<Block> {
    let mut rows: Vec<Vec<Vec<Span>>> = Vec::new();
    for row in children(table, "tr") {
        let cells: Vec<Vec<Span>> = children(row, "tc").into_iter().map(cell_spans).collect();
        rows.push(cells);
    }
    if rows.is_empty() {
        return None;
    }
    // A DrawingML table has no header row concept; the caller decides.
    Some(Block::Table {
        head: Vec::new(),
        rows,
    })
}

/// One cell's text: its paragraphs joined with a space.
fn cell_spans(cell: &Element) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for text_body in children(cell, "txbody") {
        for paragraph in children(text_body, "p") {
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

    const NS_P: &str = r#"http://schemas.openxmlformats.org/presentationml/2006/main"#;
    const NS_A: &str = r#"http://schemas.openxmlformats.org/drawingml/2006/main"#;

    fn slide_xml(members: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:p="{NS_P}" xmlns:a="{NS_A}">
  <p:cSld>
    <p:spTree>
{members}
    </p:spTree>
  </p:cSld>
</p:sld>"#
        )
    }

    fn text_shape(id: u32, name: &str, body: &str) -> String {
        format!(
            r#"      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="{id}" name="{name}"/>
          <p:cNvSpPr/>
          <p:nvPr/>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:bodyPr/>
          <a:lstStyle/>
{body}
        </p:txBody>
      </p:sp>"#
        )
    }

    fn title_shape(id: u32, name: &str, text: &str) -> String {
        format!(
            r#"      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="{id}" name="{name}"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:bodyPr/>
          <a:lstStyle/>
          <a:p><a:r><a:t>{text}</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>"#
        )
    }

    fn table_frame(id: u32) -> String {
        format!(
            r#"      <p:graphicFrame>
        <p:nvGraphicFramePr>
          <p:cNvPr id="{id}" name="Frame"/>
        </p:nvGraphicFramePr>
        <p:graphicFrameContent><a:graphic xmlns:a="{NS_A}"><a:graphicData>
          <a:tbl>
            <a:tr>
              <a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Quarter</a:t></a:r></a:p></a:txBody></a:tc>
              <a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Amount</a:t></a:r></a:p></a:txBody></a:tc>
            </a:tr>
          </a:tbl>
        </a:graphicData></a:graphic></p:graphicFrameContent>
      </p:graphicFrame>"#
        )
    }

    fn deck() -> Vec<u8> {
        let presentation = r#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst>
    <p:sldId id="256" r:id="rId10"/>
    <p:sldId id="257" r:id="rId11"/>
  </p:sldIdLst>
</p:presentation>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
  <Relationship Id="rId11" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#;
        let slide1 = slide_xml(&title_shape(2, "Title 1", "Alpha slide"));
        let slide2 = slide_xml(&format!(
            "{}{}",
            title_shape(2, "Title 2", "Zebra slide"),
            table_frame(3)
        ));
        selis_archive::store_zip(&[
            ("ppt/presentation.xml", presentation.as_bytes()),
            ("ppt/_rels/presentation.xml.rels", rels.as_bytes()),
            ("ppt/slides/slide1.xml", slide1.as_bytes()),
            ("ppt/slides/slide2.xml", slide2.as_bytes()),
        ])
    }

    #[test]
    fn slides_render_in_presentation_order_one_page_each() {
        let bytes = pptx_to_pdf(
            &deck(),
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("convert");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        let text = String::from_utf8_lossy(&bytes);
        let zebra = text.find("Zebra").expect("README marker Z");
        let alpha = text.find("Alpha").expect("README marker A");
        assert!(
            zebra < alpha,
            "the presentation order must win over file order"
        );
        opens_with_page_count(&bytes, 2);
    }

    #[test]
    fn a_title_and_a_table_reach_the_output() {
        let bytes = pptx_to_pdf(
            &deck(),
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("convert");
        let text = String::from_utf8_lossy(&bytes);
        for word in ["Zebra slide", "Alpha slide", "Quarter", "Amount"] {
            assert!(text.contains(word), "missing {word}");
        }
    }

    #[test]
    fn a_package_without_the_presentation_part_is_named() {
        let broken = selis_archive::store_zip(&[("ppt/slides/slide1.xml", b"<x/>")]);
        let Err(error) = pptx_to_pdf(
            &broken,
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("expected a refusal");
        };
        assert_eq!(error.ctx().during, Some("pptx"));
        assert!(error.to_string().contains("presentation.xml"));
    }

    #[test]
    fn a_malformed_zip_is_refused() {
        let Err(_) = pptx_to_pdf(
            b"not a zip",
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("expected a refusal");
        };
    }

    #[test]
    fn a_bulleted_paragraph_becomes_a_list_item() {
        let members = text_shape(
            2,
            "Body",
            r#"          <a:p>
            <a:pPr lvl="2"><a:buChar char="\u{2022}"/></a:pPr>
            <a:r><a:rPr b="1"/><a:t>Bold bullet</a:t></a:r>
          </a:p>"#,
        );
        let rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#;
        let presentation = r#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId10"/></p:sldIdLst></p:presentation>"#;
        let slide = slide_xml(&members);
        let package = selis_archive::store_zip(&[
            ("ppt/presentation.xml", presentation.as_bytes()),
            ("ppt/_rels/presentation.xml.rels", rels.as_bytes()),
            ("ppt/slides/slide1.xml", slide.as_bytes()),
        ]);
        let bytes = pptx_to_pdf(
            &package,
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("convert");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Bold bullet"));
        opens_with_page_count(&bytes, 1);
    }
}
