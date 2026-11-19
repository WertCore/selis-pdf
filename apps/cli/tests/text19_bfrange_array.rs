//! SL-3.TEXT.19 regression pin: a Type0/Identity-H font whose `/ToUnicode`
//! uses the `bfrange` explicit-destination ARRAY form must extract the mapped
//! text through the whole CLI pipeline (parse → display list → assembly).
//!
//! Real composite-font `/ToUnicode` CMaps often carry
//! `<lo> <hi> [<u1> <u2> ...]` instead of a single base value. Before the fix
//! the CMap parser skipped that block — and when it was the only mapping in
//! the stream, returned no CMap at all — so every code in it was lost
//! (the "missing chars" symptom of the TEXT.19 cohort). The unit test in
//! `selis-font::cmapfile` was run against the pre-fix code and failed; this
//! test pins the observable end-to-end behaviour.
//!
//! ```text
//! cargo test -p selis-cli --test text19_bfrange_array
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};

fn bytes(v: &[u8]) -> Bytes {
    Bytes::copy_from_slice(v)
}

fn name(v: &[u8]) -> Obj {
    Obj::Name(bytes(v))
}

fn int(n: i64) -> Obj {
    Obj::Int(n)
}

fn dict(pairs: &[(&[u8], Obj)]) -> Obj {
    Obj::Dict(pairs.iter().map(|(k, v)| (bytes(k), v.clone())).collect())
}

fn stream(dict_pairs: &[(&[u8], Obj)], data: &[u8]) -> Obj {
    let mut pairs: Vec<(Bytes, Obj)> = dict_pairs
        .iter()
        .map(|(k, v)| (bytes(k), v.clone()))
        .collect();
    let len = i64::try_from(data.len()).unwrap_or(i64::MAX);
    pairs.push((bytes(b"Length"), Obj::Int(len)));
    Obj::Stream {
        dict: pairs,
        data: bytes(data),
    }
}

/// A one-page document with a Type0/Identity-H font whose `/ToUnicode` has
/// exactly one `beginbfrange` entry, in the explicit-array form, mapping codes
/// 0x0001..=0x0004 to `T`, `e`, `s`, `t`. The page shows all four codes.
fn pdf_with_bfrange_array() -> Vec<u8> {
    let budget = selis_sandbox::Budget::unlimited();
    let clock = selis_sandbox::shell_clock();
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    let mut builder = DocumentBuilder::new();

    let cmap = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo <</Registry (Adobe) /Ordering (UCS) /Supplement 0>> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n1 beginbfrange\n<0001> <0004> [<0054> <0065> <0073> <0074>]\nendbfrange\nendcmap\n";
    let tu_num = builder.allocate();
    builder.add_object(tu_num, stream(&[], cmap));

    let cid_num = builder.allocate();
    builder.add_object(
        cid_num,
        dict(&[
            (b"Type", name(b"Font")),
            (b"Subtype", name(b"CIDFontType2")),
            (b"BaseFont", name(b"TestFont")),
            (
                b"CIDSystemInfo",
                dict(&[
                    (b"Registry", Obj::String(bytes(b"Adobe"))),
                    (b"Ordering", Obj::String(bytes(b"Identity"))),
                    (b"Supplement", int(0)),
                ]),
            ),
        ]),
    );

    let font_num = builder.allocate();
    builder.add_object(
        font_num,
        dict(&[
            (b"Type", name(b"Font")),
            (b"Subtype", name(b"Type0")),
            (b"BaseFont", name(b"TestFont")),
            (b"Encoding", name(b"Identity-H")),
            (
                b"DescendantFonts",
                Obj::Array(vec![Obj::Ref(Ref::new(cid_num, 0))]),
            ),
            (b"ToUnicode", Obj::Ref(Ref::new(tu_num, 0))),
        ]),
    );

    let res_num = builder.allocate();
    builder.add_object(
        res_num,
        dict(&[(b"Font", dict(&[(b"F1", Obj::Ref(Ref::new(font_num, 0)))]))]),
    );

    let content = b"BT /F1 12 Tf 10 100 Td <0001000200030004> Tj ET\n";
    let content_num = builder.allocate();
    builder.add_object(content_num, stream(&[], content));

    builder.add_page_with_extra(
        300.0,
        200.0,
        &[Ref::new(content_num, 0)],
        Some(Ref::new(res_num, 0)),
        Vec::new(),
    );
    builder.write(&budget, &mut g).expect("write")
}

fn extract(path: &PathBuf, format: &str) -> (i32, String, String) {
    let bin = match std::env::var("SELIS_TEST_BIN") {
        Ok(p) if !p.is_empty() => p,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    };
    let out = Command::new(bin)
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

/// Read-back: the four codes extract as `Test`, not as raw CIDs and not as an
/// empty / low-confidence page.
#[test]
fn text19_bfrange_array_extracts_mapped_text() {
    let mut path = std::env::temp_dir();
    path.push("selis-cli-text19-bfrange-array.pdf");
    std::fs::write(&path, pdf_with_bfrange_array()).expect("write pdf");

    let (code, text, err) = extract(&path, "text");
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(
        text.contains("Test"),
        "bfrange array mapping lost in extraction: {text}"
    );
    assert!(
        !text.contains("low-confidence"),
        "mapped CID text must not be flagged low-confidence: {text}"
    );

    let (code, json, err) = extract(&path, "json");
    assert_eq!(code, 0, "extract json failed: {err}");
    assert!(
        json.contains("Test") || (json.contains("T") && json.contains("est")),
        "JSON text layer must carry the bfrange-array mapping: {json}"
    );

    let _ = std::fs::remove_file(&path);
}
