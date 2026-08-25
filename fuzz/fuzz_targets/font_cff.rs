#![no_main]

//! CFF font parsing fuzz target (SL-3.FONT.12).
//!
//! Exercises the CFF outline path through `outline_glyph` (skrifa's CFF
//! decoder is exercised by the same function; this target feeds the same
//! entry point with arbitrary bytes for the CFF path).

use libfuzzer_sys::fuzz_target;
use selis_bytes::Bytes;
use selis_font::{outline_glyph, parse_ttf_metrics};
use selis_sandbox::{Budget, Surface};

fuzz_target!(|data: &[u8]| {
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    let bytes = Bytes::copy_from_slice(data);
    for gid in [0u16, 1] {
        let _ = outline_glyph(&bytes, gid, &mut g);
    }
    let _ = parse_ttf_metrics(&bytes, &mut g);
});