//! The `selis search` command: search a page's text for a query.
//!
//! Uses the normalised search pipeline (case folding, diacritics stripping,
//! ligature decomposition) from `selis-pdf-text`.

use selis_pdf_engine::Session;
use selis_sandbox::{Budget, Surface};

use crate::extract::page_lines;
use crate::{read_file, CliError, CliResult};

/// Search a page's text for a query.
///
/// # Errors
///
/// `IO_READ_FAILED` when the PDF cannot be read.
pub(crate) fn run(path: &str, query: &str, page: usize) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::profile(Surface::Viewer);
    let session =
        Session::open(src, &budget).map_err(|e| CliError(format!("cannot open PDF: {e}")))?;
    if page >= session.len() {
        return Err(CliError(format!(
            "page {page} out of range (document has {} pages)",
            session.len()
        )));
    }
    let mut g = crate::runtime::cli_guard(&budget);
    let dl = session
        .page_display_list(page, &budget, &mut g)
        .map_err(|e| CliError(format!("cannot interpret page: {e}")))?;

    let mcid_order = session
        .mcid_order(&budget, &mut g)
        .ok()
        .filter(|v| !v.is_empty());
    let (lines, line_texts) = page_lines(&dl, mcid_order.as_deref(), &mut g)?;
    let matches = selis_pdf_text::search_lines(&lines, &line_texts, query);
    if matches.is_empty() {
        return Ok(());
    }
    for m in &matches {
        let rect = m.rect;
        println!(
            "page {page} line {line} ({x0:.1},{y0:.1},{x1:.1},{y1:.1}): {text}",
            page = page,
            line = m.line,
            x0 = rect.x0,
            y0 = rect.y0,
            x1 = rect.x1,
            y1 = rect.y1,
            text = m.text,
        );
    }
    Ok(())
}
