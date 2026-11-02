//! The `selis render` command: render a page to a PPM or PNG image.
//!
//! This is the simplest end-to-end smoke test: open a PDF, render a page, and
//! dump the pixels. PPM (P6) stays the default because it needs no encoder and
//! has always been the smoke-test format; `--format png` writes the same pixels
//! through the owned deterministic PNG encoder the raster tools use.

use crate::{read_file, CliError, CliResult};
use selis_image::{encode_png, Image};
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, Surface};

/// Render a page to an image file.
///
/// `format` is `ppm` (default) or `png`. `dpi == 0` means 1 px/pt (72 DPI).
///
/// # Errors
///
/// `IO_WRITE_FAILED` when the output file cannot be written, and a typed
/// engine error when the page cannot be rendered.
pub(crate) fn run(
    path: &str,
    page_num: usize,
    output: &str,
    dpi: u32,
    format: &str,
) -> CliResult<()> {
    let format = format.to_ascii_lowercase();
    if format != "ppm" && format != "png" {
        return Err(CliError(format!(
            "unknown output format {format:?}; expected ppm or png"
        )));
    }
    let src = read_file(path)?;
    let budget = Budget::profile(Surface::Viewer);
    let clock = crate::shell_clock();
    let session = Session::open(src, &budget, &clock)
        .map_err(|e| CliError(format!("cannot open PDF: {e}")))?;
    if page_num >= session.len() {
        return Err(CliError(format!(
            "page {page_num} out of range (document has {} pages)",
            session.len()
        )));
    }
    // The page view carries the rendered canvas size and the page-to-device
    // transform, honouring /Rotate (SL-2.RAST.12, ISO 32000-2 §14.11.2):
    // the bitmap dimensions swap for 90/270 and the content rotates with the
    // page. `--dpi 0` means 1 px/pt (72 DPI).
    let dpi_f = if dpi == 0 { 72.0 } else { f64::from(dpi) };
    let mut g = budget.guard_with(&clock, crate::runtime::token());
    let rendered = session
        .render_page_rgba(page_num, dpi_f, &budget, &mut g)
        .map_err(|e| CliError(format!("render failed: {e}")))?;
    let (w, h) = (rendered.width, rendered.height);
    match format.as_str() {
        "png" => {
            let image = Image {
                width: w,
                height: h,
                rgba: rendered.rgba,
            };
            let bytes = encode_png(&image, 6, &mut g)
                .map_err(|e| CliError(format!("cannot encode PNG: {e}")))?;
            std::fs::write(output, &bytes)
                .map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
        }
        _ => write_ppm(output, &rendered.rgba, w, h)?,
    }
    eprintln!("rendered page {page_num} to {output} ({w}x{h}) at {dpi} DPI as {format}");
    Ok(())
}

pub(crate) fn write_ppm(path: &str, rgba: &[u8], w: u32, h: u32) -> CliResult<()> {
    let header = format!("P6\n{w} {h}\n255\n");
    let pixel_count = (w as usize).saturating_mul(h as usize);
    let mut ppm = Vec::with_capacity(header.len().saturating_add(pixel_count.saturating_mul(3)));
    ppm.extend_from_slice(header.as_bytes());
    let mut i = 0usize;
    for _ in 0..pixel_count {
        let r = rgba.get(i).copied().unwrap_or(0);
        let g = rgba.get(i.saturating_add(1)).copied().unwrap_or(0);
        let b = rgba.get(i.saturating_add(2)).copied().unwrap_or(0);
        ppm.push(r);
        ppm.push(g);
        ppm.push(b);
        i = i.saturating_add(4);
    }
    std::fs::write(path, &ppm).map_err(|e| CliError(format!("cannot write {path}: {e}")))?;
    Ok(())
}
