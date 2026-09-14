//! Fuzz target for the JPXDecode codestream entry point (SL-1.FILT.08).
//!
//! Drives `selis_pdf_filter::jpx_decode` — OpenJPEG-as-WASM behind the
//! Tier-2 sandbox — with arbitrary bytes under the Fuzz budget. The
//! contract is the sandbox's whole reason to exist: **the host must survive
//! every codestream**. No panic, no OOM, termination inside the budget; a
//! fuel/cap trap is contained by the host and surfaces as a typed error,
//! which the harness reads as success. Any crash *outside* the wasm store
//! (payload parsing, budget arithmetic) aborts here and is a real finding.
//!
//! Every iteration pays the module's one-time wasmtime compile from a fresh
//! `Engine`, so throughput is seconds, not nanoseconds — this target is a
//! depth-first hunt for OpenJPEG-shaped malformations (the narrow
//! "J2K/J2P-recognised but internally hostile" band a pure-Rust fuzzer
//! could never enter), not a breadth-first byte grinder.
//!
//! Seeds (`fuzz/seeds/jpx_stream/`): the golden lossless codestream plus the
//! `filter-jpx` corpus variants, so the campaign starts straddling the
//! valid/malformed boundary instead of rediscovering the JPEG 2000 magic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_sandbox::{Budget, Surface};

fuzz_target!(|data: &[u8]| {
    // A per-fuzz target budget: bounded bytes (the module's memory cap and
    // the copy-in/copy-out charge) and a bounded deadline (the fuel meter).
    // `Surface::Fuzz` is exactly this: tiny and short, so a hostile file
    // fails in a moment instead of soaking the box.
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    // The assertion lives in the callee's contract: `jpx_decode` returns
    // `Result` and never panics; the fuzz abort mechanism turns any panic
    // (indexing, overflow unwrapping, and wasmtime's own host panic if
    // containment ever leaked) into a crashing artifact.
    let _ = selis_pdf_filter::jpx_decode(data, &mut g);
});
