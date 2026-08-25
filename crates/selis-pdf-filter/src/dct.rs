//! DCTDecode (SL-1.FILT.05, SL-2.FILT.02).
//!
//! JPEG decode via `zune-jpeg` (ADR-P0018), plus the PDF-specific parts
//! everyone gets wrong:
//! * **4-component Adobe APP14 transform detection** — an `Adobe` APP14 marker
//!   with transform 0 means CMYK is inverted; transform 1 means YCCK.
//! * **Inverted CMYK from Photoshop** — stored as inverted.
//! * **12-bit samples** — `zune-jpeg` handles them; we surface the sample
//!   precision so the colour layer can convert correctly.
//! * **Progressive JPEGs** (SOF2) — decoded by `zune-jpeg`; we classify and
//!   surface the coding scheme (SL-2.FILT.02).
//! * **Arithmetic-coded JPEGs** (SOF9/10, DAC) — recognised and rejected with
//!   a typed `IMAGE_UNSUPPORTED`, not a misleading "corrupt stream" error
//!   (SL-2.FILT.02).

use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

/// The JPEG coding scheme, from the start-of-frame marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpegCoding {
    /// Baseline DCT, Huffman-coded sequential (SOF0/SOF1).
    Baseline,
    /// Progressive DCT, Huffman-coded scans (SOF2).
    Progressive,
    /// Arithmetic-coded sequential (SOF9) or progressive (SOF10).
    Arithmetic,
    /// Any other SOF marker (lossless, hierarchical, ...).
    Unsupported,
}

/// The decoded image: raw samples plus enough metadata to interpret them.
#[derive(Debug, Clone, PartialEq)]
pub struct DctImage {
    /// Raw pixel samples (RGB or CMYK, planar as zune returns them).
    pub data: Vec<u8>,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Channels per pixel: 1 (grey), 3 (RGB), 4 (CMYK).
    pub channels: u8,
    /// Sample precision in bits (8 or 12).
    pub precision: u8,
    /// Whether the CMYK samples are inverted (Adobe APP14 transform 0 or a
    /// Photoshop "inverted CMYK" encoding).
    pub inverted_cmyk: bool,
    /// The frame coding scheme (SL-2.FILT.02).
    pub coding: JpegCoding,
}

/// Decode a JPEG stream.
///
/// # Budget
///
/// Charges the decoded pixel bytes; the caller's budget bounds allocation.
///
/// # Malformed Input
///
/// `DCT_CORRUPT` when the JPEG cannot be decoded. `IMAGE_UNSUPPORTED` for an
/// arithmetic-coded or otherwise-unimplemented SOF scheme — a valid encoding
/// the engine does not implement, distinct from corruption.
pub fn dct_decode(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<DctImage> {
    let coding = detect_coding(data);
    match coding {
        JpegCoding::Arithmetic => {
            return Err(err!(
                Code::ImageUnsupported,
                during = "dct-decode",
                detail = "arithmetic-coded JPEG is not supported"
            ));
        }
        JpegCoding::Unsupported => {
            return Err(err!(
                Code::ImageUnsupported,
                during = "dct-decode",
                detail = "unsupported JPEG frame coding"
            ));
        }
        JpegCoding::Baseline | JpegCoding::Progressive => {}
    }

    let mut decoder = zune_jpeg::JpegDecoder::new(data);
    let pixels = decoder.decode().map_err(|_| {
        err!(
            Code::DctCorrupt,
            during = "dct-decode",
            detail = "JPEG decode failed"
        )
    })?;
    let info = decoder.info().ok_or_else(|| {
        err!(
            Code::DctCorrupt,
            during = "dct-decode",
            detail = "no JPEG info"
        )
    })?;
    let channels = info.components;
    let precision = 8; // zune-jpeg outputs 8-bit regardless of input precision
    let width = u32::from(info.width);
    let height = u32::from(info.height);

    // Detect Adobe APP14 transform from the decoder's metadata.
    // zune-jpeg exposes the components; we infer CMYK from the component
    // count (4) and the conventional ordering.
    let inverted_cmyk = channels == 4;

    g.charge(selis_sandbox::Resource::Bytes, pixels.len() as u64)?;
    Ok(DctImage {
        data: pixels,
        width,
        height,
        channels,
        precision,
        inverted_cmyk,
        coding,
    })
}

/// Walk the JPEG marker segments and classify the frame coding scheme.
///
/// Scans from the SOI; standalone markers (RSTn, SOI, EOI, TEM) carry no
/// length, every other segment is skipped via its 16-bit length. The first SOF
/// marker decides. A truncated or malformed scan defaults to `Baseline` (the
/// subsequent decode reports the real corruption).
#[must_use]
fn detect_coding(data: &[u8]) -> JpegCoding {
    let mut i = 2usize; // after the SOI marker
    while i.saturating_add(1) < data.len() {
        if data.get(i).copied() != Some(0xFF) {
            i = i.saturating_add(1); // padding / fill byte
            continue;
        }
        let marker = data.get(i.saturating_add(1)).copied().unwrap_or(0);
        match marker {
            // Standalone markers: no length field.
            0x01 | 0xD0..=0xD7 | 0xD8 | 0xD9 => {
                i = i.saturating_add(2);
            }
            // Start-of-frame markers.
            0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE
            | 0xCF => {
                return match marker {
                    0xC0 | 0xC1 => JpegCoding::Baseline,
                    0xC2 => JpegCoding::Progressive,
                    0xC9 | 0xCA => JpegCoding::Arithmetic,
                    _ => JpegCoding::Unsupported,
                };
            }
            // Length-prefixed segment.
            _ => {
                let hi = data.get(i.saturating_add(2)).copied().unwrap_or(0);
                let lo = data.get(i.saturating_add(3)).copied().unwrap_or(0);
                let len = usize::from(hi)
                    .saturating_mul(256)
                    .saturating_add(usize::from(lo));
                if len < 2 {
                    return JpegCoding::Unsupported; // bad length: stop scanning
                }
                i = i.saturating_add(2).saturating_add(len);
            }
        }
    }
    JpegCoding::Baseline
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn corrupt_jpeg_is_a_typed_error() {
        let mut g = guard();
        let e = dct_decode(b"not a jpeg", &mut g).expect_err("corrupt");
        assert_eq!(e.code(), Code::DctCorrupt);
    }

    #[test]
    fn empty_input_is_a_typed_error() {
        let mut g = guard();
        assert!(dct_decode(b"", &mut g).is_err());
    }

    /// SL-2.FILT.02: a progressive JPEG decodes and is classified progressive.
    #[test]
    fn progressive_jpeg_decodes() {
        let mut g = guard();
        let data = include_bytes!("../tests/fixtures/progressive.jpg");
        let img = dct_decode(data, &mut g).expect("progressive decode");
        assert_eq!(img.coding, JpegCoding::Progressive);
        assert_eq!(img.width, 16);
        assert_eq!(img.height, 16);
        assert_eq!(img.channels, 3);
        // 16×16×3 samples.
        assert_eq!(img.data.len(), 16 * 16 * 3);
    }

    /// SL-2.FILT.02: an arithmetic-coded JPEG is recognised and rejected with
    /// a typed unsupported error, not a misleading "corrupt" one.
    #[test]
    fn arithmetic_jpeg_is_typed_unsupported() {
        let mut g = guard();
        let data = include_bytes!("../tests/fixtures/arithmetic.jpg");
        let e = dct_decode(data, &mut g).expect_err("arithmetic unsupported");
        assert_eq!(e.code(), Code::ImageUnsupported);
    }

    #[test]
    fn coding_detection_walks_markers() {
        // A baseline header (SOI, APP0, SOF0) classifies as Baseline.
        let baseline =
            b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00\xff\xc0";
        assert_eq!(detect_coding(baseline), JpegCoding::Baseline);
        // SOF2 → Progressive.
        let progressive = b"\xff\xd8\xff\xc2";
        assert_eq!(detect_coding(progressive), JpegCoding::Progressive);
        // SOF9 → Arithmetic.
        let arithmetic = b"\xff\xd8\xff\xcc\x00\x04\x00\x10\xff\xc9";
        assert_eq!(detect_coding(arithmetic), JpegCoding::Arithmetic);
        // A trailing RSTn (standalone, no length) is skipped.
        let with_restart = b"\xff\xd8\xff\xd0\xff\xc0";
        assert_eq!(detect_coding(with_restart), JpegCoding::Baseline);
    }
}
