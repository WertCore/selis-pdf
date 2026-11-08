//! `selis extract --format text|tables|reflow` and `selis split-text` (Phase 3).
//!
//! The documents are built here rather than taken from a fixture: each test
//! asserts one property of the export, and a synthetic page says exactly what
//! that property is about. The binary is the one that shipped.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::Path;
use std::process::Command;

/// A PDF with one page per content stream, one Helvetica resource.
fn build_pdf(contents: &[&str]) -> Vec<u8> {
    let page_count = contents.len();
    let font_object = page_count * 2 + 3;
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec());
    let kids: String = (0..page_count)
        .map(|index| format!("{} 0 R", index * 2 + 3))
        .collect::<Vec<String>>()
        .join(" ");
    objects.push(
        format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {page_count} >>\nendobj\n")
            .into_bytes(),
    );
    for (index, content) in contents.iter().enumerate() {
        let page = index * 2 + 3;
        let body = index * 2 + 4;
        objects.push(
            format!(
                "{page} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 {font_object} 0 R >> >> /Contents {body} 0 R >>\nendobj\n"
            )
            .into_bytes(),
        );
        objects.push(
            format!(
                "{body} 0 obj\n<< /Length {} >>\nstream\n{content}endstream\nendobj\n",
                content.len()
            )
            .into_bytes(),
        );
    }
    objects.push(
        format!(
            "{font_object} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>\nendobj\n"
        )
        .into_bytes(),
    );
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(out.len());
        out.extend_from_slice(object);
    }
    let xref = out.len();
    let size = offsets.len() + 1;
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

fn chapter_pdf() -> Vec<u8> {
    build_pdf(&[
        "BT /F1 24 Tf 72 700 Td (Chapter 1) Tj ET\n",
        "BT /F1 24 Tf 72 700 Td (Chapter 2) Tj ET\n",
    ])
}

fn table_pdf() -> Vec<u8> {
    build_pdf(
        &["BT /F1 10 Tf 72 700 Td (Name) Tj 228 0 Td (Amount) Tj ET\n\
         BT /F1 10 Tf 72 686 Td (Ada) Tj 228 0 Td (1200) Tj ET\n"],
    )
}

fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("selis-text-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

fn selis(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_selis"))
        .args(args)
        .output()
        .expect("run selis")
}

#[test]
fn text_export_carries_page_separators_and_writes_its_quality_report() {
    let pdf = temp("chapter.pdf", &chapter_pdf());
    let quality = std::env::temp_dir()
        .join(format!("selis-text-{}", std::process::id()))
        .join("quality.json");
    let out = selis(&[
        "extract",
        pdf.to_str().expect("path"),
        "--last",
        "1",
        "--format",
        "text",
        "--separators",
        "--quality-out",
        quality.to_str().expect("path"),
    ]);
    assert!(out.status.success(), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("--- page 1 of 2 ---"), "{stdout}");
    assert!(stdout.contains("--- page 2 of 2 ---"), "{stdout}");
    assert!(stdout.contains("Chapter 2"), "{stdout}");
    let report = std::fs::read_to_string(&quality).expect("quality report");
    assert!(
        report.contains("\"schema\": \"selis-extract-quality/1\""),
        "{report}"
    );
    assert!(report.contains("\"pages\": 2"), "{report}");
    assert!(report.contains("\"verdict\": \"ok\""), "{report}");
}

#[test]
fn tables_come_out_as_csv_with_their_confidence() {
    let pdf = temp("ledger.pdf", &table_pdf());
    let out = selis(&["extract", pdf.to_str().expect("path"), "--format", "tables"]);
    assert!(out.status.success(), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("# page 1, table 1 (2 columns, confidence 1.00)"),
        "{stdout}"
    );
    assert!(stdout.contains("Name,Amount"), "{stdout}");
    assert!(stdout.contains("Ada,1200"), "{stdout}");
}

#[test]
fn reflow_writes_an_article_that_carries_the_text() {
    let pdf = temp("article.pdf", &chapter_pdf());
    let out = selis(&["extract", pdf.to_str().expect("path"), "--format", "reflow"]);
    assert!(out.status.success(), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("<!doctype html>"), "{stdout}");
    assert!(stdout.contains("<article>"), "{stdout}");
    assert!(stdout.contains("Chapter 1"), "{stdout}");
}

#[test]
fn split_text_writes_one_verified_part_per_boundary() {
    let dir = std::env::temp_dir().join(format!("selis-text-split-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let pdf = temp("split.pdf", &chapter_pdf());
    let prefix = dir.join("part-");
    let out = selis(&[
        "split-text",
        pdf.to_str().expect("path"),
        "--find",
        "Chapter 2",
        "--output",
        prefix.to_str().expect("path"),
        "--json",
    ]);
    assert!(out.status.success(), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"schema\": \"selis-split-text/1\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"matched_pages\": [2]"), "{stdout}");
    assert!(Path::new(&format!("{}0.pdf", prefix.display())).exists());
    assert!(Path::new(&format!("{}1.pdf", prefix.display())).exists());
}

#[test]
fn a_needle_that_matches_nothing_writes_no_part_and_fails() {
    let dir = std::env::temp_dir().join(format!("selis-text-nomatch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let pdf = temp("nomatch.pdf", &chapter_pdf());
    let prefix = dir.join("part-");
    let out = selis(&[
        "split-text",
        pdf.to_str().expect("path"),
        "--find",
        "Appendix",
        "--output",
        prefix.to_str().expect("path"),
    ]);
    assert!(!out.status.success(), "a refusal must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no page contains"), "{stderr}");
    assert!(!Path::new(&format!("{}0.pdf", prefix.display())).exists());
}
