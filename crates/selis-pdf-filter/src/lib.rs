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

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod flate;

pub use flate::{flate_decode, flate_decode_bounded, FlateDecodeError};

use selis_error::Result;

/// Decode a stream through a single filter.
///
/// Phase 1 supports Flate only; a stream naming any other filter returns
/// `FILTER_UNKNOWN`.
///
/// # Budget
///
/// `output_limit` bounds the decoded size — a hostile `Flate then Flate then
/// Flate` bomb hits the limit, not memory (SL-1.FILT.01).
///
/// # Malformed Input
///
/// `FLATE_CORRUPT` when the stream cannot be inflated.
pub fn decode(
    filter: &str,
    data: &[u8],
    output_limit: u64,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    match filter {
        "FlateDecode" | "Fl" => flate_decode_bounded(data, output_limit, g),
        other => Err(selis_error::err!(
            selis_error::Code::FilterUnknown,
            during = "filter-decode",
            detail = other
        )),
    }
}
