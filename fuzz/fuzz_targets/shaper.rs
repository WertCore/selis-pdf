#![no_main]

//! Shaping fuzz target (SL-3.SHAPE.04).
//!
//! Exercises the `SwashShaper` against arbitrary font data and text.  The
//! swash library handles all complex scripts (Devanagari, Arabic, Thai, CJK
//! vertical, etc.) internally; this target catches panics or hangs in the
//! shaping path.  The `Fuzz` budget bounds the input length indirectly (the
//! shaper is internally bounded by the font data size).

use libfuzzer_sys::fuzz_target;
use selis_shape::{Shaper, ShapingParams, SwashShaper};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let text_len = (data[0] as usize).min(data.len().saturating_sub(4));
    let text_bytes = data.get(4..4usize.saturating_add(text_len)).unwrap_or(&[]);
    let font_data = data.get(4usize.saturating_add(text_len)..).unwrap_or(&[]);
    // Build a valid UTF-8 text slice (or a reasonable prefix).
    let text = std::str::from_utf8(text_bytes).unwrap_or("");
    if text.is_empty() || font_data.is_empty() {
        return;
    }
    let shaper = SwashShaper;
    let params = ShapingParams {
        text,
        font_data,
        font_size: 12.0,
        script: 0x4C61746E, // Latn
        language: None,
        features: &[],
    };
    let _ = shaper.shape(&params);
});