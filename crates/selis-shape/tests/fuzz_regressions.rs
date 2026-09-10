//! Regression fixture from the SL-1.ROB.02 fuzz campaign (leg 3, 2026-09-10).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use selis_shape::{Shaper, ShapingParams, SwashShaper};

/// `fuzz-hmetrics-zero.font`: swash 0.2.10 panicked in `xmtx::advance`
/// (`long_metric_count - 1` underflow) when `hhea.numberOfHMetrics` is zero.
/// No fixed swash release exists; `SwashShaper::shape` now rejects the face
/// with a typed error before swash sees it.
#[test]
fn fuzz_hmetrics_zero_is_typed_error() {
    let data = include_bytes!("fixtures/fuzz-hmetrics-zero.font");
    assert!(data.len() > 4);
    let text_len = (data[0] as usize).min(data.len().saturating_sub(4));
    let text_bytes = data.get(4..4usize.saturating_add(text_len)).unwrap_or(&[]);
    let font_data = data.get(4usize.saturating_add(text_len)..).unwrap_or(&[]);
    let text = std::str::from_utf8(text_bytes).unwrap_or("A");
    let params = ShapingParams {
        text,
        font_data,
        font_size: 12.0,
        script: 0x4C61746E, // Latn
        language: None,
        features: &[],
    };
    // Before the guard this panicked inside swash; now it is a typed error.
    assert!(SwashShaper.shape(&params).is_err());
}
