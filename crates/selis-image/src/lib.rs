//! The image engine of the Selis PDF engine (SL-1A.TOOL.08, SL-5.CONV.01/02).
//!
//! Decodes standalone image *files* — PNG and JPEG — into an RGBA8 buffer
//! ready for embedding as a PDF image XObject. Codec work is delegated, never
//! re-implemented: JPEG goes through [`selis_pdf_filter::dct_decode`]
//! (zune-jpeg) and PNG's zlib stream through
//! [`selis_pdf_filter::flate_decode_bounded`] (miniz_oxide), per the layering
//! allowlist (selis-image → selis-pdf-filter).
//!
//! Supported PNG subset: bit depth 8, colour types 0 (grey), 2 (RGB),
//! 3 (palette), 4 (grey+alpha), 6 (RGBA), non-interlaced, CRC-checked chunks.
//! Supported JPEG subset: baseline and progressive Huffman-coded, 8-bit
//! output, grey/RGB/CMYK (the CMYK inversion zune-jpeg reports is applied).
//! Anything else is a typed `IMAGE_UNSUPPORTED`, distinct from corruption
//! (`IMAGE_MALFORMED` / `DCT_CORRUPT` / `FLATE_CORRUPT`).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

use selis_error::{err, Code, Result};
use selis_sandbox::{alloc, BudgetGuard, Resource};

/// The PNG file signature (ISO/IEC 15948 §5.2).
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// A decoded image: 8-bit RGBA samples, row-major, top-down.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// RGBA samples, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Decode a standalone image file by sniffing its magic bytes.
///
/// PNG (`\x89PNG`) and JPEG (`\xFF\xD8`) are recognised; anything else is
/// `IMAGE_UNSUPPORTED`.
///
/// # Budget
///
/// Charges the decoded pixel buffer against `Bytes` and the pixel count
/// against `Pixels`.
///
/// # Malformed Input
///
/// `IMAGE_UNSUPPORTED` for an unknown container; `IMAGE_MALFORMED`,
/// `DCT_CORRUPT`, or `FLATE_CORRUPT` for a recognised-but-broken file.
pub fn decode(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Image> {
    if data.get(..PNG_SIGNATURE.len()) == Some(PNG_SIGNATURE.as_slice()) {
        return decode_png(data, g);
    }
    if data.get(..2) == Some([0xFF, 0xD8].as_slice()) {
        return decode_jpeg(data, g);
    }
    Err(err!(
        Code::ImageUnsupported,
        during = "image-decode",
        detail = "only PNG and JPEG image files are supported"
    ))
}

/// Decode a JPEG file to RGBA8.
///
/// Grey expands to RGB; CMYK converts to RGB honouring the inversion Adobe
/// APP14 transform 0 / Photoshop files use (surfaced by the DCT decoder).
///
/// # Budget
///
/// Charges the decoded pixel buffer against `Bytes` and the pixel count
/// against `Pixels`.
///
/// # Malformed Input
///
/// `DCT_CORRUPT` when the JPEG cannot be decoded; `IMAGE_UNSUPPORTED` for an
/// arithmetic-coded or otherwise-unimplemented frame.
pub fn decode_jpeg(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Image> {
    let dct = selis_pdf_filter::dct_decode(data, g)?;
    dct_to_rgba(&dct, g)
}

/// Convert decoded JPEG-family samples ([`DctImage`]) to RGBA8.
///
/// Grey expands to RGB; CMYK converts to RGB honouring the inversion Adobe
/// APP14 transform 0 / Photoshop files use (surfaced by the DCT decoder).
///
/// # Budget
///
/// Charges the decoded pixel count against `Pixels`.
///
/// # Malformed Input
///
/// `IMAGE_UNSUPPORTED` for an unsupported component count.
pub fn dct_to_rgba(dct: &selis_pdf_filter::DctImage, g: &mut BudgetGuard<'_>) -> Result<Image> {
    let pixels = usize::try_from(dct.width)
        .unwrap_or(usize::MAX)
        .saturating_mul(usize::try_from(dct.height).unwrap_or(usize::MAX));
    let rgba = match dct.channels {
        1 => {
            let mut out = alloc::vec_with_capacity::<u8>(g, pixels.saturating_mul(4))?;
            for &gray in &dct.data {
                out.extend_from_slice(&[gray, gray, gray, 255]);
            }
            out
        }
        3 => {
            let mut out = alloc::vec_with_capacity::<u8>(g, pixels.saturating_mul(4))?;
            for px in dct.data.chunks_exact(3) {
                let r = px.first().copied().unwrap_or(0);
                let g_ch = px.get(1).copied().unwrap_or(0);
                let b = px.get(2).copied().unwrap_or(0);
                out.extend_from_slice(&[r, g_ch, b, 255]);
            }
            out
        }
        4 => {
            let mut out = alloc::vec_with_capacity::<u8>(g, pixels.saturating_mul(4))?;
            for px in dct.data.chunks_exact(4) {
                let rgb = cmyk_to_rgb(
                    px.first().copied().unwrap_or(0),
                    px.get(1).copied().unwrap_or(0),
                    px.get(2).copied().unwrap_or(0),
                    px.get(3).copied().unwrap_or(0),
                    dct.inverted_cmyk,
                );
                out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            out
        }
        other => {
            return Err(err!(
                Code::ImageUnsupported,
                during = "jpeg-to-rgba",
                detail = format!("unsupported JPEG component count {other}")
            ));
        }
    };
    g.charge(Resource::Pixels, u64::try_from(pixels).unwrap_or(u64::MAX))?;
    Ok(Image {
        width: dct.width,
        height: dct.height,
        rgba,
    })
}

/// Convert one CMYK sample (0–255 each) to RGB, undoing the Adobe-style
/// inversion when `inverted` is set.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss // clamped to 0..=255 before each cast
)]
fn cmyk_to_rgb(c: u8, m: u8, y: u8, k: u8, inverted: bool) -> [u8; 3] {
    let (c, m, y, k) = (
        f64::from(c) / 255.0,
        f64::from(m) / 255.0,
        f64::from(y) / 255.0,
        f64::from(k) / 255.0,
    );
    // Inverted storage (Adobe APP14 transform 0) holds 1-x per component.
    let (c, m, y, k) = if inverted {
        (1.0 - c, 1.0 - m, 1.0 - y, 1.0 - k)
    } else {
        (c, m, y, k)
    };
    [
        (255.0 * (1.0 - c) * (1.0 - k)).round().clamp(0.0, 255.0) as u8,
        (255.0 * (1.0 - m) * (1.0 - k)).round().clamp(0.0, 255.0) as u8,
        (255.0 * (1.0 - y) * (1.0 - k)).round().clamp(0.0, 255.0) as u8,
    ]
}

/// The IHDR fields a decoder needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PngHeader {
    width: u32,
    height: u32,
    color_type: u8,
}

impl PngHeader {
    /// Bytes per complete pixel for the supported 8-bit colour types.
    fn bytes_per_pixel(self) -> Result<usize> {
        match self.color_type {
            0 | 3 => Ok(1),
            4 => Ok(2),
            2 => Ok(3),
            6 => Ok(4),
            other => Err(err!(
                Code::ImageUnsupported,
                during = "png-decode",
                detail = format!("unsupported PNG colour type {other}")
            )),
        }
    }
}

/// Decode a PNG file to RGBA8.
///
/// Handles non-interlaced, 8-bit images of colour types 0/2/3/4/6, including
/// `tRNS` transparency and palette (`PLTE`) images. Adam7-interlaced images
/// and other bit depths are `IMAGE_UNSUPPORTED`.
///
/// # Budget
///
/// The inflate is bounded by the exact expected raw size and charged against
/// `Bytes`; the pixel count is charged against `Pixels`.
///
/// # Malformed Input
///
/// `IMAGE_MALFORMED` for structural damage (bad signature, truncated chunks,
/// CRC mismatch, missing IHDR/IEND, bad filter bytes, palette overruns);
/// `IMAGE_UNSUPPORTED` for valid encodings this engine does not implement;
/// `FLATE_CORRUPT` when the IDAT stream is damaged.
pub fn decode_png(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Image> {
    if data.get(..PNG_SIGNATURE.len()) != Some(PNG_SIGNATURE.as_slice()) {
        return Err(err!(
            Code::ImageMalformed,
            during = "png-decode",
            detail = "not a PNG file (bad signature)"
        ));
    }

    let mut pos = PNG_SIGNATURE.len();
    let mut header: Option<PngHeader> = None;
    let mut idat: Vec<u8> = Vec::new();
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut saw_iend = false;

    while pos < data.len() {
        let chunk = data.get(pos..pos.saturating_add(12)).ok_or_else(|| {
            err!(
                Code::ImageMalformed,
                during = "png-decode",
                detail = "truncated PNG chunk header"
            )
        })?;
        let len = usize::try_from(be_u32(chunk, 0)).unwrap_or(usize::MAX);
        let ctype = chunk.get(4..8).unwrap_or(&[]);
        let body_start = pos.saturating_add(8);
        let body_end = body_start
            .checked_add(len)
            .filter(|e| *e <= data.len())
            .ok_or_else(|| {
                err!(
                    Code::ImageMalformed,
                    during = "png-decode",
                    detail = "PNG chunk overruns the file"
                )
            })?;
        let body = data.get(body_start..body_end).unwrap_or(&[]);
        let crc_bytes = data
            .get(body_end..body_end.saturating_add(4))
            .ok_or_else(|| {
                err!(
                    Code::ImageMalformed,
                    during = "png-decode",
                    detail = "truncated PNG chunk CRC"
                )
            })?;
        let declared = be_u32(crc_bytes, 0);
        let mut crc = Crc32::new();
        crc.update(ctype);
        crc.update(body);
        if declared != crc.finish() {
            return Err(err!(
                Code::ImageMalformed,
                during = "png-decode",
                detail = "PNG chunk CRC mismatch"
            ));
        }

        match ctype {
            b"IHDR" => {
                if header.is_some() {
                    return Err(err!(
                        Code::ImageMalformed,
                        during = "png-decode",
                        detail = "duplicate IHDR"
                    ));
                }
                header = Some(parse_ihdr(body)?);
            }
            b"PLTE" => {
                if body.is_empty() || body.len() % 3 != 0 {
                    return Err(err!(
                        Code::ImageMalformed,
                        during = "png-decode",
                        detail = "PLTE length is not a non-zero multiple of 3"
                    ));
                }
                palette = body
                    .chunks_exact(3)
                    .map(|px| {
                        [
                            px.first().copied().unwrap_or(0),
                            px.get(1).copied().unwrap_or(0),
                            px.get(2).copied().unwrap_or(0),
                        ]
                    })
                    .collect();
            }
            b"tRNS" => {
                trns = body.to_vec();
            }
            b"IDAT" => {
                idat.extend_from_slice(body);
            }
            b"IEND" => {
                saw_iend = true;
                break;
            }
            _ => {} // ancillary and uninteresting chunks are skipped
        }
        pos = body_end.saturating_add(4);
    }

    if !saw_iend {
        return Err(err!(
            Code::ImageMalformed,
            during = "png-decode",
            detail = "PNG has no IEND chunk"
        ));
    }
    let header = header.ok_or_else(|| {
        err!(
            Code::ImageMalformed,
            during = "png-decode",
            detail = "PNG has no IHDR chunk"
        )
    })?;
    if header.color_type == 3 && palette.is_empty() {
        return Err(err!(
            Code::ImageMalformed,
            during = "png-decode",
            detail = "palette PNG has no PLTE chunk"
        ));
    }
    let bpp = header.bytes_per_pixel()?;

    let row_bytes = usize::try_from(header.width)
        .unwrap_or(usize::MAX)
        .checked_mul(bpp)
        .ok_or_else(|| {
            err!(
                Code::ImageMalformed,
                during = "png-decode",
                detail = "PNG row width overflows"
            )
        })?;
    let expected = usize::try_from(header.height)
        .unwrap_or(usize::MAX)
        .checked_mul(row_bytes.saturating_add(1))
        .ok_or_else(|| {
            err!(
                Code::ImageMalformed,
                during = "png-decode",
                detail = "PNG image size overflows"
            )
        })?;
    let raw = selis_pdf_filter::flate_decode_bounded(
        &idat,
        u64::try_from(expected).unwrap_or(u64::MAX),
        g,
    )?;
    if raw.len() < expected {
        return Err(err!(
            Code::ImageMalformed,
            during = "png-decode",
            detail = "PNG IDAT decodes short"
        ));
    }

    let unfiltered = unfilter(&raw, header.height, row_bytes, bpp, g)?;
    let rgba = to_rgba(&unfiltered, header, &palette, &trns, g)?;
    let pixels = usize::try_from(header.width)
        .unwrap_or(usize::MAX)
        .saturating_mul(usize::try_from(header.height).unwrap_or(usize::MAX));
    g.charge(Resource::Pixels, u64::try_from(pixels).unwrap_or(u64::MAX))?;
    Ok(Image {
        width: header.width,
        height: header.height,
        rgba,
    })
}

/// Validate and unpack an IHDR body (ISO/IEC 15948 §11.2.2).
fn parse_ihdr(body: &[u8]) -> Result<PngHeader> {
    if body.len() < 13 {
        return Err(err!(
            Code::ImageMalformed,
            during = "png-decode",
            detail = "IHDR too short"
        ));
    }
    let width = be_u32(body, 0);
    let height = be_u32(body, 4);
    let bit_depth = body.get(8).copied().unwrap_or(0);
    let color_type = body.get(9).copied().unwrap_or(255);
    let compression = body.get(10).copied().unwrap_or(255);
    let filter_method = body.get(11).copied().unwrap_or(255);
    let interlace = body.get(12).copied().unwrap_or(255);
    if width == 0 || height == 0 {
        return Err(err!(
            Code::ImageMalformed,
            during = "png-decode",
            detail = "IHDR declares an empty image"
        ));
    }
    if compression != 0 || filter_method != 0 {
        return Err(err!(
            Code::ImageUnsupported,
            during = "png-decode",
            detail = "unknown PNG compression or filter method"
        ));
    }
    if bit_depth != 8 {
        return Err(err!(
            Code::ImageUnsupported,
            during = "png-decode",
            detail = format!("PNG bit depth {bit_depth} is not supported (only 8)")
        ));
    }
    if interlace == 1 {
        return Err(err!(
            Code::ImageUnsupported,
            during = "png-decode",
            detail = "Adam7-interlaced PNG is not supported"
        ));
    }
    if interlace != 0 {
        return Err(err!(
            Code::ImageMalformed,
            during = "png-decode",
            detail = "invalid PNG interlace method"
        ));
    }
    Ok(PngHeader {
        width,
        height,
        color_type,
    })
}

/// Read a big-endian u32 at `offset` within `bytes` (0 when out of range).
fn be_u32(bytes: &[u8], offset: usize) -> u32 {
    let b: [u8; 4] = bytes
        .get(offset..offset.saturating_add(4))
        .and_then(|s| s.try_into().ok())
        .unwrap_or([0, 0, 0, 0]);
    u32::from_be_bytes(b)
}

/// Reverse PNG scanline filtering (ISO/IEC 15948 §9.2).
///
/// The inner loop indexes offsets that the `row_bytes`/`bpp` arithmetic in
/// [`decode_png`] bounds by construction.
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]
fn unfilter(
    raw: &[u8],
    height: u32,
    row_bytes: usize,
    bpp: usize,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let height_us = usize::try_from(height).unwrap_or(usize::MAX);
    let stride = row_bytes + 1;
    let mut out = alloc::vec_filled(g, row_bytes.saturating_mul(height_us), 0u8)?;
    for row in 0..height_us {
        let filter = *raw.get(row * stride).ok_or_else(|| {
            err!(
                Code::ImageMalformed,
                during = "png-unfilter",
                detail = "PNG filter byte missing"
            )
        })?;
        if filter > 4 {
            return Err(err!(
                Code::ImageMalformed,
                during = "png-unfilter",
                detail = format!("unknown PNG filter type {filter}")
            ));
        }
        let src = row * stride + 1;
        let dst = row * row_bytes;
        for i in 0..row_bytes {
            let x = *raw.get(src + i).ok_or_else(|| {
                err!(
                    Code::ImageMalformed,
                    during = "png-unfilter",
                    detail = "PNG scanline truncated"
                )
            })?;
            let a = if i >= bpp { out[dst + i - bpp] } else { 0 };
            let b = if row > 0 { out[dst - row_bytes + i] } else { 0 };
            let c = if row > 0 && i >= bpp {
                out[dst - row_bytes + i - bpp]
            } else {
                0
            };
            let v = match filter {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((u16::from(a) + u16::from(b)) >> 1) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => x,
            };
            out[dst + i] = v;
        }
    }
    Ok(out)
}

/// The Paeth predictor (ISO/IEC 15948 §9.2.6).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects // i32 math on 0..=255 inputs cannot overflow
)]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (i32::from(a), i32::from(b), i32::from(c));
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pick = |v: i32| v.clamp(0, 255) as u8;
    if pa <= pb && pa <= pc {
        pick(a)
    } else if pb <= pc {
        pick(b)
    } else {
        pick(c)
    }
}

/// Convert unfiltered 8-bit samples to RGBA8.
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
fn to_rgba(
    samples: &[u8],
    header: PngHeader,
    palette: &[[u8; 3]],
    trns: &[u8],
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let bpp = header.bytes_per_pixel()?;
    let mut rgba = alloc::vec_with_capacity::<u8>(
        g,
        samples.len().saturating_mul(4).saturating_div(bpp.max(1)),
    )?;
    match header.color_type {
        0 => {
            for &gray in samples {
                let transparent = trns.len() >= 2 && gray == trns[0] && trns[1] == 0;
                rgba.extend_from_slice(&[gray, gray, gray, if transparent { 0 } else { 255 }]);
            }
        }
        2 => {
            for px in samples.chunks_exact(3) {
                let (r, g, b) = (px[0], px[1], px[2]);
                // tRNS holds a 16-bit key per channel; for an 8-bit image the
                // sample compares against the key's high byte.
                let transparent = trns.len() >= 6 && r == trns[0] && g == trns[2] && b == trns[4];
                rgba.extend_from_slice(&[r, g, b, if transparent { 0 } else { 255 }]);
            }
        }
        3 => {
            for &idx in samples {
                let entry = palette.get(usize::from(idx)).ok_or_else(|| {
                    err!(
                        Code::ImageMalformed,
                        during = "png-to-rgba",
                        detail = "palette index out of range"
                    )
                })?;
                let alpha = trns.get(usize::from(idx)).copied().unwrap_or(255);
                rgba.extend_from_slice(&[entry[0], entry[1], entry[2], alpha]);
            }
        }
        4 => {
            for px in samples.chunks_exact(2) {
                rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
        }
        6 => {
            rgba.extend_from_slice(samples);
        }
        _ => {
            return Err(err!(
                Code::ImageUnsupported,
                during = "png-to-rgba",
                detail = "unsupported PNG colour type"
            ));
        }
    }
    Ok(rgba)
}

/// A CRC-32 (IEEE 802.3) accumulator, table-driven.
#[derive(Debug)]
struct Crc32 {
    state: u32,
}

impl Crc32 {
    const fn new() -> Self {
        Self { state: u32::MAX }
    }

    #[allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )]
    fn update(&mut self, bytes: &[u8]) {
        static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
        let table = TABLE.get_or_init(|| {
            let mut t = [0u32; 256];
            for (i, entry) in t.iter_mut().enumerate() {
                let mut c = i as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
                *entry = c;
            }
            t
        });
        for &b in bytes {
            let idx = ((self.state ^ u32::from(b)) & 0xFF) as usize;
            self.state = table[idx] ^ (self.state >> 8);
        }
    }

    const fn finish(&self) -> u32 {
        self.state ^ u32::MAX
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn write_chunk(out: &mut Vec<u8>, ctype: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(ctype);
        out.extend_from_slice(body);
        let mut c = Crc32::new();
        c.update(ctype);
        c.update(body);
        out.extend_from_slice(&c.finish().to_be_bytes());
    }

    /// A zlib wrapper around stored (uncompressed) deflate blocks. The header
    /// bytes are caller-supplied so tests can use any valid RFC 1950 header.
    fn deflate_stored_with_header(data: &[u8], header: [u8; 2]) -> Vec<u8> {
        let mut out = header.to_vec();
        let mut rest = data;
        if rest.is_empty() {
            out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
        }
        while !rest.is_empty() {
            let take = rest.len().min(0xFFFF);
            let block = &rest[..take];
            rest = &rest[take..];
            let len = block.len() as u16;
            out.push(u8::from(rest.is_empty())); // BFINAL on the last block
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(block);
        }
        out.extend_from_slice(&1u32.to_be_bytes()); // Adler-32 placeholder
        out
    }

    fn deflate_stored(data: &[u8]) -> Vec<u8> {
        deflate_stored_with_header(data, [0x78, 0x01])
    }

    /// Build a valid non-interlaced, bit-depth-8 PNG from raw samples.
    fn build_png(
        color_type: u8,
        width: u32,
        height: u32,
        samples: &[u8],
        palette: Option<&[u8]>,
    ) -> Vec<u8> {
        let bpp = match color_type {
            0 | 3 => 1usize,
            4 => 2,
            2 => 3,
            _ => 4,
        };
        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, color_type, 0, 0, 0]);
        write_chunk(&mut out, b"IHDR", &ihdr);
        if let Some(p) = palette {
            write_chunk(&mut out, b"PLTE", p);
        }
        let row = width as usize * bpp;
        let mut raw = Vec::new();
        for r in 0..height as usize {
            raw.push(0u8);
            raw.extend_from_slice(&samples[r * row..(r + 1) * row]);
        }
        write_chunk(&mut out, b"IDAT", &deflate_stored(&raw));
        write_chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn decodes_a_2x2_rgb_png() {
        let samples = [
            255, 0, 0, 0, 255, 0, // row 0: red, green
            0, 0, 255, 255, 255, 255, // row 1: blue, white
        ];
        let png = build_png(2, 2, 2, &samples, None);
        let img = decode_png(&png, &mut guard()).expect("decode");
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);
        assert_eq!(
            img.rgba,
            [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,]
        );
    }

    #[test]
    fn decodes_a_palette_png_with_alpha() {
        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
        write_chunk(&mut out, b"IHDR", &ihdr);
        write_chunk(&mut out, b"PLTE", &[255, 0, 0, 0, 0, 255]);
        write_chunk(&mut out, b"tRNS", &[255, 128]);
        write_chunk(&mut out, b"IDAT", &deflate_stored(&[0u8, 0, 1]));
        write_chunk(&mut out, b"IEND", &[]);
        let img = decode_png(&out, &mut guard()).expect("decode");
        assert_eq!(img.rgba, [255, 0, 0, 255, 0, 0, 255, 128]);
    }

    #[test]
    fn decodes_grey_and_grey_alpha_pngs() {
        let grey = build_png(0, 2, 1, &[10, 200], None);
        let img = decode_png(&grey, &mut guard()).expect("decode");
        assert_eq!(img.rgba, [10, 10, 10, 255, 200, 200, 200, 255]);

        let ga = build_png(4, 1, 2, &[5, 255, 9, 0], None);
        let img = decode_png(&ga, &mut guard()).expect("decode");
        assert_eq!(img.rgba, [5, 5, 5, 255, 9, 9, 9, 0]);
    }

    #[test]
    fn decodes_a_png_with_a_non_standard_zlib_header() {
        // Regression for smoke.png: its IDAT was zlib-wrapped with the valid
        // but uncommon `68 43` header (16 KiB window), and ancillary chunks
        // preceded the IDAT. The flate layer must treat that as a zlib header,
        // not as raw deflate.
        let samples = [
            255, 0, 0, 0, 255, 0, // row 0: red, green
            0, 0, 255, 255, 255, 255, // row 1: blue, white
        ];
        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        write_chunk(&mut out, b"IHDR", &ihdr);
        write_chunk(&mut out, b"sRGB", &[0]);
        let mut raw = Vec::new();
        for row in samples.chunks_exact(6) {
            raw.push(0u8);
            raw.extend_from_slice(row);
        }
        write_chunk(
            &mut out,
            b"IDAT",
            &deflate_stored_with_header(&raw, [0x68, 0x43]),
        );
        write_chunk(&mut out, b"IEND", &[]);
        let img = decode_png(&out, &mut guard()).expect("decode");
        assert_eq!(
            img.rgba,
            [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,]
        );
    }

    #[test]
    fn unfilter_handles_sub_and_up() {
        // Colour type 0 (bpp 1), 3x2: row 0 filter None, row 1 filter Sub.
        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&3u32.to_be_bytes());
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
        write_chunk(&mut out, b"IHDR", &ihdr);
        write_chunk(
            &mut out,
            b"IDAT",
            &deflate_stored(&[0, 1, 2, 3, 1, 4, 5, 6]),
        );
        write_chunk(&mut out, b"IEND", &[]);
        let img = decode_png(&out, &mut guard()).expect("decode");
        assert_eq!(
            img.rgba,
            [
                1, 1, 1, 255, 2, 2, 2, 255, 3, 3, 3, 255, 4, 4, 4, 255, 9, 9, 9, 255, 15, 15, 15,
                255,
            ]
        );
    }

    #[test]
    fn rejects_a_bad_crc() {
        let mut png = build_png(2, 1, 1, &[255, 0, 0], None);
        let last = png.len() - 1;
        png[last] ^= 0xFF; // corrupt the IEND CRC
        let err = decode_png(&png, &mut guard()).expect_err("bad crc");
        assert_eq!(err.code(), Code::ImageMalformed);
    }

    /// Rewrite the IHDR CRC after mutating the body (the builder cannot know).
    fn fix_ihdr_crc(png: &mut [u8]) {
        let mut c = Crc32::new();
        c.update(&png[12..16]); // "IHDR"
        c.update(&png[16..29]); // 13-byte body
        png[29..33].copy_from_slice(&c.finish().to_be_bytes());
    }

    #[test]
    fn rejects_interlaced_and_deep_pngs() {
        let mut png = build_png(2, 1, 1, &[255, 0, 0], None);
        png[28] = 1; // interlace = Adam7
        fix_ihdr_crc(&mut png);
        let err = decode_png(&png, &mut guard()).expect_err("interlaced");
        assert_eq!(err.code(), Code::ImageUnsupported);

        let mut png = build_png(2, 1, 1, &[255, 0, 0], None);
        png[24] = 16; // bit depth 16
        fix_ihdr_crc(&mut png);
        let err = decode_png(&png, &mut guard()).expect_err("16-bit");
        assert_eq!(err.code(), Code::ImageUnsupported);
    }

    #[test]
    fn decode_sniffs_the_container() {
        let err = decode(b"GIF89a...", &mut guard()).expect_err("gif");
        assert_eq!(err.code(), Code::ImageUnsupported);
        let err = decode(&[0xFF, 0xD8, 0x00], &mut guard()).expect_err("corrupt jpeg");
        assert_eq!(err.code(), Code::DctCorrupt);
        let err = decode(b"\x89PNG\r\n\x1a\n", &mut guard()).expect_err("truncated png");
        assert_eq!(err.code(), Code::ImageMalformed);
    }

    #[test]
    fn cmyk_conversion_honours_inversion() {
        // Pure white plate: no ink, no key.
        assert_eq!(cmyk_to_rgb(0, 0, 0, 0, false), [255, 255, 255]);
        // Full key: everything black.
        assert_eq!(cmyk_to_rgb(0, 0, 0, 255, false), [0, 0, 0]);
        // Inverted full-white plate stores 255,255,255,255.
        assert_eq!(cmyk_to_rgb(255, 255, 255, 255, true), [255, 255, 255]);
        // Inverted black stores zeros.
        assert_eq!(cmyk_to_rgb(0, 0, 0, 0, true), [0, 0, 0]);
    }
}
