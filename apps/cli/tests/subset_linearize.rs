//! CLI coverage for the `subset-fonts` and `linearize` subcommands.
//!
//! Both tests build a real PDF fixture with the engine's own writer, run the
//! binary, then RE-OPEN the output with the engine/COS parser and read the JSON
//! report back. Nothing here matches the fixture's source bytes — the claims
//! are checked against the document that was actually written.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)]

use std::path::PathBuf;
use std::process::Command;

use selis_bytes::Bytes;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::Budget;

const LIBERATION: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("selis-cli-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn name(value: &[u8]) -> Obj {
    Obj::Name(Bytes::copy_from_slice(value))
}

fn stream(dict: Vec<(Bytes, Obj)>, data: &[u8]) -> Obj {
    Obj::Stream {
        dict,
        data: Bytes::copy_from_slice(data),
    }
}

/// A one-page WinAnsi PDF embedding the real 139 KiB LiberationSans program
/// and showing "Hello, world!". A subset is far smaller than the source.
fn winansi_pdf() -> Vec<u8> {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let mut builder = selis_pdf_cos::doc_writer::DocumentBuilder::new();

    let font_bytes = LIBERATION;
    let file_num = builder.allocate();
    builder.add_object(
        file_num,
        stream(
            vec![
                (
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(font_bytes.len()).unwrap_or(i64::MAX)),
                ),
                (
                    Bytes::copy_from_slice(b"Length1"),
                    Obj::Int(i64::try_from(font_bytes.len()).unwrap_or(i64::MAX)),
                ),
            ],
            font_bytes,
        ),
    );
    let font = builder.allocate();
    builder.add_object(
        font,
        Obj::Dict(vec![
            (Bytes::copy_from_slice(b"Type"), name(b"Font")),
            (Bytes::copy_from_slice(b"Subtype"), name(b"TrueType")),
            (
                Bytes::copy_from_slice(b"BaseFont"),
                name(b"ABCDEE+LiberationSans"),
            ),
            (
                Bytes::copy_from_slice(b"Encoding"),
                name(b"WinAnsiEncoding"),
            ),
            (Bytes::copy_from_slice(b"FirstChar"), Obj::Int(32)),
            (Bytes::copy_from_slice(b"LastChar"), Obj::Int(126)),
            (
                Bytes::copy_from_slice(b"FontDescriptor"),
                Obj::Dict(vec![
                    (
                        Bytes::copy_from_slice(b"FontName"),
                        name(b"ABCDEE+LiberationSans"),
                    ),
                    (Bytes::copy_from_slice(b"Flags"), Obj::Int(4)),
                    (
                        Bytes::copy_from_slice(b"FontFile2"),
                        Obj::Ref(Ref::new(file_num, 0)),
                    ),
                ]),
            ),
        ]),
    );
    let resources = builder.allocate();
    builder.add_object(
        resources,
        Obj::Dict(vec![(
            Bytes::copy_from_slice(b"Font"),
            Obj::Dict(vec![(
                Bytes::copy_from_slice(b"F1"),
                Obj::Ref(Ref::new(font, 0)),
            )]),
        )]),
    );
    let content = b"BT /F1 24 Tf 72 700 Td (Hello, world!) Tj ET\n";
    let content_num = builder.allocate();
    builder.add_object(
        content_num,
        stream(
            vec![(
                Bytes::copy_from_slice(b"Length"),
                Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
            )],
            content,
        ),
    );
    builder.add_page_with(
        612.0,
        792.0,
        &[Ref::new(content_num, 0)],
        Some(Ref::new(resources, 0)),
    );
    builder.write(&budget, &mut g).expect("write fixture")
}

/// A three-page PDF with distinct content on each page, plus an Info dict.
fn three_page_pdf() -> Vec<u8> {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let mut b = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    b.add_page(200.0, 300.0, b"BT (PAGE-ONE) Tj ET\n");
    b.add_page(200.0, 300.0, b"BT (PAGE-TWO) Tj ET\n");
    b.add_page(200.0, 300.0, b"BT (PAGE-THREE) Tj ET\n");
    b.set_info(b"Title", "linearize CLI fixture");
    b.write(&budget, &mut g).expect("write fixture")
}

/// Re-open the written bytes through the COS verifier and report the page
/// count. `survey` follows the xref `/Prev` chain, so it sees every page of a
/// linearized file (whose final xref describes only the first page). This is a
/// structural read-back, not a scan of the source text.
fn reopen_pages(bytes: &[u8]) -> usize {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let observed = selis_pdf_cos::verify::survey(bytes, &budget, &mut g).expect("output reopens");
    usize::try_from(observed.pages).expect("page count fits usize")
}

fn run_json(args: &[&str]) -> serde_json::Value {
    let out = selis()
        .args(args)
        .arg("--json")
        .output()
        .expect("run selis");
    assert!(
        out.status.success(),
        "selis failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON {stdout:?}: {e}"))
}

fn verification_of(report: &serde_json::Value) -> &serde_json::Value {
    report.get("verification").expect("verification envelope")
}

#[test]
fn subset_fonts_cli_writes_a_smaller_verified_pdf_and_reports_glyphs() {
    let dir = temp_dir("subset-fonts");
    let input = dir.join("in.pdf");
    let output = dir.join("out.pdf");
    let source = winansi_pdf();
    std::fs::write(&input, &source).expect("write input");

    let report = run_json(&[
        "subset-fonts",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    let v = verification_of(&report);
    assert_eq!(
        v.get("verdict").and_then(serde_json::Value::as_str),
        Some("ok")
    );
    assert_eq!(
        v.get("pages_out").and_then(serde_json::Value::as_u64),
        Some(1)
    );
    assert_eq!(
        v.get("fonts_subset").and_then(serde_json::Value::as_u64),
        Some(1)
    );
    let glyphs = v
        .get("glyphs_kept")
        .and_then(serde_json::Value::as_u64)
        .expect("glyphs kept in the report");
    assert!(glyphs >= 8, "kept the distinct letters, got {glyphs}");

    let bytes = std::fs::read(&output).expect("output was written");
    assert!(
        bytes.len() < source.len(),
        "subset is smaller than the source"
    );
    assert_eq!(
        reopen_pages(&bytes),
        1,
        "output reopens with its page intact"
    );
    // Read the embedded font program back out of the OUTPUT and prove it is a
    // real, smaller TrueType stream — not just a smaller PDF container.
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let start = selis_pdf_cos::xref::find_startxref(&bytes, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(&bytes, start, &budget, &mut g).expect("parse output");
    let mut font_program = 0usize;
    for num in 1..64u32 {
        let Ok(obj) =
            selis_pdf_cos::copy::resolve_ref(&bytes, &doc, Ref::new(num, 0), &budget, &mut g)
        else {
            continue;
        };
        if let Obj::Stream { dict, data } = obj {
            let flate = dict.iter().any(|(key, value)| {
                key.as_slice() == b"Filter"
                    && matches!(value, Obj::Name(n) if n.as_slice() == b"FlateDecode")
            });
            let decoded = if flate {
                selis_pdf_filter::flate_decode_bounded(&data, 256 * 1024 * 1024, &mut g)
                    .unwrap_or_default()
            } else {
                data.as_slice().to_vec()
            };
            if decoded.starts_with(b"\x00\x01\x00\x00") {
                font_program = font_program.max(decoded.len());
            }
        }
    }
    assert!(
        font_program > 0 && font_program < LIBERATION.len() / 4,
        "the written font program is a real subset ({font_program} B of {} B)",
        LIBERATION.len()
    );
}

#[test]
fn linearize_cli_writes_a_consistent_linearized_pdf_and_reports_it() {
    let dir = temp_dir("linearize");
    let input = dir.join("in.pdf");
    let output = dir.join("out.pdf");
    std::fs::write(&input, three_page_pdf()).expect("write input");

    let report = run_json(&[
        "linearize",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    let v = verification_of(&report);
    assert_eq!(
        v.get("verdict").and_then(serde_json::Value::as_str),
        Some("ok")
    );
    assert_eq!(
        v.get("pages_out").and_then(serde_json::Value::as_u64),
        Some(3)
    );

    let bytes = std::fs::read(&output).expect("output was written");
    assert_eq!(reopen_pages(&bytes), 3, "output reopens with all pages");
    // Read the /Linearized dictionary from object 1 of the WRITTEN bytes and
    // check its declared length and page count against the file itself.
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let start = selis_pdf_cos::xref::find_startxref(&bytes, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(&bytes, start, &budget, &mut g).expect("parse output");
    let lin_obj = selis_pdf_cos::copy::resolve_ref(&bytes, &doc, Ref::new(1, 0), &budget, &mut g)
        .expect("object 1 is the lin dictionary");
    let lin = selis_pdf_cos::Linearisation::from_obj(&lin_obj, &mut g).expect("lin dictionary");
    assert!(lin.is_linearised(), "the output declares itself linearized");
    assert_eq!(
        lin.file_length,
        Some(bytes.len() as u64),
        "/L matches the bytes"
    );
    assert_eq!(lin.page_count, Some(3), "/N matches the page tree");
}

#[test]
fn subset_fonts_cli_refuses_a_pdf_without_an_eligible_font() {
    let dir = temp_dir("subset-refusal");
    let input = dir.join("in.pdf");
    let output = dir.join("out.pdf");
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let mut b = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    b.add_page(200.0, 300.0, b"BT (plain) Tj ET\n");
    std::fs::write(&input, b.write(&budget, &mut g).expect("write")).expect("write input");

    let out = selis()
        .arg("subset-fonts")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("run selis");
    assert!(!out.status.success(), "a fontless PDF is refused");
    let message = String::from_utf8_lossy(&out.stderr);
    assert!(
        message.contains("no embedded TrueType font"),
        "a clear typed reason is shown: {message}"
    );
    assert!(!output.exists(), "no output committed on refusal");
}
