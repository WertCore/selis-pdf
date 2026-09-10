#![no_main]

//! Shaping fuzz target (SL-3.SHAPE.04).
//!
//! Exercises the `SwashShaper` against arbitrary font data and text.  The
//! swash library handles all complex scripts (Devanagari, Arabic, Thai, CJK
//! vertical, etc.) internally; this target catches panics or hangs in the
//! shaping path.  The `Fuzz` budget bounds the input length indirectly (the
//! shaper is internally bounded by the font data size).
//!
//! Containment note (SL-1.ROB.02): swash 0.2.10 panics on an open-ended
//! family of malformed font inputs (overflow checks are forced on in fuzz
//! builds). `SwashShaper::shape` contains those panics behind typed errors
//! (precise mirror-guard + `catch_unwind`), so this target replaces
//! libfuzzer-sys's abort-on-panic hook with a printing one: a contained
//! swash panic is a *deviation*, not a campaign crash. Panics escaping
//! `shape` still unwind into libfuzzer-sys's own catch_unwind and abort
//! the process as usual. Every found input is pinned as a regression
//! fixture in `crates/selis-shape/tests/`.

use libfuzzer_sys::fuzz_target;
use selis_shape::{Shaper, ShapingParams, SwashShaper};
use std::sync::Once;

static PRINTING_PANIC_HOOK: Once = Once::new();

fuzz_target!(|data: &[u8]| {
    PRINTING_PANIC_HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            eprintln!("{info}");
        }));
    });
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
