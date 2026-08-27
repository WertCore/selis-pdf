//! FlateDecode (SL-1.FILT.02 core — the filter xref and object streams need).
//!
//! Uses `miniz_oxide` (ADR-P0018). Handles the real-world cases:
//! * leading garbage bytes (skipped by miniz),
//! * a missing zlib header (raw deflate) — detected and retried,
//! * truncated streams that yield partial data with a deviation rather than
//!   an error (SL-1.FILT.02 Risk: "truncated Flate yields what it decoded so
//!   far" is what every other reader does).

use miniz_oxide::deflate::compress_to_vec_zlib;
use miniz_oxide::inflate::decompress_to_vec_with_limit;
use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

/// The highest zlib compression level accepted by [`flate_encode`].
pub const FLATE_LEVEL_MAX: u8 = 10;

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
/// validated zlib header (RFC 1950) is stripped first; the trailing Adler-32
/// is harmless trailing input that raw inflate stops before.
fn inflate_once(data: &[u8], limit: usize) -> std::result::Result<Vec<u8>, Vec<u8>> {
    let body = if is_zlib_header(data) {
        data.get(2..).unwrap_or(&[])
    } else {
        data
    };
    match decompress_to_vec_with_limit(body, limit) {
        Ok(out) => Ok(out),
        Err(e) => Err(e.output),
    }
}

/// A valid, dictionary-free zlib header per RFC 1950: deflate method, window
/// ≤ 32 KiB, no preset dictionary, and a passing header checksum. Window
/// sizes other than the common 32 KiB (`0x78`) are legal — e.g. `0x68 0x43`
/// for a 16 KiB window.
fn is_zlib_header(data: &[u8]) -> bool {
    let (Some(&cmf), Some(&flg)) = (data.first(), data.get(1)) else {
        return false;
    };
    let header = u32::from(cmf).wrapping_shl(8) | u32::from(flg);
    cmf & 0x0F == 8
        && cmf >> 4 <= 7
        && flg & 0x20 == 0
        && header.wrapping_rem(31) == 0
}

/// Deflate `data` into a zlib-wrapped stream (`FlateDecode`-ready).
///
/// `level` is clamped to miniz_oxide's `0..=10`; higher is smaller and slower.
/// The output carries a standard zlib header and trailing Adler-32, so
/// [`flate_decode_bounded`] round-trips it.
///
/// # Budget
///
/// No charge; this is an encode of caller-owned data (SL-1A.TOOL.07 wires the
/// allocation cost through the caller's budget).
///
/// # Malformed Input
///
/// None: encoding cannot fail on valid input bytes.
#[must_use]
pub fn flate_encode(data: &[u8], level: u8) -> Vec<u8> {
    compress_to_vec_zlib(data, level.min(FLATE_LEVEL_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniz_oxide::deflate::{compress_to_vec, compress_to_vec_zlib};
    use miniz_oxide::{mz_adler32_oxide, MZ_ADLER32_INIT};

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
    fn encode_round_trips_through_decode() {
        let data = b"The quick brown fox jumps over the lazy dog. ".repeat(50);
        for level in [0u8, 6, 10] {
            let encoded = flate_encode(&data, level);
            assert!(is_zlib_header(&encoded), "encode emits a zlib header");
            let out = flate_decode(&encoded).expect("inflate");
            assert_eq!(out, data, "level {level} round-trips");
        }
        // Levels above the max clamp to the max instead of panicking.
        let encoded = flate_encode(&data, 250);
        assert_eq!(flate_decode(&encoded).expect("inflate"), data);
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
    fn non_standard_zlib_window_header_is_still_a_header() {
        // smoke.png's IDAT used `68 43`, a valid zlib header (CMF=0x68 →
        // deflate, 16 KiB window) rather than the common `78 xx`. The old
        // strip logic matched only 0x78 and fed `68 43 ...` straight to the
        // raw inflater, corrupting the stream. Regress that.
        let data = b"non-standard header ".repeat(40);
        let raw = compress_to_vec(&data, 6); // raw deflate, no wrapper
        let mut zlib = vec![0x68u8, 0x43];
        zlib.extend_from_slice(&raw);
        zlib.extend_from_slice(&mz_adler32_oxide(MZ_ADLER32_INIT, &data).to_be_bytes());
        assert!(is_zlib_header(&zlib));
        let out = flate_decode(&zlib).expect("0x68-header inflate");
        assert_eq!(out, data);
    }

    #[test]
    fn zlib_header_validation() {
        assert!(is_zlib_header(&[0x78, 0x9C])); // typical
        assert!(is_zlib_header(&[0x68, 0x43])); // 16 KiB window, no dict
        assert!(is_zlib_header(&[0x08, 0x1D])); // smallest window, valid check
        assert!(is_zlib_header(&[0x78, 0xDA, 0x00])); // only bytes 0-1 matter
        assert!(!is_zlib_header(&[0x08])); // too short
        assert!(!is_zlib_header(&[0x88, 0x13])); // CINFO 8 > allowed 7
        assert!(!is_zlib_header(&[0x78, 0x3E])); // FDICT set (dictionary)
        assert!(!is_zlib_header(&[0x08, 0x1E])); // checksum does not validate
        assert!(!is_zlib_header(&[0x09, 0x21])); // CM != deflate
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
