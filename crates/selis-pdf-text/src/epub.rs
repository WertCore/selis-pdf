//! EPUB 3 packaging for the reflowed article.
//!
//! This is core, not a tool wrapper: chapters, navigation, the package document,
//! the stored-ZIP container and the read-back check all live here, so the CLI,
//! the browser chunk, the desktop app and the SDK produce byte-identical books
//! without any of them owning a copy of the format. The only thing a caller
//! supplies is the reflowed pages — which is why a page that recovered no text
//! is an empty chapter rather than a picture of one.
//!
//! The container is deliberately unambitious: stored entries (no compression),
//! a fixed date stamp, entries in a fixed order and `mimetype` first. That is
//! the one ordering rule an OCF reader enforces, and the fixed stamp is what
//! makes the same input produce the same bytes.

use crate::reflow::{escape_html, ParagraphKind, ReflowPage};

pub use selis_archive::{read_zip, read_zip as read_stored_zip, store_zip};

/// The media type the first entry must carry, exactly.
const MIMETYPE: &[u8] = b"application/epub+zip";

/// `META-INF/container.xml`: the only path OCF fixes.
const CONTAINER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#;

/// The fixed modification stamp; a clock-derived one would break reproducibility.
const MODIFIED: &str = "1970-01-01T00:00:00Z";

/// The ZIP name of one page's chapter.
fn chapter_path(number: u32) -> String {
    format!("OEBPS/text/page-{number}.xhtml")
}

/// One chapter, as XHTML with every paragraph escaped.
fn chapter(page: &ReflowPage, title: &str) -> String {
    let mut html = String::new();
    html.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n");
    html.push_str("<html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"und\" lang=\"und\">\n");
    html.push_str("<head><meta charset=\"utf-8\"/><title>");
    html.push_str(&escape_html(&format!("{title} — page {}", page.number)));
    html.push_str("</title></head>\n<body>\n<section class=\"page\">\n");
    for paragraph in &page.paragraphs {
        let text = escape_html(&paragraph.text);
        match paragraph.kind {
            ParagraphKind::Body => {
                html.push_str("<p>");
                html.push_str(&text);
                html.push_str("</p>\n");
            }
            ParagraphKind::Heading(level) => {
                let digit = char::from(b'0'.saturating_add(level.clamp(1, 3)));
                html.push_str("<h");
                html.push(digit);
                html.push('>');
                html.push_str(&text);
                html.push_str("</h");
                html.push(digit);
                html.push_str(">\n");
            }
        }
    }
    html.push_str("</section>\n</body>\n</html>\n");
    html
}

/// The navigation document: one entry per chapter, in page order.
fn nav(title: &str, pages: &[ReflowPage]) -> String {
    let escaped = escape_html(title);
    let mut html = String::new();
    html.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n");
    html.push_str("<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"und\" lang=\"und\">\n");
    html.push_str("<head><meta charset=\"utf-8\"/><title>");
    html.push_str(&escaped);
    html.push_str("</title></head>\n<body>\n<nav epub:type=\"toc\" id=\"toc\"><h1>");
    html.push_str(&escaped);
    html.push_str("</h1>\n<ol>\n");
    for page in pages {
        let number = page.number.to_string();
        html.push_str("<li><a href=\"text/page-");
        html.push_str(&number);
        html.push_str(".xhtml\">Page ");
        html.push_str(&number);
        html.push_str("</a></li>\n");
    }
    html.push_str("</ol></nav>\n</body>\n</html>\n");
    html
}

/// The package document: metadata, the manifest, and the spine in page order.
fn opf(title: &str, pages: &[ReflowPage]) -> String {
    let escaped = escape_html(title);
    let mut opf = String::new();
    opf.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    opf.push_str("<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"selis-id\">\n");
    opf.push_str("<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n");
    opf.push_str("<dc:identifier id=\"selis-id\">urn:selis:epub:");
    opf.push_str(&escaped);
    opf.push_str("</dc:identifier>\n<dc:title>");
    opf.push_str(&escaped);
    opf.push_str("</dc:title>\n<dc:language>und</dc:language>\n");
    opf.push_str("<meta property=\"dcterms:modified\">");
    opf.push_str(MODIFIED);
    opf.push_str("</meta>\n</metadata>\n<manifest>\n");
    opf.push_str("<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n");
    for page in pages {
        let number = page.number.to_string();
        opf.push_str("<item id=\"page-");
        opf.push_str(&number);
        opf.push_str("\" href=\"text/page-");
        opf.push_str(&number);
        opf.push_str(".xhtml\" media-type=\"application/xhtml+xml\"/>\n");
    }
    opf.push_str("</manifest>\n<spine>\n<itemref idref=\"nav\"/>\n");
    for page in pages {
        opf.push_str("<itemref idref=\"page-");
        opf.push_str(&page.number.to_string());
        opf.push_str("\"/>\n");
    }
    opf.push_str("</spine>\n</package>\n");
    opf
}

/// Check a package by reading it back.
///
/// The mimetype must be the first entry with its exact bytes, the container must
/// name the package document, the package document must list every chapter, and
/// every paragraph's escaped text must appear in its own chapter, in order. A
/// dropped paragraph is therefore a failure, not a shorter book.
pub fn verify_epub(epub: &[u8], pages: &[ReflowPage]) -> Result<(), String> {
    let entries = read_stored_zip(epub)?;
    if entries.first().map(|(name, _)| name.as_str()) != Some("mimetype") {
        return Err("the first entry is not mimetype".to_string());
    }
    if entries.first().map(|(_, data)| data.as_slice()) != Some(MIMETYPE) {
        return Err("the mimetype is not application/epub+zip".to_string());
    }
    let entry = |wanted: &str| {
        entries
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, data)| String::from_utf8_lossy(data))
    };
    let container =
        entry("META-INF/container.xml").ok_or_else(|| "the container is missing".to_string())?;
    if !container.contains("OEBPS/content.opf") {
        return Err("the container does not name the package document".to_string());
    }
    let package =
        entry("OEBPS/content.opf").ok_or_else(|| "the package document is missing".to_string())?;
    for page in pages {
        let href = format!("text/page-{}.xhtml", page.number);
        if !package.contains(&href) {
            return Err(format!("the package document does not list {href}"));
        }
        let body = entry(&chapter_path(page.number)).ok_or_else(|| format!("{href} is missing"))?;
        let mut from = 0usize;
        for paragraph in &page.paragraphs {
            let escaped = escape_html(&paragraph.text);
            // An empty paragraph has nothing to find: `find("")` returns 0 for
            // every position, so checking it would never advance and a blank
            // line in the document would hang the verifier forever.
            if escaped.is_empty() {
                continue;
            }
            let rest = body
                .get(from..)
                .ok_or_else(|| format!("{href} ended early"))?;
            let at = rest.find(&escaped).ok_or_else(|| {
                format!("page {}: a paragraph is missing from {href}", page.number)
            })?;
            from = from.saturating_add(at).saturating_add(escaped.len());
        }
    }
    Ok(())
}

/// Build and verify an EPUB 3 package for the reflowed pages.
///
/// # Errors
///
/// A package that fails [`verify_epub`]; the check runs before the bytes are
/// returned, so a caller never ships a book it has not read back.
pub fn to_epub(title: &str, pages: &[ReflowPage]) -> Result<Vec<u8>, String> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(pages.len().saturating_add(4));
    entries.push(("mimetype".to_string(), MIMETYPE.to_vec()));
    entries.push((
        "META-INF/container.xml".to_string(),
        CONTAINER.as_bytes().to_vec(),
    ));
    entries.push((
        "OEBPS/content.opf".to_string(),
        opf(title, pages).into_bytes(),
    ));
    entries.push((
        "OEBPS/nav.xhtml".to_string(),
        nav(title, pages).into_bytes(),
    ));
    for page in pages {
        entries.push((chapter_path(page.number), chapter(page, title).into_bytes()));
    }
    let borrowed: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();
    let epub = store_zip(&borrowed);
    verify_epub(&epub, pages)?;
    Ok(epub)
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
    use crate::reflow::{ParagraphKind, ReflowPage};

    fn book() -> Vec<ReflowPage> {
        vec![
            ReflowPage {
                number: 1,
                paragraphs: vec![
                    ReflowParagraph {
                        text: "Chapter One".to_string(),
                        kind: ParagraphKind::Heading(1),
                    },
                    ReflowParagraph {
                        text: "A <script> is just text here & so is this.".to_string(),
                        kind: ParagraphKind::Body,
                    },
                ],
            },
            ReflowPage {
                number: 2,
                paragraphs: vec![ReflowParagraph {
                    text: "Second page, second chapter.".to_string(),
                    kind: ParagraphKind::Body,
                }],
            },
        ]
    }

    use crate::reflow::ReflowParagraph;

    #[test]
    fn a_package_round_trips_and_keeps_its_paragraphs() {
        let pages = book();
        let epub = to_epub("My Book", &pages).expect("the package should verify");
        let entries = read_stored_zip(&epub).expect("the package should read back");
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0].0, "mimetype");
        assert_eq!(entries[0].1, MIMETYPE);
        assert_eq!(entries[1].0, "META-INF/container.xml");
        verify_epub(&epub, &pages).expect("the check should pass");
        let chapter = String::from_utf8_lossy(
            &entries
                .iter()
                .find(|(name, _)| name == "OEBPS/text/page-1.xhtml")
                .expect("the chapter should exist")
                .1,
        );
        assert!(chapter.contains("&lt;script&gt;"));
        assert!(!chapter.contains("<script>"));
    }

    #[test]
    fn the_same_pages_produce_the_same_bytes() {
        let pages = book();
        let first = to_epub("My Book", &pages).expect("the package should verify");
        let second = to_epub("My Book", &pages).expect("the package should verify");
        assert_eq!(first, second);
    }

    #[test]
    fn a_missing_chapter_is_refused() {
        let pages = book();
        let epub = to_epub("My Book", &pages).expect("the package should verify");
        let mut broken = pages.clone();
        broken.push(ReflowPage {
            number: 3,
            paragraphs: vec![ReflowParagraph {
                text: "A page that was never written.".to_string(),
                kind: ParagraphKind::Body,
            }],
        });
        assert!(verify_epub(&epub, &broken).is_err());
    }
}
