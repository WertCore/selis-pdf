//! SL-5.ANNOT.02/03 CLI bindings: `selis annot-appearances` rebuilds a missing
//! markup `/AP` in an incremental revision, and `selis annot-ink` writes a
//! multi-stroke `/InkList` without rewriting the source bytes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::process::Command;

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

/// A one-page PDF. `annots` is spliced into the page dictionary verbatim and
/// `annot4`, when given, becomes object 4 before the xref is computed.
fn pdf(annots: &str, annot4: Option<&str>) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> {annots} >>"
        )
        .into_bytes(),
    ];
    if let Some(body) = annot4 {
        objects.push(body.as_bytes().to_vec());
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("selis-annot-cli-{name}-{}", std::process::id()))
}

#[test]
fn annot_appearances_rebuilds_a_missing_ap_in_an_incremental_revision() {
    let with_annot = pdf(
        "/Annots [4 0 R]",
        Some("<< /Type /Annot /Subtype /Square /Rect [100 100 200 200] /C [1 0 0] >>"),
    );
    let in_path = temp("appear-in.pdf");
    let out_path = temp("appear-out.pdf");
    std::fs::write(&in_path, &with_annot).expect("fixture");

    let run = selis()
        .arg("annot-appearances")
        .arg(&in_path)
        .arg("-o")
        .arg(&out_path)
        .arg("--json")
        .output()
        .expect("annot-appearances");
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = std::fs::read(&out_path).expect("output");
    assert!(out.starts_with(&with_annot), "source is a verbatim prefix");
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("/AP"), "a new appearance was written");
    assert!(text.contains("/BBox"), "the form carries a box");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("1 rebuilt"), "{stderr}");

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn annot_ink_writes_every_stroke_as_an_inklist_array() {
    let src = pdf("", None);
    let in_path = temp("ink-in.pdf");
    let out_path = temp("ink-out.pdf");
    std::fs::write(&in_path, &src).expect("fixture");

    let run = selis()
        .arg("annot-ink")
        .arg(&in_path)
        .arg("-o")
        .arg(&out_path)
        .arg("--page")
        .arg("1")
        .arg("--stroke")
        .arg("100,100;110,120;120,100")
        .arg("--stroke")
        .arg("200,200;220,240")
        .arg("--width")
        .arg("2")
        .output()
        .expect("annot-ink");
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = std::fs::read(&out_path).expect("output");
    assert!(out.starts_with(&src), "source is a verbatim prefix");
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("/Subtype /Ink"), "{text}");
    assert!(text.contains("/InkList"), "{text}");
    // The writer's `real` formatter emits three decimals for whole numbers.
    assert!(
        text.contains("100.000 100.000 110.000 120.000 120.000 100.000"),
        "{text}"
    );
    assert!(text.contains("200.000 200.000 220.000 240.000"), "{text}");
    assert!(String::from_utf8_lossy(&run.stderr).contains("2 strokes"));

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn a_single_point_stroke_is_refused_before_any_write() {
    let in_path = temp("refuse-in.pdf");
    let out_path = temp("refuse-out.pdf");
    std::fs::write(&in_path, pdf("", None)).expect("fixture");

    let run = selis()
        .arg("annot-ink")
        .arg(&in_path)
        .arg("-o")
        .arg(&out_path)
        .arg("--stroke")
        .arg("1,2")
        .output()
        .expect("annot-ink");
    assert!(!run.status.success());
    assert!(!out_path.exists(), "a refusal leaves no file behind");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("two or more points"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&in_path);
}
