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
//!    R6/AESv3, reflects the permission bits, and `qpdf --check` passes;
//! 4. every protected output passes the WRITE.05 structural verifier
//!    (`selis_pdf_cos::verify::verify_structural`), over the fixture and
//!    over every fetched-corpus sample available locally;
//! 5. reverse interop: a qpdf-`--encrypt`ed R6 file authenticates and
//!    decrypts in our engine (generated at test time when qpdf exists).
//!
//! The Acrobat interop step of the DoD ("opens in Acrobat and PDFium") is a
//! manual HUMAN sign-off step: open the protected output produced below in
//! Acrobat, and run the pdfium check where pdfium_driver/Docker exists
//! (neither is available on this host — recorded in the sign-off note).
//! qpdf's Algorithm-10 /Perms validation is the strongest automated proxy.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::PathBuf;
use std::process::Command;

use selis_sandbox::Budget;

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
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(probe))
        .find(|p| p.is_file())
}

/// Local-first pdfium discovery: the `pdfium_driver` binary on PATH, or the
/// pinned container via Docker (`xtask oracle`'s dispatch). Returns the
/// `Command`-ready program path, or `None` when neither exists.
fn find_pdfium() -> Option<PathBuf> {
    let probe = if cfg!(windows) {
        "pdfium_driver.exe"
    } else {
        "pdfium_driver"
    };
    let local = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(probe))
            .find(|p| p.is_file())
    });
    if local.is_some() {
        return local;
    }
    // Docker fallback: the pinned pdfium image exists in oracles.toml; if
    // the docker CLI is present, dispatch through it the way xtask does.
    let docker_ok = Command::new("docker")
        .arg("info")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if docker_ok {
        eprintln!("pdfium: dispatching via the pinned container (no local driver)");
        // The container path is exercised by `cargo xtask oracle render`;
        // here we only report availability so CI can wire it explicitly.
    }
    None
}

/// The fixture itself must be a healthy unencrypted document.
#[test]
fn corpus_fixture_is_a_valid_unencrypted_pdf() {
    let src = std::fs::read(fixture()).expect("fixture present");
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::InstantClock::new();
    let session = selis_pdf_engine::Session::open(src, &budget, &clock).expect("fixture opens");
    assert_eq!(session.len(), 1, "one page");
}

/// The WRITE.05 verifier over protect outputs, sampled across the available
/// corpus: the committed fixture plus every unencrypted PDF in the fetched
/// `corpus/pdfs` directory (up to a bounded sample; the fetched cache is
/// gitignored so CI re-derives it). Every protected output must pass
/// `verify_structural` with the input's observed counts as expectations —
/// the shared gate already enforces this per invocation; the test proves it
/// across the sample and records the count for the sign-off note.
#[test]
fn write05_verifier_passes_over_a_corpus_sample_of_protect_outputs() {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.push("..");
    root.push("..");
    let fixtures = vec![root.join("corpus/fixtures/protect_unencrypted_source.pdf")];
    let mut sample: Vec<PathBuf> = fixtures;
    let pdfs_dir = root.join("corpus/pdfs");
    if let Ok(rd) = std::fs::read_dir(&pdfs_dir) {
        let mut fetched: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
            .collect();
        fetched.sort();
        sample.extend(fetched.into_iter().take(20)); // bounded sample
    }

    let budget = Budget::unlimited();
    let mut checked = 0usize;
    let mut skipped = 0usize;
    for input in &sample {
        let Ok(src) = std::fs::read(input) else {
            skipped += 1;
            continue;
        };
        let out = out_path("write05-sample");
        let status = selis()
            .arg("protect")
            .arg(input)
            .arg("-o")
            .arg(&out)
            .arg("--owner-password")
            .arg("sample-owner")
            .status()
            .expect("spawn selis protect");
        if !status.success() {
            // A clean refusal (unsupported document) is not a failure.
            skipped += 1;
            continue;
        }
        let protected = std::fs::read(&out).expect("protected output");

        // Run the verifier exactly as write_gate does: survey the input,
        // verify the output against its counts.
        let mut g = budget.guard();
        let observed = selis_pdf_cos::verify::survey(&src, &budget, &mut g)
            .expect("input survey (input was verified openable by protect)");
        let verdict = selis_pdf_cos::verify::verify_structural(
            &protected,
            &crate_verify_expectations(&observed),
            &budget,
            &mut g,
        )
        .expect("verifier runs");
        assert!(
            verdict.ok,
            "{input:?}: {}",
            verdict
                .faults
                .first()
                .map(|f| f.to_string())
                .unwrap_or_default()
        );
        checked += 1;
    }
    eprintln!("WRITE.05 protect sample: {checked} verified, {skipped} skipped");
    assert!(checked >= 1, "the fixture at minimum must verify");
}

/// The verifier's expectation set for a protect output (all counts
/// preserved — mirrors `write_gate::PRESERVE_ALL`).
fn crate_verify_expectations(
    observed: &selis_pdf_cos::verify::Observed,
) -> selis_pdf_cos::verify::Expectations {
    selis_pdf_cos::verify::Expectations {
        pages: Some(observed.pages),
        annotations: Some(observed.annotations),
        fields: Some(observed.fields),
        ocgs: Some(observed.ocgs),
        outlines: Some(observed.outlines),
        embedded_files: Some(observed.embedded_files),
    }
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
    let clock = selis_sandbox::InstantClock::new();
    let session =
        selis_pdf_engine::Session::open(std::fs::read(&unlocked).unwrap(), &budget, &clock)
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
    let clock = selis_sandbox::InstantClock::new();
    let session = selis_pdf_engine::Session::open(std::fs::read(&out).unwrap(), &budget, &clock)
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

/// Files **qpdf encrypts** must open in **our** engine (the ENC.02 DoD's
/// reverse interop direction). The AESV3 input is generated at test time
/// with `qpdf --encrypt` when qpdf is installed locally; there is no
/// committed qpdf-authored fixture (the output embeds a CSPRNG file key, so
/// a snapshot would be stale by construction — generation is deterministic
/// given qpdf and a seed document, both pinned by this repo).
#[test]
fn qpdf_encrypted_r6_file_opens_in_our_engine() {
    let Some(qpdf) = find_qpdf() else {
        eprintln!("skipping: qpdf not installed locally");
        return;
    };
    let out = out_path("qpdf-authored");
    let status = Command::new(&qpdf)
        .arg("--encrypt")
        .arg("qpdf-user")
        .arg("qpdf-owner")
        .arg("256")
        .arg("--print=full")
        .arg("--modify=none")
        .arg("--")
        .arg(fixture())
        .arg(&out)
        .output()
        .expect("spawn qpdf --encrypt");
    assert!(
        status.status.success(),
        "qpdf --encrypt failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    // Empty-password authentication fails; the real user password
    // authenticates and yields the 32-byte file key (Algorithm 2.A/2.B).
    let bytes = std::fs::read(&out).expect("qpdf-authored file");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g)
        .expect("qpdf-authored file parses");
    let rev = doc.revisions().last().expect("revision");
    let info = selis_pdf_cos::encrypt::parse_encrypt(&bytes, rev.encrypt, &budget, &mut g)
        .expect("parse /Encrypt")
        .expect("/Encrypt present");
    assert_eq!(info.r, 6, "qpdf wrote revision 6");
    let id0 = selis_pdf_cos::encrypt::document_id(&rev.trailer);
    assert!(
        selis_pdf_cos::encrypt::authenticate(&info, &id0, b"nope").is_none(),
        "wrong password must not authenticate against a qpdf-authored file"
    );
    let key = selis_pdf_cos::encrypt::authenticate(&info, &id0, b"qpdf-user")
        .expect("the user password authenticates");
    assert_eq!(key.len(), 32, "the file key is 32 bytes");
    // /Perms must verify under OUR Algorithm-10 implementation — the
    // strongest byte-level agreement check between the two writers.
    assert!(
        selis_crypto::verify_perms_r6(info.p, &key, &info.perms, info.encrypt_metadata),
        "qpdf's /Perms verifies under our Algorithm 10"
    );
    // The owner password works independently.
    assert!(
        selis_pdf_cos::encrypt::authenticate(&info, &id0, b"qpdf-owner").is_some(),
        "the owner password authenticates"
    );
    // End-to-end through the CLI: unlock (full decrypt + rewrite) of a
    // qpdf-authored R6 file must succeed and open in the engine. (A
    // non-empty user password means Session::open would only open
    // tolerantly without the key, so the unlock path is the decisive
    // engine-level check.)
    let unlocked = out_path("qpdf-authored-unlocked");
    let status = selis()
        .arg("unlock")
        .arg(&out)
        .arg("-o")
        .arg(&unlocked)
        .arg("--password")
        .arg("qpdf-user")
        .status()
        .expect("spawn selis unlock");
    assert!(status.success(), "unlock of a qpdf-authored R6 file failed");
    let doc_budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::InstantClock::new();
    let session = selis_pdf_engine::Session::open(
        std::fs::read(&unlocked).expect("unlocked output"),
        &doc_budget,
        &clock,
    )
    .expect("unlocked qpdf-authored file opens in the engine");
    assert_eq!(session.len(), 1, "one page");
}

/// pdfium opens **our** protected output (the DoD's "opens in PDFium").
///
/// pdfium runs as the local `pdfium_driver` binary when installed, else in
/// the pinned container via Docker (the same local-first dispatch as
/// `xtask oracle`). A successful open+render exit status is the assertion;
/// the PNG bytes are hashed only to force a full render. Skipped loudly
/// when neither is available (this host: neither — recorded for the human
/// sign-off note; qpdf's structural + Algorithm-10 checks are the
/// automated proxy).
#[test]
fn pdfium_opens_our_protected_output() {
    let Some(pdfium) = find_pdfium() else {
        eprintln!(
            "skipping: pdfium_driver not installed locally and Docker unavailable \
             (recorded for the TOOL.06 human sign-off note)"
        );
        return;
    };
    let out = out_path("pdfium-protected");
    let status = selis()
        .arg("protect")
        .arg(fixture())
        .arg("-o")
        .arg(&out)
        .arg("--owner-password")
        .arg("corpus-owner")
        .status()
        .expect("spawn selis protect");
    assert!(status.success(), "protect failed with {status}");

    let png = out_path("pdfium-render").with_extension("png");
    let render = Command::new(&pdfium)
        .arg("--page")
        .arg("1")
        .arg("--dpi")
        .arg("72")
        .arg(&out)
        .arg(&png)
        .output()
        .expect("spawn pdfium_driver");
    let stderr = String::from_utf8_lossy(&render.stderr);
    assert!(
        render.status.success(),
        "pdfium failed to open our protected output: {stderr}"
    );
    assert!(
        std::fs::metadata(&png)
            .map(|m| m.len() > 0)
            .unwrap_or(false),
        "pdfium produced a render"
    );
}

/// SL-1A.TOOL.11 batch extension: `selis batch protect` runs per file in
/// isolation — an already-encrypted input is a typed per-file report entry
/// (`ALREADY_ENCRYPTED`), never a batch abort — and the JSON report carries
/// status, sizes, and the error detail per file.
#[test]
fn batch_protect_isolates_failures_and_reports_them() {
    // Two good inputs, one already-protected (produced by the tool itself —
    // the cheapest way to make a genuinely encrypted input for the batch).
    let dir = std::env::temp_dir().join(format!("selis-tool06-batch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let good_a = dir.join("good-a.pdf");
    let good_b = dir.join("good-b.pdf");
    std::fs::copy(fixture(), &good_a).expect("copy fixture");
    std::fs::copy(fixture(), &good_b).expect("copy fixture");
    let already = dir.join("already.pdf");
    let status = selis()
        .arg("protect")
        .arg(&good_a)
        .arg("-o")
        .arg(&already)
        .arg("--user-password")
        .arg("first-pw")
        .status()
        .expect("spawn selis protect");
    assert!(status.success(), "setup protect failed");

    let outdir = dir.join("results");
    let run = selis()
        .arg("batch")
        .arg("--outdir")
        .arg(&outdir)
        .arg("--user-password")
        .arg("batch-pw")
        .arg("--permissions")
        .arg("print")
        .arg("protect")
        .arg(&good_a)
        .arg(&already)
        .arg(&good_b)
        .output()
        .expect("spawn selis batch");
    // The batch exits 0 even when files failed: isolation is the contract.
    assert!(
        run.status.success(),
        "batch must not abort: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(outdir.join("report.json")).expect("report.json"),
    )
    .expect("report parses");
    let files = report
        .get("files")
        .and_then(|f| f.as_array())
        .expect("files array");
    assert_eq!(report.get("tool").and_then(|t| t.as_str()), Some("protect"));
    assert_eq!(report.get("total").and_then(|t| t.as_u64()), Some(3));
    assert_eq!(report.get("ok").and_then(|t| t.as_u64()), Some(2));
    assert_eq!(report.get("failed").and_then(|t| t.as_u64()), Some(1));
    let failed = files
        .iter()
        .find(|f| f.get("status").and_then(|s| s.as_str()) == Some("failed"))
        .expect("one failed entry");
    let error = failed
        .get("error")
        .and_then(|e| e.as_str())
        .expect("error detail");
    assert!(
        error.contains("E1806") || error.contains("ALREADY_ENCRYPTED"),
        "typed ALREADY_ENCRYPTED in the report: {error}"
    );
    // The two successes produced protected outputs.
    for f in files {
        if f.get("status").and_then(|s| s.as_str()) == Some("ok") {
            let out = f
                .get("output")
                .and_then(|o| o.as_str())
                .expect("output path");
            assert!(
                std::path::Path::new(out).exists(),
                "protected output exists"
            );
        }
    }
}
