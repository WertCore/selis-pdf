//! Emit laid-out pages as a PDF via the full-document writer (WRITE.01).
//!
//! Every page carries a proper `/Resources` dictionary referencing five
//! standard-14 Type 1 font objects with `/Encoding /WinAnsiEncoding`; the
//! content stream shows WinAnsi byte strings produced by the layout encoder.
//! Pages with images also carry an `/XObject` dict: JPEGs (`/DCTDecode`) and
//! PNGs (`/FlateDecode`) become `Image` streams, while SVG data URIs lowered by
//! the `svg` subset become `Form` streams with their own `/BBox` and font
//! resources. Each is drawn at its measured rectangle with `cm` + `Do`.

use selis_bytes::Bytes;
use selis_error::Result;
use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::blocks::Block;
use crate::image::{EmbeddedImage, EmbeddedSvg, ImageSource};
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
/// one content stream and shared font resources per page (plus an `/XObject`
/// dict of JPEG/PNG image streams when images were embedded), classic xref and
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
    let font_dict = Obj::Dict(font_pairs);
    let res_num = take_num(&mut b);
    b.add_object(
        res_num,
        Obj::Dict(vec![(bytes(b"Font"), font_dict.clone())]),
    );
    let res_ref = Ref::new(res_num, 0);

    let mut object_count = 6u64;
    for page in &pages {
        // Allocate one XObject per image on this page and remember the
        // content-stream placement (resource name, rectangle and the scale
        // from the XObject's own space to that rectangle).
        let mut xobjects: Vec<(Bytes, Obj)> = Vec::new();
        let mut placements: Vec<(String, f64, f64, f64, f64, f64)> = Vec::new();
        for line in &page.lines {
            for item in &line.items {
                if let Item::Image {
                    x,
                    top,
                    w,
                    h,
                    image,
                } = item
                {
                    let name = format!("Im{}", placements.len().saturating_add(1));
                    let num = take_num(&mut b);
                    let (object, scale_x, scale_y) = match image {
                        ImageSource::Raster(raster) => (image_xobject(raster), *w, *h),
                        ImageSource::Svg(svg) => (
                            form_xobject(svg, &font_dict),
                            *w / svg.width_pt,
                            *h / svg.height_pt,
                        ),
                    };
                    b.add_object(num, object);
                    object_count = object_count.saturating_add(1);
                    xobjects.push((bytes(name.as_bytes()), Obj::Ref(Ref::new(num, 0))));
                    placements.push((name, *x, *top, *h, scale_x, scale_y));
                }
            }
        }

        let mut cb = ContentBuilder::new();
        // Images/forms first, under text. An image XObject is a unit square, so
        // the CTM scales it straight to the measured rectangle; a form XObject
        // already has a natural size (its /BBox), so the CTM scales from that
        // size to the rectangle. Both place their bottom-left at the origin.
        for (name, x, top, h, scale_x, scale_y) in &placements {
            cb.save_state()
                .concat(*scale_x, 0.0, 0.0, *scale_y, *x, page_h - top - h)
                .draw_image(name)
                .restore_state();
        }
        // Rectangles next (backgrounds under text).
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
        // `text-decoration` rules last: a line-through must cross the glyphs,
        // and an underline stays just below the baseline. Each is a filled
        // rectangle in the run's own fill colour.
        for line in &page.lines {
            for item in &line.items {
                if let Item::Decoration {
                    x,
                    top,
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
        // Pages with images need their own `/Resources` carrying an `/XObject`
        // dict alongside the shared fonts; image-free pages keep the shared
        // resource object.
        let page_resources = if xobjects.is_empty() {
            res_ref
        } else {
            let num = take_num(&mut b);
            b.add_object(
                num,
                Obj::Dict(vec![
                    (bytes(b"Font"), font_dict.clone()),
                    (bytes(b"XObject"), Obj::Dict(xobjects)),
                ]),
            );
            object_count = object_count.saturating_add(1);
            Ref::new(num, 0)
        };
        b.add_page_with_extra(
            page_w,
            page_h,
            &[Ref::new(content_num, 0)],
            Some(page_resources),
            extra,
        );
        object_count = object_count.saturating_add(2);
    }

    g.charge(Resource::Objects, object_count)?;
    b.write(budget, g)
}

/// Build an image XObject from a validated JPEG or PNG.
///
/// JPEG bytes are embedded verbatim behind `/Filter /DCTDecode`; a PNG's
/// `image.data` is already Flate-encoded raw samples behind
/// `/Filter /FlateDecode`. `/Width`, `/Height` and `/ColorSpace` come from the
/// parsed source (SOF for JPEG, IHDR for PNG), so a reader can render the stream
/// without guessing its geometry.
///
/// # Budget
///
/// The caller charges the written stream bytes through `DocumentBuilder::write`
/// and one object-resolution unit per XObject via `object_count`.
///
/// # Malformed Input
///
/// None: `image` is validated by `image::parse_data_uri` before it reaches
/// here; a malformed JPEG or PNG never produces an XObject.
fn image_xobject(image: &EmbeddedImage) -> Obj {
    Obj::Stream {
        dict: vec![
            (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"Image"))),
            (bytes(b"Width"), Obj::Int(i64::from(image.width))),
            (bytes(b"Height"), Obj::Int(i64::from(image.height))),
            (
                bytes(b"ColorSpace"),
                Obj::Name(bytes(image.color_space.as_bytes())),
            ),
            (bytes(b"BitsPerComponent"), Obj::Int(8)),
            (bytes(b"Filter"), Obj::Name(bytes(image.filter.as_bytes()))),
            (
                bytes(b"Length"),
                Obj::Int(i64::try_from(image.data.len()).unwrap_or(i64::MAX)),
            ),
        ],
        data: Bytes::copy_from_slice(&image.data),
    }
}

/// Build a Form XObject from an SVG lowered by the SVG subset parser.
///
/// The form's `/BBox` is the SVG's natural canvas in points; its content stream
/// already contains the y-flip and every shape operator. The form carries its
/// own `/Resources /Font` (the page's standard-14 map) because the SVG content
/// uses the same `F1`–`F4` keys as page text. The emitter places it at the image
/// box with a `cm` that divides by the natural canvas size.
///
/// # Budget
///
/// The caller charges the written stream bytes through `DocumentBuilder::write`
/// and one object-resolution unit for the XObject via `object_count`.
///
/// # Malformed Input
///
/// None: `svg` is validated and lowered by `svg::parse_embedded_svg` before it
/// reaches here; a refused SVG never produces a Form XObject.
fn form_xobject(svg: &EmbeddedSvg, font_dict: &Obj) -> Obj {
    Obj::Stream {
        dict: vec![
            (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"Form"))),
            (
                bytes(b"BBox"),
                Obj::Array(vec![
                    real(0.0),
                    real(0.0),
                    real(svg.width_pt),
                    real(svg.height_pt),
                ]),
            ),
            (
                bytes(b"Resources"),
                Obj::Dict(vec![(bytes(b"Font"), font_dict.clone())]),
            ),
            (
                bytes(b"Length"),
                Obj::Int(i64::try_from(svg.content.len()).unwrap_or(i64::MAX)),
            ),
        ],
        data: Bytes::copy_from_slice(&svg.content),
    }
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
