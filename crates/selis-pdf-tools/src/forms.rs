//! Tool wrappers for the core `selis-pdf-form` crate (Phase 4 — fill forms).
//!
//! Everything to do with reading or rewriting a field — the model, the
//! hierarchy, the value encoding, the read-back — belongs to
//! `selis-pdf-form`. This module owns only the file boundary and the write
//! helper every writing tool shares: it reads the input bytes, hands them to
//! the core, and runs the result through the WRITE.05 verification gate
//! before it commits the file.

use selis_sandbox::Budget;

use crate::verify_report::Verification;
use crate::write_gate::{self, PRESERVE_ALL};
use crate::{read_file, CliError, ToolResult};

pub use selis_pdf_form::{
    FieldEdit, FieldKind, FillFormsOptions, FormField, FormFieldsReport, MAX_FIELD_NAME_BYTES,
    MAX_FIELD_VALUE_BYTES, MAX_FORM_EDITS,
};

impl From<selis_pdf_form::FormError> for crate::ToolError {
    fn from(error: selis_pdf_form::FormError) -> Self {
        crate::ToolError(error.to_string())
    }
}

/// Read the document's AcroForm fields.
///
/// # Errors
///
/// A [`crate::ToolError`] whose message names the refusal: an unreadable
/// document, an encrypted one, or a field structure the core cannot walk.
pub fn form_fields(path: &str) -> ToolResult<FormFieldsReport> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    selis_pdf_form::read_form_fields(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))
}

/// Fill the named fields in `path` and write the verified result to `output`.
///
/// Every value is re-read from the produced bytes before the file is
/// committed. A signature warning is printed before the write when the
/// document carries one: the rewrite invalidates it by design, and every
/// caller deserves to see that before the file exists.
///
/// # Errors
///
/// A [`crate::ToolError`] whose message names the refusal (encrypted, XFA,
/// read-only signature or push-button fields, a choice value outside its
/// options, a value that does not round-trip). On any error no file is
/// committed.
pub fn fill_forms(
    path: &str,
    options: &FillFormsOptions,
    output: &str,
) -> ToolResult<Verification> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let (bytes, info) = selis_pdf_form::fill_form(&src, options, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    if info.has_signature {
        eprintln!(
            "warning: {path} carries a digital signature; this rewrite invalidates it \
             (re-sign the output to restore one)"
        );
    }
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
        "filled {} field(s), {} in the form, values re-read from the output -> {output}",
        info.filled, info.total
    );
    verification.emit_line();
    Ok(verification)
}
