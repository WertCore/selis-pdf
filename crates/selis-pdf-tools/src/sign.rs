//! Tool wrappers for the core `selis-pdf-sign` crate (Phase 4 — sign PDF).
//!
//! The signing crate owns everything about the edit: where the signature
//! field lands, how the appearance is built, how the detached CMS blob covers
//! the file's own `/ByteRange`, and how each signature is read back. This
//! module owns only the file boundary and the write gate.
//!
//! Every signing write is an **incremental revision**, so the gate's job is
//! narrower than a rewrite's: pages, optional content, outlines and embedded
//! files must come through untouched, while annotations and form fields are
//! *expected* to grow — the visual stamp appends a content stream, and the
//! certificate signature adds a field and its widget. [`PRESERVE_ON_APPEND`]
//! names exactly that set.
//!
//! The report is read-only and never writes a byte.

use selis_pdf_sign::SignError;
use selis_sandbox::Budget;

use crate::verify_report::Verification;
use crate::write_gate::{self, PRESERVE_ON_APPEND};
use crate::{read_file, CliError, ToolResult};

pub use selis_pdf_sign::{
    signature_report_json as encode_signature_report, DigitalOptions, DigitalReport,
    SignatureReport, SignaturesReport, TrustReport, VisualLine, VisualOptions, VisualReport,
    VisualStroke,
};

impl From<SignError> for crate::ToolError {
    fn from(error: SignError) -> Self {
        crate::ToolError(error.to_string())
    }
}

/// Draw a typed and/or freehand stamp on one page and write the verified
/// result to `output`.
///
/// The bytes before the edit are preserved verbatim, so a signature over an
/// earlier revision stays verifiable; the stamp is an appended content stream
/// and an appended page object, never a rewrite.
///
/// # Errors
///
/// A [`crate::ToolError`] whose message names the refusal: an unreadable or
/// encrypted document, a page outside the document, a degenerate rectangle, a
/// character outside WinAnsi, or a stamp whose text does not fit its box. On
/// any error no file is committed.
pub fn stamp_pdf(path: &str, options: &VisualOptions, output: &str) -> ToolResult<Verification> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let (bytes, report) = selis_pdf_sign::sign_visual(&src, options, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected = write_gate::expectations_from(&observed, &PRESERVE_ON_APPEND);
    let verdict = write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;
    let verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        u64::try_from(src.len()).unwrap_or(u64::MAX),
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    eprintln!(
        "stamped page {} ({} line(s), {} stroke(s), {} stream bytes) -> {output}",
        report.page, report.lines, report.strokes, report.stream_bytes
    );
    verification.emit_line();
    Ok(verification)
}

/// Sign the document with a local RSA key and write the verified result to
/// `output`.
///
/// The key and certificate chain are PEM strings supplied by the caller. The
/// signature is an `adbe.pkcs7.detached` blob over the document's own
/// `/ByteRange`, verified against the produced bytes before the file is
/// committed.
///
/// # Errors
///
/// A [`crate::ToolError`] whose message names the refusal: an unreadable or
/// encrypted document, a page outside the document, a non-RSA or too-small
/// key, a key that does not match the certificate, or a signature that did not
/// verify from the produced bytes. On any error no file is committed.
pub fn sign_pdf(path: &str, options: &DigitalOptions, output: &str) -> ToolResult<Verification> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let (bytes, report) = selis_pdf_sign::sign_digital(&src, options, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected = write_gate::expectations_from(&observed, &PRESERVE_ON_APPEND);
    let verdict = write_gate::write_verified(&bytes, output, &expected, &budget, &mut g)?;
    let verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        u64::try_from(src.len()).unwrap_or(u64::MAX),
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    eprintln!(
        "signed field {:?} on page {} ({} digest, CMS {} bytes, verified) -> {output}",
        report.field, report.page, report.digest, report.cms_bytes
    );
    verification.emit_line();
    Ok(verification)
}

/// Read every signature in the document and check it against the bytes it
/// names.
///
/// `anchors` are DER-encoded certificates the caller trusts; an empty slice
/// means nothing is trusted, which the report says rather than assumes.
/// `now` is Unix seconds supplied by the caller because the signing crate has
/// no clock.
///
/// # Errors
///
/// A [`crate::ToolError`] when the document cannot be read or is encrypted. A
/// damaged individual signature is a report entry with `verified: false`, not
/// an error: the rest of the report still reads.
pub fn signature_report(path: &str, anchors: &[Vec<u8>], now: i64) -> ToolResult<SignaturesReport> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    selis_pdf_sign::signatures(&src, anchors, now, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))
}

/// Encode a [`SignaturesReport`] as the compact JSON shared by the CLI and the
/// browser.
///
/// # Errors
///
/// A [`crate::ToolError`] if the report cannot be serialised, which cannot
/// happen for this shape but is not a reason to unwrap.
pub fn signature_report_bytes(report: &SignaturesReport) -> ToolResult<Vec<u8>> {
    encode_signature_report(report).map_err(crate::ToolError::from)
}
