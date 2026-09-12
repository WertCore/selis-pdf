//! Regression fixtures from the SL-1.ROB.02 fuzz campaign (2026-09-10).
//!
//! Each fixture is a libFuzzer finding in the `shaper` target's input
//! format (`[text_len u8][3 pad bytes][text][font program]`); the tests
//! assert the contract the fuzzer caught — no panic, a typed error —
//! stays fixed.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use selis_shape::{Shaper, ShapingParams, SwashShaper};

/// Splits a fuzz-target input into its text and font-program parts.
fn split_input(data: &[u8]) -> (String, Vec<u8>) {
    assert!(data.len() > 4);
    let text_len = (data[0] as usize).min(data.len().saturating_sub(4));
    let text_bytes = data.get(4..4usize.saturating_add(text_len)).unwrap_or(&[]);
    let font_data = data.get(4usize.saturating_add(text_len)..).unwrap_or(&[]);
    let text = std::str::from_utf8(text_bytes).unwrap_or("A").to_string();
    (text, font_data.to_vec())
}

fn shape_params<'a>(text: &'a str, font_data: &'a [u8]) -> ShapingParams<'a> {
    ShapingParams {
        text,
        font_data,
        font_size: 12.0,
        script: 0x4C61746E, // Latn
        language: None,
        features: &[],
    }
}

/// `fuzz-hmetrics-zero.font`: swash 0.2.10 panicked in `xmtx::advance`
/// (`long_metric_count - 1` underflow) when `hhea.numberOfHMetrics` is
/// zero. No fixed swash release exists; `SwashShaper::shape` rejects the
/// face with a typed error before swash sees it.
#[test]
fn fuzz_hmetrics_zero_is_typed_error() {
    let data = include_bytes!("fixtures/fuzz-hmetrics-zero.font");
    let (text, font_data) = split_input(data);
    assert!(SwashShaper.shape(&shape_params(&text, &font_data)).is_err());
}

/// `fuzz-xmtx-unsorted-dir.font` (verification leg 34508532588 finding):
/// the table directory stops being tag-sorted after `head`, so swash's
/// binary search cannot resolve `head`/`maxp`/`hhea` at all — its
/// `MetricsProxy::from_font` then silently keeps the zero-initialized
/// `hmtx_count` and the first `advance_width` underflows in
/// `xmtx::advance`. The reject predicate mirrors swash's lookup exactly
/// and turns the face into a typed error.
#[test]
fn fuzz_xmtx_unsorted_dir_is_typed_error() {
    let data = include_bytes!("fixtures/fuzz-xmtx-unsorted-dir.font");
    let (text, font_data) = split_input(data);
    assert!(SwashShaper.shape(&shape_params(&text, &font_data)).is_err());
}

/// `fuzz-neg-i16-descent.font` (verification leg 34515763631 finding): a
/// well-formed directory whose `hhea.descender` is -32768 — swash's
/// `Metrics::fill` negates it into an i16 field and overflows. The
/// reject predicate turns the face into a typed error.
#[test]
fn fuzz_neg_i16_descent_is_typed_error() {
    let data = include_bytes!("fixtures/fuzz-neg-i16-descent.font");
    let (text, font_data) = split_input(data);
    assert!(SwashShaper.shape(&shape_params(&text, &font_data)).is_err());
}

/// `fuzz-cmap-delta-overflow.font` (verification leg 34517954828
/// finding): swash's cmap lookup adds the group delta as u32 and
/// overflows on a hostile subtable — the first of the *arithmetic*
/// family inside swash's parsers. `SwashShaper::shape`'s `catch_unwind`
/// containment turns it into a typed error; when swash ships a fixed
/// release, the containment can be dropped and this test keeps pinning
/// the input.
#[test]
fn fuzz_cmap_delta_overflow_is_typed_error() {
    let data = include_bytes!("fixtures/fuzz-cmap-delta-overflow.font");
    let (text, font_data) = split_input(data);
    assert!(SwashShaper.shape(&shape_params(&text, &font_data)).is_err());
}
