//! `selis fingerprint` end-to-end: the read never rewrites the input, and the
//! JSON twin carries the fingerprint shape the browser card reads.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::PathBuf;
use std::process::Command;

fn fixture_path() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("corpus");
    p.push("fixtures");
    p.push("verification_features.pdf");
    p
}

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

#[test]
fn fingerprint_reads_without_modifying_the_input() {
    let fixture = fixture_path();
    let before = std::fs::read(&fixture).expect("fixture");

    let out = selis()
        .arg("fingerprint")
        .arg(&fixture)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["pages"], 2);
    assert!(report["ids"].is_object(), "ids present");
    assert!(report["fonts"].is_array(), "fonts present");
    assert!(report["images"].is_array(), "images present");

    let after = std::fs::read(&fixture).expect("fixture");
    assert_eq!(before, after, "the read must not rewrite the input");
}

#[test]
fn fingerprint_writes_a_report_file() {
    let fixture = fixture_path();
    let out_path = std::env::temp_dir().join(format!("selis-fp-{}.json", std::process::id()));

    let out = selis()
        .arg("fingerprint")
        .arg(&fixture)
        .arg("-o")
        .arg(&out_path)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let json = std::fs::read_to_string(&out_path).expect("report file");
    assert!(json.contains("\"fonts\""), "report carries fonts: {json}");
    let _ = std::fs::remove_file(&out_path);
}
