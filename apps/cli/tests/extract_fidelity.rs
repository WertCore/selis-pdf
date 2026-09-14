//! SL-3.TEXT.08/09/10 regression pin: the three compounding extraction
//! defects, exercised end-to-end through `selis extract --format …` on the
//! fixtures they were diagnosed from (pinned here by name for the issue).
//!
//! * TEXT.08 — non-ASCII codes must reach **every** output format as UTF-8
//!   Unicode after `/ToUnicode`/encoding decoding (é, degree, CJK through
//!   `text|json|md|html`), never PDF-string octal digit runs.
//! * TEXT.09 — word-gap inference against a font's *space width* and pen
//!   step, not a fixed half-em of text-space font size; includes the
//!   SL-3.TEXT.03 DoD content stream with **no space characters at all**.
//! * TEXT.10 — a page whose display list drew text but recovered nothing
//!   emits a low-confidence marker instead of a silent empty string; and
//!   font state set in one BT/ET block must survive into the next (§9.4.1),
//!   the TCPDF-shaped root cause of the silent empties.
//!
//! ```text
//! cargo test -p selis-cli --test extract_fidelity
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

/// The `selis` binary under test. In CI `cargo test` builds it into a private
/// target dir and `CARGO_BIN_EXE_selis` is authoritative. Locally the target
/// dir is SHARED with concurrent agents (`C:\selis-build`), which can
/// overwrite the exe between build and run; set `SELIS_TEST_BIN` to a freshly
/// snapshotted copy in that case (the same bytes, immune to clobber).
fn selis_bin() -> String {
    match std::env::var("SELIS_TEST_BIN") {
        Ok(p) if !p.is_empty() => p,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    }
}

const TEXT08: &[u8] = include_bytes!("fixtures/text08_encoding_unicode.pdf");
const TEXT09: &[u8] = include_bytes!("fixtures/text09_word_gap_spaceless.pdf");
const TEXT10: &[u8] = include_bytes!("fixtures/text10_low_confidence.pdf");
/// The render+text-oracle corpus fixture whose headline extraction
/// (`S e lis o ra cle sm o ke te st`) made SL-3.TEXT.09 the sweep's biggest
/// diff cluster.
const SMOKE: &[u8] = include_bytes!("../../../docker/oracles/fixtures/smoke.pdf");

/// Drop the fixture into the temp dir and hand back its path. The bytes come
/// from the repo (pinned), the write is test-local.
fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-cli-extract-fidelity-{name}"));
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

fn extract(path: &PathBuf, format: &str, page: Option<usize>) -> (i32, String, String) {
    let mut cmd = Command::new(selis_bin());
    cmd.arg("extract").arg("--format").arg(format);
    if let Some(p) = page {
        cmd.arg("--page").arg(p.to_string());
    }
    cmd.arg(path);
    let out = cmd.output().expect("run selis extract");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// SL-3.TEXT.08: é (U+00E9), degree (U+00B0) and CJK (U+4E8C, U+6B21) — shown
/// through WinAnsi, MacRoman, `/Differences` and `/ToUnicode` respectively —
/// come out as literal UTF-8 in all four formats. The defect emitted the
/// octal escape *digits* ("3 5 0"); the byte is now decoded through the
/// TEXT.02 recovery chain before any formatter sees it.
#[test]
fn text08_unicode_survives_all_four_formats() {
    let path = write_fixture("text08.pdf", TEXT08);
    let (code, text, err) = extract(&path, "text", Some(0));
    assert_eq!(code, 0, "extract failed: {err}");
    assert!(text.contains("café 30°"), "WinAnsi line: {text}");
    assert!(
        text.contains("café 30°"),
        "MacRoman 0x8E/0xA1 recovered via the chain: {text}"
    );
    assert!(text.contains("é"), "Differences eacute: {text}");
    assert!(text.contains("二次元"), "ToUnicode CJK: {text}");
    // No PDF-string octal-escape digit runs anywhere.
    for digits in [
        "\\351", "\\260", "\\216", "3 5 1", "3 5 0", "2 6 0", "2 1 6",
    ] {
        assert!(
            !text.contains(digits),
            "octal escape leaked into text: {text}"
        );
    }

    let (code, json, _) = extract(&path, "json", Some(0));
    assert_eq!(code, 0);
    // JSON spans are per-run (the assembler merges same-font runs into word
    // groups): decoded UTF-8 text in every span, never the octal digits the
    // `--format text` path was leaking before SL-3.TEXT.08.
    assert!(
        json.contains("\"text\": \"café\""),
        "e-acute carried by WinAnsi + MacRoman spans: {json}"
    );
    assert!(json.contains("\"text\": \"30°\""), "degree span in JSON");
    assert!(
        json.contains("\"text\": \"éè\""),
        "Differences span in JSON"
    );
    assert!(
        json.contains("\"text\": \"二次元\""),
        "ToUnicode CJK span in JSON"
    );
    assert!(
        !json.contains("\\351") && !json.contains("\"3 5 1\""),
        "no octal escape in JSON: {json}"
    );

    let (code, md, _) = extract(&path, "md", Some(0));
    assert_eq!(code, 0);
    assert!(
        md.contains("café 30°") && md.contains("二次元"),
        "Markdown: {md}"
    );

    let (code, html, _) = extract(&path, "html", Some(0));
    assert_eq!(code, 0);
    assert!(
        html.contains("café 30°") && html.contains("二次元"),
        "HTML must pass non-ASCII through unescaped: {html}"
    );
}

/// SL-3.TEXT.09 on the oracle-corpus smoke fixture: per-glyph splitting must
/// be gone — the whole sentence in one run.
#[test]
fn text09_smoke_fixture_reads_as_words() {
    let path = write_fixture("smoke.pdf", SMOKE);
    let (code, text, err) = extract(&path, "text", Some(0));
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(text.trim_end_matches('\n'), "Selis oracle smoke test");
}

/// SL-3.TEXT.09 / the SL-3.TEXT.03 DoD *at the extractor level*: the fixture
/// content stream's string literals contain **no space characters at all**
/// (checked on the pinned bytes below), yet word gaps at one space width
/// split "Hello"+"World" and a tight run stays one word ("Nospace").
#[test]
fn text09_no_space_characters_in_content_stream() {
    // Byte-level proof: walk the fixture's PDF literal strings and require
    // none of them holds a 0x20 — the extractor's only word signal is a gap.
    let mut inside = false;
    let mut saw_literal = false;
    for &b in TEXT09 {
        match b {
            b'(' if !inside => {
                inside = true;
                saw_literal = true;
            }
            b')' => inside = false,
            0x20 if inside => panic!("fixture content stream carries a space character"),
            _ => {}
        }
    }
    assert!(saw_literal, "fixture has no literal strings to inspect");
    let path = write_fixture("text09.pdf", TEXT09);
    let (code, text, err) = extract(&path, "text", Some(0));
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(
        text.trim_end_matches('\n'),
        "Hello World\nNospace",
        "gap-inferred words + single word"
    );
}

/// SL-3.TEXT.10: page 0 shows surrogate CID codes (2-byte codes that decode
/// to nothing) yet *drew text* — every format emits the low-confidence
/// marker instead of silent empty output. Page 1 (the TCPDF shape: `/Tf` in
/// one `BT … ET`, the show in the next) proves §9.4.1 persistence — text it
/// extracts with no marker.
#[test]
fn text10_marker_instead_of_silence_and_font_survives_bt() {
    let path = write_fixture("text10.pdf", TEXT10);

    let (code, text, err) = extract(&path, "text", Some(0));
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(
        text.trim_end_matches('\n'),
        "[low-confidence: text drawn, nothing recovered]"
    );

    let (code, json, _) = extract(&path, "json", Some(0));
    assert_eq!(code, 0);
    assert!(
        json.contains("\"low_confidence\": true"),
        "JSON must carry the flag: {json}"
    );

    let (code, md, _) = extract(&path, "md", Some(0));
    assert_eq!(code, 0);
    assert!(md.contains("low-confidence"), "Markdown marker: {md}");

    let (code, html, _) = extract(&path, "html", Some(0));
    assert_eq!(code, 0);
    assert!(html.contains("low-confidence"), "HTML marker: {html}");

    // The recovered page after the §9.4.1 BT-persistence fix: TCPDF split
    // blocks are readable and carry no marker.
    let (code, survived, err) = extract(&path, "text", Some(1));
    assert_eq!(code, 0, "extract failed: {err}");
    assert_eq!(survived.trim_end_matches('\n'), "Survived");
    assert!(!survived.contains("low-confidence"));

    // A genuinely blank page (object 9 of the fixture, page index 2): no
    // text ops at all, so the silence is the truth and no marker appears.
    let (code2, blank, _) = extract(&path, "text", Some(2));
    assert_eq!(code2, 0, "blank page must extract as empty, not fail");
    assert!(
        !blank.contains("low-confidence"),
        "blank page (no text ops) must NOT be flagged: {blank}"
    );
}
