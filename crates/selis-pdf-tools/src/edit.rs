//! Tool wrapper for the core `selis-pdf-edit` crate (Phase 5 — edit text).
//!
//! The edit itself — finding the show-text operator, rewriting the string, and
//! appending the incremental update — belongs to `selis-pdf-edit`. This module
//! owns only the file boundary and the write helper every writing tool shares:
//! it reads the input bytes, hands them to the core, and runs the result
//! through the WRITE.05 verification gate before it commits the file.

use selis_sandbox::Budget;

use crate::verify_report::Verification;
use crate::write_gate::{self, PRESERVE_ALL};
use crate::{read_file, CliError, ToolResult};

pub use selis_pdf_edit::{EditError, EditReport};

impl From<selis_pdf_edit::EditError> for crate::ToolError {
    fn from(error: selis_pdf_edit::EditError) -> Self {
        crate::ToolError(error.to_string())
    }
}

/// Replace the first occurrence of `find` with `replace` in the text of page
/// `page` (0-based) and write the verified result to `output`.
///
/// The edit is an incremental update: the original bytes are preserved verbatim
/// as a prefix, so any digital signature over the original revision stays valid.
///
/// # Errors
///
/// A [`crate::ToolError`] whose message names the refusal — an unreadable or
/// encrypted document, text that was not found, or a page/stream/font shape the
/// in-place editor does not handle yet. On any error no file is committed.
pub fn edit_text(
    path: &str,
    page: u32,
    find: &str,
    replace: &str,
    output: &str,
) -> ToolResult<Verification> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let (bytes, report) = selis_pdf_edit::edit_text(&src, page, find, replace, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected = write_gate::expectations_from(&observed, &PRESERVE_ALL);
    let verdict = write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;
    let verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        in_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    eprintln!(
        "edited page {}: replaced {} occurrence(s), original bytes preserved -> {output}",
        report.page, report.replaced
    );
    verification.emit_line();
    Ok(verification)
}
