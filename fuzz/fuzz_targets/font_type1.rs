#![no_main]

//! Type 1 font parsing fuzz target (SL-3.FONT.12).
//!
//! Exercises the Type 1 eexec-decrypt + charstring pipeline through
//! `parse_type1`, `glyph_count`, and `outline_glyph` on the parsed font.

use libfuzzer_sys::fuzz_target;
use selis_bytes::Bytes;
use selis_font::{parse_type1, type1_glyph_count, type1_glyph_name};
use selis_sandbox::{Budget, Surface};

mod common;

fuzz_target!(|data: &[u8]| {
    common::install_printing_panic_hook();
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    let bytes = Bytes::copy_from_slice(data);
    if let Some(font) = parse_type1(&bytes, &mut g).ok().flatten() {
        let _ = font.glyph_count();
        for gid in [0u16, 1] {
            let _ = font.outline_glyph(gid, &mut g);
            let _ = font.glyph_name(gid);
        }
    }
    let _ = type1_glyph_count(&bytes);
    let _ = type1_glyph_name(&bytes, 0);
});