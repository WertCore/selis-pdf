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
        // Text in runs of a single fill colour. `rg` is a graphics-state
        // operator, so it is emitted between text objects rather than inside
        // a `BT`…`ET` block; this keeps every reader happy.
        let mut active_color: Option<[f64; 3]> = None;
        let mut in_text = false;
        for line in &page.lines {
            for item in &line.items {
                if let Item::Text {
                    x,
                    font,
                    size,
                    bytes: text,
                    color,
                } = item
                {
                    if active_color != Some(*color) {
                        if in_text {
                            cb.end_text();
                        }
                        cb.set_fill(color[0], color[1], color[2]);
                        cb.begin_text();
                        in_text = true;
                        active_color = Some(*color);
                    }
                    cb.set_font(&font_key(font), *size)
                        .set_text_matrix(*x, page_h - line.baseline)
                        .show_raw(text);
                }
            }
        }
        if in_text {
            cb.end_text();
        }

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

        // One genuine /Link annotation per link box. The action is
        // `/A << /S /URI /URI (url) >>` so a reader can actually click it and
        // a re-parser can assert its target.
        let mut annots: Vec<Obj> = Vec::new();
        for line in &page.lines {
            for item in &line.items {
                if let Item::LinkBox { x, top, w, h, uri } = item {
                    let num = take_num(&mut b);
                    b.add_object(
                        num,
                        Obj::Dict(vec![
                            (bytes(b"Type"), Obj::Name(bytes(b"Annot"))),
                            (bytes(b"Subtype"), Obj::Name(bytes(b"Link"))),
                            (
                                bytes(b"Rect"),
                                Obj::Array(vec![
                                    real(*x),
                                    real(page_h - top - h),
                                    real(x + w),
                                    real(page_h - top),
                                ]),
                            ),
                            (
                                bytes(b"Border"),
                                Obj::Array(vec![real(0.0), real(0.0), real(0.0)]),
                            ),
                            (
                                bytes(b"A"),
                                Obj::Dict(vec![
                                    (bytes(b"S"), Obj::Name(bytes(b"URI"))),
                                    (bytes(b"URI"), Obj::String(bytes(uri.as_bytes()))),
                                ]),
                            ),
                        ]),
                    );
                    annots.push(Obj::Ref(Ref::new(num, 0)));
                    object_count = object_count.saturating_add(1);
                }
            }
        }
        let extra = if annots.is_empty() {
            Vec::new()
        } else {
            vec![(b"Annots".to_vec(), Obj::Array(annots))]
        };
        b.add_page_with_extra(
            page_w,
            page_h,
            &[Ref::new(content_num, 0)],
            Some(res_ref),
            extra,
        );
        object_count = object_count.saturating_add(2);
    }

    g.charge(Resource::Objects, object_count)?;
    b.write(budget, g)
}

/// A real number object with millipoint precision, used for annotation rects.
fn real(v: f64) -> Obj {
    if !v.is_finite() {
        return Obj::Int(0);
    }
    let max = 9_223_372_036_854_775_807i64;
    let scaled = (v * 1000.0).round();
    let scaled = if scaled >= max as f64 {
        max
    } else {
        #[allow(clippy::cast_possible_truncation)]
        {
            scaled as i64
        }
    };
    Obj::Real { scaled, scale: 3 }
}

/// The standard-14 resource key (`F1`–`F5`) for a font name; unknown names
/// fall back to `F1` (Helvetica).
fn font_key(name: &str) -> String {
    let idx = FONTS.iter().position(|f| *f == name).unwrap_or(0);
    format!("F{}", idx.saturating_add(1))
}

/// Allocate the builder's next object number.
pub(crate) fn take_num(b: &mut DocumentBuilder) -> u32 {
    let n = *b.next_num_mut();
    *b.next_num_mut() = n.saturating_add(1);
    n
}

/// Copy a byte buffer into the document's own storage.
///
/// # Budget
///
/// The copy is charged against `Bytes` by the builder it is written into.
///
/// # Malformed Input
///
/// None: every byte sequence is a valid object body. A buffer that is not
/// valid UTF-8 or not valid PDF is refused later, where the format is
/// known.
pub(crate) fn bytes(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}
