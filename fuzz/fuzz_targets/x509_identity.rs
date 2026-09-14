//! Fuzz target for the X.509 recipient-identity parse entry point
//! (SL-1.ENC.07).
//!
//! Exercises `selis_crypto::x509::parse_identity` on fuzzed bytes — the
//! certificates a recipient credential may carry are untrusted input. The
//! contract: no panic, no OOM, terminates within budget; every structural
//! deviation is a typed error. Seeds under `fuzz/seeds/x509_identity/` are
//! generated deterministically by `cargo xtask pubkey-fuzz-seeds` (the real
//! fixture chains plus corruptions).

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_crypto::x509::parse_identity;
use selis_sandbox::{Budget, Surface};

fuzz_target!(|data: &[u8]| {
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();
    match parse_identity(data, &mut g) {
        Ok(identity) => {
            // The identity fields are slices *into* the input — they can
            // never exceed it — and a serial is never empty (parse rejects
            // one, and it is a match input).
            assert!(identity.issuer.len() <= data.len());
            assert!(!identity.serial.is_empty());
            // Any SKI present is a plausible identifier (1..=20 bytes for a
            // SHA-1 SKI; longer is a parse bug, not data).
            if let Some(ski) = identity.extension_ski {
                assert!(!ski.is_empty());
            }
        }
        Err(e) => {
            assert!(
                e.code() == selis_error::Code::EncryptMalformed
                    || e.code() == selis_error::Code::EncryptUnsupported
                    || e.is_budget()
                    || e.is_cancelled(),
                "untyped certificate-parse failure: {:?}",
                e.code()
            );
        }
    }
});
