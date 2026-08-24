//! Fuzz target for the COS parser entry point (SL-0.SEC.02).
//!
//! The contract every parser fuzz target asserts (ADR-P0006):
//!   * no panic (the harness would abort),
//!   * no OOM (the Fuzz budget bounds allocation),
//!   * terminates within the budget (the Fuzz wall-clock bound).

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_io::MemSource;
use selis_sandbox::{Budget, Surface};
use selis_pdf_cos::parse;

fuzz_target!(|data: &[u8]| {
    // Feed the whole input through the COS parse entry point under a Fuzz
    // budget. Whatever the outcome — a typed error, a partial index, or a
    // complete document — the invariants are the same: no panic, no hang,
    // no unbounded allocation.
    let source = MemSource::new(data);
    let budget = Budget::profile(Surface::Fuzz);
    let _ = parse(&source, &budget);
});
