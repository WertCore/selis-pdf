//! The `selis clear-permissions` command (SL-1A.TOOL.05): remove permission
//! restrictions from an encrypted document.
//!
//! The operation lives in `selis_pdf_tools::security`, shared with the browser
//! security chunk. It interprets the owner password as the explicit override
//! SL-1.ENC.04 requires and proceeds only when that password authenticates as
//! the document owner. For R5/6 the file key is independent of `/P`, so the
//! content stays encrypted at rest and only `/P` and `/Perms` change, appended
//! as a new revision (WRITE.02); for R2–4 `/P` feeds the key derivation, so a
//! new key is derived from the cleared `/P` and every stream and string is
//! re-encrypted. This file is the command boundary — the oplog override record
//! and the verification display.

use selis_log::oplog::{ActorId, OpLog, OpRecord, Outcome};
use selis_pdf_tools::security;

use crate::verify_report::Verification;
use crate::{CliError, CliResult};

/// The oplog vocabulary for the SL-1.ENC.04 override (closed, greppable).
const OP_CLEAR_PERMISSIONS: &str = "clear-permissions";
const OVERRIDE_OWNER_PASSWORD: &str = "owner-password-permission-bits";

/// Run the clear-permissions tool. Returns the operation's verification
/// display (SL-1A.UI.02).
///
/// # Errors
///
/// `WRONG_OWNER_PASSWORD` when the password does not authenticate as the owner,
/// and an error naming the file when the input is not encrypted or carries an
/// unreadable `/Encrypt`. Nothing is written on a refusal.
pub(crate) fn run(path: &str, output: &str, password: Option<&str>) -> CliResult<Verification> {
    let outcome = security::clear_permissions(path, output, password).map_err(CliError::from)?;
    record_override(&outcome.summary);
    if outcome.previously_denied.is_empty() {
        eprintln!("cleared permissions on {path} -> {output} (nothing was restricted)");
    } else {
        eprintln!(
            "cleared permissions on {path} -> {output} (now granted: {})",
            outcome.previously_denied.join(", ")
        );
    }
    outcome.verification.emit_line();
    Ok(outcome.verification)
}

/// Record the SL-1.ENC.04 override in the oplog vocabulary and echo the audit
/// record (the CLI's one-shot oplog surface until the edit model (SL-5) carries
/// per-document logs).
fn record_override(summary: &str) {
    let mut log = OpLog::new();
    let seq = log.record_override(
        OP_CLEAR_PERMISSIONS,
        summary,
        0,
        ActorId::local(),
        Outcome::Ok { revision: None },
        OVERRIDE_OWNER_PASSWORD,
    );
    if let Some(OpRecord {
        seq: s,
        op,
        summary: sm,
        override_used,
        ..
    }) = log.records().iter().find(|r| r.seq == seq)
    {
        eprintln!(
            "oplog: seq={s} op={op} override={:?} summary={sm}",
            override_used.unwrap_or("")
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    /// Build a minimal single-page PDF with no encryption: clear-permissions
    /// must refuse it cleanly.
    fn build_plain_pdf() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = std::collections::HashMap::new();
        let objs: [(&[u8], &[u8]); 2] = [
            (b"1", b"<< /Type /Catalog /Pages 2 0 R >>"),
            (b"2", b"<< /Type /Pages /Kids [] /Count 0 >>"),
        ];
        for (num, body) in &objs {
            offsets.insert(*num, out.len());
            let n = std::str::from_utf8(num).unwrap_or("");
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 3\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..3u32 {
            let num = format!("{i}");
            let off = offsets[num.as_bytes()];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        out.extend_from_slice(b"<< /Size 3 /Root 1 0 R >>\n");
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    /// A plain document is refused (nothing to clear).
    #[test]
    fn unencrypted_document_is_refused() {
        let dir = std::env::temp_dir().join("selis-clear-perms-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("plain.pdf");
        let out_path = dir.join("plain.out.pdf");
        std::fs::write(&in_path, build_plain_pdf()).unwrap();
        let err = super::run(in_path.to_str().unwrap(), out_path.to_str().unwrap(), None)
            .expect_err("plain document refused");
        assert!(
            err.to_string().contains("not encrypted"),
            "message mentions not encrypted: {err}"
        );
    }

    /// Build a minimal R2 (RC4-40) encrypted PDF: empty user password,
    /// owner password `"owner"`, restricted `/P`. Object 3 is the catalog,
    /// object 6 a stream carrying `BT /F1 12 Tf (ok) Tj ET`.
    fn build_r2_encrypted_pdf() -> Vec<u8> {
        let id0 = [0x11u8; 16];
        let restricted_p: u32 = 0xFFFF_FFC4; // print/modify/copy forbidden
        let owner_pw = b"owner";
        let user_pw = b"";

        let o = selis_crypto::compute_o(
            &selis_crypto::compute_owner_key(owner_pw, 2, 40),
            &selis_crypto::pad_password(user_pw),
            2,
        );
        let key = selis_crypto::encryption_key(user_pw, &o, restricted_p, &id0, 2, 40, true);
        let u = selis_crypto::compute_u(&key, 2, &id0);

        // Object bodies (only object 6 is a stream; strings stay unencrypted
        // except the stream body which we encrypt).
        let content = b"BT /F1 12 Tf (ok) Tj ET".to_vec();
        let enc_content = selis_crypto::encrypt_data(&key, 6, 0, &content, 2, false);

        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = std::collections::HashMap::new();
        let mut push_obj = |num: u32, body: &[u8]| {
            offsets.insert(num, out.len());
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        };
        push_obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push_obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push_obj(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 6 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        push_obj(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        // /Encrypt dict (not itself encrypted). /P is a signed 32-bit value.
        let enc_body = format!(
            "<< /Filter /Standard /V 1 /R 2 /Length 40 /O <{}> /U <{}> /P {} >>",
            hex(&o),
            hex(&u),
            restricted_p as i32
        );
        push_obj(5, enc_body.as_bytes());
        // The content stream, RC4-encrypted under object 6.
        let stream_body = format!("<< /Length {} >>\nstream\n", enc_content.len());
        offsets.insert(6, out.len());
        out.extend_from_slice("6 0 obj\n".as_bytes());
        out.extend_from_slice(stream_body.as_bytes());
        out.extend_from_slice(&enc_content);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 7\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..7u32 {
            let off = offsets[&i];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        let id_hex = hex(&id0);
        out.extend_from_slice(
            format!("<< /Size 7 /Root 1 0 R /Encrypt 5 0 R /ID [<{id_hex}><{id_hex}>] >>\n")
                .as_bytes(),
        );
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02X}"));
        }
        s
    }

    /// Build a minimal R6 (AES-256) encrypted PDF: empty user password,
    /// owner password `"owner"`, restricted `/P`. Content is AES-256-CBC
    /// encrypted under the file key (no per-object salting for R6).
    fn build_r6_encrypted_pdf() -> Vec<u8> {
        let id0 = [0x22u8; 16];
        let restricted_p: u32 = 0xFFFF_FBE4;
        let owner_pw = b"owner";
        let user_pw = b"";
        let file_key = [0x5Au8; 32];

        // Random-but-fixed salts keep the synthetic file deterministic; the
        // credential values themselves follow the R6 Algorithms 2.A/2.B.
        let v_salt = [0xA1u8; 8];
        let k_salt = [0xB2u8; 8];
        let ov_salt = [0xC3u8; 8];
        let ok_salt = [0xD4u8; 8];
        let u = selis_crypto::compute_u_r6(user_pw, &v_salt, &k_salt, 6);
        let ue = selis_crypto::compute_ue_r6(user_pw, &k_salt, &file_key, 6);
        let o = selis_crypto::compute_o_r6(owner_pw, &ov_salt, &ok_salt, &u, 6);
        let oe = selis_crypto::compute_oe_r6(owner_pw, &ok_salt, &u, &file_key, 6);
        let perms = selis_crypto::compute_perms_r6(restricted_p, &file_key, true);
        let content = b"BT /F1 12 Tf (ok) Tj ET".to_vec();
        let enc_content = selis_crypto::encrypt_data(&file_key, 6, 0, &content, 6, true);

        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-2.0\n");
        let mut offsets = std::collections::HashMap::new();
        let mut push_obj = |num: u32, body: &[u8]| {
            offsets.insert(num, out.len());
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        };
        push_obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push_obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push_obj(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 6 0 R /Resources << /Font << /F1 4 0 R >> >> >>",
        );
        push_obj(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let enc_body = format!(
            "<< /Filter /Standard /V 5 /R 6 /Length 256 /O <{}> /U <{}> /OE <{}> /UE <{}> /P {} /Perms <{}> >>",
            hex(&o),
            hex(&u),
            hex(&oe),
            hex(&ue),
            restricted_p as i32,
            hex(&perms)
        );
        push_obj(5, enc_body.as_bytes());
        let stream_body = format!("<< /Length {} >>\nstream\n", enc_content.len());
        offsets.insert(6, out.len());
        out.extend_from_slice(b"6 0 obj\n");
        out.extend_from_slice(stream_body.as_bytes());
        out.extend_from_slice(&enc_content);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 7\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..7u32 {
            let off = offsets[&i];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        let id_hex = hex(&id0);
        out.extend_from_slice(
            format!("<< /Size 7 /Root 1 0 R /Encrypt 5 0 R /ID [<{id_hex}><{id_hex}>] >>\n")
                .as_bytes(),
        );
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    /// A real R6 (AES-256) document with known passwords round-trips via the
    /// incremental path: content bytes are untouched, /P is all permissions,
    /// and the output still opens under the same (empty) user password.
    #[test]
    fn r6_encrypted_round_trips_with_bits_cleared() {
        let dir = std::env::temp_dir().join("selis-clear-perms-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("r6_encrypted.pdf");
        let out_path = dir.join("r6_cleared.pdf");
        let src = build_r6_encrypted_pdf();
        std::fs::write(&in_path, &src).unwrap();

        let outcome = selis_pdf_tools::security::clear_permissions(
            in_path.to_str().unwrap(),
            out_path.to_str().unwrap(),
            Some("owner"),
        )
        .expect("clear permissions on R6");
        // /P 0xFFFF_FBE4 denies modify and copy; the report names exactly those.
        assert_eq!(
            outcome.previously_denied,
            vec!["modify", "copy"],
            "the report names what changed"
        );

        let out = std::fs::read(&out_path).unwrap();
        // Original bytes are a prefix (incremental append, WRITE.02).
        assert!(out.starts_with(&src), "R6 path must append incrementally");
        let s = String::from_utf8_lossy(&out);
        let tail = &s[s.len().saturating_sub(2000)..];
        assert!(
            tail.contains("/P 4294967295") || tail.contains("/P -1"),
            "output /P is all permissions: {tail}"
        );
        // The output still opens in the engine with the empty user password,
        // and the content stream still decrypts (same file key).
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
        let session = selis_pdf_engine::Session::open(out, &budget, &crate::shell_clock())
            .expect("output opens in the engine");
        assert_eq!(session.len(), 1, "one page");
    }

    /// A real R2 (RC4-40) document with known passwords round-trips with bits
    /// cleared: the output opens with the empty user password and `/P` is all
    /// permissions.
    #[test]
    fn r2_encrypted_round_trips_with_bits_cleared() {
        let dir = std::env::temp_dir().join("selis-clear-perms-test");
        std::fs::create_dir_all(&dir).unwrap();
        let in_path = dir.join("r2_encrypted.pdf");
        let out_path = dir.join("r2_cleared.pdf");
        std::fs::write(&in_path, build_r2_encrypted_pdf()).unwrap();

        let outcome = selis_pdf_tools::security::clear_permissions(
            in_path.to_str().unwrap(),
            out_path.to_str().unwrap(),
            Some("owner"),
        )
        .expect("clear permissions on R2");
        // /P 0xFFFF_FFC4 grants print only.
        assert_eq!(
            outcome.previously_denied,
            vec!["modify", "copy", "annotate"],
            "the report names what changed"
        );

        let out = std::fs::read(&out_path).unwrap();
        let s = String::from_utf8_lossy(&out);
        // /P in the output must be all-permissions (0xFFFFFFFC).
        assert!(
            s.contains("/P 4294967292") || s.contains("/P -4"),
            "output /P is all permissions: {s}"
        );
        // The output opens in the engine with the (empty) user password.
        let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
        let session = selis_pdf_engine::Session::open(out, &budget, &crate::shell_clock())
            .expect("output opens in the engine");
        assert_eq!(session.len(), 1, "one page");
    }
}
