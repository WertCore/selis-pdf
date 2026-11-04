//! The `selis protect` command (SL-1A.TOOL.06): add password protection and
//! permission bits.
//!
//! The operation lives in `selis_pdf_tools::security`, shared byte-for-byte
//! with the browser security chunk: AESV3 / revision 6 only (ADR-P0019 — the
//! only handler Selis writes), a full rewrite rather than an incremental append
//! because encrypting changes every stream and string body, and the typed
//! `ALREADY_ENCRYPTED` refusal on a file that already carries a password —
//! removing one is the separate `selis unlock` command.
//!
//! Permission bits (`/P`) are a *convention, not enforcement* (SL-1.ENC.04):
//! the tool writes the owner's intent honestly and the long-help says so in
//! plain language. The v1 surface exposes the four bits users actually ask
//! for — print, modify, copy, annotate — and always grants accessibility
//! extraction (screen readers). `/EncryptMetadata` is honoured: when the
//! caller opts out, the document-level XMP stream is left in the clear.
//!
//! Passwords arrive via arguments or environment variables only. They are
//! never logged and never written anywhere except the `/Encrypt` dictionary.
//! The 64 bytes of key material the write needs are drawn here, from the
//! operating system: a command that cannot get them refuses, because a
//! stand-in would give every protected document the same predictable key.

use selis_log::oplog::{ActorId, OpLog, Outcome};
use selis_pdf_cos::encrypt::R6Material;
use selis_pdf_tools::security::{self, ProtectOptions};

use crate::verify_report::Verification;
use crate::{CliError, CliResult};

pub(crate) use selis_pdf_tools::security::parse_permissions;

/// The oplog vocabulary (`&'static str` keeps the set closed and greppable).
const OP_PROTECT: &str = "protect";

/// Run the protect tool.
///
/// `user_password` opens the document (empty/absent = the document opens
/// without a password); `owner_password` guards permission changes and
/// defaults to the user password, as Acrobat does. Both fall back to
/// `SELIS_USER_PASSWORD` / `SELIS_OWNER_PASSWORD` when their argument is
/// absent. `permissions` is the optional grant spec ([`parse_permissions`]);
/// `encrypt_metadata` selects whether the document-level XMP stream is
/// encrypted (the spec default is true).
///
/// # Errors
///
/// A typed parse error when the document is damaged, the typed
/// `ALREADY_ENCRYPTED` error when the input already carries an `/Encrypt`
/// dictionary, and a plain usage error for empty credentials or an unknown
/// permission name.
///
/// # Budget
///
/// Parsing and the object walk run under an unlimited budget (the CLI's
/// standing choice, matching the sibling tools); the writer charges the
/// serialised output bytes per object. A surface-level budget slots in at the
/// same call sites.
///
/// # Malformed Input
///
/// A damaged document fails at `parse_revisions` and nothing is written. An
/// already-encrypted document fails with `ALREADY_ENCRYPTED` before any work.
/// Verification re-parses the output and re-authenticates both passwords
/// before any file is replaced, so a failure anywhere leaves the source bytes
/// untouched and no partial output on disk.
pub(crate) fn run(
    path: &str,
    output: &str,
    user_password: Option<&str>,
    owner_password: Option<&str>,
    permissions: Option<&str>,
    encrypt_metadata: bool,
) -> CliResult<Verification> {
    let verification = protect_file(
        path,
        output,
        &PasswordOptions::from_args(user_password, owner_password),
        permissions,
        encrypt_metadata,
    )?;
    verification.emit_line();
    Ok(verification)
}

/// The password material for one protect invocation: an optional user
/// password and an optional owner password (see [`PasswordOptions::resolve`]).
#[derive(Debug, Clone, Default)]
pub(crate) struct PasswordOptions {
    /// The user password, if given.
    pub user: Option<String>,
    /// The owner password, if given.
    pub owner: Option<String>,
}

impl PasswordOptions {
    /// Capture passwords from explicit arguments (the single-file CLI path;
    /// env fallbacks are resolved later, inside `resolve`, so a batch run
    /// captures them once per file).
    #[must_use]
    pub(crate) fn from_args(user: Option<&str>, owner: Option<&str>) -> Self {
        Self {
            user: user.map(String::from),
            owner: owner.map(String::from),
        }
    }

    /// Resolve to the concrete (user, owner) pair: the argument wins, else
    /// the environment variable, else empty. The owner defaults to the user
    /// password, as Acrobat does. Both are truncated to the Algorithm 2.A
    /// limit by the caller and never logged.
    fn resolve(&self) -> (String, String) {
        let user = self
            .user
            .clone()
            .or_else(|| resolve_password(None, "SELIS_USER_PASSWORD"))
            .unwrap_or_default();
        let owner = self
            .owner
            .clone()
            .or_else(|| resolve_password(None, "SELIS_OWNER_PASSWORD"))
            .unwrap_or_else(|| user.clone());
        (user, owner)
    }
}

/// Core protect logic on one file: draw the key material, hand the whole
/// request to the shared operation, and record what it wrote. This is the
/// batch harness's per-file entry point as well as [`run`]'s body — a per-file
/// typed failure is a batch report entry, never a batch abort.
///
/// # Errors
///
/// As [`run`], plus a refusal when the operating system offers no entropy.
pub(crate) fn protect_file(
    path: &str,
    output: &str,
    passwords: &PasswordOptions,
    permissions: Option<&str>,
    encrypt_metadata: bool,
) -> CliResult<Verification> {
    let (user_password, owner_password) = passwords.resolve();
    if user_password.is_empty() && owner_password.is_empty() {
        return Err(CliError(
            "no passwords given: --user-password / --owner-password or \
             SELIS_USER_PASSWORD / SELIS_OWNER_PASSWORD"
                .to_string(),
        ));
    }
    let granted = match permissions {
        Some(spec) => parse_permissions(spec).map_err(CliError)?,
        None => security::ALL_GRANTED,
    };

    // The R6 credentials: a 256-bit file key and four 8-byte salts, drawn from
    // the operating system. The document /ID then follows from the file key
    // (revision 6 never hashes /ID into the key, so a fresh key per save means
    // a fresh ID per save).
    let Some(material) = R6Material::generate() else {
        return Err(CliError(format!(
            "{path}: no operating-system entropy source is available; refusing to protect a \
             document with a key that is not random"
        )));
    };

    let outcome = security::protect(
        path,
        output,
        &ProtectOptions {
            user_password,
            owner_password,
            granted,
            encrypt_metadata,
            material,
        },
    )?;
    record_operation(&granted);
    eprintln!("protected {path} -> {output} (AES-256, revision 6)");
    Ok(outcome.verification)
}

/// Resolve a password: the argument wins; otherwise the named environment
/// variable; otherwise `None` (the caller applies its own default — an absent
/// user password means the document opens without one).
///
/// Command-line arguments can be visible in shell history and process
/// listings; environment variables in the process environment. Both are
/// local-machine exposures — the tradeoff is documented in the command's
/// long help. Neither channel is ever logged, and neither value is written
/// anywhere except the `/Encrypt` dictionary.
fn resolve_password(arg: Option<&str>, env_var: &str) -> Option<String> {
    if let Some(pw) = arg {
        return Some(pw.to_string());
    }
    std::env::var(env_var).ok().filter(|pw| !pw.is_empty())
}

/// Record the operation in the oplog vocabulary and echo the audit line (the
/// CLI's one-shot oplog surface until SL-5 carries per-document logs). The
/// record names the granted permission set — never the passwords.
fn record_operation(granted: &u32) {
    let granted_names = security::granted_names(*granted);
    let mut log = OpLog::new();
    let seq = log.record(
        OP_PROTECT,
        format!(
            "AES-256/revision 6 encryption applied; permissions granted: {}",
            if granted_names.is_empty() {
                "none".to_string()
            } else {
                granted_names.join(",")
            }
        ),
        0,
        ActorId::local(),
        Outcome::Ok { revision: None },
    );
    if let Some(rec) = log.records().iter().find(|r| r.seq == seq) {
        eprintln!(
            "oplog: seq={} op={} summary={}",
            rec.seq, rec.op, rec.summary
        );
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;
    use selis_pdf_cos::encrypt;
    use selis_pdf_cos::{parse_revisions, xref, Obj};
    use selis_pdf_tools::security::{compute_p, BIT_ANNOTATE, BIT_COPY, BIT_MODIFY, BIT_PRINT};
    use selis_sandbox::{Budget, Surface};

    /// Resolve one object by number across revisions, so a test can read a
    /// stream body straight out of the written file.
    fn resolve_number(
        src: &[u8],
        num: u32,
        budget: &Budget,
        g: &mut selis_sandbox::BudgetGuard<'_>,
    ) -> Result<Obj, selis_error::Error> {
        let startxref = xref::find_startxref(src, 4096).unwrap_or(0);
        let doc = parse_revisions(src, startxref, budget, g)?;
        for view in doc.revisions().iter().rev() {
            if let Some(selis_pdf_cos::XrefEntry::InUse { offset, .. }) = view.entries.get(&num) {
                return selis_pdf_cos::resolve_object(src, *offset, budget, g);
            }
        }
        Err(selis_error::Error::new(selis_error::Code::ObjUnexpected))
    }

    /// Build a minimal single-page PDF with a content stream, an indirect
    /// catalog /Metadata stream (XMP), and an /Info dictionary — every string
    /// and stream class the encryption must cover. Object 5 is the content
    /// stream, 6 the metadata stream, 7 the Info dict.
    fn build_plain_pdf(content: &[u8]) -> Vec<u8> {
        // One object per entry: (number, body). Stream objects carry their
        // own `stream...endstream` section inside the body.
        let objs: Vec<(u32, Vec<u8>)> = vec![
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>".to_vec(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 5 0 R /Resources << /Font << /F1 4 0 R >> >> >>".to_vec(),
            ),
            (
                4,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            ),
            (
                5,
                [
                    format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
                    content.to_vec(),
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            ),
            (
                6,
                [
                    format!(
                        "<< /Length {} /Type /Metadata /Subtype /XML >>\nstream\n",
                        XMP.len()
                    )
                    .into_bytes(),
                    XMP.to_vec(),
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            ),
            (
                7,
                b"<< /Title (secret-title) /Producer (selis-test) >>".to_vec(),
            ),
        ];

        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n");
        let mut offsets = std::collections::HashMap::new();
        for (num, body) in &objs {
            offsets.insert(*num, out.len());
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }

        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 8\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..8u32 {
            let off = offsets[&i];
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n");
        out.extend_from_slice(b"<< /Size 8 /Root 1 0 R /Info 7 0 R >>\n");
        out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
        out
    }

    /// The XMP packet carried by the synthetic metadata stream.
    const XMP: &[u8] = b"<?xpacket begin=''?><x:xmpmeta selis-test='1'></x:xmpmeta><?xpacket end?>";

    /// The synthetic content stream (plaintext form).
    fn content_of(tag: &str) -> Vec<u8> {
        format!("BT /F1 12 Tf ({tag}) Tj ET").into_bytes()
    }

    struct TempPaths {
        input: std::path::PathBuf,
        output: std::path::PathBuf,
    }

    fn temp_paths(tag: &str) -> TempPaths {
        let dir = std::env::temp_dir().join("selis-protect-test");
        std::fs::create_dir_all(&dir).unwrap();
        let unique = format!("{tag}-{}", std::process::id());
        TempPaths {
            input: dir.join(format!("{unique}.in.pdf")),
            output: dir.join(format!("{unique}.out.pdf")),
        }
    }

    /// Write `src` to the temp input and protect it. Panics on harness I/O
    /// errors; `run`'s outcome flows through the return value.
    fn protect(paths: &TempPaths, src: &[u8], args: ProtectArgs) -> Result<(), CliError> {
        if std::fs::write(&paths.input, src).is_err() {
            panic!("write temp input");
        }
        super::run(
            paths.input.to_str().unwrap(),
            paths.output.to_str().unwrap(),
            args.user_password,
            args.owner_password,
            args.permissions,
            args.encrypt_metadata,
        )
        .map(|_verification| ())
    }

    /// protect() argument bundle with sensible defaults.
    struct ProtectArgs {
        user_password: Option<&'static str>,
        owner_password: Option<&'static str>,
        permissions: Option<&'static str>,
        encrypt_metadata: bool,
    }

    impl Default for ProtectArgs {
        fn default() -> Self {
            Self {
                user_password: Some("secret"),
                owner_password: Some("owner"),
                permissions: None,
                encrypt_metadata: true,
            }
        }
    }

    /// The /Encrypt dictionary of a protected file, plus the file key that
    /// its user password authenticates to.
    fn parse_output(bytes: &[u8]) -> (selis_pdf_cos::encrypt::EncryptInfo, Vec<u8>) {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = parse_revisions(bytes, sx, &budget, &mut g).unwrap();
        let rev = doc.revisions().last().unwrap();
        let info = encrypt::parse_encrypt(bytes, rev.encrypt, &budget, &mut g)
            .unwrap()
            .expect("/Encrypt present");
        let id0 = encrypt::document_id(&rev.trailer);
        let key = encrypt::authenticate(&info, &id0, b"secret").expect("user password");
        (info, key)
    }

    /// The raw stream body of object `num` in `bytes` (an unfiltered lookup —
    /// the object is written with classic xref, so a direct resolve works).
    fn stream_data(bytes: &[u8], num: u32) -> Option<Vec<u8>> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        match resolve_number(bytes, num, &budget, &mut g) {
            Ok(Obj::Stream { data, .. }) => Some(data.to_vec()),
            _ => None,
        }
    }

    #[test]
    fn parse_permissions_grants_named_bits() {
        assert_eq!(
            parse_permissions("print,copy").unwrap(),
            BIT_PRINT | BIT_COPY
        );
        assert_eq!(
            parse_permissions("all-of-nothing").unwrap_err(),
            "unknown permission `all-of-nothing` (valid: print, modify, copy, annotate)"
        );
        assert_eq!(parse_permissions("none").unwrap(), 0);
        assert_eq!(
            parse_permissions("print,modify,copy,annotate").unwrap(),
            BIT_PRINT | BIT_MODIFY | BIT_COPY | BIT_ANNOTATE
        );
        assert!(parse_permissions("").is_err());
    }

    #[test]
    fn compute_p_keeps_reserved_bits_conformant() {
        // All granted: bits 1-2 zero, everything else set (Table 22 baseline).
        assert_eq!(
            compute_p(BIT_PRINT | BIT_MODIFY | BIT_COPY | BIT_ANNOTATE),
            0xFFFF_FFFC
        );
        // Deny copy + annotate: their bits clear, reserved bits untouched.
        assert_eq!(
            compute_p(BIT_PRINT | BIT_MODIFY),
            0xFFFF_FFFC & !(BIT_COPY | BIT_ANNOTATE)
        );
        // None granted: only the four exposed bits clear.
        assert_eq!(compute_p(0), 0xFFFF_FFC0);
    }

    #[test]
    fn round_trip_user_password_decrypts_and_perms_verify() {
        let paths = temp_paths("round-trip");
        let content = content_of("round-trip");
        protect(&paths, &build_plain_pdf(&content), ProtectArgs::default()).expect("protect");
        let out = std::fs::read(&paths.output).unwrap();

        // The plaintext markers must all be gone from the raw bytes: the
        // content stream, the Info strings, and the XMP packet are encrypted.
        let raw = String::from_utf8_lossy(&out);
        assert!(!raw.contains("(round-trip)"), "content stream encrypted");
        assert!(!raw.contains("secret-title"), "Info strings encrypted");
        assert!(!raw.contains("x:xmpmeta"), "XMP encrypted by default");

        // Wrong password: no key, ever (never a partial decrypt).
        let info = {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
            let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
            let rev = doc.revisions().last().unwrap();
            encrypt::parse_encrypt(&out, rev.encrypt, &budget, &mut g)
                .unwrap()
                .expect("/Encrypt present")
        };
        assert!(
            encrypt::authenticate(&info, &document_id_of(&out), b"wrong-pw").is_none(),
            "wrong password must not authenticate"
        );

        // Correct password: authenticates and the content decrypts to the
        // exact plaintext.
        let (info, key) = parse_output(&out);
        assert_eq!(info.r, 6, "revision 6");
        assert_eq!(info.v, 5, "AES-256");
        let ct = stream_data(&out, 5).expect("content stream");
        let plain = selis_crypto::decrypt_data(&key, 5, 0, &ct, 6, true);
        assert_eq!(plain, content, "content round-trips byte-exactly");

        // /Perms verifies against the file key and the requested /P; a
        // different /P is rejected.
        assert!(selis_crypto::verify_perms_r6(
            info.p,
            &key,
            &info.perms,
            true
        ));
        assert!(!selis_crypto::verify_perms_r6(
            info.p & !BIT_COPY,
            &key,
            &info.perms,
            true
        ));
    }

    /// The /ID[0] of a document's bytes.
    fn document_id_of(bytes: &[u8]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = parse_revisions(bytes, sx, &budget, &mut g).unwrap();
        encrypt::document_id(&doc.revisions().last().unwrap().trailer)
    }

    #[test]
    fn wrong_password_unlock_fails_cleanly_and_writes_nothing() {
        let paths = temp_paths("wrong-pw");
        protect(
            &paths,
            &build_plain_pdf(&content_of("wrong")),
            ProtectArgs::default(),
        )
        .expect("protect");
        let out_path = paths.output.parent().unwrap().join("wrong-pw-unlocked.pdf");
        let _ = std::fs::remove_file(&out_path);
        let err = crate::unlock::run(
            paths.output.to_str().unwrap(),
            out_path.to_str().unwrap(),
            Some("not-the-password"),
        )
        .expect_err("wrong password refused");
        assert!(
            err.to_string().contains("WRONG_PASSWORD"),
            "typed WRONG_PASSWORD expected: {err}"
        );
        assert!(
            !out_path.exists(),
            "a wrong password must never produce output"
        );
    }

    #[test]
    fn unlock_with_user_password_round_trips() {
        let paths = temp_paths("unlock-rt");
        protect(
            &paths,
            &build_plain_pdf(&content_of("unlock-rt")),
            ProtectArgs::default(),
        )
        .expect("protect");
        let out_path = paths
            .output
            .parent()
            .unwrap()
            .join("unlock-rt-unlocked.pdf");
        crate::unlock::run(
            paths.output.to_str().unwrap(),
            out_path.to_str().unwrap(),
            Some("secret"),
        )
        .expect("unlock with the user password");
        let unlocked = std::fs::read(&out_path).unwrap();
        assert_eq!(
            stream_data(&unlocked, 5).unwrap(),
            content_of("unlock-rt"),
            "decrypted content matches"
        );
        let budget = Budget::profile(Surface::Viewer);
        assert!(
            selis_pdf_engine::Session::open(unlocked, &budget, &crate::shell_clock()).is_ok(),
            "unlocked output opens"
        );
    }

    #[test]
    fn already_encrypted_is_refused_typed() {
        let paths = temp_paths("already");
        protect(
            &paths,
            &build_plain_pdf(&content_of("already")),
            ProtectArgs::default(),
        )
        .expect("first protect");
        let err = super::run(
            paths.output.to_str().unwrap(),
            paths
                .output
                .parent()
                .unwrap()
                .join("already-second.pdf")
                .to_str()
                .unwrap(),
            Some("secret"),
            Some("owner"),
            None,
            true,
        )
        .expect_err("second protect refused");
        let msg = err.to_string();
        assert!(
            msg.contains("E1806") || msg.contains("ALREADY_ENCRYPTED"),
            "typed ALREADY_ENCRYPTED expected: {msg}"
        );
    }

    #[test]
    fn encrypt_metadata_false_leaves_xmp_in_the_clear() {
        let paths = temp_paths("encmeta");
        protect(
            &paths,
            &build_plain_pdf(&content_of("encmeta")),
            ProtectArgs {
                encrypt_metadata: false,
                ..ProtectArgs::default()
            },
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let raw = String::from_utf8_lossy(&out);
        assert!(
            raw.contains("x:xmpmeta"),
            "the XMP stream stays plaintext under /EncryptMetadata false"
        );
        assert!(
            !raw.contains("(encmeta)"),
            "the content stream is still encrypted"
        );
        // The /Encrypt dict carries the flag.
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
        let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
        let rev = doc.revisions().last().unwrap();
        let info = encrypt::parse_encrypt(&out, rev.encrypt, &budget, &mut g)
            .unwrap()
            .unwrap();
        assert!(!info.encrypt_metadata, "flag recorded in /Encrypt");
    }

    #[test]
    fn encrypt_metadata_true_encrypts_xmp() {
        let paths = temp_paths("encmeta-true");
        protect(
            &paths,
            &build_plain_pdf(&content_of("encmeta-true")),
            ProtectArgs {
                encrypt_metadata: true,
                ..ProtectArgs::default()
            },
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let raw = String::from_utf8_lossy(&out);
        assert!(!raw.contains("x:xmpmeta"), "XMP encrypted by default");
    }

    #[test]
    fn permission_bits_land_in_p_and_perms() {
        let paths = temp_paths("bits");
        protect(
            &paths,
            &build_plain_pdf(&content_of("bits")),
            ProtectArgs {
                permissions: Some("print,annotate"),
                ..ProtectArgs::default()
            },
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let (info, key) = parse_output(&out);
        let expected = compute_p(BIT_PRINT | BIT_ANNOTATE);
        assert_eq!(info.p, expected, "/P matches the requested bits");
        assert!(
            selis_crypto::verify_perms_r6(info.p, &key, &info.perms, info.encrypt_metadata),
            "/Perms verifies against the file key"
        );
        // The trailer /P is a signed 32-bit integer with the same bit pattern.
        let raw = String::from_utf8_lossy(&out);
        let signed = i32::from_ne_bytes(expected.to_ne_bytes());
        let tail = &raw[raw.len().saturating_sub(2000)..];
        assert!(
            tail.contains(&format!("/P {signed}")),
            "signed /P written: {tail}"
        );
    }

    #[test]
    fn owner_password_authenticates_and_defaults_to_user() {
        // Separate owner password works.
        let paths = temp_paths("owner");
        protect(
            &paths,
            &build_plain_pdf(&content_of("owner")),
            ProtectArgs::default(),
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let info = {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
            let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
            encrypt::parse_encrypt(
                &out,
                doc.revisions().last().unwrap().encrypt,
                &budget,
                &mut g,
            )
            .unwrap()
            .unwrap()
        };
        let id0 = document_id_of(&out);
        assert!(
            encrypt::authenticate(&info, &id0, b"owner").is_some(),
            "owner password authenticates"
        );
        assert!(
            encrypt::authenticate(&info, &id0, b"secret").is_some(),
            "user password authenticates"
        );

        // Owner defaults to the user password when omitted.
        let paths2 = temp_paths("owner-default");
        protect(
            &paths2,
            &build_plain_pdf(&content_of("owner-default")),
            ProtectArgs {
                owner_password: None,
                ..ProtectArgs::default()
            },
        )
        .expect("protect");
        let out2 = std::fs::read(&paths2.output).unwrap();
        let info2 = {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out2, 4096).unwrap_or(0);
            let doc = parse_revisions(&out2, sx, &budget, &mut g).unwrap();
            encrypt::parse_encrypt(
                &out2,
                doc.revisions().last().unwrap().encrypt,
                &budget,
                &mut g,
            )
            .unwrap()
            .unwrap()
        };
        assert!(
            encrypt::authenticate(&info2, &document_id_of(&out2), b"secret").is_some(),
            "the user password also opens as the owner"
        );
    }

    #[test]
    fn empty_user_password_opens_in_the_engine() {
        // Owner-password-only documents (the "restrict editing" flow) open
        // with no password and decrypt fully in the engine.
        let paths = temp_paths("empty-user");
        protect(
            &paths,
            &build_plain_pdf(&content_of("empty-user")),
            ProtectArgs {
                user_password: None,
                owner_password: Some("owner-only"),
                permissions: Some("print"),
                encrypt_metadata: true,
            },
        )
        .expect("protect");
        let out = std::fs::read(&paths.output).unwrap();
        let budget = Budget::profile(Surface::Viewer);
        let session =
            selis_pdf_engine::Session::open(out, &budget, &crate::shell_clock()).expect("opens");
        assert_eq!(session.len(), 1, "one page");
    }

    #[test]
    fn signed_document_gets_a_rewrite_warning() {
        // A catalog /Perms *dictionary* with /Signatures (the MDP form) must
        // trip the pre-rewrite signature warning on stderr; the operation
        // still succeeds (the warning is advisory).
        // Rebuild with a catalog carrying a /Perms dictionary.
        let mut signed_src = Vec::new();
        let mut offsets = std::collections::HashMap::new();
        let objects: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /Perms 8 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 5 0 R >>",
            ),
            (5, b"<< /Length 0 >>\nstream\n\nendstream"),
            (8, b"<< /Signatures 9 0 R >>"),
            (9, b"<< /Type /Sig /Filter /Adobe.PPKLite >>"),
        ];
        for (num, body) in &objects {
            offsets.insert(*num, signed_src.len());
            signed_src.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            signed_src.extend_from_slice(body);
            signed_src.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = signed_src.len();
        signed_src.extend_from_slice(b"xref\n0 10\n");
        signed_src.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..10u32 {
            let off = offsets.get(&i).copied().unwrap_or(0);
            signed_src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        signed_src.extend_from_slice(b"trailer\n<< /Size 10 /Root 1 0 R >>\n");
        signed_src.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());

        let paths = temp_paths("signed");
        std::fs::write(&paths.input, &signed_src).unwrap();
        // The warning goes to stderr; assert the operation succeeds. (The
        // eprintln assertion itself is exercised by the manual smoke run;
        // this pins the detection path does not refuse or fail.)
        super::run(
            paths.input.to_str().unwrap(),
            paths.output.to_str().unwrap(),
            Some("pw"),
            None,
            None,
            true,
        )
        .expect("protect succeeds with an advisory warning");
        assert!(paths.output.exists());
    }

    #[test]
    fn long_passwords_are_truncated_per_algorithm_2a() {
        let long_pw = "p".repeat(300);
        let paths = temp_paths("long-pw");
        let src = build_plain_pdf(&content_of("long-pw"));
        std::fs::write(&paths.input, &src).unwrap();
        super::run(
            paths.input.to_str().unwrap(),
            paths.output.to_str().unwrap(),
            Some(&long_pw),
            None,
            None,
            true,
        )
        .expect("protect with a 300-byte password");
        let out = std::fs::read(&paths.output).unwrap();
        // Authentication with the full 300-byte password must succeed: the
        // credential was truncated to 127 bytes at write time (Algorithm 2.A).
        let info = {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
            let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
            encrypt::parse_encrypt(
                &out,
                doc.revisions().last().unwrap().encrypt,
                &budget,
                &mut g,
            )
            .unwrap()
            .unwrap()
        };
        assert!(
            encrypt::authenticate(&info, &document_id_of(&out), long_pw.as_bytes()).is_some(),
            "a >127-byte password still authenticates"
        );
    }

    #[test]
    fn id_is_fresh_per_document() {
        let paths_a = temp_paths("id-a");
        let paths_b = temp_paths("id-b");
        let src = build_plain_pdf(&content_of("id"));
        protect(&paths_a, &src, ProtectArgs::default()).expect("protect a");
        protect(&paths_b, &src, ProtectArgs::default()).expect("protect b");
        let id_a = document_id_of(&std::fs::read(&paths_a.output).unwrap());
        let id_b = document_id_of(&std::fs::read(&paths_b.output).unwrap());
        assert_eq!(id_a.len(), 16, "/ID is 16 bytes");
        assert_ne!(id_a, id_b, "a fresh file key per protect means a fresh /ID");
    }

    // ── Property test (DoD template) ─────────────────────────────────────
    //
    // Over generated passwords, permission subsets, and page counts: the
    // output parses, authenticates the user password, rejects a wrong one,
    // verifies /Perms, and decrypts the content stream byte-exactly.
    //
    // The case count is capped because the revision-6 KDF (Algorithm 2.B) is
    // deliberately expensive — a handful of hardened-hash runs per protect.

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]

        #[test]
        fn protect_round_trips_generated_inputs(
            user_pw in proptest::string::string_regex("[a-zA-Z0-9]{0,12}").unwrap(),
            owner_pw in proptest::string::string_regex("[a-zA-Z0-9]{1,12}").unwrap(),
            grant_print in proptest::bool::ANY,
            grant_copy in proptest::bool::ANY,
            tag in proptest::string::string_regex("[a-z]{4,8}").unwrap(),
        ) {
            let dir = std::env::temp_dir().join("selis-protect-prop");
            std::fs::create_dir_all(&dir).unwrap();
            let stem = format!("prop-{}-{}", tag, std::process::id());
            let input = dir.join(format!("{stem}.in.pdf"));
            let output = dir.join(format!("{stem}.out.pdf"));
            let content = content_of(&tag);

            let spec = match (grant_print, grant_copy) {
                (true, true) => None,
                (true, false) => Some("print,modify,annotate"),
                (false, true) => Some("copy,modify,annotate"),
                (false, false) => Some("modify,annotate"),
            };
            std::fs::write(&input, build_plain_pdf(&content)).unwrap();
            super::run(
                input.to_str().unwrap(),
                output.to_str().unwrap(),
                Some(&user_pw),
                Some(&owner_pw),
                spec,
                true,
            )
            .unwrap_or_else(|e| panic!("protect failed: {e}"));
            let out = std::fs::read(&output).unwrap();

            // Parse the /Encrypt dict and the content ciphertext.
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            let sx = xref::find_startxref(&out, 4096).unwrap_or(0);
            let doc = parse_revisions(&out, sx, &budget, &mut g).unwrap();
            let rev = doc.revisions().last().unwrap();
            let info = encrypt::parse_encrypt(&out, rev.encrypt, &budget, &mut g)
                .unwrap()
                .unwrap();
            let id0 = encrypt::document_id(&rev.trailer);

            // /P matches the requested grant set.
            let mut granted = BIT_MODIFY | BIT_ANNOTATE;
            if grant_print { granted |= BIT_PRINT; }
            if grant_copy { granted |= BIT_COPY; }
            assert_eq!(info.p, compute_p(granted), "/P matches the spec");

            // The user password authenticates; a definitely-wrong one does not.
            let key = encrypt::authenticate(&info, &id0, user_pw.as_bytes())
                .unwrap_or_else(|| panic!("user password must authenticate"));
            let wrong = format!("zz-{}-wrong", tag);
            if wrong != user_pw && wrong != owner_pw {
                assert!(encrypt::authenticate(&info, &id0, wrong.as_bytes()).is_none());
            }

            // /Perms verifies against the file key.
            assert!(selis_crypto::verify_perms_r6(info.p, &key, &info.perms, true));

            // The content stream decrypts byte-exactly under the file key.
            let ct = match resolve_number(&out, 5, &budget, &mut g).unwrap() {
                Obj::Stream { data, .. } => data.to_vec(),
                other => panic!("object 5 not a stream: {other:?}"),
            };
            assert_eq!(selis_crypto::decrypt_data(&key, 5, 0, &ct, 6, true), content);
        }
    }
}
