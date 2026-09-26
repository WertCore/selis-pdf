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
//! SL-3.TEXT.12 adds the *paint* half of the same probes (§9.4.2): the glyph
//! outlines must ride `Tm`'s linear part too, so the rotated run draws rotated
//! and the scale-trick draws at 12 pt — verified against the rendered ink
//! extent, which is the geometry an oracle pixel-diffs, not just the origins
//! the extractor reports.
//!
//! SL-3.TEXT.13 (vertical-run line assembly) is pinned here too: the rotated
//! fixture assembles as **one line** whose bbox is [100, 100, 100, 182.68],
//! with the glyphs in reading order along the writing direction. The old
//! horizontal baseline model (|Δy| continuity, Δx gap) fragmented the vertical
//! run into six per-glyph lines, which is the 0.444 the sweep pinned this task
//! to; MuPDF keeps it one word.
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
/// SL-3.TEXT.13 keeps the vertical run as **one line** (MuPDF keeps it one
/// word — the rotated pin scores 0.444 purely for the per-glyph line
/// fragmentation the old horizontal baseline model caused), so the rotated
/// span is the single bbox [100, 100, 100, 182.68] rather than six
/// per-glyph points. The identity control remains at y=500.
#[test]
fn rotated_tm_lays_vertical_run_at_fixed_x_on_user_y() {
    let json = extract_json(&write_fixture("rotated.pdf", ROTATED));
    // The rotated run is one line: x=100 throughout, y from 100 to 182.68
    // (MuPDF's last 'F' origin is user-y 182.68 — device f 609.32 on a 792
    // page). The old horizontal baseline model fragmented it into six
    // per-glyph lines, which is the 0.444 the sweep pinned this task to.
    assert!(
        json.contains("[100.00, 100.00, 100.00, 182.68]"),
        "rotated run as one vertical line: {json}"
    );
    assert!(
        json.contains("\"text\": \"ABCDEF\""),
        "the vertical run reads ABCDEF in order: {json}"
    );
    // Identity control unaffected by the rotation, still at its own baseline.
    assert!(
        json.contains("[100.00, 500.00, 182.68, 500.00]"),
        "identity control ABCDEF along +x at y=500: {json}"
    );
}

// --- SL-3.TEXT.12: paint-time `Tm` (the outline geometry, not the origins) ---

/// The axis-aligned ink extent of a rendered page (non-white pixels), in device
/// pixels. `None` when the page painted no ink.
fn ink_extent(bytes: &[u8]) -> Option<(usize, usize, usize, usize)> {
    // Parse the P6 header: magic, width, height, max sample, then one byte of
    // whitespace and the raw samples.
    let mut it = 0usize;
    let mut nums = Vec::new();
    while nums.len() < 4 {
        while bytes[it].is_ascii_whitespace() {
            it += 1;
        }
        if bytes[it..].starts_with(b"#") {
            while !bytes[it..].starts_with(b"\n") {
                it += 1;
            }
            continue;
        }
        let start = it;
        while !bytes[it].is_ascii_whitespace() {
            it += 1;
        }
        nums.push(std::str::from_utf8(&bytes[start..it]).ok()?.to_string());
    }
    it += 1; // the single separator before the binary block
    let width = nums[1].parse::<usize>().ok()?;
    let height = nums[2].parse::<usize>().ok()?;
    let px = &bytes[it..];
    let mut min_x = usize::MAX;
    let mut min_y = usize::MAX;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut ink = false;
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 3;
            let (r, g, b) = (px[i], px[i + 1], px[i + 2]);
            if r < 250 || g < 250 || b < 250 {
                ink = true;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    ink.then_some((min_x, min_y, max_x, max_y))
}

fn render_ppm(path: &PathBuf, tag: &str) -> Vec<u8> {
    let mut out = std::env::temp_dir();
    out.push(format!("selis-cli-text-matrix-paint-{tag}.ppm"));
    let res = Command::new(selis_bin())
        .args(["render", "--page", "0", "--dpi", "72"])
        .arg(path)
        .arg(&out)
        .output()
        .expect("run selis render");
    assert_eq!(res.status.code().unwrap_or(-1), 0, "render failed");
    std::fs::read(&out).expect("read ppm")
}

/// SL-3.TEXT.12 paint probe: the `12 0 0 12 … Tm /F 1 Tf` row must paint at
/// **12 pt**, not 1 pt. Both rows of the fixture are "Hello World" at baselines
/// 700 (scale-trick) and 650 (identity `/F 12`); at 12 pt the caps reach
/// 700 + 8.6 = 708.6 user units → device y ≈ 83, and the two rows' ink spans
/// ≈ 83..151. A 1 pt scaled row would only reach device y ≈ 91, and the whole
/// extent would collapse toward the identity row. MuPDF's own extent on this
/// fixture is x[61,121] y[83,142] (mutool 1.23.0, `-r 72`).
#[test]
fn scaled_tm_paints_outlines_at_12pt_like_mupdf() {
    let ppm = render_ppm(&write_fixture("scaled-paint.pdf", SCALED), "scaled");
    let (x0, y0, x1, y1) = ink_extent(&ppm).expect("the fixture paints ink");
    assert!(
        x0 >= 58 && x0 <= 64,
        "ink starts at the text origin: x0={x0}"
    );
    assert!(
        y0 >= 80 && y0 <= 88,
        "the scaled row reaches 12 pt above baseline 700 (MuPDF y0=83): y0={y0}"
    );
    // The two 12 pt rows are 50 pt apart: the extent must span both baselines.
    assert!(
        y1 >= 135 && y1 <= 160,
        "both rows paint at 12 pt (MuPDF y1=142): y1={y1}"
    );
    assert!(
        x1 >= 115 && x1 <= 125,
        "the 12 pt advance spans 'World' (MuPDF x1=121): x1={x1}"
    );
}

/// SL-3.TEXT.12 paint probe: the 90° `Tm` must paint **rotated** outlines. The
/// rotated run sits at fixed x=100 with 24 pt glyphs whose cap height (0.717 em
/// = 17.2 pt) now runs along user-x, so the ink reaches ≈ 100 − 17.2 = 82.8
/// → device x0 ≈ 83. Upright glyphs (the pre-TEXT.12 path) would leave x0 at
/// the side bearing, ≈ 98. MuPDF's extent is x[82,196] y[274,691].
#[test]
fn rotated_tm_paints_outlines_rotated_like_mupdf() {
    let ppm = render_ppm(&write_fixture("rotated-paint.pdf", ROTATED), "rotated");
    let (x0, y0, x1, y1) = ink_extent(&ppm).expect("the fixture paints ink");
    assert!(
        x0 <= 86,
        "rotated glyphs reach cap-height left of x=100 (MuPDF x0=82): x0={x0}"
    );
    assert!(
        x0 >= 78,
        "the rotation extent is bounded by the cap height: x0={x0}"
    );
    // The identity control 'ABCDEF' at 24 pt from x=100 ends at ≈ 196.
    assert!(
        x1 >= 190 && x1 <= 200,
        "identity control sets the right edge (MuPDF x1=196): x1={x1}"
    );
    // The rotated run steps along +y from 100 to 182.68, the control sits at
    // y=500: the vertical extent must cover both.
    assert!(
        y1 >= 685 && y1 <= 695,
        "rotated run reaches device y≈691 (user 101): y1={y1}"
    );
    assert!(
        y0 >= 268 && y0 <= 282,
        "identity control caps reach device y≈274 (user 517): y0={y0}"
    );
}
