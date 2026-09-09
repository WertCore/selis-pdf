//! TOOL.06 corpus entry: `selis protect` over the committed fixture
//! `corpus/fixtures/protect_unencrypted_source.pdf` (Selis-authored, a7b9700
//! pattern).
//!
//! The DoD for every 1A.TOOL operation: a corpus entry exercising it, the
//! output verified structurally, and an oracle interop check where the local
//! tooling allows. The protected output is deliberately non-deterministic
//! (CSPRNG file key, salts, IVs, /ID), so the corpus carries the unencrypted
//! source fixture and this test exercises the operation end-to-end:
//!
//! 1. `selis protect` with separate user/owner passwords and a restricted
//!    `--permissions` spec;
//! 2. `selis unlock` with the user password decrypts the output; a wrong
//!    password is the typed `WRONG_PASSWORD` error and writes nothing;
//! 3. the qpdf oracle (structural, SL-0.ORACLE.03) recognises the output as
//!    R6/AESv3, reflects the permission bits, and `qpdf --check` passes.
//!
//! The Acrobat interop step of the DoD ("opens in Acrobat and PDFium") is a
//! manual HUMAN sign-off step: open the protected output produced below in
//! Acrobat and PDFium (pdfium_driver is not installed locally). qpdf's
//! Algorithm-10 /Perms validation is the strongest automated proxy available.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::PathBuf;
use std::process::Command;

fn fixture() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("corpus");
    p.push("fixtures");
    p.push("protect_unencrypted_source.pdf");
    p
}

fn out_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("selis-tool06-{tag}-{}.pdf", std::process::id()))
}

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

fn find_qpdf() -> Option<PathBuf> {
    let probe = if cfg!(windows) { "qpdf.exe" } else { "qpdf" };
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(std::path::MAIN_SEPARATOR)
        .map(|dir| PathBuf::from(dir).join(probe))
        .find(|p| p.is_file())
}

/// The fixture itself must be a healthy unencrypted document.
#[test]
fn corpus_fixture_is_a_valid_unencrypted_pdf() {
    let src = std::fs::read(fixture()).expect("fixture present");
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let session = selis_pdf_engine::Session::open(src, &budget).expect("fixture opens");
    assert_eq!(session.len(), 1, "one page");
}

/// Protect with separate user/owner passwords and a restricted spec; the
/// output must decrypt with the user password and refuse a wrong one.
#[test]
fn protect_corpus_fixture_round_trips() {
    let out = out_path("protected");
    let status = selis()
        .arg("protect")
        .arg(fixture())
        .arg("-o")
        .arg(&out)
        .arg("--user-password")
        .arg("corpus-user")
        .arg("--owner-password")
        .arg("corpus-owner")
        .arg("--permissions")
        .arg("print,copy")
        .status()
        .expect("spawn selis protect");
    assert!(status.success(), "protect failed with {status}");
    let protected = std::fs::read(&out).expect("protected output exists");
    assert!(protected.starts_with(b"%PDF-"));

    // The wrong password must produce the typed WRONG_PASSWORD error and no
    // output file (never a partial decrypt).
    let wrong_out = out_path("wrong-pw");
    let failed = selis()
        .arg("unlock")
        .arg(&out)
        .arg("-o")
        .arg(&wrong_out)
        .arg("--password")
        .arg("definitely-not-it")
        .output()
        .expect("spawn selis unlock");
    assert!(
        !failed.status.success(),
        "unlock with a wrong password must fail"
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        stderr.contains("WRONG_PASSWORD"),
        "typed WRONG_PASSWORD expected: {stderr}"
    );
    assert!(!wrong_out.exists(), "no output on a wrong password");

    // The correct user password unlocks; the result opens in the engine.
    let unlocked = out_path("unlocked");
    let status = selis()
        .arg("unlock")
        .arg(&out)
        .arg("-o")
        .arg(&unlocked)
        .arg("--password")
        .arg("corpus-user")
        .status()
        .expect("spawn selis unlock");
    assert!(status.success(), "unlock failed with {status}");
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let session = selis_pdf_engine::Session::open(std::fs::read(&unlocked).unwrap(), &budget)
        .expect("unlocked output opens");
    assert_eq!(session.len(), 1, "one page after unlock");
}

/// Owner-password-only flow (the "restrict editing" shape): no user password,
/// every exposed permission denied, metadata left in the clear. The engine
/// must open and decrypt it with the empty password.
#[test]
fn protect_owner_only_corpus_variant_opens_without_password() {
    let out = out_path("owner-only");
    let status = selis()
        .arg("protect")
        .arg(fixture())
        .arg("-o")
        .arg(&out)
        .arg("--owner-password")
        .arg("corpus-owner")
        .arg("--permissions")
        .arg("none")
        .arg("--no-encrypt-metadata")
        .status()
        .expect("spawn selis protect");
    assert!(status.success(), "protect failed with {status}");
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let session = selis_pdf_engine::Session::open(std::fs::read(&out).unwrap(), &budget)
        .expect("owner-only protected output opens with the empty password");
    assert_eq!(session.len(), 1, "one page");
}

/// qpdf oracle interop (SL-0.ORACLE.03): the protected output is R6/AESv3,
/// the permission bits match the requested spec, no /Perms warning is
/// raised, and `qpdf --check` finds no syntax errors. Skipped loudly when
/// qpdf is not installed locally (CI installs it).
#[test]
fn qpdf_oracle_recognises_protected_output() {
    let Some(qpdf) = find_qpdf() else {
        eprintln!("skipping: qpdf not installed locally");
        return;
    };
    let out = out_path("qpdf-protected");
    let status = selis()
        .arg("protect")
        .arg(fixture())
        .arg("-o")
        .arg(&out)
        .arg("--user-password")
        .arg("corpus-user")
        .arg("--owner-password")
        .arg("corpus-owner")
        .arg("--permissions")
        .arg("print,copy")
        .status()
        .expect("spawn selis protect");
    assert!(status.success(), "protect failed with {status}");

    // --show-encryption: R6, AESv3, supplied password accepted, bits right.
    let show = Command::new(&qpdf)
        .arg("--show-encryption")
        .arg("--password=corpus-user")
        .arg(&out)
        .output()
        .expect("spawn qpdf --show-encryption");
    let text = String::from_utf8_lossy(&show.stdout).to_string();
    let stderr = String::from_utf8_lossy(&show.stderr);
    assert!(
        show.status.success(),
        "qpdf --show-encryption failed: {stderr}"
    );
    assert!(text.contains("R = 6"), "revision 6: {text}");
    assert!(text.contains("AESv3"), "AESv3 everywhere: {text}");
    assert!(
        text.contains("Supplied password is user password"),
        "user password accepted: {text}"
    );
    assert!(text.contains("print high resolution: allowed"), "{text}");
    assert!(text.contains("modify other: not allowed"), "{text}");
    // The Algorithm-10 /Perms check is qpdf's strongest validation of our
    // write path; a mismatch warns on stderr.
    assert!(
        !stderr.contains("/Perms field"),
        "no /Perms mismatch warning: {stderr}"
    );

    // --check: structural validity (oracle for the WRITE.05 obligation).
    let check = Command::new(&qpdf)
        .arg("--check")
        .arg("--password=corpus-user")
        .arg(&out)
        .output()
        .expect("spawn qpdf --check");
    let check_err = String::from_utf8_lossy(&check.stderr);
    assert!(check.status.success(), "qpdf --check failed: {check_err}");
    assert!(
        !check_err.contains("/Perms field"),
        "no /Perms mismatch warning: {check_err}"
    );
}
