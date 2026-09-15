//! SL-3.TEXT.11 regression pin: text-space advances and `Td`/`TD`/`T*` must
//! pre-multiply the text matrix (§9.4.3), not add to its `e`/`f`. The two
//! probe shapes are the ones the defect was filed from:
//!
//! * the 90° `0 1 -1 0 x y Tm` — MuPDF runs "ABCDEF" along user-space **+y**
//!   (every glyph at x=100, y stepping to 182.68); a raw `e`/`f` add ran it
//!   along +x at fixed y, the exact bug (`text11_tm_rotated.pdf`).
//! * the generator scale-trick `12 0 0 12 e f Tm /F 1 Tf` (bug1057544 line 3,
//!   the 8 veraPDF "Hello world" files) — MuPDF lays it at **twelve** times the
//!   text-space advance; a raw add laid it at one-twelfth
//!   (`text11_tm_scaled.pdf`).
//!
//! The exact per-glyph origin deltas are asserted at the interpreter level in
//! `crates/selis-pdf-content/src/text.rs` against the MuPDF device numbers
//! captured off the pinned oracle (`mutool 1.23.0`, `mutool draw -F svg`'s
//! per-glyph `matrix(...)` translation column). This end-to-end file proves the
//! corrected `at` positions reach `selis extract --format json` unchanged from
//! MuPDF, and that the §9.4.3 mapping is scale-invariant for the TEXT.09 word
//! gap (`advance`/`space` ride it too, so a scaled line still splits at a
//! single space — no word-break regression in the identity-Tm majority).
//!
//! ```text
//! cargo test -p selis-cli --test text_matrix_layout
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

fn selis_bin() -> String {
    match std::env::var("SELIS_TEST_BIN") {
        Ok(p) if !p.is_empty() => p,
        _ => env!("CARGO_BIN_EXE_selis").to_string(),
    }
}

const SCALED: &[u8] = include_bytes!("fixtures/text11_tm_scaled.pdf");
const ROTATED: &[u8] = include_bytes!("fixtures/text11_tm_rotated.pdf");

fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("selis-cli-text-matrix-{name}"));
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

fn extract_json(path: &PathBuf) -> String {
    let out = Command::new(selis_bin())
        .args(["extract", "--format", "json", "--page", "0"])
        .arg(path)
        .output()
        .expect("run selis extract");
    assert_eq!(out.status.code().unwrap_or(-1), 0);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// MuPDF lays the scale-trick line at exactly the same user-space origins as
/// the `/F 12 Tf` identity line ("Hello" x0 60.00 → x1 80.66, "World" 90.67 →
/// 115.33). A raw one-twelfth layout would leave `Hello` ending near 61.7, so
/// it would neither match the identity row nor split `World` at the 90.67 x0.
#[test]
fn scaled_tm_matches_mu_device_positions_and_splits_words() {
    let json = extract_json(&write_fixture("scaled.pdf", SCALED));
    // MuPDF device column for the scaled 'Hello' is `…60 …80.664`, 'World'
    // `…90.672 …115.332` — both rows present and equal for size 1.00 and 12.00.
    assert_eq!(
        json.matches("\"text\": \"Hello\"").count(),
        2,
        "scaled + identity Hello spans: {json}"
    );
    assert!(
        json.contains("[60.00, 700.00, 80.66, 700.00]"),
        "scaled Hello span hits MuPDF x 60..80.664: {json}"
    );
    assert!(
        json.contains("[90.67, 700.00, 115.33, 700.00]"),
        "scaled World span x 90.672..115.332 (12× layout): {json}"
    );
    // Identical geometry to the `/F 12 Tf` identity row.
    assert!(
        json.contains("[60.00, 650.00, 80.66, 650.00]"),
        "identity Hello matches the same layout: {json}"
    );
}

/// MuPDF lays the 90° run's glyphs along +y at fixed x=100, ending at y=182.68
/// (last glyph 'F'). A raw add spread them along +x to 182.68 at fixed y=100.
/// The vertical run lands at x=100 exactly (each per-glyph line's bbox is the
/// point [100, y, 100, y] with y stepping up to MuPDF's 182.68; per-glyph
/// column grouping is the reading-order axis concern SL-3.TEXT.11 does not own),
/// while the identity control remains at y=500.
#[test]
fn rotated_tm_lays_vertical_run_at_fixed_x_on_user_y() {
    let json = extract_json(&write_fixture("rotated.pdf", ROTATED));
    // Every vertical glyph shares x = 100; MuPDF's last 'F' origin is user-y
    // 182.68 (device f 609.32 on a 792 page).
    assert!(
        json.contains("[100.00, 182.68, 100.00, 182.68]"),
        "MuPDF 'F' at x=100, y=182.68: {json}"
    );
    assert!(
        json.contains("[100.00, 116.01, 100.00, 116.01]"),
        "MuPDF 'B' at x=100, y=116.008: {json}"
    );
    // Identity control unaffected by the rotation, still at its own baseline.
    assert!(
        json.contains("[100.00, 500.00, 182.68, 500.00]"),
        "identity control ABCDEF along +x at y=500: {json}"
    );
}
