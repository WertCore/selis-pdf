//! SL-3.TEXT.14 regression pin: annotation `/AP` appearance text joins
//! extraction, search, and reading order.
//!
//! A page whose content stream is empty but whose annotation carries text in
//! `/AP /N` must extract that text (previously `empty_selis`), with the same
//! encoding chain and word rules as page content. Hidden appearances stay out,
//! as in render.
//!
//! ```text
//! cargo test -p selis-cli --test text14_annot_ap
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};

fn selis_bin() -> String {
    match std::env::var("SELIS_TEST_BIN") {
        Ok(p) if !p.is_empty() => p,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    }
}

fn bytes(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}

fn name(v: &[u8]) -> Obj {
    Obj::Name(bytes(v))
}

fn int(n: i64) -> Obj {
    Obj::Int(n)
}

fn real(v: f64) -> Obj {
    #[allow(clippy::cast_possible_truncation)]
    let scaled = (v * 1000.0).round() as i64;
    Obj::Real { scaled, scale: 3 }
}

/// One page with `page_text` in its content stream (empty when `None`) and one
/// Square annotation whose `/AP /N` form shows `annot_text` with undeclared
/// `/Helvetica` (the synthetic `annotation_appearance` shape).
fn pdf_with(page_text: Option<&[u8]>, annot_text: &[u8]) -> Vec<u8> {
    let budget = selis_sandbox::Budget::unlimited();
    let clock = selis_sandbox::shell_clock();
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    let mut builder = DocumentBuilder::new();
    let page_content: Vec<u8> = match page_text {
        Some(t) => {
            let mut c = b"BT /Helvetica 24 Tf 20 700 Td (".to_vec();
            c.extend_from_slice(t);
            c.extend_from_slice(b") Tj ET");
            c
        }
        None => Vec::new(),
    };
    let content_num = builder.allocate();
    builder.add_object(
        content_num,
        Obj::Stream {
            dict: vec![(
                bytes(b"Length"),
                int(i64::try_from(page_content.len()).unwrap_or(i64::MAX)),
            )],
            data: Bytes::copy_from_slice(&page_content),
        },
    );
    let mut form_content = b"0 0 0 rg BT /Helvetica 24 Tf 20 90 Td (".to_vec();
    form_content.extend_from_slice(annot_text);
    form_content.extend_from_slice(b") Tj ET");
    let form_num = builder.allocate();
    builder.add_object(
        form_num,
        Obj::Stream {
            dict: vec![
                (bytes(b"Type"), name(b"XObject")),
                (bytes(b"Subtype"), name(b"Form")),
                (
                    bytes(b"BBox"),
                    Obj::Array(vec![int(0), int(0), int(400), int(200)]),
                ),
                (
                    bytes(b"Length"),
                    int(i64::try_from(form_content.len()).unwrap_or(i64::MAX)),
                ),
            ],
            data: Bytes::copy_from_slice(&form_content),
        },
    );
    let ap_num = builder.allocate();
    builder.add_object(
        ap_num,
        Obj::Dict(vec![(bytes(b"N"), Obj::Ref(Ref::new(form_num, 0)))]),
    );
    let annot_num = builder.allocate();
    builder.add_object(
        annot_num,
        Obj::Dict(vec![
            (bytes(b"Type"), name(b"Annot")),
            (bytes(b"Subtype"), name(b"Square")),
            (
                bytes(b"Rect"),
                Obj::Array(vec![real(100.0), real(100.0), real(500.0), real(300.0)]),
            ),
            (bytes(b"F"), int(4)),
            (bytes(b"AP"), Obj::Ref(Ref::new(ap_num, 0))),
        ]),
    );
    builder.add_page_with_extra(
        612.0,
        792.0,
        &[Ref::new(content_num, 0)],
        None,
        vec![(
            b"Annots".to_vec(),
            Obj::Array(vec![Obj::Ref(Ref::new(annot_num, 0))]),
        )],
    );
    builder.write(&budget, &mut g).expect("write")
}

fn write_pdf(name: &str, data: &[u8]) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-cli-text14-{name}"));
    std::fs::write(&path, data).expect("write pdf");
    path
}

fn extract(path: &PathBuf, format: &str) -> (i32, String, String) {
    let out = Command::new(selis_bin())
        .arg("extract")
        .arg("--format")
        .arg(format)
        .arg("--page")
        .arg("0")
        .arg(path)
        .output()
        .expect("run selis extract");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn search(path: &PathBuf, query: &str) -> (i32, String, String) {
    let out = Command::new(selis_bin())
        .arg("search")
        .arg(path)
        .arg(query)
        .arg("--page")
        .arg("0")
        .output()
        .expect("run selis search");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Empty page + appearance-only text: previously `empty_selis`, now extracts.
#[test]
fn text14_empty_page_extracts_appearance_text() {
    let path = write_pdf("empty.pdf", &pdf_with(None, b"Annotation appearance"));
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(
        text.contains("Annotation appearance"),
        "appearance text missing: {text}"
    );
    assert!(
        !text.contains("low-confidence"),
        "appearance text is confident, not a marker: {text}"
    );
    let (code, json, err) = extract(&path, "json");
    assert_eq!(code, 0, "extract json failed: {err}");
    // JSON spans are per-word: the two words appear as separate spans.
    assert!(
        json.contains("Annotation") && json.contains("appearance"),
        "JSON must carry appearance text: {json}"
    );
}

/// Page text + appearance text: both walk into the same assembly, in order
/// (page content first, then `/Annots`).
#[test]
fn text14_page_and_appearance_text_join_in_order() {
    let path = write_pdf("both.pdf", &pdf_with(Some(b"Page text"), b"Annot text"));
    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(text.contains("Page text"), "page text missing: {text}");
    assert!(
        text.contains("Annot text"),
        "appearance text missing: {text}"
    );
    let page_pos = text.find("Page text").unwrap_or(usize::MAX);
    let annot_pos = text.find("Annot text").unwrap_or(usize::MAX);
    assert!(
        page_pos < annot_pos,
        "page content precedes appearances: {text}"
    );
}

/// Search finds appearance-only text (matches are normalised lowercase).
#[test]
fn text14_search_finds_appearance_text() {
    let path = write_pdf("search.pdf", &pdf_with(None, b"Findable appearance"));
    let (code, out, err) = search(&path, "Findable");
    assert_eq!(code, 0, "search failed: {err}");
    assert!(
        out.to_lowercase().contains("findable"),
        "search must hit appearance text: {out}"
    );
}
