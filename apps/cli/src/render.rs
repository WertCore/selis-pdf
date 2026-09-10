//! The `selis render` command: render a page to a PPM image.
//!
//! This is the simplest end-to-end smoke test: open a PDF, render a page, and
//! dump the pixels. The output is a portable pixmap (PPM/P6) — no PNG
//! dependency needed.

use crate::{read_file, CliError, CliResult};
use selis_pdf_engine::{Session, TinySkiaBackend};
use selis_sandbox::{Budget, Surface};

/// Render a page to a PPM file.
///
/// # Errors
///
/// `IO_WRITE_FAILED` when the output file cannot be written.
pub(crate) fn run(path: &str, page_num: usize, output: &str, dpi: u32) -> CliResult<()> {
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
    let (w_pt, h_pt) = session
        .page_size(page_num)
        .ok_or_else(|| CliError(format!("page {page_num} has no media box")))?;
    // Scale points to device pixels at the requested DPI (72 pt/in).
    let scale = if dpi == 0 { 1.0 } else { f64::from(dpi) / 72.0 };
    let w = dim(w_pt * scale);
    let h = dim(h_pt * scale);
    if w == 0 || h == 0 {
        return Err(CliError(format!("page {page_num} has zero area")));
    }
    let mut backend = TinySkiaBackend::new(w, h)
        .ok_or_else(|| CliError(format!("cannot create {w}x{h} canvas")))?;
    let mut g = budget.guard_with(&clock, crate::runtime::token());
    session
        .render_page(page_num, &mut backend, &budget, &mut g)
        .map_err(|e| CliError(format!("render failed: {e}")))?;
    let data = backend.pixmap().data();
    write_ppm(output, data, w, h)?;
    eprintln!("rendered page {page_num} to {output} ({w}x{h}) at {dpi} DPI");
    Ok(())
}

/// A finite, non-negative f64 as a u32 dimension (ceil, saturate).
pub(crate) fn dim(v: f64) -> u32 {
    if !v.is_finite() || v < 0.0 {
        return 0;
    }
    let c = v.ceil();
    if c >= u32::MAX as f64 {
        return u32::MAX;
    }
    // c is finite, non-negative, and below u32::MAX — exact in range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        c as u32
    }
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
