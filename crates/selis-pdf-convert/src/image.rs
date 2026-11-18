//! Inbound JPEG and PNG images from `data:` URIs (HTML `<img>` / Markdown `![]()`).
//!
//! Accepted media types are `data:image/jpeg;base64,…` and
//! `data:image/png;base64,…` (case-insensitive). A JPEG's SOF marker is read
//! for the *true* width, height and component count, and the JPEG bytes are
//! embedded verbatim as a `DCTDecode` stream (no decoder runs on that path).
//! A PNG is decoded with the engine's [`selis_image::decode_png`] (zlib Flate
//! IDAT + PNG scanline filters), its alpha — if any — is flattened over white,
//! and the raw samples are re-encoded with
//! [`selis_pdf_filter::flate_encode`] into a `FlateDecode` stream. Both paths
//! refuse truncated or malformed input before an XObject is emitted.
//!
//! Anything else — remote `http(s)` URLs, file paths, other schemes, GIF/WebP/
//! SVG data URIs, palette/interlaced/non-8-bit PNGs, and malformed or truncated
//! files — is refused with a typed [`ImageError`] and skipped by the parsers;
//! the offending source text is not painted and no XObject is emitted.
//!
//! See the crate docs for the honest list of what is *not* supported.

use selis_error::{Code, Ctx, Error};
use selis_sandbox::Budget;

/// The PDF device colour space implied by an image's samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageColorSpace {
    /// One component (JPEG SOF `Ns = 1`, or a grey PNG).
    DeviceGray,
    /// Three components (JPEG SOF `Ns = 3`, or a colour PNG).
    DeviceRGB,
    /// Four components (JPEG SOF `Ns = 4`). PNGs never produce this.
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

/// The PDF image filter carrying [`EmbeddedImage::data`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageFilter {
    /// Verbatim JPEG bytes.
    DctDecode,
    /// Raw samples that have already been re-encoded with zlib Flate.
    FlateDecode,
}

impl ImageFilter {
    /// The PDF name to write in the image XObject's `/Filter` entry.
    ///
    /// # Budget
    ///
    /// No budget charge: this returns a static byte slice.
    ///
    /// # Malformed Input
    ///
    /// None: the enum is closed and every variant maps to a fixed name.
    #[must_use]
    pub(crate) const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::DctDecode => b"DCTDecode",
            Self::FlateDecode => b"FlateDecode",
        }
    }
}

/// A validated, embed-ready image recovered from a data URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddedImage {
    /// JPEG bytes (verbatim) or Flate-compressed raw samples, per `filter`.
    pub data: Vec<u8>,
    /// Intrinsic pixel width (SOF for JPEG, IHDR for PNG).
    pub width: u32,
    /// Intrinsic pixel height (SOF for JPEG, IHDR for PNG).
    pub height: u32,
    /// Colour space of the decoded samples.
    pub color_space: ImageColorSpace,
    /// The filter the emitter must write for `data`.
    pub filter: ImageFilter,
}

/// A typed refusal of an image source. The parsers currently skip on `Err`;
/// the enum is kept typed so a stricter caller can choose to fail instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageError {
    /// A URL with a scheme we never fetch (or a bare path). Carries the scheme
    /// so a diagnostic can refuse it *by name*.
    NonDataUri(String),
    /// A `data:` URI whose media type is not JPEG/PNG. Carries the kind.
    NotImage(String),
    /// A `data:` URI missing the `;base64` marker or a comma.
    InvalidDataUri,
    /// The base64 payload is malformed.
    InvalidBase64,
    /// The JPEG bytes are truncated or structurally invalid; the label says
    /// which invariant failed.
    MalformedJpeg(&'static str),
    /// The PNG bytes are truncated, corrupt, or fail a structural check; the
    /// label says which invariant failed.
    MalformedPng(&'static str),
    /// A structurally sound PNG whose feature set this subset refuses (palette,
    /// interlacing, a bit depth other than 8, an unknown colour type).
    UnsupportedPng,
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
            Self::NotImage(kind) => (
                Code::ConvertUnsupported,
                "image-format",
                format!("unsupported image media type: {kind}"),
            ),
            Self::InvalidDataUri => (
                Code::ConvertMalformed,
                "image-data-uri",
                "expected data:image/jpeg;base64,… or data:image/png;base64,…".to_string(),
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
            Self::MalformedPng(why) => (
                Code::ConvertMalformed,
                "image-png",
                format!("malformed PNG: {why}"),
            ),
            Self::UnsupportedPng => (
                Code::ConvertUnsupported,
                "image-png",
                "unsupported PNG feature (only 8-bit grey/RGB/RGBA, non-interlaced)".to_string(),
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

/// Parse one image source into an embed-ready JPEG or PNG.
///
/// Accepted forms: `data:image/jpeg;base64,<payload>` and
/// `data:image/png;base64,<payload>` (media type is case-insensitive). Nothing
/// is fetched: any other scheme or a bare path is refused by name with
/// [`ImageError::NonDataUri`].
///
/// # Budget
///
/// No budget charge here: the decoder allocates only the decoded payload and
/// the caller charges the resulting buffer's bytes when it writes the XObject.
/// PNG decoding runs under an internal unlimited guard; its allocations are
/// bounded by the IHDR caps in [`parse_png`] before that guard is created.
/// The payload is bounded by the document text that carried it.
///
/// # Malformed Input
///
/// Returns a typed [`ImageError`] for a non-data URI, an unsupported media type,
/// a missing `;base64` marker, invalid base64, a JPEG with no readable SOF
/// marker, or a PNG that is truncated, corrupt, or outside the supported
/// feature subset. No partial image is returned.
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
    let encoded = src.get(comma.saturating_add(1)..).unwrap_or("");
    let bytes = decode_base64(encoded).ok_or(ImageError::InvalidBase64)?;
    match meta {
        "image/jpeg;base64" => {
            let (width, height, color_space) = parse_sof(&bytes)?;
            Ok(EmbeddedImage {
                data: bytes,
                width,
                height,
                color_space,
                filter: ImageFilter::DctDecode,
            })
        }
        "image/png;base64" => parse_png(&bytes),
        _ => {
            let kind = meta
                .strip_suffix(";base64")
                .unwrap_or(meta)
                .split(';')
                .next()
                .unwrap_or(meta)
                .to_string();
            Err(ImageError::NotImage(kind))
        }
    }
}

/// PNG file signature (ISO/IEC 15948 §5.2).
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Hard ceiling on either declared PNG dimension, checked before decoding so a
/// hostile IHDR cannot ask the decoder for an unbounded buffer.
const MAX_PNG_DIM: u32 = 8192;
/// Hard ceiling on declared PNG pixels, for the same reason.
const MAX_PNG_PIXELS: u64 = 8_000_000;

/// Decode a PNG into raw 8-bit samples and Flate-encode them for an XObject.
///
/// Colour types 0 (grey) and 2 (RGB) map straight to `/DeviceGray` and
/// `/DeviceRGB`. Alpha (types 4 and 6) is **flattened over white** rather than
/// refused: the engine decoder yields RGBA8 and [`selis_image::flatten_over_white`]
/// composites it, after which grey/RGB samples are chosen from the source IHDR
/// colour type. Palette (3), interlaced, and non-8-bit PNGs are refused with
/// [`ImageError::UnsupportedPng`]. Decoding is delegated to the engine's
/// [`selis_image::decode_png`]; only the colour-space reduction and re-encode
/// happen here.
///
/// # Budget
///
/// Decoding uses a fresh unlimited guard because the HTML/Markdown parsers do
/// not carry one; the IHDR caps above bound its allocations, and the re-encoded
/// stream bytes are charged later by `layout::lay_out` and the document writer.
///
/// # Malformed Input
///
/// Refuses a bad signature, a missing/short/oversized IHDR, an unsupported
/// colour type or bit depth, interlacing, a corrupt/missing IDAT/IEND, and any
/// PNG the engine decoder rejects. Nothing is returned partially: a failure
/// produces no `EmbeddedImage` and therefore no XObject.
fn parse_png(png: &[u8]) -> Result<EmbeddedImage, ImageError> {
    let (width, height, color_type) = png_ihdr(png)?;
    let mut g = Budget::unlimited().guard();
    let decoded = selis_image::decode_png(png, &mut g).map_err(map_png_error)?;
    let rgb = selis_image::flatten_over_white(&decoded.rgba, &mut g).map_err(map_png_error)?;
    // Grey PNGs stay grey: the engine decoder repeats the sample across
    // R/G/B, so taking one channel per pixel loses nothing.
    let (color_space, raw) = if color_type == 0 || color_type == 4 {
        let mut gray = Vec::with_capacity(rgb.len().saturating_div(3));
        for px in rgb.chunks_exact(3) {
            gray.push(px.first().copied().unwrap_or(0));
        }
        (ImageColorSpace::DeviceGray, gray)
    } else {
        (ImageColorSpace::DeviceRGB, rgb)
    };
    let data = selis_pdf_filter::flate_encode(&raw, 6);
    Ok(EmbeddedImage {
        data,
        width,
        height,
        color_space,
        filter: ImageFilter::FlateDecode,
    })
}

/// Map an engine image-decode failure onto this module's typed refusal.
///
/// # Budget
///
/// No budget charge: this inspects only the error's code.
///
/// # Malformed Input
///
/// No input is parsed here; the code decides whether the PNG was unsupported
/// or malformed.
fn map_png_error(err: selis_error::Error) -> ImageError {
    match err.code() {
        Code::ImageUnsupported => ImageError::UnsupportedPng,
        _ => ImageError::MalformedPng("decode failed"),
    }
}

/// Read and sanity-check the first IHDR, returning `(width, height, colour type)`.
///
/// # Budget
///
/// Allocates nothing.
///
/// # Malformed Input
///
/// Refuses a bad signature, a missing/truncated IHDR, zero or oversized
/// dimensions, a bit depth other than 8, interlacing, and colour types outside
/// 0/2/4/6 (palette and unknown types are explicitly unsupported here).
fn png_ihdr(png: &[u8]) -> Result<(u32, u32, u8), ImageError> {
    if png.get(..PNG_SIGNATURE.len()) != Some(PNG_SIGNATURE.as_slice()) {
        return Err(ImageError::MalformedPng("bad signature"));
    }
    // The signature is 8 bytes; the first chunk header is length(4) + type(4).
    let len = be_u32(png, 8)?;
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    if len < 13 {
        return Err(ImageError::MalformedPng("short IHDR"));
    }
    if png.get(12..16) != Some(b"IHDR".as_slice()) {
        return Err(ImageError::MalformedPng("missing IHDR"));
    }
    let end = 16usize.saturating_add(len);
    let body = png
        .get(16..end)
        .ok_or(ImageError::MalformedPng("IHDR overruns file"))?;
    let width = be_u32(body, 0)?;
    let height = be_u32(body, 4)?;
    let bit_depth = body.get(8).copied().unwrap_or(0);
    let color_type = body.get(9).copied().unwrap_or(255);
    let interlace = body.get(12).copied().unwrap_or(255);
    if width == 0 || height == 0 {
        return Err(ImageError::MalformedPng("empty image"));
    }
    if width > MAX_PNG_DIM
        || height > MAX_PNG_DIM
        || u64::from(width).saturating_mul(u64::from(height)) > MAX_PNG_PIXELS
    {
        return Err(ImageError::MalformedPng("image too large"));
    }
    if bit_depth != 8 || interlace != 0 || !matches!(color_type, 0 | 2 | 4 | 6) {
        return Err(ImageError::UnsupportedPng);
    }
    Ok((width, height, color_type))
}

/// Read one big-endian u32 at `offset`, refusing a truncated buffer.
///
/// # Budget
///
/// Allocates nothing.
///
/// # Malformed Input
///
/// `MalformedPng` when fewer than four bytes remain at `offset`.
fn be_u32(data: &[u8], offset: usize) -> Result<u32, ImageError> {
    let end = offset.saturating_add(4);
    let bytes = data
        .get(offset..end)
        .ok_or(ImageError::MalformedPng("truncated PNG field"))?;
    let array: [u8; 4] = bytes
        .try_into()
        .map_err(|_| ImageError::MalformedPng("truncated PNG field"))?;
    Ok(u32::from_be_bytes(array))
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
                // Padding may occupy the last one or two positions of the
                // final chunk (`XXX=` for a 2-byte remainder, `XX==` for a
                // 1-byte remainder), but never starts before two data bytes.
                // A data byte after padding is refused below.
                if k < 2 {
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
        // One- and two-byte remainders use `XX==` / `XXX=`.
        assert_eq!(decode_base64("YQ==").as_deref(), Some(b"a".as_slice()));
        assert_eq!(decode_base64("YWI=").as_deref(), Some(b"ab".as_slice()));
        assert_eq!(decode_base64("not base64!"), None);
        assert_eq!(decode_base64("aGVsbG8=x"), None);
        assert_eq!(decode_base64("Y===bG8="), None);
    }

    #[test]
    fn rejects_non_image_and_remote_sources_by_name() {
        assert!(matches!(
            parse_data_uri("https://example.com/a.jpg"),
            Err(ImageError::NonDataUri(scheme)) if scheme == "https"
        ));
        assert!(matches!(
            parse_data_uri("/tmp/a.jpg"),
            Err(ImageError::NonDataUri(scheme)) if scheme == "path"
        ));
        assert!(matches!(
            parse_data_uri("data:image/gif;base64,R0lGOD"),
            Err(ImageError::NotImage(kind)) if kind == "image/gif"
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
        assert_eq!(img.data, jpeg);
        assert_eq!((img.width, img.height), (2, 3));
        assert_eq!(img.color_space, ImageColorSpace::DeviceRGB);
        assert_eq!(img.filter, ImageFilter::DctDecode);
    }

    /// Build a legal, unfiltered 8-bit PNG with a single IDAT. `color_type`
    /// is 0/2, `samples` is one scanline of raw bytes *without* filter bytes.
    fn png_fixture(width: u32, height: u32, color_type: u8, row: &[u8]) -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, color_type, 0, 0, 0]);
        let mut raw = Vec::new();
        for _ in 0..height {
            raw.push(0); // filter: None
            raw.extend_from_slice(row);
        }
        let idat = selis_pdf_filter::flate_encode(&raw, 6);
        let mut out = PNG_SIGNATURE.to_vec();
        push_chunk(&mut out, b"IHDR", &ihdr);
        push_chunk(&mut out, b"IDAT", &idat);
        push_chunk(&mut out, b"IEND", &[]);
        out
    }

    /// CRC-32 (IEEE 802.3), used only to build test fixtures.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc_input = Vec::with_capacity(kind.len().saturating_add(body.len()));
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    }

    #[test]
    fn png_rgb_data_uri_becomes_a_flate_image() {
        let png = png_fixture(2, 1, 2, &[0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00]);
        let src = format!("data:image/png;base64,{}", encode_base64(&png));
        let img = parse_data_uri(&src).expect("PNG image");
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.color_space, ImageColorSpace::DeviceRGB);
        assert_eq!(img.filter, ImageFilter::FlateDecode);
        let raw = selis_pdf_filter::flate_decode(&img.data).expect("re-encoded samples");
        assert_eq!(raw, [0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00]);
    }

    #[test]
    fn png_grey_data_uri_stays_device_gray() {
        let png = png_fixture(2, 1, 0, &[0x10, 0xE0]);
        let src = format!("data:image/png;base64,{}", encode_base64(&png));
        let img = parse_data_uri(&src).expect("grey PNG");
        assert_eq!(img.color_space, ImageColorSpace::DeviceGray);
        let raw = selis_pdf_filter::flate_decode(&img.data).expect("samples");
        assert_eq!(raw, [0x10, 0xE0]);
    }

    #[test]
    fn png_truncated_and_refused_features_have_typed_errors() {
        let png = png_fixture(1, 1, 2, &[1, 2, 3]);
        let src = format!("data:image/png;base64,{}", encode_base64(&png));
        // Cut the IEND chunk off: decode_png refuses a PNG with no IEND.
        let cut = png.len().saturating_sub(12);
        let truncated = format!(
            "data:image/png;base64,{}",
            encode_base64(png.get(..cut).unwrap_or(&png))
        );
        assert!(matches!(
            parse_data_uri(&truncated),
            Err(ImageError::MalformedPng(_))
        ));
        assert!(parse_data_uri(&src).is_ok());

        // Palette PNGs are explicitly outside the subset.
        let mut pal = png_fixture(1, 1, 3, &[0]);
        // Fix the IHDR colour type we wrote as RGB into a palette marker.
        if let Some(slot) = pal.get_mut(25) {
            *slot = 3;
        }
        let pal_src = format!("data:image/png;base64,{}", encode_base64(&pal));
        assert!(matches!(
            parse_data_uri(&pal_src),
            Err(ImageError::UnsupportedPng) | Err(ImageError::MalformedPng(_))
        ));
    }
}
