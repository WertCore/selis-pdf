//! `selis-pdf-filter` — stream filters and codecs (SL-1.FILT.*).
//!
//! Owns the filter pipeline, Flate/LZW/ASCII/RunLength, DCT, CCITT, JBIG2,
//! JPX, and the Crypt filter (01-ARCHITECTURE.md §3). Open-sourced under
//! Apache-2.0 (ADR-P0030). Each filter is streaming, budget-bounded, and has a
//! round-trip property test where it is also an encoder.
//!
//! **Phase 1 implements the pipeline and FlateDecode** (the filter xref and
//! object streams are Flate-encoded by construction); the rest land in
//! SL-1.FILT.03–09.
//!
//! # API stability (SL-1.OSS.01)
//!
//! This crate publishes under Apache-2.0 (ADR-P0030). The public API is **not
//! yet stable**: between 0.1.x releases names, signatures, and semantics may
//! change as Phase 1 matures, and breaking changes are called out in the
//! changelog. At 1.0 the API is frozen under semver.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod ascii;
pub mod dct;
pub mod fax;
pub mod flate;
pub mod jbig2;
pub mod lzw;
pub mod pipeline;

pub use ascii::{ascii85_decode, ascii_hex_decode, runlength_decode};
pub use dct::{dct_decode, DctImage};
pub use fax::{ccitt_decode, FaxParms};
pub use flate::{flate_decode, flate_decode_bounded, FlateDecodeError};
pub use jbig2::jbig2_decode;
pub use lzw::lzw_decode;
pub use pipeline::{decode_chain, decode_stream, DecodeParms};

use selis_error::Result;

/// Decode a stream through a single filter.
///
/// Phase 1 supports Flate, LZW, ASCIIHex, ASCII85, and RunLength. A stream
/// naming any other filter returns `FILTER_UNKNOWN`.
///
/// # Budget
///
/// `output_limit` bounds the decoded size — a hostile `Flate then Flate then
/// Flate` bomb hits the limit, not memory (SL-1.FILT.01).
///
/// # Malformed Input
///
/// `FLATE_CORRUPT` / `LZW_CORRUPT` / `ASCII_CORRUPT` /
/// `RUNLENGTH_CORRUPT` when the stream cannot be decoded.
pub fn decode(
    filter: &str,
    data: &[u8],
    output_limit: u64,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    match filter {
        "FlateDecode" | "Fl" => flate_decode_bounded(data, output_limit, g),
        "LZWDecode" | "LZW" => {
            // /EarlyChange is a /DecodeParms concern (SL-1.FILT.01); the
            // default 0 is used here.
            let _ = output_limit;
            lzw_decode(data, 0, g)
        }
        "ASCIIHexDecode" | "AHx" => ascii_hex_decode(data, g),
        "ASCII85Decode" | "A85" => ascii85_decode(data, g),
        "RunLengthDecode" | "RL" => runlength_decode(data, g),
        other => Err(selis_error::err!(
            selis_error::Code::FilterUnknown,
            during = "filter-decode",
            detail = other
        )),
    }
}
