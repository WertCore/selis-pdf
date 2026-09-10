//! The `selis convert` command: render a page range to images.
//!
//! The "PDF → images" direction of the product's convert story: render every
//! page (or a `--first`/`--last` range) to a portable pixmap (PPM/P6) in the
//! output directory.  Text/HTML conversion is `selis extract`; `selis render`
//! is the single-page case.

use selis_pdf_engine::{Session, TinySkiaBackend};
use selis_sandbox::{Budget, CancelToken, Surface};

use crate::render::{dim, write_ppm};
use crate::{read_file, CliError, CliResult};

/// Convert a page range of a PDF to PPM images in `output_dir`.
///
/// # Errors
///
/// `IO_READ_FAILED` when the PDF cannot be read; `IO_WRITE_FAILED` when an
/// output file cannot be written.
pub(crate) fn run(
    path: &str,
    output_dir: &str,
    first: usize,
    last: Option<usize>,
) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::profile(Surface::Viewer);
    let clock = crate::shell_clock();
    let session = Session::open(src, &budget, &clock)
        .map_err(|e| CliError(format!("cannot open PDF: {e}")))?;
    if session.is_empty() {
        return Err(CliError("document has no pages".to_string()));
    }
    let page_count = session.len();
    let last = last
        .unwrap_or(page_count.saturating_sub(1))
        .min(page_count.saturating_sub(1));
    if first > last {
        return Err(CliError(format!(
            "page range {first}..{last} is empty (document has {page_count} pages)"
        )));
    }
    std::fs::create_dir_all(output_dir)
        .map_err(|e| CliError(format!("cannot create {output_dir}: {e}")))?;

    let mut converted = 0usize;
    for page in first..=last {
        let (w_pt, h_pt) = session
            .page_size(page)
            .ok_or_else(|| CliError(format!("page {page} has no media box")))?;
        let w = dim(w_pt);
        let h = dim(h_pt);
        if w == 0 || h == 0 {
            return Err(CliError(format!("page {page} has zero area")));
        }
        let mut backend = TinySkiaBackend::new(w, h)
            .ok_or_else(|| CliError(format!("cannot create {w}x{h} canvas")))?;
        let mut g = budget.guard_with(&clock, CancelToken::new());
        session
            .render_page(page, &mut backend, &budget, &mut g)
            .map_err(|e| CliError(format!("render page {page}: {e}")))?;
        let file = format!("{output_dir}/page-{page}.ppm");
        write_ppm(&file, backend.pixmap().data(), w, h)?;
        eprintln!("converted page {page} -> {file} ({w}x{h})");
        converted = converted.saturating_add(1);
    }
    eprintln!("converted {converted} page(s) to {output_dir}");
    Ok(())
}
