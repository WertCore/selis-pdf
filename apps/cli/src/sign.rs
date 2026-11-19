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
    connect_tsa: bool,
    tsa_url: Option<&str>,
    tsa_anchors: &[String],
    connect_aia: bool,
    ocsp_url: Option<&str>,
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
    let verification = selis_pdf_tools::sign_pdf(path, &options, output).map_err(CliError::from)?;

    // SL-5.SIGN.04: external services are strictly opt-in. With neither flag
    // this whole block is skipped — no socket, no consent line, no behaviour
    // change from the offline signing path above.
    if !connect_tsa && !connect_aia {
        return Ok(verification);
    }
    embed_consented_trust(
        output,
        field,
        connect_tsa,
        tsa_url,
        tsa_anchors,
        connect_aia,
        ocsp_url,
    )?;
    Ok(verification)
}

/// Add consented timestamp / revocation data to a freshly signed PDF.
///
/// The network lives only in `tsa.rs`; here we read the signature's CMS blob,
/// call the consent-gated providers and hand the returned DER to the engine's
/// PURE `embed_dss`. No document bytes or keys are sent anywhere.
#[cfg(not(target_arch = "wasm32"))]
fn embed_consented_trust(
    output: &str,
    field: &str,
    connect_tsa: bool,
    tsa_url: Option<&str>,
    tsa_anchors: &[String],
    connect_aia: bool,
    ocsp_url: Option<&str>,
) -> CliResult<()> {
    let signed = std::fs::read(output)
        .map_err(|e| CliError(format!("cannot re-open {output} for DSS: {e}")))?;
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let mut g = budget.guard();
    let cms = selis_pdf_sign::signature_contents(&signed, field, &budget, &mut g)
        .map_err(|e| CliError(e.to_string()))?;

    let mut timestamps: Vec<selis_pdf_sign::TimestampToken> = Vec::new();
    let mut ocsp: Vec<Vec<u8>> = Vec::new();
    if connect_tsa {
        let token = crate::tsa::timestamp_signature(
            &cms,
            &crate::tsa::NetworkConsent {
                connect: true,
                url: tsa_url,
            },
            &mut |line| eprintln!("{line}"),
        )?;
        timestamps.push(selis_pdf_sign::TimestampToken::from_der(token));
    }
    if connect_aia {
        let data = crate::tsa::fetch_revocation(
            &crate::tsa::NetworkConsent {
                connect: true,
                url: ocsp_url,
            },
            &mut |line| eprintln!("{line}"),
        )?;
        ocsp.push(data.ocsp);
    }

    // Trust anchors for the TSA's own certificate are caller-supplied; with
    // none the token is embedded but reported as an untrusted time authority.
    let mut tsa_anchor_der = Vec::with_capacity(tsa_anchors.len());
    for anchor in tsa_anchors {
        let text = read_text(anchor)?;
        let certs = selis_crypto::cert::pem_certificates(&text);
        if certs.is_empty() {
            return Err(CliError(format!(
                "{anchor}: not a PEM X.509 certificate (TSA anchor)"
            )));
        }
        tsa_anchor_der.extend(certs);
    }
    let options = selis_pdf_sign::DssOptions {
        field_name: field,
        timestamps: &timestamps,
        ocsp: &ocsp,
        tsa_anchors: &tsa_anchor_der,
        now_unix: now_seconds(),
        ..selis_pdf_sign::DssOptions::default()
    };
    let (out, _) = selis_pdf_sign::embed_dss(&signed, &options, &budget, &mut g)
        .map_err(|e| CliError(e.to_string()))?;
    crate::write_file(output, &out)
}

/// `selis generate-identity --name NAME --output DIR [--bits N] [--days N] [--ca]`
///
/// Writes a local self-signed signing identity. No CA, no network, no upload.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn generate_identity(
    name: &str,
    output_dir: &str,
    bits: usize,
    days: u32,
    is_ca: bool,
    force: bool,
) -> CliResult<()> {
    use std::path::Path;

    let dir = Path::new(output_dir);
    if !dir.is_dir() {
        return Err(CliError(format!(
            "{output_dir}: output directory does not exist"
        )));
    }
    let key_path = dir.join("key.pem");
    let cert_path = dir.join("cert.pem");
    if !force && (key_path.exists() || cert_path.exists()) {
        return Err(CliError(format!(
            "{output_dir}: key.pem or cert.pem already exists; pass --force to overwrite"
        )));
    }

    // Entropy comes from the OS CSPRNG here, the one boundary allowed to draw
    // it; the clock is read here too. Nothing is sent anywhere.
    let serial_bytes = selis_crypto::random_bytes(16)
        .and_then(|bytes| <[u8; 16]>::try_from(&bytes[..]).ok())
        .ok_or_else(|| CliError("no OS entropy source is available".to_string()))?;

    let identity =
        selis_crypto::identity::generate_local_identity(&selis_crypto::identity::IdentityParams {
            common_name: name,
            organization: Some("Selis local identity"),
            key_bits: bits,
            validity_days: days,
            not_before_unix: now_seconds(),
            serial: serial_bytes,
            is_ca,
        })
        .map_err(|e| CliError(format!("identity generation failed: {e}")))?;

    super::write_file(&key_path.to_string_lossy(), identity.key_pem.as_bytes())?;
    // The key is a secret: on Unix restrict it to owner read/write. Windows
    // inherits the directory ACL, which is the correct model there.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CliError(format!("cannot secure {}: {e}", key_path.display())))?;
    }
    super::write_file(&cert_path.to_string_lossy(), identity.cert_pem.as_bytes())?;

    eprintln!("{}", identity.trust_note());
    eprintln!("Wrote {} and {}", key_path.display(), cert_path.display());
    eprintln!(
        "Sign with: selis sign <input.pdf> --key {} --cert {}",
        key_path.display(),
        cert_path.display()
    );
    eprintln!(
        "Verify trust with: selis signatures <signed.pdf> --anchor {}",
        cert_path.display()
    );
    Ok(())
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
        // A PEM bundle may hold a whole chain of CERTIFICATE blocks (an OS
        // trust-store export, a CA bundle). Load all of them so the verifier
        // sees the full set of caller-trusted anchors; a bare base64 DER
        // certificate is accepted too.
        let mut certs = selis_crypto::cert::pem_certificates(&text);
        if certs.is_empty() {
            if let Some(cert) = selis_crypto::cert::base64_decode(&text) {
                certs.push(cert);
            }
        }
        if certs.is_empty() {
            return Err(CliError(format!(
                "{anchor}: not a PEM (optionally bundled) or base64 X.509 certificate"
            )));
        }
        der.extend(certs);
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
