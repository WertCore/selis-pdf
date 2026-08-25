#![no_main]

//! CMap parsing fuzz target (SL-3.FONT.12).
//!
//! Exercises the CMap parser (`begincidrange`/`begincidchar`/`usecmap`/`WMode`)
//! and the CID width resolution against arbitrary bytes.

use libfuzzer_sys::fuzz_target;
use selis_font::{parse_cmap, resolve_cid_widths, CidWidthEntry};
use selis_sandbox::{Budget, Surface};

fuzz_target!(|data: &[u8]| {
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    if let Some(cmap) = parse_cmap(data, &mut g).ok().flatten() {
        for code in [0u32, 0x20, 0x41, 0xFFFF] {
            let _ = cmap.code_to_cid(code);
        }
        let _ = cmap.wmode;
        let _ = cmap.uses.as_deref();
    }
    // CID width resolution on a synthetic /W array.
    let _ = resolve_cid_widths(
        &[CidWidthEntry::Cid(0), CidWidthEntry::Widths(vec![100.0])],
        50.0,
    );
});