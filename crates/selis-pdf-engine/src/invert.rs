//! Invert colours: render every page and rebuild the document as a PDF whose
//! pages are the inverted raster.
//!
//! This is a render-based operation — the only honest way to invert a PDF whose
//! colour comes from text, vector fills, shadings and images alike. Each page is
//! rendered to RGBA through the engine, every colour channel is complemented
//! (alpha is kept), and the result is embedded full-bleed as a lossless
//! Flate `DeviceRGB` image on a page of the original size. The output is a new
//! PDF, so a reader gets a dark-on-light document turned light-on-dark exactly
//! as it rendered, not a best-effort content-stream rewrite that would miss
//! images and patterns.
//!
//! The core lives here, in the engine, because it needs both the renderer and
//! the document writer; the CLI, the browser raster chunk and a later desktop
//! shell call it unchanged.

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_pdf_cos::doc_writer::{embed_image_rgba_flate, DocumentBuilder};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, CancelToken, Clock};

use crate::Session;

/// The default render resolution for the inverted output, in DPI.
pub const DEFAULT_INVERT_DPI: u32 = 150;
/// The lowest render resolution the operation accepts.
pub const MIN_INVERT_DPI: u32 = 1;
/// The highest render resolution the operation accepts.
pub const MAX_INVERT_DPI: u32 = 600;

/// Render every page of `src`, invert its colours, and return a new PDF whose
/// pages are the inverted raster at `dpi`.
///
/// # Errors
///
/// A typed engine [`Error`](selis_error::Error) when `dpi` is out of range, the
/// document cannot be opened, has no pages, a page cannot be rendered, or the
/// output cannot be written.
pub fn invert_colors(
    src: Vec<u8>,
    dpi: u32,
    budget: &Budget,
    clock: &dyn Clock,
) -> Result<Vec<u8>> {
    if !(MIN_INVERT_DPI..=MAX_INVERT_DPI).contains(&dpi) {
        return Err(err!(
            Code::ObjUnexpected,
            during = "invert",
            detail = "dpi out of range"
        ));
    }
    let session = Session::open(src, budget, clock)?;
    let pages = session.len();
    if pages == 0 {
        return Err(err!(
            Code::ObjUnexpected,
            during = "invert",
            detail = "document has no pages"
        ));
    }

    let mut g = budget.guard_with(clock, CancelToken::new());
    let mut builder = DocumentBuilder::new();
    let dpi_f = f64::from(dpi);
    let to_points = 72.0 / dpi_f;

    for page in 0..pages {
        let mut rendered = session.render_page_rgba(page, dpi_f, budget, &mut g)?;
        invert_rgba(&mut rendered.rgba);
        let image = embed_image_rgba_flate(
            &mut builder,
            rendered.width,
            rendered.height,
            &rendered.rgba,
            6,
        );
        // The output page is the original size in points; the inverted raster
        // fills it edge to edge (the render already baked in /Rotate).
        let page_w = f64::from(rendered.width) * to_points;
        let page_h = f64::from(rendered.height) * to_points;
        let content = format!(
            "q\n{} 0 0 {} 0 0 cm\n/Im{} Do\nQ\n",
            fmt(page_w),
            fmt(page_h),
            image.num
        );
        let content_num = builder.allocate();
        builder.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(
                    bytes(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                data: Bytes::from(content.into_bytes()),
            },
        );
        let res_num = builder.allocate();
        builder.add_object(
            res_num,
            Obj::Dict(vec![(
                bytes(b"XObject"),
                Obj::Dict(vec![(
                    bytes(format!("Im{}", image.num).as_bytes()),
                    Obj::Ref(Ref::new(image.num, 0)),
                )]),
            )]),
        );
        builder.add_page_with(
            page_w,
            page_h,
            &[Ref::new(content_num, 0)],
            Some(Ref::new(res_num, 0)),
        );
    }

    builder.write(budget, &mut g)
}

/// Copy a byte slice into owned [`Bytes`].
fn bytes(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}

/// Format a coordinate without exponent notation or a trailing point.
fn fmt(v: f64) -> String {
    let text = format!("{v:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.').to_string();
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed
    }
}

/// Complement every colour channel in place, keeping alpha. For a `u8` sample
/// `!x == 255 - x`, so this is the exact colour inversion with no arithmetic.
fn invert_rgba(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        if let [r, g, b, _a] = pixel {
            *r = !*r;
            *g = !*g;
            *b = !*b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_sandbox::FixedClock;

    fn build_xref(objects: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(&object);
        }
        let xref = out.len();
        let size = offsets.len() + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// A blank `pages`-page document whose pages render as opaque white.
    fn blank_pdf(pages: usize) -> Vec<u8> {
        let mut objects = vec![b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()];
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i)).collect();
        objects.push(
            format!(
                "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {pages} >>\nendobj\n",
                kids.join(" ")
            )
            .into_bytes(),
        );
        for i in 0..pages {
            objects.push(
                format!(
                    "{} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n",
                    3 + i
                )
                .into_bytes(),
            );
        }
        build_xref(objects)
    }

    fn center_rgb(page: &crate::session::RenderedPage) -> (u8, u8, u8) {
        let x = page.width / 2;
        let y = page.height / 2;
        let idx = ((y * page.width + x) * 4) as usize;
        (
            page.rgba.get(idx).copied().unwrap_or(0),
            page.rgba.get(idx + 1).copied().unwrap_or(0),
            page.rgba.get(idx + 2).copied().unwrap_or(0),
        )
    }

    #[test]
    fn inverts_rgb_and_keeps_alpha() {
        let mut rgba = vec![10, 20, 30, 255, 0, 0, 0, 128];
        super::invert_rgba(&mut rgba);
        assert_eq!(rgba, vec![245, 235, 225, 255, 255, 255, 255, 128]);
    }

    #[test]
    fn a_white_page_inverts_to_black() {
        let budget = Budget::unlimited();
        let clock = FixedClock(0);
        let src = blank_pdf(1);

        // The input renders as opaque white.
        let session = Session::open(src.clone(), &budget, &clock).expect("open input");
        let mut g = budget.guard_with(&clock, CancelToken::new());
        let input = session
            .render_page_rgba(0, 72.0, &budget, &mut g)
            .expect("render input");
        assert_eq!(
            center_rgb(&input),
            (255, 255, 255),
            "the fixture is white paper"
        );
        drop(g);

        let out = invert_colors(src, 72, &budget, &clock).expect("invert");
        let out_session = Session::open(out, &budget, &clock).expect("open output");
        assert_eq!(out_session.len(), 1, "one page in, one page out");
        let mut g2 = budget.guard_with(&clock, CancelToken::new());
        let rendered = out_session
            .render_page_rgba(0, 72.0, &budget, &mut g2)
            .expect("render output");
        let (r, green, b) = center_rgb(&rendered);
        assert!(
            r <= 5 && green <= 5 && b <= 5,
            "white inverts to black, got {r},{green},{b}"
        );
    }

    #[test]
    fn page_count_is_preserved() {
        let budget = Budget::unlimited();
        let clock = FixedClock(0);
        let out = invert_colors(blank_pdf(3), 72, &budget, &clock).expect("invert");
        let session = Session::open(out, &budget, &clock).expect("open output");
        assert_eq!(session.len(), 3);
    }

    #[test]
    fn a_bad_dpi_is_refused() {
        let budget = Budget::unlimited();
        let clock = FixedClock(0);
        assert!(invert_colors(blank_pdf(1), 0, &budget, &clock).is_err());
        assert!(invert_colors(blank_pdf(1), 10_000, &budget, &clock).is_err());
    }
}
