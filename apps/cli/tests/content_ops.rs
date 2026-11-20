//! End-to-end evidence for the first-class content ops the CLI dispatches
//! (SL-5.TEXTEDIT.07/08). The bridge is shared with the browser tools chunk,
//! so this test also proves the wasm path's engine call and read-back.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::fs;
use std::path::PathBuf;

use serde_json::json;

/// A one-page PDF with one text draw, so there is content to read back and a
/// byte prefix to preserve.
fn text_pdf() -> Vec<u8> {
    let stream = "BT /F1 12 Tf 20 160 Td (Invoice total: 900 USD) Tj ET";
    let objects = vec![
        "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
        "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n".to_string(),
        format!(
            "4 0 obj\n<< /Length {} >>\nstream\n{stream}\nendstream\nendobj\n",
            stream.len()
        ),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(out.len());
        out.extend_from_slice(object.as_bytes());
    }
    let xref = out.len();
    let size = offsets.len().saturating_add(1);
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

fn scratch(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-content-{}-{name}", std::process::id()));
    path
}

#[test]
fn add_text_box_appends_and_the_original_bytes_survive_as_a_prefix() {
    let input = scratch("in.pdf");
    let output = scratch("out.pdf");
    let source = text_pdf();
    fs::write(&input, &source).expect("write fixture");

    let result = selis_pdf_tools::content_edit::run(
        "add_text_box",
        &json!({
            "page": 0,
            "rect": [10.0, 10.0, 120.0, 40.0],
            "text": "Added with add_text_box",
            "font": "Helvetica",
            "font_size": 10.0,
            "color": [0, 0, 0],
        }),
        input.to_str().expect("utf8 path"),
        output.to_str().expect("utf8 path"),
        None,
    )
    .expect("add_text_box succeeds");

    assert_eq!(result.verification.verdict, "ok");
    let produced = fs::read(&output).expect("read output");
    assert!(
        produced.starts_with(&source),
        "the original bytes are a verbatim prefix"
    );
    let text = String::from_utf8_lossy(&produced);
    assert!(
        text.contains("Added with add_text_box"),
        "the new text reads back"
    );
    assert!(
        text.contains("Invoice total"),
        "the original draw is untouched"
    );

    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
}

#[test]
fn list_objects_reports_a_typed_refusal_for_a_bad_id() {
    let input = scratch("bad-in.pdf");
    let output = scratch("bad-out.pdf");
    fs::write(&input, text_pdf()).expect("write fixture");

    let error = selis_pdf_tools::content_edit::run(
        "move_object",
        &json!({
            "page": 0,
            "id": { "name": "Im9", "op_index": 999 },
            "dx": 1.0,
            "dy": 1.0,
        }),
        input.to_str().expect("utf8 path"),
        output.to_str().expect("utf8 path"),
        None,
    )
    .expect_err("an unknown object id is refused");
    assert!(
        error.to_string().contains("list the page's objects first"),
        "the refusal names the cause: {error}"
    );
    assert!(!output.exists(), "a refused op writes no output");

    let _ = fs::remove_file(&input);
}
