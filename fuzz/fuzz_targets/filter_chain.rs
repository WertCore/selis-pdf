//! Fuzz target for the filter pipeline (SL-1.FILT.01).
//!
//! Exercises `decode_chain` on fuzzed data with a `/FlateDecode` filter. The
//! contract: no panic, no OOM, terminates within budget.

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_pdf_filter::decode_chain;
use selis_sandbox::{Budget, Surface};

fuzz_target!(|data: &[u8]| {
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    let filters = vec![String::from("FlateDecode")];
    // The decoder tolerates invalid zlib — a typed error or BudgetBytes is
    // fine; a panic would abort the harness.
    let _ = decode_chain(&filters, &[], data, 1_048_576, &mut g);
});