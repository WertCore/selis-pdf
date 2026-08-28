//! Fuzz target for the COS object parser (SL-1.COS.01 DoD: `cos_parse`).
//!
//! The contract every parser fuzz target asserts (ADR-P0006):
//!   * no panic (the harness would abort),
//!   * no OOM (the Fuzz budget bounds allocation),
//!   * terminates within the budget (the Fuzz wall-clock bound).

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_pdf_cos::{parse_all, tokenise};
use selis_sandbox::{Budget, Surface};

fuzz_target!(|data: &[u8]| {
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    // Whatever the lexer or parser returns, the invariants are the same: no
    // panic, no hang, no unbounded allocation.
    if let Ok(tokens) = tokenise(data, &mut g) {
        let _ = parse_all(&tokens, &budget, &mut g);
    }
});
