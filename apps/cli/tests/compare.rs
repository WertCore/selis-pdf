//! `selis compare` end-to-end: a file compared with itself is identical, and a
//! file compared with a visibly-changed copy is not. Read-only on both inputs.
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
fn a_file_compared_with_itself_is_identical() {
    let fixture = fixture_path();
    let out = selis()
        .arg("compare")
        .arg(&fixture)
        .arg(&fixture)
        .arg("--dpi")
        .arg("72")
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["identical"], true);
    assert_eq!(report["text_identical"], true);
    assert_eq!(report["pages"][0]["pixels_identical"], true);
}

#[test]
fn a_changed_copy_is_not_identical() {
    let fixture = fixture_path();
    let inverted = std::env::temp_dir().join(format!("selis-cmp-{}.pdf", std::process::id()));

    let invert = selis()
        .arg("invert")
        .arg(&fixture)
        .arg(&inverted)
        .arg("--dpi")
        .arg("72")
        .output()
        .expect("invert");
    assert!(
        invert.status.success(),
        "invert: {}",
        String::from_utf8_lossy(&invert.stderr)
    );

    let out = selis()
        .arg("compare")
        .arg(&fixture)
        .arg(&inverted)
        .arg("--dpi")
        .arg("72")
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(
        report["identical"], false,
        "inverted colours differ: {report}"
    );
    assert_eq!(report["pages"][0]["pixels_identical"], false);
    let _ = std::fs::remove_file(&inverted);
}
