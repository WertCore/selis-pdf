//! Fuzz target for the PKCS#7/CMS parse entry point (SL-1.ENC.03).
//!
//! Exercises `parse_enveloped_data` on fuzzed data. The contract: no panic,
//! no OOM, terminates within budget — every structural deviation is a typed
//! error.

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_crypto::pkcs7::parse_enveloped_data;
use selis_sandbox::{Budget, Surface};

fuzz_target!(|data: &[u8]| {
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    // A typed error or BudgetBytes is fine; a panic would abort the harness.
    let _ = parse_enveloped_data(data, &mut g);
});
