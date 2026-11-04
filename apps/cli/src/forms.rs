//! The `selis form-fields` and `selis fill-forms` commands (Phase 4 — fill
//! forms).
//!
//! `form-fields` prints the AcroForm as JSON: every field's fully qualified
//! name, kind, current value, choice options / button states, and flags. This
//! is the read half: it never writes a byte of the input.
//!
//! `fill-forms` takes repeatable `--set NAME=VALUE` pairs and writes a filled
//! PDF. Every filled value is re-read from the produced bytes before the file
//! is committed (see `selis_pdf_tools::fill_forms`); a value that does not
//! round-trip leaves no file behind. The read-only, signature, push-button,
//! multi-select and XFA refusals are deliberate and identical in both halves.

use crate::{CliError, CliResult};

/// `selis form-fields <path> [--output file]`
///
/// # Errors
///
/// [`CliError`] when the file cannot be read, is encrypted, or the report
/// cannot be encoded.
pub(crate) fn list(path: &str, output: Option<&str>) -> CliResult<()> {
    let report = selis_pdf_tools::form_fields(path).map_err(CliError::from)?;
    let json = report
        .to_json()
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    match output {
        Some(file) => super::write_file(file, &json),
        None => {
            println!("{}", String::from_utf8_lossy(&json));
            Ok(())
        }
    }
}

/// `selis fill-forms <path> --output <file> --set NAME=VALUE ...`
///
/// # Errors
///
/// [`CliError`] when a `--set` pair is malformed (it must hold exactly one
/// `=` separator; everything after it is the value), or when the fill is
/// refused for any of the typed reasons `selis_pdf_tools::fill_forms`
/// reports.
pub(crate) fn fill(
    path: &str,
    sets: &[String],
    output: &str,
) -> CliResult<crate::verify_report::Verification> {
    let mut fields = Vec::with_capacity(sets.len());
    for item in sets {
        let (name, value) = item.split_once('=').ok_or_else(|| {
            CliError(format!(
                "--set expects NAME=VALUE, got {item:?}; everything after the first = is the value"
            ))
        })?;
        let name = name.trim();
        if name.is_empty() {
            return Err(CliError(format!(
                "--set has an empty field name in {item:?}"
            )));
        }
        fields.push(selis_pdf_tools::FieldEdit {
            name: name.to_string(),
            value: value.to_string(),
        });
    }
    let options = selis_pdf_tools::FillFormsOptions { fields };
    selis_pdf_tools::fill_forms(path, &options, output).map_err(CliError::from)
}
