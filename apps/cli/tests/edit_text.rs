//! `selis edit-text` end-to-end: replacing a run of text writes an incremental
//! update whose prefix is the original bytes verbatim, the new text shows, and
//! the input file is never touched.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::process::Command;

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

/// A one-page PDF whose content shows `(Invoice total: 900 USD)`.
fn invoice_pdf() -> Vec<u8> {
    let content = b"BT /F1 12 Tf 10 100 Td (Invoice total: 900 USD) Tj ET\n";
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n".to_vec(),
        {
            let mut o = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).into_bytes();
            o.extend_from_slice(content);
            o.extend_from_slice(b"\nendstream\nendobj\n");
            o
        },
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
    ];
    build_xref(objects)
}

fn build_xref(objects: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(out.len());
        out.extend_from_slice(&object);
    }
    let xref = out.len();
    let size = offsets.len() + 1;
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

#[test]
fn edits_text_as_an_incremental_update() {
    let dir = std::env::temp_dir().join("selis-edit-text-it");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.pdf");
    let output = dir.join("out.pdf");
    let original = invoice_pdf();
    std::fs::write(&input, &original).unwrap();

    let status = selis()
        .arg("edit-text")
        .arg(&input)
        .args(["--find", "900", "--replace", "1250", "--page", "0"])
        .arg("-o")
        .arg(&output)
        .status()
        .expect("run selis edit-text");
    assert!(status.success(), "edit-text exited with failure");

    // The input file is untouched.
    assert_eq!(std::fs::read(&input).unwrap(), original, "input preserved");

    let out = std::fs::read(&output).unwrap();
    // The incremental-update invariant: the original is a byte-identical prefix.
    assert_eq!(
        &out[..original.len()],
        &original[..],
        "original is the prefix"
    );
    assert!(out.len() > original.len(), "the update only appends");

    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("(Invoice total: 1250 USD) Tj"),
        "the replacement shows: {text}"
    );
    // The old number survives only inside the original prefix, not the new revision.
    assert!(
        text[original.len()..].contains("1250") && !text[original.len()..].contains("900 USD"),
        "the new revision carries the edit"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn missing_text_fails_and_writes_nothing() {
    let dir = std::env::temp_dir().join("selis-edit-text-it-missing");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.pdf");
    let output = dir.join("out.pdf");
    std::fs::write(&input, invoice_pdf()).unwrap();
    let _ = std::fs::remove_file(&output);

    let status = selis()
        .arg("edit-text")
        .arg(&input)
        .args(["--find", "Nonexistent", "--replace", "x"])
        .arg("-o")
        .arg(&output)
        .status()
        .expect("run selis edit-text");
    assert!(!status.success(), "a missing search must fail");
    assert!(!output.exists(), "no file is written on refusal");

    let _ = std::fs::remove_file(&input);
}
