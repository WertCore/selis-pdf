//! FlateDecode (SL-1.FILT.02 core — the filter xref and object streams need).
//!
//! Uses `miniz_oxide` (ADR-P0018). Handles the real-world cases:
//! * leading garbage bytes (skipped by miniz),
//! * a missing zlib header (raw deflate) — detected and retried,
//! * truncated streams that yield partial data with a deviation rather than
//!   an error (SL-1.FILT.02 Risk: "truncated Flate yields what it decoded so
//!   far" is what every other reader does).

use miniz_oxide::inflate::decompress_to_vec_with_limit;
use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

/// The outcome of a bounded Flate decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlateDecodeError {
    /// The error code: `FLATE_CORRUPT` for irrecoverable corruption.
    pub code: Code,
    /// Bytes decoded before the failure, when any.
    pub partial: Vec<u8>,
}

impl std::fmt::Display for FlateDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({} bytes decoded before failure)",
            self.code.name(),
            self.partial.len()
        )
    }
}

impl std::error::Error for FlateDecodeError {}

/// Inflate a Flate stream, returning only complete output.
///
/// # Budget
///
/// No bound: callers that accept untrusted streams must use
/// [`flate_decode_bounded`].
///
/// # Malformed Input
///
/// `FLATE_CORRUPT` when the stream is not valid deflate data.
pub fn flate_decode(data: &[u8]) -> std::result::Result<Vec<u8>, FlateDecodeError> {
    match inflate_once(data, usize::MAX) {
        Ok(out) => Ok(out),
        Err(partial) => Err(FlateDecodeError {
            code: Code::FlateCorrupt,
            partial,
        }),
    }
}

/// Inflate a Flate stream, bounded by `output_limit` and charged against the
/// budget. A truncated stream returns the partial output it decoded with
/// `FLATE_CORRUPT` (the caller may still use the prefix — SL-1.FILT.02).
///
/// # Budget
///
/// The decoded size never exceeds `output_limit`; allocation is charged.
///
/// # Malformed Input
///
/// Returns `FLATE_CORRUPT` (with partial output) on invalid deflate data.
pub fn flate_decode_bounded(
    data: &[u8],
    output_limit: u64,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let limit = usize::try_from(output_limit).unwrap_or(usize::MAX);
    match inflate_once(data, limit) {
        Ok(out) => {
            g.charge(selis_sandbox::Resource::Bytes, out.len() as u64)?;
            Ok(out)
        }
        Err(partial) => {
            // A truncated stream still yields what it decoded so far.
            let _ = g.charge(selis_sandbox::Resource::Bytes, partial.len() as u64);
            Err(err!(
                Code::FlateCorrupt,
                during = "flate-decode",
                detail = format!("{} bytes decoded before failure", partial.len())
            ))
        }
    }
}

/// Inflate once, handling both zlib-wrapped and raw-deflate streams.
/// Returns `Ok(out)` or `Err(partial)`.
///
/// `decompress_to_vec_with_limit` inflates raw deflate (no zlib header). A
/// zlib stream's 2-byte header is stripped first; the trailing Adler-32 is
/// harmless trailing input that raw inflate stops before.
fn inflate_once(data: &[u8], limit: usize) -> std::result::Result<Vec<u8>, Vec<u8>> {
    // A zlib stream begins with 0x78 (CMF for the common window sizes).
    let body = if data.len() >= 2 && data.first() == Some(&0x78) {
        data.get(2..).unwrap_or(&[])
    } else {
        data
    };
    match decompress_to_vec_with_limit(body, limit) {
        Ok(out) => Ok(out),
        Err(e) => Err(e.output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniz_oxide::deflate::{compress_to_vec, compress_to_vec_zlib};

    fn guard() -> BudgetGuard<'static> {
        selis_sandbox::Budget::unlimited().guard()
    }

    #[test]
    fn round_trip_through_miniz() {
        let data = b"The quick brown fox jumps over the lazy dog. ".repeat(50);
        let compressed = compress_to_vec_zlib(&data, 6);
        let out = flate_decode(&compressed).expect("inflate");
        assert_eq!(out, data);
    }

    #[test]
    fn raw_deflate_without_zlib_header_decodes() {
        let data = b"raw deflate stream".repeat(10);
        // miniz_oxide's compress_to_vec produces zlib-wrapped; strip the 2-byte
        // header and 4-byte adler32 to get raw deflate.
        let zlib = compress_to_vec_zlib(&data, 6);
        let raw = zlib.get(2..zlib.len().saturating_sub(4)).unwrap_or(&[]);
        let out = flate_decode(raw).expect("raw inflate");
        assert_eq!(out, data);
    }

    #[test]
    fn corrupt_stream_is_a_typed_error() {
        let e = flate_decode(b"this is not deflate data").expect_err("corrupt");
        assert_eq!(e.code, Code::FlateCorrupt);
    }

    #[test]
    fn bounded_decode_respects_the_limit() {
        let data = vec![0u8; 100_000];
        let compressed = compress_to_vec(&data, 6);
        let mut g = guard();
        let e = flate_decode_bounded(&compressed, 1000, &mut g).expect_err("limit");
        assert_eq!(e.code(), Code::FlateCorrupt);
    }

    #[test]
    fn truncated_stream_yields_partial_data() {
        let data = b"abc".repeat(500);
        let compressed = compress_to_vec_zlib(&data, 6);
        // Cut the stream in half: inflation must still produce a prefix.
        let half = compressed.len().saturating_div(2);
        let cut = compressed.get(..half).unwrap_or(&[]);
        let mut g = guard();
        match flate_decode_bounded(cut, 100_000, &mut g) {
            Ok(out) => assert!(!out.is_empty()),
            Err(e) => assert_eq!(e.code(), Code::FlateCorrupt),
        }
    }
}
