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

// ---------------------------------------------------------------------------
// SL-1.ROB.06 — synthetic malformed-font hardening.
//
// The upstream fontation crates carry an open-ended overflow-panic family on
// hostile fonts; `selis_font::contain` catches those at the L1 boundary and
// turns them into the documented `None` deviation (never a crash). The fixtures
// above pin the *found* sites. These in-test inputs exercise additional hostile
// shapes (absurd counts, overflowed ranges, truncated tables) and assert the
// same contract: every entry point terminates with a typed `None`/`Result`, and
// no panic escapes even under the fuzz profile with overflow checks forced on.
// ---------------------------------------------------------------------------

/// A CMap whose `bfrange` declares a huge code range with a tiny destination
/// array: the array is truncated to the declared count and the lookup saturates
/// rather than overflowing or panicking (SL-3.TEXT.19 hardening).
#[test]
fn rob06_hostile_bfrange_range_is_bounded() {
    let data = b"1 beginbfrange\n<0000> <FFFF> [<0041>]\nendbfrange\n";
    let result = std::panic::catch_unwind(|| {
        let budget = Budget::profile(Surface::Fuzz);
        let mut g = budget.guard();
        parse_cmap(data, &mut g).expect("typed Result, never a panic")
    });
    let cmap = result.expect("no panic escapes the font boundary");
    let cmap = cmap.expect("a valid bfrange block still parses");
    // Only one destination survived; every lookup outside it is a typed None.
    assert_eq!(cmap.unicode_map(0x0000), Some(0x0041));
    assert_eq!(cmap.unicode_map(0x0001), None);
    assert_eq!(cmap.unicode_map(0xFFFF), None);
}

/// Truncated SFNT headers with absurd `numGlyphs` / table counts must be
/// deviations, not panics. The values are generated in-test (no corpus).
#[test]
fn rob06_truncated_sfnt_tables_never_panic() {
    let hostiles: &[&[u8]] = &[
        b"",                                 // empty
        b"\x00\x01\x00\x00",                 // magic only
        b"OTTO\x00\x00\x00\x00",             // no tables
        b"\x00\x01\x00\x00\xFF\xFF\xFF\xFF", // absurd table count
        b"ttcf\xFF\xFF\xFF\xFF",             // broken collection
        &[0xAB; 512],                        // noise
    ];
    for data in hostiles {
        let bytes = Bytes::copy_from_slice(data);
        let outcome = std::panic::catch_unwind(|| {
            let budget = Budget::profile(Surface::Fuzz);
            let mut g = budget.guard();
            let _ = glyph_count(&bytes);
            let _ = outline_glyph(&bytes, 0, &mut g);
            let _ = outline_glyph(&bytes, u16::MAX, &mut g);
            let _ = parse_ttf_metrics(&bytes, &mut g);
        });
        assert!(
            outcome.is_ok(),
            "malformed SFNT escaped as a panic: {data:02X?}"
        );
    }
}

/// Type 1 parsers on hostile/truncated programs likewise terminate with a
/// typed `None`. This is the boundary `contain` protects on the read-fonts
/// real-number scaling site (ps/type1.rs:1444, pinned by the fixture above).
#[test]
fn rob06_hostile_type1_programs_never_panic() {
    let hostiles: &[&[u8]] = &[
        b"",
        b"%!PS-AdobeFont-1.0",
        b"/FontType 1 def 999999999999999999999999 dict begin",
        b"currentfile eexec\n\xFF\xFF\xFF\xFF",
        &[0x5C; 1024], // backslashes: escape-processing stress
    ];
    for data in hostiles {
        let bytes = Bytes::copy_from_slice(data);
        let outcome = std::panic::catch_unwind(|| {
            let budget = Budget::profile(Surface::Fuzz);
            let mut g = budget.guard();
            let _ = parse_type1(&bytes, &mut g);
            let _ = type1_glyph_count(&bytes);
        });
        assert!(
            outcome.is_ok(),
            "malformed Type1 escaped as a panic: {data:02X?}"
        );
    }
}
