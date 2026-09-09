//! Emit laid-out pages as a PDF via the full-document writer (WRITE.01).
//!
//! Every page carries a proper `/Resources` dictionary referencing five
//! standard-14 Type 1 font objects with `/Encoding /WinAnsiEncoding`; the
//! content stream shows WinAnsi byte strings produced by the layout encoder.

use selis_bytes::Bytes;
use selis_error::Result;
use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::blocks::Block;
use crate::layout::{self, Item};
use crate::Options;

/// The standard-14 fonts this writer references, in resource-name order.
const FONTS: [&str; 5] = [
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Courier",
];

/// Build a complete PDF from a block sequence.
///
/// # Budget
///
/// Charges layout bytes, one object-resolution unit per emitted object, and
/// the serialised output against `budget`.
///
/// # Malformed Input
///
/// Content never fails; `BUDGET_*` on exhaustion.
///
/// # Output Guarantees
///
/// A complete single-revision PDF 1.4 document: header, catalog, page tree,
/// one content stream and shared font resources per page, classic xref and
/// trailer. The output opens in this engine and conforming readers.
pub(crate) fn build(
    blocks: &[Block],
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let (page_w, page_h) = opts.page_size.dims();
    let pages = layout::lay_out(blocks, opts.page_size, budget, g)?;

    let mut b = DocumentBuilder::new();
    if let Some(title) = &opts.title {
        if !title.is_empty() {
            b.set_info(b"Title", title);
        }
    }

    // The five font objects, shared by every page.
    let mut font_pairs: Vec<(Bytes, Obj)> = Vec::new();
    for (idx, name) in FONTS.iter().enumerate() {
        let num = take_num(&mut b);
        b.add_object(
            num,
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
                (bytes(b"Subtype"), Obj::Name(bytes(b"Type1"))),
                (bytes(b"BaseFont"), Obj::Name(bytes(name.as_bytes()))),
                (bytes(b"Encoding"), Obj::Name(bytes(b"WinAnsiEncoding"))),
            ]),
        );
        let key = format!("F{}", idx.saturating_add(1));
        font_pairs.push((bytes(key.as_bytes()), Obj::Ref(Ref::new(num, 0))));
    }
    let res_num = take_num(&mut b);
    b.add_object(
        res_num,
        Obj::Dict(vec![(bytes(b"Font"), Obj::Dict(font_pairs))]),
    );
    let res_ref = Ref::new(res_num, 0);

    let mut object_count = 6u64;
    for page in &pages {
        let mut cb = ContentBuilder::new();
        // Rectangles first (backgrounds under text).
        for line in &page.lines {
            for item in &line.items {
                if let Item::Rect {
                    x,
                    top,
                    height: _,
                    w,
                    h,
                    color,
                } = item
                {
                    cb.set_fill(color[0], color[1], color[2]).fill_rect(
                        *x,
                        page_h - top - h,
                        *w,
                        *h,
                    );
                }
            }
        }
        // Text, restoring black after any rectangle fills.
        let mut any_rect = false;
        for line in &page.lines {
            any_rect = any_rect || line.items.iter().any(|i| matches!(i, Item::Rect { .. }));
        }
        if any_rect {
            cb.set_fill(0.0, 0.0, 0.0);
        }
        cb.begin_text();
        for line in &page.lines {
            for item in &line.items {
                if let Item::Text {
                    x,
                    font,
                    size,
                    bytes: text,
                    color: _,
                } = item
                {
                    cb.set_font(&font_key(font), *size)
                        .set_text_matrix(*x, page_h - line.baseline)
                        .show_raw(text);
                }
            }
        }
        cb.end_text();

        let data = cb.to_bytes();
        let content_num = take_num(&mut b);
        b.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(
                    bytes(b"Length"),
                    Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
                )],
                data: Bytes::from(data),
            },
        );
        b.add_page_with(page_w, page_h, &[Ref::new(content_num, 0)], Some(res_ref));
        object_count = object_count.saturating_add(2);
    }

    g.charge(Resource::Objects, object_count)?;
    b.write(budget, g)
}

/// The standard-14 resource key (`F1`–`F5`) for a font name; unknown names
/// fall back to `F1` (Helvetica).
fn font_key(name: &str) -> String {
    let idx = FONTS.iter().position(|f| *f == name).unwrap_or(0);
    format!("F{}", idx.saturating_add(1))
}

/// Allocate the builder's next object number.
fn take_num(b: &mut DocumentBuilder) -> u32 {
    let n = *b.next_num_mut();
    *b.next_num_mut() = n.saturating_add(1);
    n
}

fn bytes(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}
