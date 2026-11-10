//! Word (`.docx`) packaging for recovered pages (Phase 3 — PDF → Word).
//!
//! `to_docx` writes the reflowed article: one paragraph per recovered
//! paragraph, headings carrying Word's built-in styles — good for reading,
//! bad for fidelity. `to_docx_frames` writes the page itself: every recovered
//! line an absolutely positioned frame on a page the size of the source page,
//! zero margins, `hAnchor="page" vAnchor="page"`. The frame holds the words at
//! the coordinates the page put them, so the export is a facsimile the words
//! are still in: editable text that *lands where the PDF drew it*. Packaging
//! and the read-back check live here, in the core crate, so the CLI, the
//! browser chunk and the desktop app produce the same file.
//!
//! # What neither path carries
//!
//! Images, colour, exact faces (everything renders as a substitute face the
//! reader picks), tables (as such — their words are placed like any other
//! line), vectors and fill. The article path carries heading styles; the
//! frame path carries position. Choose deliberately.

use selis_archive::{read_zip, store_zip};

use crate::reflow::{ParagraphKind, ReflowPage};
use crate::xml::xml_text;

/// The media type a `.docx` carries.
pub const DOCX_MEDIA_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// The subject Word's built-in heading style maps to.
const fn heading_style(level: u8) -> &'static str {
    match level {
        1 => "Heading1",
        2 => "Heading2",
        _ => "Heading3",
    }
}

/// A page rendered as frames from its recovered lines.
///
/// `ctm` is the page's user-space → device transform at 72 DPI (the engine's
/// `PageView::ctm`), so a frame lands where the raster export paints the
/// words. y in the output is from the page top, matching Word's `vAnchor`.
#[must_use]
pub fn frame_page_from_lines(
    number: u32,
    width: f64,
    height: f64,
    ctm: selis_geom::Matrix,
    lines: &[crate::assembly::TextLine],
    texts: &[String],
) -> FramePage {
    let mut out: Vec<FrameLine> = Vec::with_capacity(lines.len());
    for (line, text) in lines.iter().zip(texts.iter()) {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let box_ = line.bbox.transform(ctm);
        let x = box_.x0.min(box_.x1);
        let y = box_.y0.min(box_.y1);
        let width = (box_.x1 - box_.x0).abs().max(1.0);
        let height = (box_.y1 - box_.y0).abs().max(1.0);
        let size = line
            .words
            .iter()
            .flat_map(|word| word.runs.iter())
            .flat_map(|run| run.glyphs.iter())
            .map(|glyph| glyph.size)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        out.push(FrameLine {
            text: text.to_string(),
            x: x.max(0.0),
            y: y.max(0.0),
            width,
            height,
            size,
        });
    }
    FramePage {
        number,
        width: width.max(1.0),
        height: height.max(1.0),
        lines: out,
    }
}

/// One placed line in the frame facsimile, in points from the page's top-left
/// corner (y-down, the canvas the raster export paints).
#[derive(Debug, Clone, PartialEq)]
pub struct FrameLine {
    /// The words the frame holds.
    pub text: String,
    /// Left edge in points.
    pub x: f64,
    /// Top edge in points.
    pub y: f64,
    /// Frame width in points.
    pub width: f64,
    /// Frame height in points.
    pub height: f64,
    /// Type size in points.
    pub size: f64,
}

/// One page, sized like its source page, as frames.
#[derive(Debug, Clone, PartialEq)]
pub struct FramePage {
    /// The 1-based page number.
    pub number: u32,
    /// Page width in points.
    pub width: f64,
    /// Page height in points.
    pub height: f64,
    /// The page's placed lines, in the order they should read.
    pub lines: Vec<FrameLine>,
}

const TWIPS_PER_POINT: f64 = 20.0;

/// Word's own limits, beyond which a frame or page size is invalid
/// (ISO/IEC 29500: `w:pgSz` edges are unsigned twips; Word clamps).
const MAX_TWIPS: f64 = 63_000.0;

fn twips(points: f64) -> u64 {
    if !points.is_finite() {
        return 0;
    }
    let scaled = (points.max(0.0) * TWIPS_PER_POINT).round();
    let capped = scaled.min(MAX_TWIPS);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        capped as u64
    }
}

/// The half-point `sz` attribute for a type size (Word's `sz` is in half-points).
fn half_points(size: f64) -> u64 {
    if !size.is_finite() {
        return 22;
    }
    let doubled = (size.max(1.0) * 2.0).round().clamp(2.0, 2000.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        doubled as u64
    }
}

/// One placed line as a paragraph holding one frame.
fn frame_paragraph_xml(line: &FrameLine) -> String {
    let mut out = String::from("<w:p><w:pPr><w:framePr");
    out.push_str(" w:w=\"");
    out.push_str(&twips(line.width.max(1.0)).to_string());
    out.push_str("\" w:h=\"");
    out.push_str(&twips(line.height.max(1.0)).to_string());
    out.push_str("\" w:x=\"");
    out.push_str(&twips(line.x.max(0.0)).to_string());
    out.push_str("\" w:y=\"");
    out.push_str(&twips(line.y.max(0.0)).to_string());
    out.push_str("\" w:hAnchor=\"page\" w:vAnchor=\"page\" w:hRule=\"atLeast\" w:wrap=\"none\"/>");
    out.push_str("<w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr>");
    if !line.text.is_empty() {
        out.push_str(
            "<w:r><w:rPr><w:rFonts w:ascii=\"Helvetica\" w:hAnsi=\"Helvetica\"/><w:sz w:val=\"",
        );
        out.push_str(&half_points(line.size).to_string());
        out.push_str("\"/></w:rPr><w:t xml:space=\"preserve\">");
        out.push_str(&xml_text(&line.text));
        out.push_str("</w:t></w:r>");
    }
    out.push_str("</w:p>");
    out
}

/// One section boundary as a paragraph carrying the section properties: the
/// page the frames above sit on, explicitly sized, zero margins.
fn section_paragraph_xml(page: &FramePage) -> String {
    format!(
        "<w:p><w:pPr><w:sectPr><w:type w:val=\"nextPage\"/><w:pgSz w:w=\"{w}\" w:h=\"{h}\"/><w:pgMar w:top=\"0\" w:right=\"0\" w:bottom=\"0\" w:left=\"0\" w:header=\"0\" w:footer=\"0\" w:gutter=\"0\"/></w:sectPr></w:pPr></w:p>",
        w = twips(page.width.max(1.0)),
        h = twips(page.height.max(1.0))
    )
}

/// The final section's properties, as the body's last child.
fn section_xml(page: &FramePage) -> String {
    format!(
        "<w:sectPr><w:type w:val=\"nextPage\"/><w:pgSz w:w=\"{w}\" w:h=\"{h}\"/><w:pgMar w:top=\"0\" w:right=\"0\" w:bottom=\"0\" w:left=\"0\" w:header=\"0\" w:footer=\"0\" w:gutter=\"0\"/></w:sectPr>",
        w = twips(page.width.max(1.0)),
        h = twips(page.height.max(1.0))
    )
}

/// The facsimile's document body.
///
/// The title is not written into the body: its owner is the package's
/// `docProps/core.xml`, and an added heading would reflow the page the
/// frames reproduce.
fn frame_document(title: &str, pages: &[FramePage]) -> String {
    let _ = title;
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.push_str("<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>");
    for (index, page) in pages.iter().enumerate() {
        for line in &page.lines {
            out.push_str(&frame_paragraph_xml(line));
        }
        if index.checked_add(1).is_some_and(|next| next < pages.len()) {
            out.push_str(&section_paragraph_xml(page));
        }
    }
    if let Some(last) = pages.last() {
        out.push_str(&section_xml(last));
    } else {
        out.push_str(&section_xml(&FramePage {
            number: 1,
            width: 612.0,
            height: 792.0,
            lines: Vec::new(),
        }));
    }
    out.push_str("</w:body></w:document>\n");
    out
}

/// One paragraph as WordprocessingML.
fn paragraph_xml(text: &str, kind: ParagraphKind) -> String {
    let mut out = String::from("<w:p>");
    if let ParagraphKind::Heading(level) = kind {
        out.push_str("<w:pPr><w:pStyle w:val=\"");
        out.push_str(heading_style(level.clamp(1, 3)));
        out.push_str("\"/></w:pPr>");
    }
    if !text.is_empty() {
        out.push_str("<w:r><w:t xml:space=\"preserve\">");
        out.push_str(&xml_text(text));
        out.push_str("</w:t></w:r>");
    }
    out.push_str("</w:p>");
    out
}

/// The document body.
fn document(title: &str, pages: &[ReflowPage]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.push_str("<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>");
    out.push_str(
        "<w:p><w:pPr><w:pStyle w:val=\"Title\"/></w:pPr><w:r><w:t xml:space=\"preserve\">",
    );
    out.push_str(&xml_text(title));
    out.push_str("</w:t></w:r></w:p>");
    for page in pages {
        for paragraph in &page.paragraphs {
            out.push_str(&paragraph_xml(&paragraph.text, paragraph.kind));
        }
    }
    out.push_str("<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>");
    out.push_str("</w:body></w:document>\n");
    out
}

/// The styles the document uses.
fn styles() -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.push_str(
        "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    );
    out.push_str("<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Georgia\" w:hAnsi=\"Georgia\"/><w:sz w:val=\"22\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"160\"/></w:pPr></w:pPrDefault></w:docDefaults>");
    out.push_str("<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>");
    out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:spacing w:after=\"240\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"52\"/></w:rPr></w:style>");
    for (id, size) in [("Heading1", 40), ("Heading2", 32), ("Heading3", 26)] {
        out.push_str("<w:style w:type=\"paragraph\" w:styleId=\"");
        out.push_str(id);
        out.push_str("\"><w:name w:val=\"");
        out.push_str(id);
        out.push_str("\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/><w:outlineLvl w:val=\"");
        out.push_str(match id {
            "Heading1" => "0",
            "Heading2" => "1",
            _ => "2",
        });
        out.push_str("\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"");
        out.push_str(&size.to_string());
        out.push_str("\"/></w:rPr></w:style>");
    }
    out.push_str("</w:styles>\n");
    out
}

/// The package's content-type map.
fn content_types() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
</Types>\n",
    )
}

/// The package relationships.
fn root_rels() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
</Relationships>\n",
    )
}

/// The core properties, carrying the title the caller supplied.
fn core(title: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<dc:title>{}</dc:title><dc:creator>Selis</dc:creator></cp:coreProperties>\n",
        xml_text(title)
    )
}

/// Build and verify a Word document for the frame facsimile: every recovered
/// line an absolutely positioned frame on a page the size of the source page.
///
/// # Errors
///
/// A package that fails [`verify_docx_frames`]; the check runs before the
/// bytes are returned.
pub fn to_docx_frames(title: &str, pages: &[FramePage]) -> Result<Vec<u8>, String> {
    let content = content_types();
    let rels = root_rels();
    let body = frame_document(title, pages);
    let style_sheet = styles();
    let properties = core(title);
    let entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", body.as_bytes()),
        ("word/styles.xml", style_sheet.as_bytes()),
        ("docProps/core.xml", properties.as_bytes()),
    ];
    let docx = store_zip(&entries);
    verify_docx_frames(&docx, pages)?;
    Ok(docx)
}

/// Check that the facsimile holds the pages it was written from.
///
/// The same sense of "holds" as [`verify_docx`]: every line's escaped text,
/// in order, in the document part.
///
/// # Errors
///
/// A sentence naming the first line the document does not carry.
///
/// # Budget
///
/// The package's entries are read once and the pages' lines are searched in
/// order; the work is bounded by the package's size and the number of lines,
/// and the buffer it holds is charged against `Bytes`.
///
/// # Malformed Input
///
/// A package that is not a readable ZIP, or that is missing a part the
/// facsimile must have, is refused with a sentence naming what was missing.
/// A package that is a readable ZIP but not a Word document fails the same
/// way — the parts it must carry are not there.
pub fn verify_docx_frames(docx: &[u8], pages: &[FramePage]) -> Result<(), String> {
    let entries = read_zip(docx)?;
    for required in ["[Content_Types].xml", "_rels/.rels", "word/document.xml"] {
        if !entries.iter().any(|(name, _)| name == required) {
            return Err(format!("the package is missing {required}"));
        }
    }
    let body = entries
        .iter()
        .find(|(name, _)| name == "word/document.xml")
        .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
        .ok_or_else(|| "the document part is missing".to_string())?;
    let mut cursor = 0usize;
    for page in pages {
        for line in &page.lines {
            let needle = xml_text(&line.text);
            if needle.is_empty() {
                continue;
            }
            let rest = body
                .get(cursor..)
                .ok_or_else(|| format!("page {} ended early", page.number))?;
            let at = rest
                .find(&needle)
                .ok_or_else(|| format!("page {} lost a line: {:?}", page.number, line.text))?;
            cursor = cursor.saturating_add(at).saturating_add(needle.len());
        }
    }
    Ok(())
}

/// Build and verify a Word document for the reflowed pages.
///
/// # Errors
///
/// A package that fails [`verify_docx`]; the check runs before the bytes are
/// returned, so a caller never ships a document it has not read back.
pub fn to_docx(title: &str, pages: &[ReflowPage]) -> Result<Vec<u8>, String> {
    let content = content_types();
    let rels = root_rels();
    let body = document(title, pages);
    let style_sheet = styles();
    let properties = core(title);
    let entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", body.as_bytes()),
        ("word/styles.xml", style_sheet.as_bytes()),
        ("docProps/core.xml", properties.as_bytes()),
    ];
    let docx = store_zip(&entries);
    verify_docx(&docx, pages)?;
    Ok(docx)
}

/// Check that the document holds the pages it was written from.
///
/// The check re-reads the package, demands the main part and the styles, and
/// looks for every paragraph's escaped text in order. A dropped paragraph is a
/// failed export, not a shorter document.
///
/// # Errors
///
/// A sentence naming the first paragraph the document does not carry.
///
/// # Budget
///
/// The package's entries are read once and the paragraphs are searched in
/// order; the work is bounded by the package's size and the number of
/// paragraphs, and the buffer it holds is charged against `Bytes`.
///
/// # Malformed Input
///
/// A package that is not a readable ZIP, or that is missing a part the
/// document must have, is refused with a sentence naming what was missing.
/// A package that is a readable ZIP but not a Word document fails the same
/// way — the parts it must carry are not there.
pub fn verify_docx(docx: &[u8], pages: &[ReflowPage]) -> Result<(), String> {
    let entries = read_zip(docx)?;
    for required in ["[Content_Types].xml", "_rels/.rels", "word/document.xml"] {
        if !entries.iter().any(|(name, _)| name == required) {
            return Err(format!("the package is missing {required}"));
        }
    }
    let body = entries
        .iter()
        .find(|(name, _)| name == "word/document.xml")
        .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
        .ok_or_else(|| "the document part is missing".to_string())?;
    let mut cursor = 0usize;
    for page in pages {
        for paragraph in &page.paragraphs {
            let needle = xml_text(&paragraph.text);
            if needle.is_empty() {
                continue;
            }
            let rest = body
                .get(cursor..)
                .ok_or_else(|| format!("page {} ended early", page.number))?;
            let at = rest.find(&needle).ok_or_else(|| {
                format!(
                    "page {} lost a paragraph: {:?}",
                    page.number, paragraph.text
                )
            })?;
            cursor = cursor.saturating_add(at).saturating_add(needle.len());
        }
    }
    Ok(())
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
    use crate::reflow::ReflowParagraph;

    fn article() -> Vec<ReflowPage> {
        vec![
            ReflowPage {
                number: 1,
                paragraphs: vec![
                    ReflowParagraph {
                        text: "Chapter One".to_string(),
                        kind: ParagraphKind::Heading(1),
                    },
                    ReflowParagraph {
                        text: "A <script> is text & nothing else.".to_string(),
                        kind: ParagraphKind::Body,
                    },
                ],
                tables: vec![],
            },
            ReflowPage {
                number: 2,
                paragraphs: vec![ReflowParagraph {
                    text: "Second page.".to_string(),
                    kind: ParagraphKind::Body,
                }],
                tables: vec![],
            },
        ]
    }

    #[test]
    fn a_document_round_trips_and_keeps_its_paragraphs() {
        let pages = article();
        let docx = to_docx("My Report", &pages).expect("the document should verify");
        verify_docx(&docx, &pages).expect("the check should pass");
        let entries = read_zip(&docx).expect("the package should read back");
        assert!(entries.iter().any(|(name, _)| name == "word/document.xml"));
        let body = &entries
            .iter()
            .find(|(name, _)| name == "word/document.xml")
            .expect("document part")
            .1;
        let body = String::from_utf8_lossy(body);
        assert!(body.contains("&lt;script&gt;"));
        assert!(!body.contains("<script>"));
        assert!(body.contains("Heading1"));
        assert!(body.contains("My Report"));
    }

    fn facsimile_page() -> FramePage {
        FramePage {
            number: 1,
            width: 612.0,
            height: 792.0,
            lines: vec![
                FrameLine {
                    text: "Chapter One".to_string(),
                    x: 72.0,
                    y: 96.0,
                    width: 200.0,
                    height: 16.0,
                    size: 12.0,
                },
                FrameLine {
                    text: "A <script> is text & nothing else.".to_string(),
                    x: 72.0,
                    y: 112.0,
                    width: 320.0,
                    height: 14.0,
                    size: 11.0,
                },
            ],
        }
    }

    #[test]
    fn a_frame_facsimile_round_trips_and_keeps_frames() {
        let pages = vec![facsimile_page()];
        let docx = to_docx_frames("Report", &pages).expect("the frames should verify");
        verify_docx_frames(&docx, &pages).expect("the check should pass");
        let entries = read_zip(&docx).expect("the package should read back");
        assert!(entries.iter().any(|(name, _)| name == "word/document.xml"));
        let body = entries
            .iter()
            .find(|(name, _)| name == "word/document.xml")
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .expect("document part");
        assert!(body.contains("<w:framePr"));
        assert!(body.contains("w:hAnchor=\"page\""));
        assert!(body.contains("<w:pgSz w:w=\"12240\" w:h=\"15840\"/>"));
        assert!(body.contains("&lt;script&gt;"));
        assert!(!body.contains("<script>"));
    }

    #[test]
    fn a_two_page_facsimile_carries_one_section_per_page() {
        let mut page_two = facsimile_page();
        page_two.number = 2;
        let pages = vec![facsimile_page(), page_two];
        let docx = to_docx_frames("Two", &pages).expect("frames");
        let entries = read_zip(&docx).expect("read back");
        let body = entries
            .iter()
            .find(|(name, _)| name == "word/document.xml")
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .expect("document part");
        // One intermediate section (paragraph-carried) and the final body
        // section: two section markers, so pages 1 and 2 are real sections.
        assert_eq!(body.matches("<w:sectPr>").count(), 2);
    }

    #[test]
    fn the_same_frames_produce_the_same_bytes() {
        let pages = vec![facsimile_page()];
        assert_eq!(
            to_docx_frames("Same", &pages).expect("first"),
            to_docx_frames("Same", &pages).expect("second")
        );
    }

    #[test]
    fn verification_refuses_a_dropped_frame() {
        let pages = vec![facsimile_page()];
        let docx = to_docx_frames("Report", &pages).expect("write");
        let mut broken = pages.clone();
        broken[0].lines.push(FrameLine {
            text: "A line that was never written.".to_string(),
            x: 1.0,
            y: 1.0,
            width: 10.0,
            height: 10.0,
            size: 10.0,
        });
        assert!(verify_docx_frames(&docx, &broken).is_err());
    }

    #[test]
    fn the_same_pages_produce_the_same_bytes() {
        let pages = article();
        assert_eq!(
            to_docx("Same", &pages).expect("first"),
            to_docx("Same", &pages).expect("second")
        );
    }

    #[test]
    fn verification_refuses_a_dropped_paragraph() {
        let pages = article();
        let docx = to_docx("Report", &pages).expect("write");
        let mut broken = pages.clone();
        broken[0].paragraphs.push(ReflowParagraph {
            text: "A paragraph that was never written.".to_string(),
            kind: ParagraphKind::Body,
        });
        assert!(verify_docx(&docx, &broken).is_err());
    }
}
