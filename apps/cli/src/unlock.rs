//! The `selis unlock` command (SL-1A.TOOL.04): remove password protection.
//!
//! The operation itself lives in `selis_pdf_tools::security`, shared byte-for-
//! byte with the browser security chunk: authenticate the password, derive the
//! file key, decrypt every stream and string through the engine's resolver,
//! drop `/Encrypt` from the trailer, and emit a fresh single-revision document.
//! This file is the command boundary — the audit line, the verification
//! display, and nothing else.

use selis_pdf_tools::security;

use crate::verify_report::Verification;
use crate::{CliError, CliResult};

/// Run the unlock tool: decrypt `path` and write the result to `output`.
///
/// `password` defaults to the empty string (the user password of many
/// encrypted documents that require no password to open). A wrong password
/// returns a clean `WRONG_PASSWORD` typed error.
///
/// # Errors
///
/// A typed parse error when the document is damaged, and `WRONG_PASSWORD` when
/// the password does not match the document's user or owner password. An
/// unencrypted input is copied verbatim and the display says so.
pub(crate) fn run(path: &str, output: &str, password: Option<&str>) -> CliResult<Verification> {
    let outcome = security::unlock(path, output, password).map_err(CliError::from)?;
    if outcome.encrypted {
        eprintln!("unlocked {path} -> {output}");
    } else {
        eprintln!("{path}: not encrypted; copied as-is to {output}");
    }
    outcome.verification.emit_line();
    Ok(outcome.verification)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    /// Build a minimal single-page PDF. When `encrypt` is true the trailer
    /// carries an `/Encrypt` reference to object 4. The xref offsets are
    /// computed so the bytes reparse cleanly.
    fn build_pdf(encrypt: bool) -> Vec<u8> {
        let objs = [
            (1u32, b"<< /Type /Catalog /Pages 2 0 R >>" as &[u8]),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
            ),
            (4, b"<< /Filter /Standard /V 1 /R 2 /Length 40 >>"),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = std::collections::HashMap::new();
        for (num, body) in &objs {
            offsets.insert(*num, out.len());
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        let count = objs.len() + 1;
        out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..u32::try_from(count).expect("fits u32") {
            let off = offsets[&i];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        if encrypt {
            out.extend_from_slice(b"<< /Size 5 /Root 1 0 R /Encrypt 4 0 R >>\n");
        } else {
            out.extend_from_slice(b"<< /Size 5 /Root 1 0 R >>\n");
        }
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    /// An encrypted document with an unsupported handler is refused.
    #[test]
    fn encrypted_with_unsupported_handler_is_refused() {
        let dir = std::env::temp_dir().join("selis-unlock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("unsupported.pdf");
        let out_path = dir.join("unsupported.out.pdf");
        std::fs::write(&in_path, build_pdf(true)).unwrap();
        let err = super::run(in_path.to_str().unwrap(), out_path.to_str().unwrap(), None)
            .expect_err("unsupported handler refused");
        assert!(
            err.to_string().contains("unsupported"),
            "message mentions unsupported: {err}"
        );
    }

    /// An unencrypted document is copied as-is.
    #[test]
    fn unencrypted_document_is_copied_as_is() {
        let dir = std::env::temp_dir().join("selis-unlock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("plain.pdf");
        let out_path = dir.join("plain.out.pdf");
        let src = build_pdf(false);
        std::fs::write(&in_path, &src).unwrap();
        super::run(in_path.to_str().unwrap(), out_path.to_str().unwrap(), None)
            .expect("copy as-is");
        let copied = std::fs::read(&out_path).unwrap();
        assert_eq!(copied, src, "bytes must be identical for unencrypted input");
    }

    /// Real encrypted document with empty password is unlocked successfully.
    #[test]
    fn real_encrypted_document_is_unlocked() {
        let corpus = "D:\\selis\\corpus\\pdfs\\bug900822.pdf";
        if !std::path::Path::new(corpus).exists() {
            eprintln!("skipping: {corpus} not found");
            return;
        }
        let dir = std::env::temp_dir().join("selis-unlock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out_path = dir.join("bug900822_unlocked.pdf");
        super::run(corpus, out_path.to_str().unwrap(), None).expect("unlock");

        let out_bytes = std::fs::read(&out_path).unwrap();
        let tail = String::from_utf8_lossy(
            &out_bytes[out_bytes.len().saturating_sub(4096).min(out_bytes.len())..],
        );
        assert!(
            !tail.contains("/Encrypt"),
            "output must not have /Encrypt trailer entry"
        );
        assert!(
            out_bytes.starts_with(b"%PDF-"),
            "output must start with PDF header"
        );
        // Verify the output opens with the engine.
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
        assert!(
            selis_pdf_engine::Session::open(out_bytes, &budget, &crate::shell_clock()).is_ok(),
            "output must open in the engine"
        );
    }

    /// A non-conformant/broken encrypted file (V=4 + RC4 + /Length 40, which
    /// is invalid per spec) must produce a clean error, not a crash.
    #[test]
    fn broken_encrypted_file_errors_cleanly() {
        let corpus = "D:\\selis\\corpus\\pdfs\\issue19484_1.pdf";
        if !std::path::Path::new(corpus).exists() {
            eprintln!("skipping: {corpus} not found");
            return;
        }
        let dir = std::env::temp_dir().join("selis-unlock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out_path = dir.join("issue19484_unlocked.pdf");
        let err = super::run(corpus, out_path.to_str().unwrap(), None)
            .expect_err("broken encrypted file must error");
        // The error should mention "damaged object container" or similar
        // (the engine also reports 0 pages for this file).
        let msg = err.to_string();
        assert!(
            !msg.contains("panicked") && !msg.contains("unreachable"),
            "error is clean, not a crash: {msg}"
        );
    }
}
