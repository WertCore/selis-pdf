#![no_main]

//! TrueType font parsing fuzz target (SL-3.FONT.12).
//!
//! Exercises the embedded-font parse entry points against arbitrary bytes:
//! glyph outlines (including composite resolution), glyph counts, glyph names,
//! and embedded TrueType metrics. The `Fuzz` budget bounds allocation; any
//! panic, OOM, or budget overrun is a finding.

use libfuzzer_sys::fuzz_target;
use selis_bytes::Bytes;
use selis_font::{glyph_count, glyph_id_for_name, glyph_name, outline_glyph, parse_ttf_metrics};
use selis_sandbox::{Budget, Surface};

mod common;

fuzz_target!(|data: &[u8]| {
    common::install_printing_panic_hook();
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    let bytes = Bytes::copy_from_slice(data);
    // Outline a few glyph ids, including out-of-range.
    for gid in [0u16, 1, 2, u16::MAX] {
        let _ = outline_glyph(&bytes, gid, &mut g);
    }
    let _ = glyph_count(&bytes);
    let _ = glyph_name(&bytes, 0);
    let _ = glyph_id_for_name(&bytes, "A");
    let _ = parse_ttf_metrics(&bytes, &mut g);
});
