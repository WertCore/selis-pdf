//! Fuzz target for the COS lexer (SL-1.COS.01 DoD: `cos_lex`).
//!
//! The contract every parser fuzz target asserts (ADR-P0006):
//!   * no panic (the harness would abort),
//!   * no OOM (the Fuzz budget bounds allocation),
//!   * terminates within the budget (the Fuzz wall-clock bound).

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_sandbox::{Budget, Surface};
use selis_pdf_cos::Lexer;

fuzz_target!(|data: &[u8]| {
    // Whatever the lexer returns — a token, `None`, or a typed error — the
    // invariants are the same: no panic, no hang, no unbounded allocation.
    let mut g = Budget::profile(Surface::Fuzz).guard();
    let mut lexer = Lexer::new(data);
    loop {
        match lexer.next_token(&mut g) {
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break,
        }
    }
});
