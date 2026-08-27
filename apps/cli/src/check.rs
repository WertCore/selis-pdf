//! The `selis check` command: evaluate the conformance rules for a PDF.
//!
//! Runs the registered conformance rules (SL-1.DOC.09) against the document
//! for a profile and reports pass/fail/unevaluated per rule.

use selis_pdf_engine::Session;
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

use crate::{read_file, CliError, CliResult};

/// Check a PDF against a conformance profile.
///
/// # Errors
///
/// `IO_READ_FAILED` when the PDF cannot be read.
pub(crate) fn run(path: &str, profile: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::profile(Surface::Viewer);
    let session =
        Session::open(src, &budget).map_err(|e| CliError(format!("cannot open PDF: {e}")))?;
    let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let profile = match profile {
        "readable" | "Readable" => selis_pdf_doc::Profile::Readable,
        "ua" | "Ua" | "pdfua" | "PDFUA" => selis_pdf_doc::Profile::PdfUa,
        other => {
            return Err(CliError(format!(
                "unknown profile '{other}' (use 'readable' or 'ua')"
            )));
        }
    };
    let results = session
        .conformance(profile, &budget, &mut g)
        .map_err(|e| CliError(format!("conformance: {e}")))?;
    for (i, rule) in selis_pdf_doc::registry().iter().enumerate() {
        let outcome = results.get(i).ok_or_else(|| {
            CliError(format!("conformance: no result for rule {}", rule.id))
        })?;
        let status = match outcome {
            selis_pdf_doc::RuleResult::Pass => "pass".to_string(),
            selis_pdf_doc::RuleResult::Fail { detail } => {
                format!("FAIL  {detail}")
            }
            selis_pdf_doc::RuleResult::Unevaluated { needs, level } => {
                format!("n/a   (needs {needs:?} at {level:?})")
            }
        };
        println!("{:24} {status}", rule.id);
    }
    Ok(())
}
