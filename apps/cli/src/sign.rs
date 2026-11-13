//! The `selis stamp`, `selis sign` and `selis signatures` commands (Phase 4 —
//! sign PDF).
//!
//! `stamp` draws typed lines on one page as an appended content stream.
//! `sign` writes a detached CMS signature over the document's own
//! `/ByteRange` with a key the user names. `signatures` reads every signature
//! back and checks it against the bytes it covers; pins are the certificates
//! the caller trusts, and an empty set means nothing is trusted.
//!
//! Nothing here invents cryptographic material: the key and certificate files
//! are read as the user supplied them, and a refusal before the write leaves
//! no output file.

use crate::{CliError, CliResult};

/// `selis stamp <path> --output <file> [--text LINE]...`
///
/// # Errors
///
/// [`CliError`] when a rectangle or font size is malformed, a text line is
/// empty, or the stamp is refused for any of the typed reasons
/// `selis_pdf_tools::stamp_pdf` reports.
pub(crate) fn stamp(
    path: &str,
    output: &str,
    page: usize,
    rect: Option<&str>,
    texts: &[String],
    font_size: f64,
) -> CliResult<crate::verify_report::Verification> {
    if texts.is_empty() {
        return Err(CliError("--text needs at least one line".to_string()));
    }
    let lines = texts
        .iter()
        .map(|text| selis_pdf_tools::VisualLine {
            text: text.clone(),
            size: font_size,
            bold: false,
            italic: false,
            underline: false,
        })
        .collect();
    let options = selis_pdf_tools::VisualOptions {
        page,
        rect: parse_rect(rect)?,
        lines,
        strokes: Vec::new(),
        color: [0.0, 0.0, 0.0],
        stroke_width: 1.0,
        font_size,
        border: true,
    };
    selis_pdf_tools::stamp_pdf(path, &options, output).map_err(CliError::from)
}

/// `selis sign <path> --output <file> --key <pem> --cert <pem>...`
///
/// # Errors
///
/// [`CliError`] when a key or certificate file cannot be read, when `--rect`
/// or `--time` is malformed, or when the signature is refused for any of the
/// typed reasons `selis_pdf_tools::sign_pdf` reports.
pub(crate) fn sign(
    path: &str,
    output: &str,
    page: usize,
    rect: Option<&str>,
    key_path: &str,
    cert_paths: &[String],
    field: &str,
    name: Option<&str>,
    reason: Option<&str>,
    location: Option<&str>,
    time: Option<&str>,
) -> CliResult<crate::verify_report::Verification> {
    if cert_paths.is_empty() {
        return Err(CliError(
            "--cert needs at least one certificate (leaf first)".to_string(),
        ));
    }
    let key_pem = read_text(key_path)?;
    let mut certificates_pem = Vec::with_capacity(cert_paths.len());
    for cert_path in cert_paths {
        certificates_pem.push(read_text(cert_path)?);
    }
    let signer_name = match name {
        Some(name) => name.to_string(),
        None => certificate_name(cert_paths.first().map(String::as_str))?,
    };
    let signing_time = match time {
        Some(time) => time.to_string(),
        None => now_stamp(),
    };
    let options = selis_pdf_tools::DigitalOptions {
        page,
        rect: parse_rect(rect)?,
        field_name: field.to_string(),
        signer_name,
        reason: reason.map(str::to_string),
        location: location.map(str::to_string),
        signing_time,
        key_pem,
        certificates_pem,
    };
    selis_pdf_tools::sign_pdf(path, &options, output).map_err(CliError::from)
}

/// `selis signatures <path> [--output file] [--anchor <pem>]...`
///
/// # Errors
///
/// [`CliError`] when the document or an anchor cannot be read, or the report
/// cannot be encoded.
pub(crate) fn signatures(path: &str, output: Option<&str>, anchors: &[String]) -> CliResult<()> {
    let mut der = Vec::with_capacity(anchors.len());
    for anchor in anchors {
        let text = read_text(anchor)?;
        let cert = selis_crypto::cert::pem_body(&text, "CERTIFICATE")
            .or_else(|| selis_crypto::cert::base64_decode(&text))
            .ok_or_else(|| CliError(format!("{anchor}: not a PEM or base64 X.509 certificate")))?;
        der.push(cert);
    }
    let report =
        selis_pdf_tools::signature_report(path, &der, now_seconds()).map_err(CliError::from)?;
    let json = selis_pdf_tools::signature_report_bytes(&report).map_err(CliError::from)?;
    match output {
        Some(file) => super::write_file(file, &json),
        None => {
            println!("{}", String::from_utf8_lossy(&json));
            Ok(())
        }
    }
}

fn read_text(path: &str) -> CliResult<String> {
    std::fs::read_to_string(path).map_err(|e| CliError(format!("cannot read {path}: {e}")))
}

/// The signer's display name, read from the leaf certificate's subject CN.
fn certificate_name(leaf: Option<&str>) -> CliResult<String> {
    let Some(leaf) = leaf else {
        return Ok("Selis signer".to_string());
    };
    let text = read_text(leaf)?;
    let der = selis_crypto::cert::pem_body(&text, "CERTIFICATE")
        .ok_or_else(|| CliError(format!("{leaf}: not a PEM certificate")))?;
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
    let mut g = budget.guard();
    let cert =
        selis_crypto::cert::parse(&der, &mut g).map_err(|e| CliError(format!("{leaf}: {e}")))?;
    Ok(cert.subject_common_name())
}

/// `x,y,w,h` in display points, or the bottom-left default block.
fn parse_rect(rect: Option<&str>) -> CliResult<[f64; 4]> {
    let Some(rect) = rect else {
        return Ok([36.0, 36.0, 256.0, 106.0]);
    };
    let parts: Vec<&str> = rect.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(CliError(format!("--rect expects x,y,w,h, got {rect:?}")));
    }
    let value = |index: usize| -> CliResult<f64> {
        let text = parts
            .get(index)
            .ok_or_else(|| CliError(format!("--rect is missing component {index}")))?;
        text.parse::<f64>()
            .map_err(|_| CliError(format!("--rect component {text:?} is not a number")))
    };
    let x = value(0)?;
    let y = value(1)?;
    let w = value(2)?;
    let h = value(3)?;
    Ok([x, y, x + w, y + h])
}

/// Unix seconds now. The CLI is the one boundary allowed to read the wall
/// clock; the core crates take `now` from their caller instead.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `YYYYMMDDHHMMSSZ`, the form the signing crate writes as the signature's
/// `/M` (it prefixes the PDF `D:` itself).
fn now_stamp() -> String {
    let now = now_seconds();
    format_stamp(now)
}

#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::integer_division
)]
fn format_stamp(unix: i64) -> String {
    // Days-to-civil (Howard Hinnant), so this stays dependency-free.
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let hour = secs / 3_600;
    let minute = (secs % 3_600) / 60;
    let second = secs % 60;
    format!("{y:04}{m:02}{d:02}{hour:02}{minute:02}{second:02}Z")
}
