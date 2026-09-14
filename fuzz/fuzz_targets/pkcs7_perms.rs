//! Fuzz target for the CMS payload permission-block decoder (SL-1.ENC.09).
//!
//! The 24-byte decrypted payload (20-byte seed + 4-byte little-endian
//! permission block) is the *only* thing standing between a recipient key
//! and a grant — this target pins the decoder's hard rule: it either
//! decodes exactly a 24-byte payload or refuses (`None`); it never yields a
//! silent zero/default grant for damaged input (a "silent bypass" would be
//! the decoder returning `Some` for the wrong byte count). Seeds from
//! `cargo xtask pubkey-fuzz-seeds` cover both boundaries.

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_crypto::pkcs7::{decode_permission_block, payload_seed, CMS_PAYLOAD_LEN};

fuzz_target!(|data: &[u8]| {
    // No panics, no allocation, total on all inputs — these are `pure`
    // byte-slice functions.
    let decoded = decode_permission_block(data);
    let seed = payload_seed(data);
    match (decoded, seed) {
        (Some(bits), Some(seed)) => {
            assert_eq!(data.len(), CMS_PAYLOAD_LEN, "decoded a non-24-byte payload");
            assert_eq!(seed.len(), 20);
            // The grant is the exact little-endian block — recomputed here
            // (the test-side mirror) so a decoder regression flips a bit.
            let expected = u32::from_le_bytes(data[20..24].try_into().expect("24-byte slice"));
            assert_eq!(bits, expected);
        }
        (None, None) => {}
        _ => panic!("split-brain decode: permissions and seed disagree"),
    }
});
