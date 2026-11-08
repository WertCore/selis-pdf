//! `selis form-fields` and `selis fill-forms`: read the AcroForm as JSON,
//! fill it, commit, and keep the input untouched.
//!
//! The fixture is a one-page document with one text field, one checkbox, one
//! choice and a two-widget radio group. Every claim the parity checklist
//! makes about this tool is asserted here: the list names the fields, the
//! fill writes values that read back, and the input bytes are unchanged.
//!
//! ```text
//! cargo test -p selis-cli --test fill_forms
//! ```
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::PathBuf;
use std::process::Command;

fn selis_bin() -> String {
    match std::env::var("SELIS_TEST_BIN") {
        Ok(path) if !path.is_empty() => path,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    }
}

fn temp_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-cli-fill-forms-{name}"));
    path
}

fn run_selis(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(selis_bin())
        .args(args)
        .output()
        .expect("run selis");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Build a valid xref for arbitrary object bodies, so the fixture is honest
/// rather than hand-counted.
fn valid_form_pdf() -> Vec<u8> {
    let objects: &[(u32, &[u8])] = &[
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R 7 0 R 9 0 R 12 0 R 13 0 R] >>",
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        (
            5,
            b"<< /Fields [6 0 R 7 0 R 9 0 R 11 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 15 0 R >> >> >>",
        ),
        (
            6,
            b"<< /FT /Tx /T (Name) /V (Old) /Subtype /Widget /Rect [10 10 200 30] /P 3 0 R /DA (/Helv 12 Tf 0 g) >>",
        ),
        (
            7,
            b"<< /FT /Btn /T (Agree) /V /Off /Subtype /Widget /Rect [10 40 30 60] /P 3 0 R /AP << /N << /Off 8 0 R /Yes 8 0 R >> >> >>",
        ),
        (
            8,
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream",
        ),
        (
            9,
            b"<< /FT /Ch /T (Color) /V (Red) /Opt [(Red) (Green) (Blue)] /Subtype /Widget /Rect [10 70 120 90] /P 3 0 R >>",
        ),
        (
            10,
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream",
        ),
        (
            11,
            b"<< /FT /Btn /Ff 32768 /T (Size) /V /Small /Kids [12 0 R 13 0 R] >>",
        ),
        (
            12,
            b"<< /Subtype /Widget /Parent 11 0 R /Rect [10 100 30 120] /P 3 0 R /AP << /N << /Off 10 0 R /Small 10 0 R >> >> >>",
        ),
        (
            13,
            b"<< /Subtype /Widget /Parent 11 0 R /Rect [40 100 60 120] /P 3 0 R /AP << /N << /Off 10 0 R /Large 10 0 R >> >> >>",
        ),
        (
            15,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        ),
    ];
    let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = vec![0u64; max as usize + 1];
    for (num, body) in objects {
        offsets[*num as usize] = out.len() as u64;
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for index in 1..=max {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            max + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn form_fields_lists_the_fixture() {
    let input = temp_path("list.pdf");
    std::fs::write(&input, valid_form_pdf()).expect("write fixture");
    let (code, stdout, stderr) = run_selis(&["form-fields", input.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json report");
    let fields = value["fields"].as_array().expect("fields");
    let names: Vec<&str> = fields
        .iter()
        .filter_map(|field| field["name"].as_str())
        .collect();
    assert_eq!(names, ["Name", "Agree", "Color", "Size"]);
    assert_eq!(value["has_signature"], serde_json::Value::Bool(false));
    assert_eq!(fields[0]["kind"], "text");
    assert_eq!(fields[0]["value"], "Old");
    assert_eq!(fields[3]["kind"], "radio");
    assert_eq!(fields[3]["value"], "Small");
}

#[test]
fn fill_forms_sets_values_and_keeps_the_input() {
    let input = temp_path("fill.pdf");
    let before = valid_form_pdf();
    std::fs::write(&input, &before).expect("write fixture");
    let output = temp_path("fill-out.pdf");
    let (code, _stdout, stderr) = run_selis(&[
        "fill-forms",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--set",
        "Name=Ada Lovelace",
        "--set",
        "Agree=Yes",
        "--set",
        "Color=Green",
        "--set",
        "Size=Large",
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(&input).expect("read input"), before);
    let (code, stdout, stderr) = run_selis(&["form-fields", output.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json report");
    let fields = value["fields"].as_array().expect("fields");
    let find = |name: &str| {
        fields
            .iter()
            .find(|field| field["name"] == name)
            .and_then(|field| field["value"].as_str().map(str::to_string))
    };
    assert_eq!(find("Name").as_deref(), Some("Ada Lovelace"));
    assert_eq!(find("Agree").as_deref(), Some("Yes"));
    assert_eq!(find("Color").as_deref(), Some("Green"));
    assert_eq!(find("Size").as_deref(), Some("Large"));
}

#[test]
fn fill_forms_refuses_a_bad_choice_and_writes_nothing() {
    let input = temp_path("bad.pdf");
    std::fs::write(&input, valid_form_pdf()).expect("write fixture");
    let output = temp_path("bad-out.pdf");
    let (code, _stdout, stderr) = run_selis(&[
        "fill-forms",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--set",
        "Color=Mauve",
    ]);
    assert_ne!(code, 0);
    assert!(stderr.contains("no option"), "{stderr}");
    assert!(!output.exists(), "a refused fill writes no file");
}

#[test]
fn fill_forms_rejects_a_set_pair_without_an_equals_sign() {
    let input = temp_path("pair.pdf");
    std::fs::write(&input, valid_form_pdf()).expect("write fixture");
    let output = temp_path("pair-out.pdf");
    let (code, _stdout, stderr) = run_selis(&[
        "fill-forms",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--set",
        "Name",
    ]);
    assert_ne!(code, 0);
    assert!(stderr.contains("NAME=VALUE"), "{stderr}");
    assert!(!output.exists());
}
