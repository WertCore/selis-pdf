//! The `selis img2pdf` tool (SL-1A.TOOL.08): JPEG/PNG images → PDF.
//!
//! Pure generation on the full-document writer (WRITE.01): each image becomes
//! one page, embedded as an 8-bit DeviceRGB image XObject via
//! `embed_image_rgba`. With `--page-size fit` (the default) the page is the
//! image size plus margins; with `letter`/`a4` the image scales to fit the
//! printable area and is centred.

use selis_pdf_cos::doc_writer::{embed_image_rgba, DocumentBuilder};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::Budget;

use crate::{read_file, CliError, CliResult};

/// The page-size mode for image pages.
pub(crate) enum PageFit {
    /// The page is the image size plus margins.
    Fit,
    /// US Letter: 612 × 792 pt.
    Letter,
    /// A4: 595.276 × 841.89 pt.
    A4,
}

impl PageFit {
    pub(crate) fn parse(s: &str) -> CliResult<PageFit> {
        match s {
            "fit" => Ok(PageFit::Fit),
            "letter" => Ok(PageFit::Letter),
            "a4" => Ok(PageFit::A4),
            other => Err(CliError(format!(
                "unknown page size `{other}` (expected fit, letter or a4)"
            ))),
        }
    }

    fn dims(&self) -> Option<(f64, f64)> {
        match self {
            PageFit::Fit => None,
            PageFit::Letter => Some((612.0, 792.0)),
            PageFit::A4 => Some((595.276, 841.89)),
        }
    }
}

/// Convert images to a PDF, one page per image (SL-1A.TOOL.08).
pub(crate) fn img2pdf(
    inputs: &[String],
    output: &str,
    fit: &PageFit,
    margin: f64,
) -> CliResult<()> {
    if inputs.is_empty() {
        return Err(CliError(
            "img2pdf needs at least one input image".to_string(),
        ));
    }
    if margin < 0.0 {
        return Err(CliError("margin must be >= 0".to_string()));
    }
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let mut builder = DocumentBuilder::new();

    for path in inputs {
        let data = read_file(path)?;
        let image =
            selis_image::decode(&data, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
        let img_w = f64::from(image.width);
        let img_h = f64::from(image.height);
        if img_w <= 0.0 || img_h <= 0.0 {
            return Err(CliError(format!("{path}: empty image")));
        }
        let img_ref = embed_image_rgba(&mut builder, image.width, image.height, &image.rgba);

        // Page size: fixed (letter/a4) or fitted to the image plus margins.
        let (page_w, page_h) = fit
            .dims()
            .unwrap_or((img_w + 2.0 * margin, img_h + 2.0 * margin));

        // Draw size: for fixed pages, scale to fit the printable area.
        let (draw_w, draw_h) = match fit.dims() {
            None => (img_w, img_h),
            Some((w, h)) => {
                let avail_w = (w - 2.0 * margin).max(1.0);
                let avail_h = (h - 2.0 * margin).max(1.0);
                let scale = (avail_w / img_w).min(avail_h / img_h).min(1.0);
                (img_w * scale, img_h * scale)
            }
        };
        let x = (page_w - draw_w) / 2.0;
        let y = (page_h - draw_h) / 2.0;

        // Content stream: place the image under a scaled CTM.
        let content = format!(
            "q\n{} 0 0 {} {} {} cm\n/Im{} Do\nQ\n",
            fmt(draw_w),
            fmt(draw_h),
            fmt(x),
            fmt(y),
            img_ref.num
        );
        let content_num = take_num(&mut builder);
        builder.add_object(
            content_num,
            Obj::Stream {
                dict: vec![(
                    bytes(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                data: selis_bytes::Bytes::from(content.into_bytes()),
            },
        );

        // Resources: /XObject << /Im<n> ref >>.
        let res_num = take_num(&mut builder);
        builder.add_object(
            res_num,
            Obj::Dict(vec![(
                bytes(b"XObject"),
                Obj::Dict(vec![(
                    bytes(format!("Im{}", img_ref.num).as_bytes()),
                    Obj::Ref(Ref::new(img_ref.num, 0)),
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

    let bytes = builder
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    eprintln!("wrote {} page(s) to {output}", inputs.len());
    Ok(())
}

/// Allocate the builder's next object number.
fn take_num(b: &mut DocumentBuilder) -> u32 {
    let n = *b.next_num_mut();
    *b.next_num_mut() = n.saturating_add(1);
    n
}

fn bytes(v: &[u8]) -> selis_bytes::Bytes {
    selis_bytes::Bytes::copy_from_slice(v)
}

/// Format a coordinate without exponent notation.
fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s
    }
}
