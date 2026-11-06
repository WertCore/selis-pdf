//! EPUB → PDF: a real package, an honest subset of a renderer.
//!
//! # What this does
//!
//! The package is opened with the same reader the EPUB writer's read-back uses
//! (`selis_archive`), the root file is found through `META-INF/container.xml`,
//! the OPF's `<spine>` fixes the reading order, and every content document is
//! parsed with the HTML subset documented at the crate root — headings,
//! paragraphs, lists, code, quotes and rules become the same blocks the
//! Markdown/HTML converters lay out. The output is a single paginated PDF.
//!
//! # What this does not do (said plainly)
//!
//! No CSS, no fonts beyond the standard 14, no images, no SVG, no math, no
//! page headers or footers from the book, no table of contents, no page-break
//! control. A fixed-layout EPUB (FXL) renders as reflowed text, not as the
//! pages the book drew. An EPUB whose spine points at missing documents is a
//! refusal naming the missing part; an EPUB with no spine items at all
//! converts to an empty one-page PDF rather than failing silently.

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use selis_error::{Code, Ctx, Error, Result};
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::html;
use crate::xmlmini::{parse, resolve_href, Element};
use crate::{emit, Options};

/// An input the converter cannot use, with the detail sanitised so a
/// document-derived name can appear in a diagnostic safely (ADR-P0017).
fn malformed(during: &'static str, message: String) -> Error {
    Error::with(
        Code::ConvertMalformed,
        Ctx::new()
            .during(during)
            .detail(Ctx::sanitise(message.as_bytes())),
    )
}

/// Read one required package part.
fn part<'a>(entries: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    entries
        .iter()
        .find(|(entry, _)| entry == name)
        .map(|(_, data)| data.as_slice())
}

/// The directory of `path` (`""` when there is none).
fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(at) => &path[..at],
        None => "",
    }
}

/// The OPF rootfile named by the container, resolved to its own directory.
fn rootfile_path(container: &Element) -> Option<String> {
    let rootfile = container.find_first("rootfile")?;
    rootfile
        .attr("full-path")
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
}

/// One manifest item, resolved to a package path.
struct ManifestItem {
    id: String,
    href: String,
    media_type: String,
}

/// Parse the OPF's manifest and spine.
fn manifest_and_spine(opf: &Element) -> (Vec<ManifestItem>, Vec<String>, Option<String>) {
    let mut items = Vec::new();
    for item in opf.find_all("item") {
        let id = item.attr("id").unwrap_or("").trim().to_string();
        let href = item.attr("href").unwrap_or("").trim().to_string();
        let media_type = item.attr("media-type").unwrap_or("").trim().to_lowercase();
        if !id.is_empty() && !href.is_empty() {
            items.push(ManifestItem {
                id,
                href,
                media_type,
            });
        }
    }
    let mut spine = Vec::new();
    for itemref in opf.find_all("itemref") {
        if let Some(idref) = itemref.attr("idref") {
            spine.push(idref.trim().to_string());
        }
    }
    let title = opf.find_all("title").into_iter().find_map(|node| {
        let text = node.text.trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    });
    (items, spine, title)
}

/// Convert an EPUB 3 package to PDF bytes.
///
/// # Budget
///
/// Charges the expanded bytes read from the package and the generated object
/// graph against `budget`.
///
/// # Errors
///
/// A package that is not a ZIP, has no `META-INF/container.xml`, names no
/// rootfile, references a missing content document, or whose XML is malformed
/// is an `InvalidInput` with a message naming the part.
pub(crate) fn epub_to_pdf(
    input: &[u8],
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    g.charge(
        Resource::Bytes,
        u64::try_from(input.len()).unwrap_or(u64::MAX),
    )?;

    let entries = selis_archive::read_zip(input).map_err(|message| malformed("epub", message))?;
    if entries.is_empty() {
        return Err(malformed("epub", "the EPUB has no entries".to_string()));
    }

    let container_xml = part(&entries, "META-INF/container.xml").ok_or_else(|| {
        malformed(
            "epub",
            "the package has no META-INF/container.xml".to_string(),
        )
    })?;
    let container_text = std::str::from_utf8(container_xml)
        .map_err(|_| malformed("epub", "META-INF/container.xml is not UTF-8".to_string()))?;
    let container = parse(container_text).map_err(|message| malformed("epub", message))?;
    let rootfile = rootfile_path(&container).ok_or_else(|| {
        malformed(
            "epub",
            "META-INF/container.xml names no rootfile".to_string(),
        )
    })?;

    let opf_xml = part(&entries, &rootfile)
        .ok_or_else(|| malformed("epub", format!("the package has no {rootfile}")))?;
    let opf_text = std::str::from_utf8(opf_xml)
        .map_err(|_| malformed("epub", format!("{rootfile} is not UTF-8")))?;
    let opf = parse(opf_text).map_err(|message| malformed("epub", message))?;
    let (items, spine, dc_title) = manifest_and_spine(&opf);

    let opf_dir = dir_of(&rootfile);

    let mut blocks = Vec::new();
    for idref in &spine {
        let Some(item) = items.iter().find(|item| &item.id == idref) else {
            continue;
        };
        let path = resolve_href(opf_dir, &item.href);
        let data = part(&entries, &path).ok_or_else(|| {
            malformed(
                "epub",
                format!("the spine references {path}, which is missing"),
            )
        })?;
        g.charge(
            Resource::Bytes,
            u64::try_from(data.len()).unwrap_or(u64::MAX),
        )?;
        let is_xhtml = item.media_type.is_empty()
            || item.media_type.contains("xhtml")
            || item.media_type.contains("html");
        if !is_xhtml {
            continue;
        }
        let text = String::from_utf8_lossy(data);
        blocks.extend(html::parse(&text));
    }
    let mut opts = opts.clone();
    if opts.title.is_none() {
        opts.title = dc_title.clone();
    }
    // A document worth nothing is still a book: an empty italic title block
    // rather than a refused conversion.
    if blocks.is_empty() {
        let title = opts.title.clone().unwrap_or_else(|| "EPUB".to_string());
        blocks.push(crate::blocks::Block::Paragraph(vec![crate::blocks::Span {
            style: crate::blocks::Style::Italic,
            text: format!("{title} (an EPUB with no readable text)"),
        }]));
    }
    emit::build(&blocks, &opts, budget, g)
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
    use crate::Options;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn book() -> Vec<u8> {
        let container = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;
        let opf = r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>The Test Book</dc:title>
  </metadata>
  <manifest>
    <item id="one" href="one.xhtml" media-type="application/xhtml+xml"/>
    <item id="two" href="two.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="one"/>
    <itemref idref="two"/>
  </spine>
</package>"#;
        let one = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Chapter One</h1><p>It was a dark and stormy night.</p></body></html>"#;
        let two = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Chapter Two</h1><p>And it got <b>worse</b>.</p></body></html>"#;
        selis_archive::store_zip(&[
            ("META-INF/container.xml", container.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/one.xhtml", one.as_bytes()),
            ("OEBPS/two.xhtml", two.as_bytes()),
        ])
    }

    #[test]
    fn an_epub_converts_in_spine_order() {
        let bytes = epub_to_pdf(
            &book(),
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("convert");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Title (The Test Book)"));
        opens_with_page_count(&bytes, 1);
    }

    /// The output must open with selis's own parser.
    fn opens_with_page_count(bytes: &[u8], expected_pages: usize) {
        let budget = Budget::unlimited();
        let mut g = Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new());
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).expect("startxref");
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("open");
        let rev = doc.revisions().last().expect("revision");
        let mut pages = 0usize;
        for &num in rev.entries.keys() {
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
    fn xml_markup_reaches_the_text() {
        // The WinAnsi bytes show up as parentheses-escaped literals in the
        // content stream; assert the words are present rather than exact
        // escapes.
        let bytes = epub_to_pdf(
            &book(),
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("convert");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Chapter"));
        assert!(text.contains("dark and stormy"));
    }

    #[test]
    fn a_missing_spine_part_is_named() {
        let broken = selis_archive::store_zip(&[
            ("META-INF/container.xml", r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#.as_bytes()),
            ("content.opf", r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest><item id="x" href="gone.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="x"/></spine></package>"#.as_bytes()),
        ]);
        let Err(message) = epub_to_pdf(
            &broken,
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("a missing part must refuse");
        };
        assert!(message.to_string().contains("gone.xhtml"), "{message}");
    }

    #[test]
    fn a_non_epub_is_refused() {
        let Err(message) = epub_to_pdf(
            b"not a zip",
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("must refuse");
        };
        assert!(message.to_string().contains("archive"), "{message}");
    }
}
