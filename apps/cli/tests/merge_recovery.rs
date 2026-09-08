//! Regression tests: the A3 recovery corpus files merge cleanly (TASK A3).
//!
//! `selis merge` must accept files whose xref needs the recovery paths —
//! `form_two_pages.pdf` (xref stream with a PNG predictor) and
//! `outlines_for_editor.pdf` (mis-aimed `startxref` and off-by-8 object
//! offsets) — without rewriting the inputs, and the merged output must be a
//! valid PDF per `selis inspect` and `selis check`.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::PathBuf;
use std::process::Command;

fn corpus(name: &str) -> PathBuf {
    // Selis-authored synthetic fixtures live in corpus/fixtures/ (committed);
    // fetched third-party corpus stays in the gitignored corpus/pdfs/ cache.
    let fixtures = [
        "form_two_pages.pdf",
        "outlines_for_editor.pdf",
        "structure_simple.pdf",
    ];
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    if fixtures.contains(&name) {
        p.push("corpus");
        p.push("fixtures");
    } else {
        p.push("corpus");
        p.push("pdfs");
    }
    p.push(name);
    p
}

fn out_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("selis-a3-{tag}-{}.pdf", std::process::id()))
}

fn merge_to(inputs: &[&str], out: &PathBuf) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_selis"));
    cmd.arg("merge");
    for name in inputs {
        cmd.arg(corpus(name));
    }
    let status = cmd.arg("-o").arg(out).status().expect("spawn selis");
    assert!(status.success(), "merge of {inputs:?} failed with {status}");
    assert!(out.exists(), "merge produced no output file");
}

fn verify_output(out: &PathBuf) {
    let inspect = Command::new(env!("CARGO_BIN_EXE_selis"))
        .arg("inspect")
        .arg(out)
        .output()
        .expect("spawn inspect");
    assert!(
        inspect.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let text = String::from_utf8_lossy(&inspect.stdout);
    assert!(
        text.contains("root: 1 0"),
        "merged output has no /Root: {text}"
    );
    let check = Command::new(env!("CARGO_BIN_EXE_selis"))
        .arg("check")
        .arg(out)
        .output()
        .expect("spawn check");
    assert!(
        check.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(out);
}

/// The original failure: merging `form_two_pages.pdf` failed with
/// `resolve /Root: [E1010]` because the xref stream's PNG predictor was not
/// undone and `/Root` decoded to a free entry.
#[test]
fn merge_accepts_form_two_pages() {
    let out = out_path("form");
    merge_to(&["structure_simple.pdf", "form_two_pages.pdf"], &out);
    verify_output(&out);
}

/// The original failure: `outlines_for_editor.pdf` failed to open at all
/// (`[E1101]` during `xref-table`) because `startxref` points 8 bytes short
/// of the `xref` keyword and every object offset is off by 8.
#[test]
fn merge_accepts_outlines_for_editor() {
    let out = out_path("outlines");
    merge_to(&["structure_simple.pdf", "outlines_for_editor.pdf"], &out);
    verify_output(&out);
}

/// DoD: the three-way merge succeeds and produces a valid PDF.
#[test]
fn three_way_merge_succeeds_and_output_is_valid() {
    let out = out_path("threeway");
    merge_to(
        &[
            "structure_simple.pdf",
            "form_two_pages.pdf",
            "outlines_for_editor.pdf",
        ],
        &out,
    );
    verify_output(&out);
}
