//! Word (`.docx`) packaging for the reflowed article (Phase 3 — PDF → Word).
//!
//! The document is the same reading-order text the HTML reflow writes: one
//! paragraph per recovered paragraph, headings carrying Word's built-in
//! `Heading1`–`Heading3` styles. Packaging and the read-back check live here, in
//! the core crate, so the CLI, the browser chunk and the desktop app produce the
//! same file without any of them owning a copy of the format.
//!
//! # What this does not claim
//!
//! Layout reconstruction. Word has no page canvas here: positions, columns,
//! images, tables and colour are not carried. A Word export is an editable
//! article, not a facsimile — use the positioned HTML export for the page.

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
            },
            ReflowPage {
                number: 2,
                paragraphs: vec![ReflowParagraph {
                    text: "Second page.".to_string(),
                    kind: ParagraphKind::Body,
                }],
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
