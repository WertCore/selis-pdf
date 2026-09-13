//! Fuzz target for the Worker-protocol dispatch entry (SL-4.WASM.01).
//!
//! The request deserializer is a parser entry point: arbitrary bytes reach
//! `Worker::handle`, which must always answer a well-formed response and
//! never panic, OOM, or poison the worker (the fuzz-budget discipline of
//! every target here, applied to the binding boundary).
//!
//! Contract: no panic, no OOM, terminates within the budget; the worker
//! stays usable after every message.

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_pdf_wasm::worker::{NullProgress, Worker, WorkerEnv};
use selis_sandbox::{CancelToken, FixedClock};

fuzz_target!(|data: &[u8]| {
    let mut worker = Worker::new();
    // The injected clock never moves; the wall deadline cannot fire, so the
    // target exercises parse/validate/route containment, not the deadline.
    let clock = FixedClock(0);
    // The input splits into (request, payload): the first byte is the
    // payload length (so long payloads are constructible but bounded by the
    // fuzz input itself).
    let (payload_len, rest) = match data.split_first() {
        Some((&n, rest)) => (usize::from(n), rest),
        None => (0, &[][..]),
    };
    let payload_len = payload_len.min(rest.len());
    let (payload, request) = rest.split_at(payload_len);

    let env = WorkerEnv {
        clock: &clock,
        cancel: CancelToken::new(),
        progress: &NullProgress,
    };
    let out = worker.handle(request, payload, &env);
    // Every dispatch must answer with a well-formed response envelope.
    assert_eq!(out.response.v, 1, "the response must echo schema v1");
    // The worker must still be usable: a second dispatch with a trivial
    // valid message must answer (any response, not a hang or panic).
    let follow_up = br#"{"v":1,"id":424242,"op":"cancel","target":1}"#;
    let _ = worker.handle(follow_up, &[], &env);
});
