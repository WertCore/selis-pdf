//! Image → PDF (SL-1A.TOOL.08): JPEG and PNG images become PDF pages.
//!
//! This is the shared engine path used by both `selis img2pdf` and the browser
//! tools chunk. Each input is decoded through [`selis_image`] (JPEG via the
//! DCT decoder, PNG through the bounded Flate decoder), composited over white,
//! and embedded as a Flate-compressed `DeviceRGB` image XObject. One image is
//! one page.
//!
//! Page geometry:
//!
//! * `fit` — the page is the image size plus the margin on every side, so the
//!   image is placed at one pixel per point;
//! * `letter` / `a4` — the image is scaled down to fit the printable area and
//!   centred; it is never enlarged.
//!
//! The output is generated, not a rewrite of an input document, so the
//! verification gate asserts the page count implied by the inputs (one page
//! per image) and reports no input page count.

use selis_pdf_cos::doc_writer::{embed_image_rgba_flate, DocumentBuilder};
use selis_pdf_cos::verify::Expectations;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::Budget;

use crate::verify_report::Verification;
use crate::{read_file, CliError, CliResult};

/// The page-size mode for image pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagePageSize {
    /// The page is the image size plus margins.
    Fit,
    /// US Letter: 612 × 792 pt.
    Letter,
    /// A4: 595.276 × 841.89 pt.
    A4,
}

impl ImagePageSize {
    /// Parse the CLI and wire spelling.
    ///
    /// # Errors
    ///
    /// [`crate::ToolError`] naming the accepted values when the spelling is
    /// unknown.
    pub fn parse(value: &str) -> CliResult<Self> {
        match value {
            "fit" => Ok(Self::Fit),
            "letter" => Ok(Self::Letter),
            "a4" => Ok(Self::A4),
            other => Err(CliError(format!(
                "unknown page size `{other}` (expected fit, letter or a4)"
            ))),
        }
    }

    fn dims(self) -> Option<(f64, f64)> {
        match self {
            Self::Fit => None,
            Self::Letter => Some((612.0, 792.0)),
            Self::A4 => Some((595.276, 841.89)),
        }
    }
}

/// Options for [`image_to_pdf`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageToPdfOptions {
    /// How the page is sized around each image.
    pub page_size: ImagePageSize,
    /// Margin in points on every side.
    pub margin: f64,
}

impl Default for ImageToPdfOptions {
    fn default() -> Self {
        Self {
            page_size: ImagePageSize::Fit,
            margin: 36.0,
        }
    }
}

/// Convert `inputs` to a PDF at `output`, one page per image.
///
/// # Errors
///
/// [`crate::ToolError`] when there are no inputs, the margin is invalid, an
/// image cannot be read or decoded, or the generated PDF fails structural
/// verification. No output is committed on error.
///
/// # Budget
///
/// The decoded pixel buffer is charged by [`selis_image::decode`]; the writer
/// charges its output bytes.
#[allow(clippy::arithmetic_side_effects)]
pub fn image_to_pdf(
    inputs: &[String],
    options: ImageToPdfOptions,
    output: &str,
) -> CliResult<Verification> {
    if inputs.is_empty() {
        return Err(CliError(
            "img2pdf needs at least one input image".to_string(),
        ));
    }
    if !options.margin.is_finite() || options.margin < 0.0 {
        return Err(CliError("margin must be a finite number >= 0".to_string()));
    }

    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let mut builder = DocumentBuilder::new();
    let mut in_bytes = 0u64;

    for path in inputs {
        let data = read_file(path)?;
        in_bytes = in_bytes.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        let image =
            selis_image::decode(&data, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
        let img_w = f64::from(image.width);
        let img_h = f64::from(image.height);
        if img_w <= 0.0 || img_h <= 0.0 {
            return Err(CliError(format!("{path}: empty image")));
        }
        let img_ref =
            embed_image_rgba_flate(&mut builder, image.width, image.height, &image.rgba, 6);

        // Page size: fitted to the image plus margins, or a fixed sheet.
        let (page_w, page_h) = options
            .page_size
            .dims()
            .unwrap_or((img_w + 2.0 * options.margin, img_h + 2.0 * options.margin));

        // Draw size: fixed sheets scale the image down to the printable area.
        let (draw_w, draw_h) = match options.page_size.dims() {
            None => (img_w, img_h),
            Some((w, h)) => {
                let avail_w = (w - 2.0 * options.margin).max(1.0);
                let avail_h = (h - 2.0 * options.margin).max(1.0);
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
        let content_num = builder.allocate();
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
        let res_num = builder.allocate();
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

    // One image must produce exactly one page. Asserting that count is what
    // makes "nothing was dropped" a measurement rather than a promise.
    let expected = Expectations {
        pages: Some(u64::try_from(inputs.len()).unwrap_or(u64::MAX)),
        ..Expectations::none()
    };
    let verdict = crate::write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;
    let display = Verification::from_gate(
        None,
        &verdict,
        &expected,
        in_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    eprintln!("wrote {} page(s) to {output}", inputs.len());
    display.emit_line();
    Ok(display)
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
