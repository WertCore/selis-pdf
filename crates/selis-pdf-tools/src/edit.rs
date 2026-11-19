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

pub use selis_pdf_edit::{
    BoxAck, BoxEdit, BoxEditError, BoxEditPreview, BoxEditWarning, EditError, EditReport,
};

impl From<selis_pdf_edit::EditError> for crate::ToolError {
    fn from(error: selis_pdf_edit::EditError) -> Self {
        crate::ToolError(error.to_string())
    }
}

/// Options for the browser/CLI edit-text tool, deserialized from the tools
/// request. `page` is 0-based, matching [`edit_text`]; the web UI shows a
/// 1-based page number and subtracts one before it sends the request.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct EditTextOptions {
    /// 0-based index of the page whose text is edited.
    pub page: u32,
    /// The exact run of text to find (ASCII / WinAnsi).
    pub find: String,
    /// The text to put in its place; may be empty to delete the run.
    pub replace: String,
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

/// SL-5.TEXTEDIT.03 — replace text in a single line/run WITHOUT reflowing the
/// paragraph, and only after the caller has acknowledged the no-reflow warning.
///
/// The warning is always printed. `acknowledge_no_reflow` is the tool boundary's
/// equivalent of acknowledging the typed [`BoxEditWarning`]: when it is false
/// the edit is refused and no file is committed, so a box edit is never
/// applied silently.
///
/// # Errors
///
/// A [`crate::ToolError`] naming the refusal — the same reasons as [`edit_text`],
/// plus the unacknowledged-warning refusal when `acknowledge_no_reflow` is false.
pub fn edit_text_in_box(
    path: &str,
    page: u32,
    find: &str,
    replace: &str,
    acknowledge_no_reflow: bool,
    output: &str,
) -> ToolResult<Verification> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let preview = selis_pdf_edit::edit_box(&src, page, find, replace, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;

    // "Tell the user" is the whole point of the fallback: surface the warning
    // before deciding whether to commit.
    eprintln!(
        "WARNING [{}]: {}",
        preview.warning().code,
        preview.warning().message()
    );
    if !acknowledge_no_reflow {
        return Err(CliError(format!(
            "{path}: box edit refused; the caller did not acknowledge the warning: {}",
            preview.warning().message()
        )));
    }
    let committed = preview
        .commit(preview.warning().acknowledge())
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let bytes = committed.bytes;

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
    verification.emit_line();
    Ok(verification)
}

/// SL-5.TEXTEDIT.04 — plan the substitute face for an edit whose original font
/// is unembedded (the common case), returning the full disclosure so the caller
/// can tell the user before the document's appearance changes.
///
/// Callers that know the face is embedded should call
/// [`selis_font::font_match::match_substitute`] with an
/// [`selis_font::OriginalFont::embedded`] descriptor instead, so a complete
/// embedded face is correctly kept.
#[must_use]
pub fn plan_font_substitution(base_font: &str, needed_chars: &[char]) -> selis_font::Substitute {
    let budget = Budget::unlimited();
    selis_font::font_match::match_substitute(
        &selis_font::OriginalFont::unembedded(base_font),
        needed_chars,
        &budget,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn substitution_disclosure_is_surfaced() {
        let s = plan_font_substitution("Helvetica", &['H', 'i']);
        assert!(s.substituted);
        assert_eq!(s.face, "LiberationSans-Regular");
        assert!(
            s.disclosure.contains("substituted metric-compatible"),
            "{}",
            s.disclosure
        );
    }

    #[test]
    fn embedded_complete_face_is_kept() {
        let budget = Budget::unlimited();
        let s = selis_font::font_match::match_substitute(
            &selis_font::OriginalFont::embedded("Helvetica", &['H', 'i']),
            &['H', 'i'],
            &budget,
        );
        assert!(!s.substituted);
        assert!(s.disclosure.is_empty());
    }
}
