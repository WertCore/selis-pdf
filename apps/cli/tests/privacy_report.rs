//! `selis privacy` end-to-end: the scan never rewrites the input, and the JSON
//! twin carries the risk shape the browser card reads.

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
fn privacy_reads_without_modifying_the_input() {
    let fixture = fixture_path();
    let before = std::fs::read(&fixture).expect("fixture");

    let out = selis().arg("privacy").arg(&fixture).output().expect("run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["schema"], "selis-privacy/1");
    assert!(report["risk"].is_string(), "an overall risk is reported");
    let findings = report["findings"].as_array().expect("findings array");
    // The fixture carries an optional-content group and embedded files, so the
    // scan must surface at least one risk — a clean report here would be a bug.
    assert!(!findings.is_empty(), "the rich fixture has risks: {report}");

    let after = std::fs::read(&fixture).expect("fixture");
    assert_eq!(before, after, "the scan must not rewrite the input");
}

#[test]
fn privacy_writes_a_report_file() {
    let fixture = fixture_path();
    let out_path = std::env::temp_dir().join(format!("selis-privacy-{}.json", std::process::id()));

    let out = selis()
        .arg("privacy")
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
    assert!(
        json.contains("\"findings\""),
        "report carries findings: {json}"
    );
    let _ = std::fs::remove_file(&out_path);
}
