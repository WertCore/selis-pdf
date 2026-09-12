//! Regression fixtures from the SL-1.ROB.02 fuzz campaign (leg 3, 2026-09-10).
//!
//! Each fixture is a minimized libFuzzer finding; the test asserts the
//! contract the fuzzer caught — no panic, no hang — stays fixed.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use selis_bytes::Bytes;
use selis_font::{
    glyph_count, outline_glyph, parse_cmap, parse_ttf_metrics, parse_type1, resolve_cid_widths,
    type1_glyph_count, CidWidthEntry,
};
use selis_sandbox::{Budget, Surface};

/// `fuzz-cmap-stray-gt.bin`: a stray `>` byte made the CMap tokenizer return
/// a zero-length token without advancing, hanging `parse_cmap` forever (the
/// libFuzzer `-timeout=60` finding; fixed in `cmapfile::Tokenizer::next`).
#[test]
fn fuzz_cmap_stray_gt_terminates() {
    let data = include_bytes!("fixtures/fuzz-cmap-stray-gt.bin");
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    // Before the fix this never returned; now it parses in microseconds.
    let cmap = parse_cmap(data, &mut g).expect("parse_cmap must not error");
    assert!(cmap.is_none(), "no mapping tables in this input");
}

/// `fuzz-glyf-repeat-flag.ttf`: a repeat-flag glyf run panicked inside
/// read-fonts 0.41 (`glyf.rs:244`) via `outline_glyph`. Fixed upstream;
/// pinned here so a supply-chain downgrade cannot reintroduce it.
#[test]
fn fuzz_glyf_repeat_flag_no_panic() {
    let data = include_bytes!("fixtures/fuzz-glyf-repeat-flag.ttf");
    let bytes = Bytes::copy_from_slice(data);
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    for gid in [0u16, 1, 2, u16::MAX] {
        let _ = outline_glyph(&bytes, gid, &mut g);
    }
    let _ = glyph_count(&bytes);
    let _ = parse_ttf_metrics(&bytes, &mut g);
    let _ = resolve_cid_widths(
        &[CidWidthEntry::Cid(0), CidWidthEntry::Widths(vec![100.0])],
        50.0,
    );
}

/// `fuzz-type1-matrix-overflow.bin` (campaign leg 34520827872 finding): a
/// Type 1 font whose real-number token overflows read-fonts 0.44's
/// `integral *= 10` scaling loop (`ps/type1.rs:1444`). The panic is
/// contained by `selis_font::contain` — the font is the documented
/// `Ok(None)` deviation, never a crash.
#[test]
fn fuzz_type1_matrix_overflow_is_a_deviation() {
    let data = include_bytes!("fixtures/fuzz-type1-matrix-overflow.bin");
    let bytes = Bytes::copy_from_slice(data);
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    assert!(parse_type1(&bytes, &mut g).expect("parse").is_none());
    assert!(type1_glyph_count(&bytes).is_none());
}
