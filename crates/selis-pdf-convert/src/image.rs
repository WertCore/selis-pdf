//! Inbound JPEG images from `data:` URIs (HTML `<img>` / Markdown `![]()`).
//!
//! Only `data:image/jpeg;base64,…` is accepted. The payload is decoded and its
//! SOF marker is read for the *true* width, height and component count; the
//! JPEG bytes are then embedded verbatim as a `DCTDecode` stream (no decoder
//! runs on the conversion path). Anything else — remote `http(s)` URLs, file
//! paths, other schemes, PNG/GIF/WebP data URIs, and malformed or truncated
//! JPEGs — is refused with a typed [`ImageError`] and skipped by the parsers;
//! the offending source text is not painted and no XObject is emitted.
//!
//! See the crate docs for the honest list of what is *not* supported.

use selis_error::{Code, Ctx, Error};

/// The PDF device colour space implied by a JPEG's SOF component count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageColorSpace {
    /// One component (SOF `Ns = 1`).
    DeviceGray,
    /// Three components (SOF `Ns = 3`, the usual YCbCr JPEG).
    DeviceRGB,
    /// Four components (SOF `Ns = 4`).
    DeviceCMYK,
}

impl ImageColorSpace {
    /// The PDF name to write in the image XObject's `/ColorSpace` entry.
    #[must_use]
    pub(crate) const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::DeviceGray => b"DeviceGray",
            Self::DeviceRGB => b"DeviceRGB",
            Self::DeviceCMYK => b"DeviceCMYK",
        }
    }
}

/// A validated, embed-ready JPEG recovered from a data URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddedImage {
    /// The raw JPEG file bytes, embedded verbatim behind `DCTDecode`.
    pub jpeg: Vec<u8>,
    /// Intrinsic pixel width, read from the SOF marker.
    pub width: u32,
    /// Intrinsic pixel height, read from the SOF marker.
    pub height: u32,
    /// Colour space implied by the SOF component count.
    pub color_space: ImageColorSpace,
}

/// A typed refusal of an image source. The parsers currently skip on `Err`;
/// the enum is kept typed so a stricter caller can choose to fail instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageError {
    /// A URL with a scheme we never fetch (or a bare path). Carries the scheme
    /// so a diagnostic can refuse it *by name*.
    NonDataUri(String),
    /// A `data:` URI whose media type is not `image/jpeg`.
    NotJpeg(String),
    /// A `data:` URI missing the `;base64` marker or a comma.
    InvalidDataUri,
    /// The base64 payload is malformed.
    InvalidBase64,
    /// The JPEG bytes are truncated or structurally invalid; the label says
    /// which invariant failed.
    MalformedJpeg(&'static str),
}

impl ImageError {
    /// Convert to the crate's typed engine error with a stable code and a
    /// sanitised, human-readable detail (document-derived text never reaches a
    /// diagnostic verbatim, ADR-P0017).
    ///
    /// # Budget
    ///
    /// No budget charge: this only builds a small error context.
    ///
    /// # Malformed Input
    ///
    /// The error's own detail is sanitised; no input is parsed here.
    ///
    /// # Note
    ///
    /// The parsers currently skip refused images rather than fail the whole
    /// document, so this conversion is exercised by unit tests and is the
    /// typed-error surface a stricter caller can adopt without re-parsing.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn into_error(self) -> Error {
        let (code, during, detail) = match self {
            Self::NonDataUri(scheme) => (
                Code::ConvertUnsupported,
                "image-src-scheme",
                format!("refused image source scheme: {scheme}"),
            ),
            Self::NotJpeg(kind) => (
                Code::ConvertUnsupported,
                "image-format",
                format!("unsupported image media type: {kind}"),
            ),
            Self::InvalidDataUri => (
                Code::ConvertMalformed,
                "image-data-uri",
                "expected data:image/jpeg;base64,…".to_string(),
            ),
            Self::InvalidBase64 => (
                Code::ConvertMalformed,
                "image-base64",
                "invalid base64 payload".to_string(),
            ),
            Self::MalformedJpeg(why) => (
                Code::ConvertMalformed,
                "image-jpeg",
                format!("malformed JPEG: {why}"),
            ),
        };
        Error::with(
            code,
            Ctx::new()
                .during(during)
                .detail(Ctx::sanitise(detail.as_bytes())),
        )
    }
}

/// Parse one image source into an embed-ready JPEG.
///
/// Accepted form: `data:image/jpeg;base64,<payload>` (media type is
/// case-insensitive). Nothing is fetched: any other scheme or a bare path is
/// refused by name with [`ImageError::NonDataUri`].
///
/// # Budget
///
/// No budget charge here: the decoder allocates only the decoded payload and
/// the caller charges that buffer's bytes when it writes the XObject. The
/// payload is bounded by the document text that carried it.
///
/// # Malformed Input
///
/// Returns a typed [`ImageError`] for a non-data URI, a non-JPEG media type, a
/// missing `;base64` marker, invalid base64, or a JPEG with no readable SOF
/// marker (truncated or malformed). No partial image is returned.
pub(crate) fn parse_data_uri(src: &str) -> Result<EmbeddedImage, ImageError> {
    let lower = src.to_ascii_lowercase();
    if !lower.starts_with("data:") {
        let scheme = src
            .split_once(':')
            .map_or("path", |(scheme, _)| scheme)
            .to_string();
        return Err(ImageError::NonDataUri(scheme));
    }
    // `to_ascii_lowercase` preserves byte length, so indices from the original
    // string are valid on `lower` too. Only the media type is compared
    // case-insensitively; the payload keeps base64's case-sensitive alphabet.
    let Some(comma) = src.find(',') else {
        return Err(ImageError::InvalidDataUri);
    };
    let meta = lower.get("data:".len()..comma).unwrap_or("");
    if meta != "image/jpeg;base64" {
        if let Some(kind) = meta.strip_suffix(";base64") {
            return Err(ImageError::NotJpeg(kind.to_string()));
        }
        let kind = meta.split(';').next().unwrap_or(meta).to_string();
        return Err(ImageError::NotJpeg(kind));
    }
    let encoded = src.get(comma.saturating_add(1)..).unwrap_or("");
    let jpeg = decode_base64(encoded).ok_or(ImageError::InvalidBase64)?;
    let (width, height, color_space) = parse_sof(&jpeg)?;
    Ok(EmbeddedImage {
        jpeg,
        width,
        height,
        color_space,
    })
}

/// Decode unpadded or padded standard base64 (whitespace ignored).
///
/// # Budget
///
/// Allocates one output buffer sized from the input; no budget guard is
/// available at this pure-function boundary — callers bound the source text.
///
/// # Malformed Input
///
/// `None` on an invalid alphabet byte, a stray `=`, or a one-character
/// remainder.
// The base64 accumulator is assembled with bit shifts that provably leave
// 8-bit values; the explicit casts below are lossless by construction.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
    let mut i = 0usize;
    while i < bytes.len() {
        let take = (bytes.len() - i).min(4);
        let mut vals = [0u8; 4];
        let mut explicit_pad = 0u8;
        for (k, &byte) in bytes[i..i + take].iter().enumerate() {
            let value = b64_value(byte)?;
            if value == 64 {
                // `=` only in the last position of the final chunk, never
                // before a data byte.
                if k < 2 || k != take - 1 {
                    return None;
                }
                explicit_pad = explicit_pad.saturating_add(1);
            } else if explicit_pad > 0 {
                return None;
            } else {
                vals[k] = value;
            }
        }
        let pad = if explicit_pad > 0 {
            explicit_pad
        } else {
            (4 - take) as u8
        };
        if pad > 2 {
            return None;
        }
        if explicit_pad > 0 && i + take != bytes.len() {
            return None;
        }
        let triple = (vals[0] as u32) << 18
            | (vals[1] as u32) << 12
            | (vals[2] as u32) << 6
            | (vals[3] as u32);
        out.push((triple >> 16) as u8);
        if pad < 2 {
            out.push((triple >> 8) as u8);
        }
        if pad < 1 {
            out.push(triple as u8);
        }
        i += take;
    }
    Some(out)
}

/// Map one base64 alphabet byte to 0..63, or 64 for `=`.
///
/// # Budget
///
/// None: a single-byte lookup.
///
/// # Malformed Input
///
/// `None` for any byte outside the standard alphabet and padding.
fn b64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte.wrapping_sub(b'A')),
        b'a'..=b'z' => Some(byte.wrapping_sub(b'a').wrapping_add(26)),
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0').wrapping_add(52)),
        b'+' => Some(62),
        b'/' => Some(63),
        b'=' => Some(64),
        _ => None,
    }
}

/// Read the first SOF marker of a JPEG, returning `(width, height, colour)`.
///
/// Walks the marker stream up to SOS/EOI, skipping length-prefixed segments
/// (APPn, DQT, DHT, …) and standalone markers (RSTn, TEM). It never enters the
/// entropy-coded scan, where `0xFF` has stuffed-byte semantics.
///
/// # Budget
///
/// Linear in the JPEG length; allocates nothing.
///
/// # Malformed Input
///
/// Returns a typed [`ImageError::MalformedJpeg`] when SOI is missing, a segment
/// length overruns, a marker or header is truncated, the SOF payload is short,
/// dimensions are zero, or the component count is not 1/3/4.
fn parse_sof(jpeg: &[u8]) -> Result<(u32, u32, ImageColorSpace), ImageError> {
    if byte(jpeg, 0)? != 0xFF || byte(jpeg, 1)? != 0xD8 {
        return Err(ImageError::MalformedJpeg("missing SOI marker"));
    }
    let mut i = 2usize;
    while i < jpeg.len() {
        let mut code = byte(jpeg, i)?;
        // Fill bytes before a marker code are legal (T.81 §B.1.1.2).
        while code == 0xFF {
            i = i.saturating_add(1);
            code = byte(jpeg, i)?;
        }
        i = i.saturating_add(1);
        if code == 0xD9 {
            break; // EOI before SOF
        }
        if code == 0x01 || code == 0xD8 || (0xD0..=0xD7).contains(&code) {
            continue; // standalone markers carry no length
        }
        if code == 0xDA {
            break; // SOS: the scan starts; no SOF was found
        }
        let len = u16::from_be_bytes([byte(jpeg, i)?, byte(jpeg, i.saturating_add(1))?]);
        if len < 2 {
            return Err(ImageError::MalformedJpeg("zero segment length"));
        }
        let start = i.saturating_add(2);
        let end = start.saturating_add(usize::from(len).saturating_sub(2));
        if end > jpeg.len() {
            return Err(ImageError::MalformedJpeg("segment overruns JPEG"));
        }
        if is_sof(code) {
            let p = start;
            // precision(1) + height(2) + width(2) + Ns(1) = 6 bytes minimum.
            if end < p.saturating_add(6) {
                return Err(ImageError::MalformedJpeg("short SOF payload"));
            }
            let height = u16::from_be_bytes([
                byte(jpeg, p.saturating_add(1))?,
                byte(jpeg, p.saturating_add(2))?,
            ]);
            let width = u16::from_be_bytes([
                byte(jpeg, p.saturating_add(3))?,
                byte(jpeg, p.saturating_add(4))?,
            ]);
            let components = byte(jpeg, p.saturating_add(5))?;
            let need = p
                .saturating_add(6)
                .saturating_add(usize::from(components).saturating_mul(3));
            if need > end {
                return Err(ImageError::MalformedJpeg("short SOF component table"));
            }
            if width == 0 || height == 0 {
                return Err(ImageError::MalformedJpeg("zero image dimension"));
            }
            let color_space = match components {
                1 => ImageColorSpace::DeviceGray,
                3 => ImageColorSpace::DeviceRGB,
                4 => ImageColorSpace::DeviceCMYK,
                _ => return Err(ImageError::MalformedJpeg("unsupported component count")),
            };
            return Ok((u32::from(width), u32::from(height), color_space));
        }
        i = end;
    }
    Err(ImageError::MalformedJpeg("no SOF marker before scan"))
}

/// Whether `code` is a Start-of-Frame marker (C0–CF except C4/C8/CC).
fn is_sof(code: u8) -> bool {
    matches!(
        code,
        0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
    )
}

/// Read one byte without panicking on a truncated buffer.
fn byte(jpeg: &[u8], i: usize) -> Result<u8, ImageError> {
    jpeg.get(i)
        .copied()
        .ok_or(ImageError::MalformedJpeg("truncated JPEG"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    // A tiny, hand-built baseline JPEG skeleton: SOI + APP0 + SOF0 (2×3,
    // three components) + SOS. It is never decoded — only its markers are read.
    fn jpeg_2x3() -> Vec<u8> {
        vec![
            0xFF, 0xD8, // SOI
            0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0,
            0, // APP0
            0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x03, 0x00, 0x02, 0x03, 0x01, 0x11, 0x00, 0x02,
            0x11, 0x01, 0x03, 0x11, 0x01, // SOF0: 2 w × 3 h, 3 components
            0xFF, 0xDA, 0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F,
            0x00, // SOS
            0x00, 0x00, 0xFF, 0xD9, // minimal scan + EOI
        ]
    }

    #[test]
    fn parses_jpeg_geometry_from_sof() {
        let (w, h, cs) = parse_sof(&jpeg_2x3()).expect("SOF");
        assert_eq!((w, h), (2, 3));
        assert_eq!(cs, ImageColorSpace::DeviceRGB);
        assert_eq!(cs.as_bytes(), b"DeviceRGB");
    }

    #[test]
    fn grey_and_cmyk_component_counts() {
        // In `jpeg_2x3` the SOF0 payload starts at index 24 (precision); Ns is
        // the sixth payload byte, at index 29.
        let mut grey = jpeg_2x3();
        grey[29] = 1;
        let (_, _, cs) = parse_sof(&grey).expect("SOF");
        assert_eq!(cs, ImageColorSpace::DeviceGray);
        let mut cmyk = jpeg_2x3();
        // A four-component SOF needs a longer segment (2 + 6 + 12 = 20 bytes)
        // and three more component-spec bytes.
        cmyk[23] = 0x14;
        cmyk[29] = 4;
        cmyk.extend_from_slice(&[0x01, 0x11, 0x01]);
        let (_, _, cs) = parse_sof(&cmyk).expect("SOF");
        assert_eq!(cs, ImageColorSpace::DeviceCMYK);
    }

    #[test]
    fn base64_handles_padded_and_unpadded() {
        assert_eq!(
            decode_base64("aGVsbG8=").as_deref(),
            Some(b"hello".as_slice())
        );
        assert_eq!(
            decode_base64("aGVsbG8").as_deref(),
            Some(b"hello".as_slice())
        );
        assert_eq!(
            decode_base64("aGVs\nbG8=").as_deref(),
            Some(b"hello".as_slice())
        );
        assert_eq!(decode_base64("not base64!"), None);
        assert_eq!(decode_base64("aGVsbG8=x"), None);
    }

    #[test]
    fn rejects_non_jpeg_and_remote_sources_by_name() {
        assert!(matches!(
            parse_data_uri("https://example.com/a.jpg"),
            Err(ImageError::NonDataUri(scheme)) if scheme == "https"
        ));
        assert!(matches!(
            parse_data_uri("/tmp/a.jpg"),
            Err(ImageError::NonDataUri(scheme)) if scheme == "path"
        ));
        assert!(matches!(
            parse_data_uri("data:image/png;base64,iVBOR"),
            Err(ImageError::NotJpeg(kind)) if kind == "image/png"
        ));
    }

    #[test]
    fn rejects_malformed_jpeg_with_a_typed_error() {
        assert!(matches!(
            parse_data_uri("data:image/jpeg;base64,notbase64!!"),
            Err(ImageError::InvalidBase64)
        ));
        let bare = "data:image/jpeg;base64,";
        let err = parse_data_uri(bare).expect_err("empty JPEG");
        assert!(matches!(err, ImageError::MalformedJpeg(_)));
        assert_eq!(
            err.clone().into_error().code(),
            selis_error::Code::ConvertMalformed
        );
    }

    /// A minimal standard-base64 encoder for test fixtures (the inverse of the
    /// decoder above; no dependency needed).
    fn encode_base64(data: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let n = (u32::from(chunk[0]) << 16)
                | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
                | u32::from(chunk.get(2).copied().unwrap_or(0));
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                TABLE[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    #[test]
    fn data_uri_round_trips_a_real_jpeg() {
        let jpeg = jpeg_2x3();
        let src = format!("data:image/jpeg;base64,{}", encode_base64(&jpeg));
        let img = parse_data_uri(&src).expect("image");
        assert_eq!(img.jpeg, jpeg);
        assert_eq!((img.width, img.height), (2, 3));
        assert_eq!(img.color_space, ImageColorSpace::DeviceRGB);
    }
}
