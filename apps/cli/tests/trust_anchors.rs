//! SL-5 trust-anchor end-to-end evidence (the "Foxit model").
//!
//! A locally generated self-signed identity is trusted ONLY when its own
//! certificate is supplied as an anchor. With no anchors the report says so,
//! and the verdict is explicitly scoped to Selis caller-supplied anchors — it
//! never implies public/Acrobat trust. A PEM bundle of several CERTIFICATE
//! blocks counts as several anchors.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

fn temp(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("selis-trust-{}-{n}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn one_page_pdf(path: &PathBuf) {
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(out.len());
        out.extend_from_slice(object);
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    std::fs::write(path, out).unwrap();
}

fn generate(dir: &PathBuf, name: &str) {
    let out = selis()
        .arg("generate-identity")
        .arg("--name")
        .arg(name)
        .arg("--output")
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "generate-identity failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a publicly trusted"),
        "honesty note missing"
    );
    assert!(dir.join("key.pem").exists());
    assert!(dir.join("cert.pem").exists());
}

fn sign_with(dir: &PathBuf, input: &PathBuf, output: &PathBuf) {
    let out = selis()
        .arg("sign")
        .arg(input)
        .arg("--output")
        .arg(output)
        .arg("--key")
        .arg(dir.join("key.pem"))
        .arg("--cert")
        .arg(dir.join("cert.pem"))
        .arg("--field")
        .arg("Signature1")
        .arg("--time")
        .arg("20261104120000Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sign failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn report(signed: &PathBuf, anchors: &[PathBuf]) -> serde_json::Value {
    let mut cmd = selis();
    cmd.arg("signatures").arg(signed);
    for anchor in anchors {
        cmd.arg("--anchor").arg(anchor);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "signatures failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn a_local_identity_is_trusted_only_through_its_own_anchor() {
    let dir = temp("local");
    generate(&dir, "Ada Lovelace (Selis local)");
    let input = dir.join("in.pdf");
    let signed = dir.join("signed.pdf");
    one_page_pdf(&input);
    sign_with(&dir, &input, &signed);

    let untrusted = report(&signed, &[]);
    let t0 = &untrusted["signatures"][0]["trust"];
    assert_eq!(t0["trusted"], false);
    assert_eq!(t0["trustScope"], "selis-caller-anchors");
    assert!(t0["reason"].as_str().unwrap().contains("self-signed"));
    assert_eq!(t0["revocationChecked"], false);

    let trusted = report(&signed, &[dir.join("cert.pem")]);
    let t1 = &trusted["signatures"][0]["trust"];
    assert_eq!(t1["trusted"], true);
    assert_eq!(t1["trustScope"], "selis-caller-anchors");
    assert_eq!(t1["chain"][0]["anchor"], true);
    assert_eq!(trusted["signatures"][0]["verified"], true);
}

#[test]
fn a_pem_bundle_of_anchors_is_loaded_in_full() {
    let dir = temp("bundle");
    generate(&dir, "Selis Local CA");
    let input = dir.join("in.pdf");
    let signed = dir.join("signed.pdf");
    one_page_pdf(&input);
    sign_with(&dir, &input, &signed);

    // Put the real anchor SECOND in a bundle, behind a decoy certificate, to
    // prove the loader reads every CERTIFICATE block and not just the first.
    let real = std::fs::read_to_string(dir.join("cert.pem")).unwrap();
    let other = temp("decoy");
    generate(&other, "Decoy CA");
    let decoy = std::fs::read_to_string(other.join("cert.pem")).unwrap();
    let bundle = dir.join("bundle.pem");
    std::fs::write(&bundle, format!("{decoy}\n{real}\n")).unwrap();

    let trusted = report(&signed, &[bundle]);
    assert_eq!(trusted["signatures"][0]["trust"]["trusted"], true);
}

#[test]
fn generate_refuses_to_overwrite_without_force() {
    let dir = temp("force");
    generate(&dir, "First");
    let again = selis()
        .arg("generate-identity")
        .arg("--name")
        .arg("Second")
        .arg("--output")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        !again.status.success(),
        "overwrite must be refused by default"
    );
    assert!(String::from_utf8_lossy(&again.stderr).contains("--force"));

    let forced = selis()
        .arg("generate-identity")
        .arg("--name")
        .arg("Second")
        .arg("--output")
        .arg(&dir)
        .arg("--force")
        .output()
        .unwrap();
    assert!(forced.status.success());
}
