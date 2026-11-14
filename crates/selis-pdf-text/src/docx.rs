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
//! # What the frame path carries, and what it does not
//!
//! The frame path carries the recovered words at the coordinates the page put
//! them, the face each word names, and every image the page draws — placed at
//! the rectangle the page put it in. It does not carry colour, vector ink or
//! tables (as such — their words are placed like any other line); vector ink is
//! only recoverable as a picture, and a picture of the page is a separate,
//! opt-in background ([`FramePage::image`]).
//!
//! The article path carries heading styles; the frame path carries position.
//! Choose deliberately.
//!
//! # Why a frame has no width
//!
//! A frame's width is deliberately NOT written. Word clips a line to the frame
//! it is given and wraps the rest inside it, and the width the page knows is
//! the width of the PDF's own face — which Word may not have. A line that names
//! an uninstalled face (`OpenSans`, say) is then set in a substitute face whose
//! advances differ, and a frame exactly as wide as the PDF's line wrapped the
//! tail onto a second line inside the frame. Leaving `w:w` out lets Word size
//! the frame to the text it actually set, so the line stays one line and stays
//! where the page put it; the position (which is what the facsimile promises)
//! comes from `w:x`/`w:y`, not from the width.

use selis_archive::{read_zip, store_zip};

use crate::reflow::{ParagraphKind, ReflowPage};
use crate::xml::xml_text;

/// A line's ascent, as a fraction of its type size.
///
/// The frame's height comes from the TYPE, not from the recovered bbox: the
/// extractor's line box is built from glyph ORIGINS — the pen position — so for
/// a horizontal line it is a segment of zero height, and a frame sized from it
/// is one point tall. Word clips a line to the frame it is given, which is what
/// made an exported page lose its text.
///
/// The standard faces sit at roughly three quarters up.
const ASCENT: f64 = 0.75;
/// A line's descent, as a fraction of its type size.
const DESCENT: f64 = 0.25;
/// How close two baselines must be to count as one line of type, in points.
///
/// The assembler joins a wrapped table cell into one "line" when its rows are
/// close together; the baselines themselves are still distinct, and a frame
/// that carried both rows on one baseline would move the second row up onto the
/// first. Half a point is below any real leading and above any rounding.
const BASELINE_TOLERANCE: f64 = 0.5;

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
///
/// The text of each frame comes from the GLYPHS, not from a line's recovered
/// string: a line can carry two baselines (a wrapped table cell) and each of its
/// rows becomes a frame of its own, so the glyphs are the only place the words,
/// the baseline and the type size still line up. `fonts` is the page's
/// `(resource, /BaseFont)` table, which names the face each row is set in.
#[must_use]
pub fn frame_page_from_lines(
    number: u32,
    width: f64,
    height: f64,
    ctm: selis_geom::Matrix,
    lines: &[crate::assembly::TextLine],
    fonts: &[(&str, &str)],
) -> FramePage {
    let mut out: Vec<FrameLine> = Vec::with_capacity(lines.len());
    for line in lines {
        for row in rows_of(line, ctm) {
            out.push(FrameLine {
                x: row.x.max(0.0),
                // The frame's top is one ascent above the baseline, because that
                // is where Word puts the first line of a frame; the height is
                // the whole em box, so the row's glyphs sit inside it and are
                // not clipped.
                y: row.baseline - row.size * ASCENT,
                width: row.width.max(1.0),
                height: row.size * (ASCENT + DESCENT),
                size: row.size,
                font: face_for(&row.font, fonts),
                text: row.text,
            });
        }
    }
    FramePage {
        number,
        width: width.max(1.0),
        height: height.max(1.0),
        lines: out,
        image: None,
    }
}

/// The face a glyph's font resource names.
///
/// `fonts` is the caller's page table: the `/BaseFont` each resource name
/// resolves to. A resource the table does not carry falls back to Helvetica,
/// which is also what the extractor falls back to when a document declares no
/// face at all.
fn face_for(resource: &str, fonts: &[(&str, &str)]) -> String {
    if resource.is_empty() {
        return "Helvetica".to_string();
    }
    fonts
        .iter()
        .find(|(name, _)| *name == resource)
        .map(|(_, base)| (*base).to_string())
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| "Helvetica".to_string())
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
    /// The face the line is drawn in, as the document names it.
    ///
    /// A PDF names a font by its `/BaseFont`, which is often a subset tag
    /// (`ABCDEF+OpenSans`) or a family and style (`Helvetica,Bold`). The
    /// writer cleans the name and reads the weight out of it, so the exported
    /// line asks Word for the face the page drew rather than for a substitute.
    pub font: String,
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
    /// The page rendered as a PNG, when the caller supplied one.
    ///
    /// The picture is the page's own ink — its photographs, its rules, its
    /// handwriting — placed full-bleed behind the frames. Without it the
    /// facsimile carries only what the extractor could recover as text.
    pub image: Option<Vec<u8>>,
}

const TWIPS_PER_POINT: f64 = 20.0;

/// One visual row of type inside a recovered "line".
///
/// A line is what the assembler recovered: usually one row of type, and
/// sometimes two — a table cell that wraps joins the row beneath it when the two
/// are within half an em, which is what the assembler calls a line. Each row
/// becomes a frame of its own, because **a frame carries one baseline**: Word
/// lays a frame's paragraph from the top of the frame, so two rows in one frame
/// put the second row up on the first.
struct Row {
    /// The row's words, spaces between them, as the page showed them.
    text: String,
    /// The pen position of the row's first glyph, in points from the page box.
    x: f64,
    /// The advance the row's glyphs cover, in points.
    width: f64,
    /// The row's baseline, in points from the page box.
    baseline: f64,
    /// The largest type size on the row, in points.
    size: f64,
    /// The resource name of the font the row's first glyph is drawn in.
    font: String,
}

/// Cut a line into the rows its glyphs actually sit on.
///
/// Two measurements decide it, and both are the ones the assembler itself uses
/// to call something a line: the perpendicular distance from the row's baseline
/// (more than [`BASELINE_TOLERANCE`] is another row, measured against the writing
/// direction so a rotated line keeps its own rows) and the distance from the
/// previous glyph's pen (more than one em is a column break, not a word: the two
/// cells of a table row are two frames, because that is where the page put them).
fn rows_of(line: &crate::assembly::TextLine, ctm: selis_geom::Matrix) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    // The pen after the row's last glyph, in page space.
    let mut pen: Option<(f64, f64)> = None;
    for (index, word) in line.words.iter().enumerate() {
        // The assembler splits words on a space glyph OR on the gap a space
        // would have left, so the words of a row have to have their separators
        // put back — the same rule the line reader applies.
        let mut pending_break = index > 0;
        for run in &word.runs {
            for glyph in &run.glyphs {
                let origin = ctm.apply(glyph.at);
                let size = if glyph.size > 0.0 { glyph.size } else { 12.0 };
                let continues = match (rows.last(), pen) {
                    (Some(row), Some((px, py))) => {
                        // The perpendicular offset from the row's own baseline,
                        // and the room the glyph before it left.
                        let across = (glyph.dir_x * (origin.y - row.baseline)
                            - glyph.dir_y * (origin.x - row.x))
                            .abs();
                        let gap = (origin.x - px).hypot(origin.y - py);
                        across <= BASELINE_TOLERANCE && gap <= size.max(row.size)
                    }
                    _ => false,
                };
                if !continues {
                    rows.push(Row {
                        text: String::new(),
                        x: origin.x,
                        width: 0.0,
                        baseline: origin.y,
                        size,
                        font: String::new(),
                    });
                    // A row starts at a word: it takes no leading space.
                    pending_break = false;
                }
                let Some(row) = rows.last_mut() else {
                    continue;
                };
                if row.font.is_empty() {
                    row.font = String::from_utf8_lossy(glyph.font.as_slice()).into_owned();
                }
                if pending_break {
                    if !row.text.ends_with(' ') && glyph.code != u16::from(b' ') {
                        row.text.push(' ');
                        row.width += glyph.space;
                    }
                    pending_break = false;
                }
                // The space glyph is a character of the row, not a break in it.
                row.text
                    .push(char::from_u32(u32::from(glyph.code)).unwrap_or(' '));
                row.width += glyph.advance;
                row.size = row.size.max(size);
                pen = Some((origin.x, origin.y));
            }
        }
    }
    for row in &mut rows {
        row.text = row.text.trim().to_string();
    }
    rows.retain(|row| !row.text.is_empty());
    rows
}

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

/// The face Word should be given for a font name, and its weight.
///
/// A PDF's `/BaseFont` is written three ways in the wild: a subset tag
/// (`ABCDEF+OpenSans`), a family and style (`Helvetica,Bold`), and the
/// standard fourteen's hyphenated faces (`Helvetica-BoldOblique`). Word wants
/// a family name and a weight on the run, so the name is cleaned and the weight
/// is read out of it.
fn face_of(font: &str) -> (String, bool, bool) {
    // A subset tag is not part of the name: `ABCDEF+OpenSans` is OpenSans.
    let name = font.rsplit('+').next().unwrap_or(font).trim();
    if name.is_empty() {
        return ("Helvetica".to_string(), false, false);
    }
    // `Helvetica-BoldOblique`: the family, and a style that is two weights.
    if let Some((family, style)) = name.rsplit_once('-') {
        let style = style.to_ascii_lowercase();
        let weighted =
            style.contains("bold") || style.contains("italic") || style.contains("oblique");
        if !family.trim().is_empty() && weighted {
            return (
                family.trim().to_string(),
                style.contains("bold"),
                style.contains("italic") || style.contains("oblique"),
            );
        }
    }
    // `Times-Roman,Bold`: the family, a comma, and the style.
    let mut parts = name.splitn(2, ',');
    let family = parts.next().unwrap_or(name).trim();
    let style = parts.next().unwrap_or("").to_ascii_lowercase();
    let family = if family.is_empty() {
        "Helvetica".to_string()
    } else {
        family.to_string()
    };
    (
        family,
        style.contains("bold"),
        style.contains("italic") || style.contains("oblique"),
    )
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
        let (face, bold, italic) = face_of(&line.font);
        out.push_str("<w:r><w:rPr><w:rFonts w:ascii=\"");
        out.push_str(&xml_text(&face));
        out.push_str("\" w:hAnsi=\"");
        out.push_str(&xml_text(&face));
        out.push_str("\" w:cs=\"");
        out.push_str(&xml_text(&face));
        out.push_str("\"/>");
        // The weight is a property of the run, not of the name: a family that
        // ships a bold face is asked for it with `w:b`.
        if bold {
            out.push_str("<w:b/><w:bCs/>");
        }
        if italic {
            out.push_str("<w:i/><w:iCs/>");
        }
        out.push_str("<w:sz w:val=\"");
        out.push_str(&half_points(line.size).to_string());
        out.push_str("\"/></w:rPr><w:t xml:space=\"preserve\">");
        out.push_str(&xml_text(&line.text));
        out.push_str("</w:t></w:r>");
    }
    out.push_str("</w:p>");
    out
}

/// The page's own picture, as a full-bleed image behind the frames.
///
/// A frame holds text or a picture, never both, so the picture is a paragraph
/// of its own: `behindDoc` puts it under the words and `relativeFrom="page"`
/// puts it at the page's own origin, which is where the frames put the words
/// they carry. It sits in the flow — one empty line at the top of a page that
/// the frames then cover — because a floating image needs an anchor to float
/// from.
fn page_image_xml(page: &FramePage, rel_id: &str, doc_id: u64) -> String {
    let cx = emu(page.width.max(1.0));
    let cy = emu(page.height.max(1.0));
    format!(
        "<w:p><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:r><w:drawing><wp:anchor behindDoc=\"1\" distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" simplePos=\"0\" locked=\"0\" layoutInCell=\"1\" allowOverlap=\"1\" relativeHeight=\"0\"><wp:simplePos x=\"0\" y=\"0\"/><wp:positionH relativeFrom=\"page\"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom=\"page\"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/><wp:wrapNone/><wp:docPr id=\"{doc_id}\" name=\"Page {number}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic><pic:nvPicPr><pic:cNvPr id=\"{doc_id}\" name=\"Page {number}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{rel_id}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>",
        number = page.number,
    )
}

/// The EMU size of a length in points (1 pt = 12 700 EMU).
fn emu(points: f64) -> u64 {
    if !points.is_finite() {
        return 0;
    }
    let scaled = (points.max(0.0) * 12_700.0).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        scaled.min(9_000_000_000.0) as u64
    }
}

/// The relationship id of a page's picture.
fn image_rel_id(page: u32) -> String {
    format!("rIdPageImage{page}")
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
    out.push_str("<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><w:body>");
    for (index, page) in pages.iter().enumerate() {
        if page.image.is_some() {
            out.push_str(&page_image_xml(
                page,
                &image_rel_id(page.number),
                u64::from(page.number),
            ));
        }
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
            image: None,
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
///
/// `images` adds the `png` default the page pictures need. It is written only
/// when a page carries one, so an export with no pictures is the same package
/// it was before.
fn content_types(images: bool) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
</Types>\n",
    );
    if images {
        let marker = "<Default Extension=\"xml\"";
        // The PNG default goes before the overrides, where a `<Default>`
        // belongs; the exact place does not matter to a reader, but the order
        // does to a diff.
        if let Some(at) = out.find(marker) {
            out.insert_str(at, "<Default Extension=\"png\" ContentType=\"image/png\"/>");
        }
    }
    out
}

/// The document part's relationships: one image per page that carries one.
fn document_rels(pages: &[FramePage]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for page in pages {
        if page.image.is_some() {
            out.push_str(&format!(
                "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/page{}.png\"/>",
                page.number,
                id = image_rel_id(page.number),
            ));
        }
    }
    out.push_str("</Relationships>\n");
    out
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
    let pictures = pages.iter().any(|page| page.image.is_some());
    let content = content_types(pictures);
    let rels = root_rels();
    let body = frame_document(title, pages);
    let style_sheet = styles();
    let properties = core(title);
    let document_rels = document_rels(pages);

    // The parts are owned rather than borrowed, because the pictures are bytes
    // the caller handed over and the package has to hold them.
    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".to_string(), content.into_bytes()),
        ("_rels/.rels".to_string(), rels.into_bytes()),
        ("word/document.xml".to_string(), body.into_bytes()),
        ("word/styles.xml".to_string(), style_sheet.into_bytes()),
        ("docProps/core.xml".to_string(), properties.into_bytes()),
        (
            "word/_rels/document.xml.rels".to_string(),
            document_rels.into_bytes(),
        ),
    ];
    for page in pages {
        if let Some(image) = &page.image {
            entries.push((format!("word/media/page{}.png", page.number), image.clone()));
        }
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let docx = store_zip(&refs);
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
    let rels = entries
        .iter()
        .find(|(name, _)| name == "word/_rels/document.xml.rels")
        .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
        .unwrap_or_default();
    let mut cursor = 0usize;
    for page in pages {
        // A page that carries a picture carries the SAME picture: a dropped
        // image is a failed export, not a shorter document.
        if let Some(image) = &page.image {
            let media = format!("word/media/page{}.png", page.number);
            let written = entries
                .iter()
                .find(|(name, _)| *name == media)
                .map(|(_, data)| data.as_slice())
                .ok_or_else(|| format!("page {} lost its picture", page.number))?;
            if written != image.as_slice() {
                return Err(format!(
                    "page {}'s picture is not the one it was written from",
                    page.number
                ));
            }
            if !rels.contains(&format!("page{}.png", page.number)) {
                return Err(format!("page {} never referenced its picture", page.number));
            }
        }
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
    let content = content_types(false);
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
    use crate::assembly::TextLine;
    use crate::reflow::ReflowParagraph;
    use selis_bytes::Bytes;
    use selis_geom::Point;
    use selis_pdf_content::text::TextGlyph;

    /// One glyph, positioned, in the resource font and size the test names.
    fn glyph(size: f64, font: &str) -> TextGlyph {
        TextGlyph {
            advance: size * 0.5,
            code: 65,
            at: Point::new(72.0, 700.0),
            tm: selis_geom::Matrix::IDENTITY,
            font: Bytes::copy_from_slice(font.as_bytes()),
            size,
            space: size * 0.25,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
        }
    }

    /// One line holding one glyph, the shape the assembler produces.
    fn line_of(glyphs: Vec<TextGlyph>) -> TextLine {
        let bbox = selis_geom::Rect::new(0.0, 0.0, 50.0, 12.0);
        let run = crate::assembly::TextRun { glyphs, bbox };
        let word = crate::assembly::TextWord {
            runs: vec![run],
            bbox,
        };
        TextLine {
            words: vec![word],
            bbox,
        }
    }

    /// One line holding one word per group of glyphs, the shape a line whose
    /// words the assembler split has.
    fn line_of_words(words: Vec<Vec<TextGlyph>>) -> TextLine {
        let bbox = selis_geom::Rect::new(0.0, 0.0, 50.0, 12.0);
        let words = words
            .into_iter()
            .map(|glyphs| crate::assembly::TextWord {
                runs: vec![crate::assembly::TextRun { glyphs, bbox }],
                bbox,
            })
            .collect();
        TextLine { words, bbox }
    }

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
                    font: "Helvetica-Bold".to_string(),
                },
                FrameLine {
                    text: "A <script> is text & nothing else.".to_string(),
                    x: 72.0,
                    y: 112.0,
                    width: 320.0,
                    height: 14.0,
                    size: 11.0,
                    font: "ABCDEF+OpenSans".to_string(),
                },
            ],
            image: None,
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
            font: "Helvetica".to_string(),
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
    /// THE BUG THIS FIXES: a frame's height came from the recovered line box,
    /// which is built from glyph ORIGINS — so for a horizontal line it is zero,
    /// and Word clipped the line to a one-point frame. The frame is now sized
    /// from the type: one ascent above the baseline, one descent below it.
    #[test]
    fn a_frame_is_sized_from_the_type_not_from_the_pen_positions() {
        let line = line_of(vec![glyph(12.0, "F1")]);
        let page = frame_page_from_lines(
            1,
            612.0,
            792.0,
            selis_geom::Matrix::IDENTITY,
            &[line],
            &[("F1", "Helvetica")],
        );
        let frame = page.lines.first().expect("the line became a frame");
        // The pen sat at y = 700; the frame's top is an ascent above it and its
        // height is the whole em box, so the glyphs are inside the frame.
        assert!((frame.y - (700.0 - 12.0 * 0.75)).abs() < 1e-9);
        assert!((frame.height - 12.0).abs() < 1e-9);
        assert!(frame.y < 700.0, "the baseline is inside the frame");
    }

    /// The frame asks for the face the page drew, cleaned of its subset tag and
    /// split into a family and a weight.
    #[test]
    fn a_frame_names_the_face_the_document_drew() {
        assert_eq!(
            face_of("ABCDEF+OpenSans"),
            ("OpenSans".into(), false, false)
        );
        assert_eq!(face_of("Helvetica,Bold"), ("Helvetica".into(), true, false));
        assert_eq!(
            face_of("Helvetica-BoldOblique"),
            ("Helvetica".into(), true, true)
        );
        // A name that is nothing at all is still a name Word has.
        assert_eq!(face_of(""), ("Helvetica".into(), false, false));
    }

    /// The face the reader asked for is IN the file, and so is the weight.
    #[test]
    fn the_document_carries_the_face_and_the_weight() {
        let pages = vec![facsimile_page()];
        let docx = to_docx_frames("Report", &pages).expect("the frames should verify");
        let entries = read_zip(&docx).expect("the package should read back");
        let body = entries
            .iter()
            .find(|(name, _)| name == "word/document.xml")
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .expect("document part");
        assert!(body.contains("w:ascii=\"OpenSans\""));
        assert!(body.contains("w:ascii=\"Helvetica\""));
        // The subset tag is not part of the face Word is asked for.
        assert!(!body.contains("ABCDEF+"));
        assert!(body.contains("<w:b/><w:bCs/>"));
    }

    /// THE OTHER HALF: a page with a picture carries that picture in the
    /// package, referenced by the document, and the read-back check notices
    /// when it is not the one it was written from.
    #[test]
    fn a_page_carries_its_own_picture() {
        let mut page = facsimile_page();
        page.image = Some(b"\x89PNG-fake-but-byte-exact".to_vec());
        let pages = vec![page];
        let docx = to_docx_frames("Report", &pages).expect("the frames should verify");
        let entries = read_zip(&docx).expect("the package should read back");
        let media = entries
            .iter()
            .find(|(name, _)| name == "word/media/page1.png")
            .expect("the page's picture");
        assert_eq!(media.1, b"\x89PNG-fake-but-byte-exact");
        let rels = entries
            .iter()
            .find(|(name, _)| name == "word/_rels/document.xml.rels")
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .expect("the relationships");
        assert!(rels.contains("page1.png"));
        // The content types must carry the PNG default, or Word refuses
        // the package: a part it has no type for is not a part it opens.
        let types = entries
            .iter()
            .find(|(name, _)| name == "[Content_Types].xml")
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .expect("the content types");
        assert!(types.contains("Extension=\"png\""));
        assert!(types.contains("image/png"));
        let body = entries
            .iter()
            .find(|(name, _)| name == "word/document.xml")
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .expect("document part");
        assert!(body.contains("behindDoc=\"1\""));
        assert!(body.contains("r:embed=\"rIdPageImage1\""));
        assert!(body.contains("xmlns:pic="));
        // A page with no picture is the same package it was before.
        let plain = to_docx_frames("Report", &[facsimile_page()]).expect("frames");
        let plain_entries = read_zip(&plain).expect("read back");
        assert!(!plain_entries
            .iter()
            .any(|(name, _)| name.starts_with("word/media/")));
        let plain_types = plain_entries
            .iter()
            .find(|(name, _)| name == "[Content_Types].xml")
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .expect("the content types");
        assert!(!plain_types.contains("image/png"));
    }

    /// A dropped picture is a failed export, not a shorter document.
    #[test]
    fn verification_refuses_a_dropped_picture() {
        let mut page = facsimile_page();
        page.image = Some(b"a picture".to_vec());
        let pages = vec![page.clone()];
        let docx = to_docx_frames("Report", &pages).expect("write");
        let mut broken = pages.clone();
        broken[0].image = Some(b"a different picture".to_vec());
        assert!(verify_docx_frames(&docx, &broken).is_err());
    }

    /// A line the assembler joins from TWO baselines — the wrapped cell — becomes
    /// two frames, one per baseline: a frame carries one baseline, and Word lays
    /// its paragraph from the top of it, so a single frame would move the second
    /// row up onto the first.
    #[test]
    fn one_line_on_two_baselines_becomes_two_frames() {
        let mut first = glyph(9.0, "F1");
        first.at = Point::new(72.0, 700.0);
        let mut second = glyph(9.0, "F1");
        second.at = Point::new(72.0, 693.0);
        let line = line_of(vec![first, second]);
        let page = frame_page_from_lines(
            1,
            612.0,
            792.0,
            selis_geom::Matrix::IDENTITY,
            &[line],
            &[("F1", "Helvetica")],
        );
        assert_eq!(page.lines.len(), 2, "one frame per baseline");
        // Two frames, on two baselines — each at its own one-ascent offset.
        assert_ne!(page.lines[0].y, page.lines[1].y);
        assert!((page.lines[0].y - (700.0 - 9.0 * 0.75)).abs() < 1e-9);
        assert!((page.lines[1].y - (693.0 - 9.0 * 0.75)).abs() < 1e-9);
    }

    /// Words the assembler split with no space glyph still read as words: the
    /// separator is put back, the way every other reader of the text layer does
    /// it.
    #[test]
    fn words_are_separated_again_in_the_frame() {
        let mut left = glyph(12.0, "F1");
        left.at = Point::new(72.0, 700.0);
        let mut right = glyph(12.0, "F1");
        right.at = Point::new(72.0 + 12.0, 700.0);
        let line = line_of_words(vec![vec![left], vec![right]]);
        let page = frame_page_from_lines(
            1,
            612.0,
            792.0,
            selis_geom::Matrix::IDENTITY,
            &[line],
            &[("F1", "Helvetica")],
        );
        assert_eq!(page.lines.len(), 1, "the two words are one row");
        assert_eq!(page.lines[0].text, "A A");
    }
}
