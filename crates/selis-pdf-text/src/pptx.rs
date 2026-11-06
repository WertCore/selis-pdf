//! PowerPoint (`.pptx`) packaging for extracted pages (Phase 3 — PDF → PowerPoint).
//!
//! One slide per page, each recovered line a text box placed where the page put
//! it. The page canvas is the PDF page at 72 DPI with `/Rotate` applied, so a
//! box lands where the raster export paints the same words. That is the honest
//! fidelity this format can hold without rendering the page: the text is real,
//! editable and positioned, while images, colour, vector art and exact font
//! faces are not carried.
//!
//! Packaging and the read-back check live here, in the core crate, so the CLI,
//! the browser chunk and the desktop app produce the same deck.

#![allow(clippy::arithmetic_side_effects)]

use selis_archive::{read_zip, store_zip};
use selis_geom::Matrix;

use crate::assembly::TextLine;
use crate::xml::xml_text;

/// The media type a `.pptx` carries.
pub const PPTX_MEDIA_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";

/// English Metric Units per point.
const EMU_PER_POINT: f64 = 12_700.0;

/// One placed run of text on a slide.
#[derive(Debug, Clone, PartialEq)]
pub struct TextShape {
    /// The text the box holds.
    pub text: String,
    /// Left edge in page points (y-down, origin top-left).
    pub x: f64,
    /// Top edge in page points.
    pub y: f64,
    /// Box width in points.
    pub width: f64,
    /// Box height in points.
    pub height: f64,
    /// Type size in points.
    pub size: f64,
}

/// One slide, sized like its source page.
#[derive(Debug, Clone, PartialEq)]
pub struct Slide {
    /// The 1-based page number the slide came from.
    pub number: u32,
    /// Canvas width in points.
    pub width: f64,
    /// Canvas height in points.
    pub height: f64,
    /// The page's text boxes.
    pub shapes: Vec<TextShape>,
}

/// Recovered lines as positioned text boxes.
///
/// `ctm` is the page's user-space → device transform at 72 DPI (the engine's
/// `PageView::ctm`), so the box edges are the ones the raster export paints.
#[must_use]
pub fn slide_from_lines(
    number: u32,
    width: f64,
    height: f64,
    ctm: Matrix,
    lines: &[TextLine],
    texts: &[String],
) -> Slide {
    let mut shapes: Vec<TextShape> = Vec::with_capacity(lines.len());
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
        shapes.push(TextShape {
            text: text.to_string(),
            x: x.max(0.0),
            y: y.max(0.0),
            width,
            height,
            size,
        });
    }
    Slide {
        number,
        width: width.max(1.0),
        height: height.max(1.0),
        shapes,
    }
}

fn emu(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    let scaled = value * EMU_PER_POINT;
    if scaled >= i64::MAX as f64 {
        i64::MAX
    } else if scaled <= i64::MIN as f64 {
        i64::MIN
    } else {
        #[allow(clippy::cast_possible_truncation)]
        {
            scaled as i64
        }
    }
}

fn content_types(slides: &[Slide]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/>\
<Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml\"/>\
<Override PartName=\"/ppt/slideLayouts/slideLayout1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml\"/>\
<Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>",
    );
    for index in 1..=slides.len() {
        out.push_str(&format!(
            "<Override PartName=\"/ppt/slides/slide{index}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slide+xml\"/>"
        ));
    }
    out.push_str("</Types>\n");
    out
}

fn root_rels() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"ppt/presentation.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
</Relationships>\n",
    )
}

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

fn presentation(slides: &[Slide]) -> String {
    let width = slides
        .iter()
        .map(|slide| slide.width)
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let height = slides
        .iter()
        .map(|slide| slide.height)
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<p:presentation xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\">\
<p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst><p:sldIdLst>",
    );
    for index in 1..=slides.len() {
        out.push_str(&format!(
            "<p:sldId id=\"{}\" r:id=\"rId{}\"/>",
            255 + index,
            index + 1
        ));
    }
    out.push_str(&format!(
        "</p:sldIdLst><p:sldSz cx=\"{}\" cy=\"{}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>\n",
        emu(width),
        emu(height)
    ));
    out
}

fn presentation_rels(slides: &[Slide]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/>",
    );
    for index in 1..=slides.len() {
        out.push_str(&format!(
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide\" Target=\"slides/slide{index}.xml\"/>",
            index + 1
        ));
    }
    out.push_str("</Relationships>\n");
    out
}

fn theme() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Selis\">\
<a:themeElements>\
<a:clrScheme name=\"Selis\"><a:dk1><a:srgbClr val=\"000000\"/></a:dk1><a:lt1><a:srgbClr val=\"FFFFFF\"/></a:lt1><a:dk2><a:srgbClr val=\"1F1F1F\"/></a:dk2><a:lt2><a:srgbClr val=\"EFEFEF\"/></a:lt2><a:accent1><a:srgbClr val=\"2F5597\"/></a:accent1><a:accent2><a:srgbClr val=\"C00000\"/></a:accent2><a:accent3><a:srgbClr val=\"70AD47\"/></a:accent3><a:accent4><a:srgbClr val=\"FFC000\"/></a:accent4><a:accent5><a:srgbClr val=\"5B9BD5\"/></a:accent5><a:accent6><a:srgbClr val=\"A5A5A5\"/></a:accent6><a:hlink><a:srgbClr val=\"0563C1\"/></a:hlink><a:folHlink><a:srgbClr val=\"954F72\"/></a:folHlink></a:clrScheme>\
<a:fontScheme name=\"Selis\"><a:majorFont><a:latin typeface=\"Calibri Light\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont><a:minorFont><a:latin typeface=\"Calibri\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont></a:fontScheme>\
<a:fmtScheme name=\"Selis\"><a:fillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme>\
</a:themeElements></a:theme>\n",
    )
}

fn empty_tree() -> &'static str {
    "<p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr></p:spTree>"
}

fn slide_master() -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<p:sldMaster xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\">\
<p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg>",
    );
    out.push_str(empty_tree());
    out.push_str(
        "</p:cSld><p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>\
<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst>\
<p:txStyles><p:titleStyle><a:lvl1pPr><a:defRPr sz=\"4400\"/></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr><a:defRPr sz=\"1800\"/></a:lvl1pPr></p:bodyStyle><p:otherStyle><a:lvl1pPr><a:defRPr sz=\"1800\"/></a:lvl1pPr></p:otherStyle></p:txStyles>\
</p:sldMaster>\n",
    );
    out
}

fn slide_master_rels() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme\" Target=\"../theme/theme1.xml\"/>\
</Relationships>\n",
    )
}

fn slide_layout() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<p:sldLayout xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" type=\"blank\" preserve=\"1\">\
<p:cSld name=\"Blank\">{}</p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>\n",
        empty_tree()
    )
}

fn slide_layout_rels() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster\" Target=\"../slideMasters/slideMaster1.xml\"/>\
</Relationships>\n",
    )
}

fn slide(slide: &Slide) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<p:sld xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"><p:cSld><p:spTree>\
<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>",
    );
    for (index, shape) in slide.shapes.iter().enumerate() {
        let id = index.saturating_add(2);
        let size = shape.size.clamp(1.0, 400.0);
        let points = (size * 100.0).round();
        #[allow(clippy::cast_possible_truncation)]
        let size_attr = if points >= 4000.0 {
            4000
        } else if points <= 100.0 {
            100
        } else {
            points as i64
        };
        out.push_str(&format!(
            "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"Text {id}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></p:spPr>\
<p:txBody><a:bodyPr wrap=\"none\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\"/><a:lstStyle/>\
<a:p><a:pPr algn=\"l\"/><a:r><a:rPr lang=\"en-US\" sz=\"{size_attr}\" dirty=\"0\"/><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp>",
            emu(shape.x),
            emu(shape.y),
            emu(shape.width),
            emu(shape.height),
            xml_text(&shape.text)
        ));
    }
    out.push_str("</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>\n");
    out
}

fn slide_rels() -> String {
    String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>\
</Relationships>\n",
    )
}

/// Build and verify a PowerPoint deck for the extracted slides.
///
/// # Errors
///
/// A package that fails [`verify_pptx`]; the check runs before the bytes are
/// returned, so a caller never ships a deck it has not read back.
pub fn to_pptx(title: &str, slides: &[Slide]) -> Result<Vec<u8>, String> {
    let content = content_types(slides);
    let root = root_rels();
    let pres = presentation(slides);
    let pres_rels = presentation_rels(slides);
    let core_props = core(title);
    let master = slide_master();
    let master_rels = slide_master_rels();
    let layout = slide_layout();
    let layout_rels = slide_layout_rels();
    let theme_xml = theme();
    let slide_bodies: Vec<String> = slides.iter().map(slide).collect();
    let slide_rel = slide_rels();

    let mut owned: Vec<(String, &[u8])> =
        Vec::with_capacity(slides.len().saturating_mul(2).saturating_add(9));
    owned.push(("[Content_Types].xml".to_string(), content.as_bytes()));
    owned.push(("_rels/.rels".to_string(), root.as_bytes()));
    owned.push(("docProps/core.xml".to_string(), core_props.as_bytes()));
    owned.push(("ppt/presentation.xml".to_string(), pres.as_bytes()));
    owned.push((
        "ppt/_rels/presentation.xml.rels".to_string(),
        pres_rels.as_bytes(),
    ));
    owned.push((
        "ppt/slideMasters/slideMaster1.xml".to_string(),
        master.as_bytes(),
    ));
    owned.push((
        "ppt/slideMasters/_rels/slideMaster1.xml.rels".to_string(),
        master_rels.as_bytes(),
    ));
    owned.push((
        "ppt/slideLayouts/slideLayout1.xml".to_string(),
        layout.as_bytes(),
    ));
    owned.push((
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels".to_string(),
        layout_rels.as_bytes(),
    ));
    owned.push(("ppt/theme/theme1.xml".to_string(), theme_xml.as_bytes()));
    for (index, body) in slide_bodies.iter().enumerate() {
        owned.push((
            format!("ppt/slides/slide{}.xml", index.saturating_add(1)),
            body.as_bytes(),
        ));
        owned.push((
            format!("ppt/slides/_rels/slide{}.xml.rels", index.saturating_add(1)),
            slide_rel.as_bytes(),
        ));
    }
    let entries: Vec<(&str, &[u8])> = owned
        .iter()
        .map(|(name, data)| (name.as_str(), *data))
        .collect();
    let pptx = store_zip(&entries);
    verify_pptx(&pptx, slides)?;
    Ok(pptx)
}

/// Check that the deck holds the slides and text it was written from.
///
/// # Errors
///
/// A sentence naming the first slide or text the package does not carry.
pub fn verify_pptx(pptx: &[u8], slides: &[Slide]) -> Result<(), String> {
    let entries = read_zip(pptx)?;
    let has = |name: &str| entries.iter().any(|(entry, _)| entry == name);
    for required in [
        "[Content_Types].xml",
        "ppt/presentation.xml",
        "ppt/slideLayouts/slideLayout1.xml",
        "ppt/slideMasters/slideMaster1.xml",
    ] {
        if !has(required) {
            return Err(format!("the package is missing {required}"));
        }
    }
    for (index, slide) in slides.iter().enumerate() {
        let name = format!("ppt/slides/slide{}.xml", index.saturating_add(1));
        let body = entries
            .iter()
            .find(|(entry, _)| entry == &name)
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .ok_or_else(|| format!("slide {} is missing from the package", slide.number))?;
        let mut cursor = 0usize;
        for shape in &slide.shapes {
            let needle = xml_text(&shape.text);
            if needle.is_empty() {
                continue;
            }
            let rest = body
                .get(cursor..)
                .ok_or_else(|| format!("slide {} ended early", slide.number))?;
            let at = rest
                .find(&needle)
                .ok_or_else(|| format!("slide {} lost text: {:?}", slide.number, shape.text))?;
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
    use selis_bytes::Bytes;
    use selis_geom::{Point, Rect};
    use selis_pdf_content::text::TextGlyph;

    fn glyph(code: u16, size: f64, x: f64) -> TextGlyph {
        TextGlyph {
            advance: size * 0.5,
            code,
            at: Point::new(x, 0.0),
            tm: Matrix::IDENTITY,
            font: Bytes::copy_from_slice(b"F1"),
            size,
            space: size * 0.25,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
        }
    }

    fn line(text: &str, x: f64, y: f64) -> TextLine {
        let glyphs = text
            .chars()
            .enumerate()
            .map(|(index, ch)| glyph(ch as u16, 10.0, x + 5.0 * index as f64))
            .collect();
        TextLine {
            words: vec![crate::assembly::TextWord {
                runs: vec![crate::assembly::TextRun {
                    glyphs,
                    bbox: Rect::new(x, y, x + 40.0, y + 10.0),
                }],
                bbox: Rect::new(x, y, x + 40.0, y + 10.0),
            }],
            bbox: Rect::new(x, y, x + 40.0, y + 10.0),
        }
    }

    fn deck() -> Vec<Slide> {
        vec![
            slide_from_lines(
                1,
                612.0,
                792.0,
                Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 792.0),
                &[line("Title of the deck", 72.0, 700.0)],
                &["Title of the deck".to_string()],
            ),
            slide_from_lines(
                2,
                612.0,
                792.0,
                Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 792.0),
                &[line("A < b & c", 72.0, 650.0)],
                &["A < b & c".to_string()],
            ),
        ]
    }

    #[test]
    fn a_deck_round_trips_and_keeps_its_text() {
        let slides = deck();
        let pptx = to_pptx("Deck", &slides).expect("the deck should verify");
        verify_pptx(&pptx, &slides).expect("the check should pass");
        let entries = read_zip(&pptx).expect("read back");
        let second = &entries
            .iter()
            .find(|(name, _)| name == "ppt/slides/slide2.xml")
            .expect("slide 2")
            .1;
        let second = String::from_utf8_lossy(second);
        assert!(second.contains("A &lt; b &amp; c"));
        assert!(!second.contains("<p:sp><p:nvSpPr><p:cNvPr id=\"1\""));
    }

    #[test]
    fn boxes_are_placed_by_the_page_transform() {
        let slides = deck();
        assert_eq!(slides[0].shapes.len(), 1);
        // y=700 in user space becomes 92 px from the top at 72 DPI.
        assert!((slides[0].shapes[0].y - 82.0).abs() < 0.001);
        assert_eq!(slides[0].shapes[0].width, 40.0);
    }

    #[test]
    fn the_same_slides_produce_the_same_bytes() {
        let slides = deck();
        assert_eq!(
            to_pptx("Deck", &slides).expect("first"),
            to_pptx("Deck", &slides).expect("second")
        );
    }

    #[test]
    fn verification_refuses_a_dropped_text_box() {
        let slides = deck();
        let pptx = to_pptx("Deck", &slides).expect("write");
        let mut broken = slides.clone();
        broken[0].shapes.push(TextShape {
            text: "never written".to_string(),
            x: 1.0,
            y: 1.0,
            width: 10.0,
            height: 10.0,
            size: 10.0,
        });
        assert!(verify_pptx(&pptx, &broken).is_err());
    }
}
