//! XFDF export: the annotations a document carries, as Adobe's XML.
//!
//! XFDF is the format every annotation tool can read. Exporting it from the
//! annotation MODEL — rather than from a DOM in the web layer — means the
//! file says exactly what the document says, and the same writer serves the
//! CLI, the tools page and the editor.
//!
//! The subset written here is the markup this crate owns: highlight,
//! underline, strikeout, note, ink and square, with their rectangles,
//! colours, comments, authors, quads, reply back-references and review
//! states. Forms (FDF fields) are not markup and are not written.

use selis_error::Result;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

use crate::{list_annotations, open};

/// Write every annotation in the document as XFDF.
///
/// The page numbers in the result are ZERO-based, the convention the engine
/// uses everywhere else; each `<annots>` element says so in its `page`
/// attribute, as the format requires.
///
/// # Budget
///
/// The caller's budget bounds the parse and the walk of every page.
///
/// # Malformed Input
///
/// A document that cannot be parsed is refused with `MALFORMED`. A page with
/// a broken annotation is skipped: the other pages still have a file.
pub fn export_xfdf(src: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<String> {
    let (_doc, pages) = open(src, budget, g)?;
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n<annots>\n");
    for index in 0..pages.len() {
        let page = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
        // A page whose annotations cannot be listed contributes nothing; the
        // rest of the document still has a file.
        let Ok(found) = list_annotations(src, page, budget, g) else {
            continue;
        };
        for annotation in &found {
            write_annotation(&mut out, index, annotation);
        }
    }
    out.push_str("</annots>\n</xfdf>\n");
    Ok(out)
}

/// Write one annotation element.
fn write_annotation(out: &mut String, page: usize, annotation: &crate::PageAnnotation) {
    let element = match annotation.subtype.as_str() {
        "Highlight" => "highlight",
        "Underline" => "underline",
        "StrikeOut" => "strikeout",
        "Text" => "text",
        "Ink" => "ink",
        "Square" => "square",
        other => other,
    };
    out.push('<');
    out.push_str(element);
    out.push_str(&format!(" page=\"{page}\""));
    let rect = annotation
        .rect
        .iter()
        .map(|value| format!("{value:.3}"))
        .collect::<Vec<_>>()
        .join(",");
    out.push_str(&format!(" rect=\"{rect}\""));
    let colour = annotation
        .colour
        .iter()
        .map(|value| {
            let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            format!("{byte:02X}")
        })
        .collect::<String>();
    out.push_str(&format!(" color=\"#{colour}\""));
    if !annotation.contents.is_empty() {
        out.push_str(&format!(" contents=\"{}\"", escape(&annotation.contents)));
    }
    if !annotation.author.is_empty() {
        out.push_str(&format!(" author=\"{}\"", escape(&annotation.author)));
    }
    if let Some(name) = &annotation.name {
        out.push_str(&format!(" name=\"{}\"", escape(name)));
    }
    if let Some(irt) = annotation.irt {
        out.push_str(&format!(" irt=\"{irt}\""));
    }
    if let Some(state) = &annotation.state {
        out.push_str(&format!(" state=\"{}\"", escape(state)));
    }
    if let Some(model) = &annotation.state_model {
        out.push_str(&format!(" stateModel=\"{}\"", escape(model)));
    }
    if !annotation.quads.is_empty() {
        let quads = annotation
            .quads
            .iter()
            .flat_map(|quad| quad.iter())
            .map(|value| format!("{value:.3}"))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!(" quads=\"{quads}\""));
    }
    out.push_str("/>\n");
}

/// Escape the XML markup characters and the control characters XML cannot
/// carry. The engine writes XFDF from untrusted document bytes, so a comment
/// with a `<` in it must stay text rather than become a tag.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if c.is_control() => out.push_str(&format!("&#{:x};", c as u32)),
            c => out.push(c),
        }
    }
    out
}
