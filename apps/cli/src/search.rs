//! The `selis search` command: search a page's text for a query.
//!
//! Uses the normalised search pipeline (case folding, diacritics stripping,
//! ligature decomposition) from `selis-pdf-text`.

use crate::extract::page_lines;
use crate::{read_file, CliError, CliResult};
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, Surface};

/// Search a page's text for a query.
///
/// A page that drew text but recovered none of it (SL-3.TEXT.10) prints
/// [`selis_pdf_text::LOW_CONFIDENCE_MARKER`] and exits non-zero instead of
/// reporting "no matches": a query that cannot be answered is not a query that
/// found nothing.
///
/// # Errors
///
/// `IO_READ_FAILED` when the PDF cannot be read, and a non-zero exit when the
/// page is low-confidence (text drawn, nothing recovered) and the query
/// therefore cannot be answered.
pub(crate) fn run(path: &str, query: &str, page: usize) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::profile(Surface::Viewer);
    let clock = crate::shell_clock();
    let session = Session::open(src, &budget, &clock)
        .map_err(|e| CliError(format!("cannot open PDF: {e}")))?;
    if page >= session.len() {
        return Err(CliError(format!(
            "page {page} out of range (document has {} pages)",
            session.len()
        )));
    }
    let mut g = budget.guard_with(&clock, crate::runtime::token());
    let dl = session
        .page_text_display_list(page, &budget, &mut g)
        .map_err(|e| CliError(format!("cannot interpret page: {e}")))?;

    let mcid_order = session
        .mcid_order(&budget, &mut g)
        .ok()
        .filter(|v| !v.is_empty());
    let (lines, line_texts, low_confidence) =
        page_lines(&session, page, &budget, &dl, mcid_order.as_deref(), &mut g)?;
    let matches = selis_pdf_text::search_lines(&lines, &line_texts, query);
    // SL-3.TEXT.10: a page that **drew** text but recovered none of it answers
    // every query with "no matches" — indistinguishable, to a caller, from a
    // page that genuinely has no text. The verdict already exists (the same
    // one `extract` turns into `LOW_CONFIDENCE_MARKER`); discarding it here,
    // as an unnamed `_`, is precisely the silent-absence defect this task
    // exists to close. So the marker line is printed and the command reports
    // failure: "not found" and "we could not read the page" are different
    // answers, and only one of them is a search result.
    if low_confidence {
        println!("{}", selis_pdf_text::LOW_CONFIDENCE_MARKER);
        return Err(CliError(format!(
            "page {page} drew text but none of it could be recovered, so `{query}` \
             cannot be searched (see the marker line above)"
        )));
    }
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
