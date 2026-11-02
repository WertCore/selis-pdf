//! Baseline JPEG writer — the encode side of `DCTDecode`
//! (35-PARITY-CHECKLIST.md, Phase 2 "PDF → JPG").
//!
//! [`crate::dct_decode`] reads JPEG through `zune-jpeg`, and that is the right
//! call: decoding an untrusted stream is a parser's job, and a parser has to be
//! lenient about everything a file can get wrong. Writing a JPEG is the opposite
//! problem — the bytes are ours, the tables are the standard's own, and nothing
//! needs to be lenient — so the writer is owned. It emits baseline sequential
//! 8-bit YCbCr 4:4:4 with the ITU-T T.81 Annex K quantiser and Huffman tables
//! and no third-party code in the path.
//!
//! Every arithmetic step is integer on purpose. The raster chunk runs in a
//! browser (wasm32) and the same command runs natively in the CLI, and the
//! determinism gate wants one pixmap to produce one byte string on both. A float
//! DCT would put `cos` transcendentals and compiler-side FP contraction on that
//! path; a Q15 fixed-point cosine table cannot disagree with itself across
//! targets.

use selis_error::{err, Code, Result};
use selis_sandbox::{BudgetGuard, Resource};

/// The largest coefficient the Annex K AC tables can name: ten bits.
///
/// Quantised coefficients are saturated to ±`MAX_COEFFICIENT`. The worst case
/// the format allows — an 8×8 full-contrast checkerboard encoded at quality
/// 100, where every quantiser step is 1 — would otherwise produce an
/// eleven-bit coefficient, and symbol `0x0B` is not in the tables this file
/// declares. Saturating keeps the stream inside its own header; the cost is at
/// most one quantiser step on a pixmap no rendered page approaches.
const MAX_COEFFICIENT: i64 = 1023;

/// Marker: start of image.
const SOI: u8 = 0xD8;
/// Marker: application-data segment 0, the JFIF header.
const APP0: u8 = 0xE0;
/// Marker: start of frame, baseline DCT, sequential Huffman coding.
const SOF0: u8 = 0xC0;
/// Marker: define quantisation table.
const DQT: u8 = 0xDB;
/// Marker: define Huffman table.
const DHT: u8 = 0xC4;
/// Marker: start of scan.
const SOS: u8 = 0xDA;
/// Marker: end of image.
const EOI: u8 = 0xD9;

/// Annex K.1 luminance quantiser, in zigzag order — the order the `DQT` segment
/// carries it and the order the entropy coder walks it.
#[rustfmt::skip]
const STD_LUMA_QUANT: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61,
    12, 12, 14, 19, 26, 58, 60, 55,
    14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62,
    18, 22, 37, 56, 68, 109, 103, 77,
    24, 35, 55, 64, 81, 104, 113, 92,
    49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103, 99,
];

/// Annex K.2 chrominance quantiser, in zigzag order.
#[rustfmt::skip]
const STD_CHROMA_QUANT: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99,
    18, 21, 26, 66, 99, 99, 99, 99,
    24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
];

/// Annex K.3 luminance DC table: `BITS[L]` symbols of length `L + 1`.
const LUMA_DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
/// Annex K.3 luminance DC symbols: the eleven difference categories.
const LUMA_DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
/// Annex K.4 luminance AC counts.
const LUMA_AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125];
/// Annex K.4 luminance AC symbols: every run/size pair, EOB (`0x00`) fourth.
#[rustfmt::skip]
const LUMA_AC_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// Annex K.5 chrominance DC counts.
const CHROMA_DC_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
/// Annex K.5 chrominance DC symbols.
const CHROMA_DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
/// Annex K.6 chrominance AC counts.
const CHROMA_AC_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119];
/// Annex K.6 chrominance AC symbols: EOB (`0x00`) first, and a different
/// ordering from the luminance table — the two are not the same list.
#[rustfmt::skip]
const CHROMA_AC_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// Zigzag scan order: entry `i` is the row-major index of the `i`th coefficient
/// in scan order, so entry 0 is the DC term and entry 1 is `(0, 1)`.
#[rustfmt::skip]
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10,
    17, 24, 32, 25, 18, 11, 4, 5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// The DCT-II kernel in Q15: `round(2^15 * C(u) * cos((2x+1)*u*pi/16))`.
///
/// The standard's `C(0) = 1/sqrt(2)` normalisation is folded into row 0 so the
/// separable passes below need no separate DC correction; the outer `1/4` is
/// applied when quantising.
#[rustfmt::skip]
const COS_Q15: [[i32; 8]; 8] = [
    [ 23170,  23170,  23170,  23170,  23170,  23170,  23170,  23170],
    [ 32138,  27246,  18205,   6393,  -6393, -18205, -27246, -32138],
    [ 30274,  12540, -12540, -30274, -30274, -12540,  12540,  30274],
    [ 27246,  -6393, -32138, -18205,  18205,  32138,   6393, -27246],
    [ 23170, -23170, -23170,  23170,  23170, -23170, -23170,  23170],
    [ 18205, -32138,   6393,  27246, -27246,  -6393,  32138, -18205],
    [ 12540, -30274,  30274, -12540, -12540,  30274, -30274,  12540],
    [  6393, -18205,  27246, -32138,  32138, -27246,  18205,  -6393],
];

/// Two Q15 passes put a product at `2^30`, and the standard's outer `1/4` makes
/// the quantisation divisor `2^32 * step`.
const COEFF_SCALE: i64 = 1 << 32;

/// Encode an 8-bit RGB image as a baseline JPEG.
///
/// `rgb` is row-major, three bytes per pixel, exactly `width * height * 3`.
/// `quality` is the usual 1..=100 knob and scales the Annex K quantiser the way
/// the reference does: below 50 the table is stretched, above it is squeezed, so
/// 50 is the unmodified standard table and 100 is the finest the tables allow.
///
/// Sampling is 4:4:4 — no chroma subsampling. A rendered page is mostly text and
/// thin rules, and halving chroma resolution is what makes a JPEG of a document
/// look worse than a JPEG of a photograph at the same size.
///
/// # Budget
///
/// Charges the pixel buffer it reads and the scan bytes it writes against
/// `Bytes`, so an enormous page cannot be encoded past the caller's ceiling.
///
/// # Malformed Input
///
/// `IMAGE_MALFORMED` when either dimension is zero, `quality` is outside
/// 1..=100, or `rgb` is not exactly `width * height * 3` bytes.
pub fn dct_encode(
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: u8,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(err!(
            Code::ImageMalformed,
            during = "dct-encode",
            detail = "zero-sized image"
        ));
    }
    if quality > 100 {
        return Err(err!(
            Code::ImageMalformed,
            during = "dct-encode",
            detail = "quality must be between 1 and 100"
        ));
    }
    // The frame header carries 16-bit dimensions, so a page rendered at a DPI
    // that produces more than 65535 pixels in one axis cannot be written as
    // baseline JPEG. Refusing beats the silent clamp the header would allow.
    if width > 65535 || height > 65535 {
        return Err(err!(
            Code::ImageMalformed,
            during = "dct-encode",
            detail = "image edge exceeds the 65535-pixel JPEG frame limit"
        ));
    }
    let px = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().map(|h| w.saturating_mul(h)))
        .ok_or_else(|| {
            err!(
                Code::ImageMalformed,
                during = "dct-encode",
                detail = "image dimensions overflow"
            )
        })?;
    let expected = px.saturating_mul(3);
    if rgb.len() != expected {
        return Err(err!(
            Code::ImageMalformed,
            during = "dct-encode",
            detail = "pixel buffer length does not match the dimensions"
        ));
    }
    g.charge(Resource::Bytes, u64::try_from(expected).unwrap_or(u64::MAX))?;

    // Quality 0 would divide by itself in the table scaler; the standard
    // reference treats it as 1.
    let level = quality.max(1);
    let luma_table = scaled_table(&STD_LUMA_QUANT, level);
    let chroma_table = scaled_table(&STD_CHROMA_QUANT, level);
    let luma_dc = build_huff(&LUMA_DC_BITS, &LUMA_DC_VALS);
    let luma_ac = build_huff(&LUMA_AC_BITS, &LUMA_AC_VALS);
    let chroma_dc = build_huff(&CHROMA_DC_BITS, &CHROMA_DC_VALS);
    let chroma_ac = build_huff(&CHROMA_AC_BITS, &CHROMA_AC_VALS);

    let mut out: Vec<u8> = Vec::with_capacity(expected.saturating_div(4).saturating_add(1024));
    out.extend_from_slice(&[0xFF, SOI]);
    push_segment(&mut out, APP0, &jfif_payload());
    push_quant_tables(&mut out, &luma_table, &chroma_table);
    push_frame(&mut out, width, height);
    push_huff_tables(&mut out);
    push_scan_header(&mut out);

    let blocks_w = px_width(width).saturating_add(7).saturating_div(8);
    let blocks_h = px_height(height).saturating_add(7).saturating_div(8);
    let mut scan: Vec<u8> = Vec::with_capacity(expected.saturating_div(4));
    let mut writer = BitWriter {
        out: &mut scan,
        acc: 0,
        acc_bits: 0,
    };
    let mut block = Block::default();
    let mut prev_dc = [0i32; 3];
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            // 4:4:4 with 1x1 sampling factors: one MCU is one block of each
            // component, emitted in component order.
            for (component, previous_dc) in prev_dc.iter_mut().enumerate() {
                block.samples(
                    rgb,
                    px_width(width),
                    px_height(height),
                    bx.saturating_mul(8),
                    by.saturating_mul(8),
                    component,
                );
                block.forward();
                let (table, dc, ac) = if component == 0 {
                    (&luma_table, &luma_dc, &luma_ac)
                } else {
                    (&chroma_table, &chroma_dc, &chroma_ac)
                };
                block.quantize(table);
                encode_block(&mut writer, dc, ac, &block.coeff, previous_dc)?;
            }
        }
    }
    writer.flush();
    // A stuffed final byte is legal and expected: the marker that must not
    // appear inside the scan is 0xFF, and EOI is the first thing after it.
    if scan.last() == Some(&0xFF) {
        scan.push(0x00);
    }
    g.charge(
        Resource::Bytes,
        u64::try_from(scan.len()).unwrap_or(u64::MAX),
    )?;
    out.extend_from_slice(&scan);
    out.extend_from_slice(&[0xFF, EOI]);
    Ok(out)
}

/// One 8x8 block: level-shifted samples, then the transform, then the
/// zigzag-ordered coefficients.
#[derive(Clone, Copy)]
struct Block {
    /// Row-major samples, level-shifted by -128, in `data[row][col]`.
    data: [[i64; 8]; 8],
    /// Quantised coefficients in zigzag order.
    coeff: [i32; 64],
}

impl Default for Block {
    fn default() -> Self {
        Self {
            data: [[0i64; 8]; 8],
            coeff: [0i32; 64],
        }
    }
}

impl Block {
    /// Gather the window whose top-left sample is `origin_x`/`origin_y` for
    /// `component` (0 = Y, 1 = Cb, 2 = Cr), replicating the last row and column
    /// at the right and bottom edges as the standard requires.
    ///
    /// The colour transform is the integer BT.601 form: `+128` before each shift
    /// is the rounding term. The -128 level shift of Y cancels against nothing;
    /// the +128 bias of Cb and Cr is exactly the level shift, so chroma needs no
    /// subtraction.
    fn samples(
        &mut self,
        rgb: &[u8],
        width: usize,
        height: usize,
        origin_x: usize,
        origin_y: usize,
        component: usize,
    ) {
        for row in 0..8usize {
            let sy = origin_y.saturating_add(row).min(height.saturating_sub(1));
            let mut line = [0i64; 8];
            for (col, slot) in line.iter_mut().enumerate().take(8) {
                let sx = origin_x.saturating_add(col).min(width.saturating_sub(1));
                let offset = sy
                    .saturating_mul(width)
                    .saturating_add(sx)
                    .saturating_mul(3);
                let r = i64::from(rgb.get(offset).copied().unwrap_or(0));
                let g = i64::from(rgb.get(offset.saturating_add(1)).copied().unwrap_or(0));
                let b = i64::from(rgb.get(offset.saturating_add(2)).copied().unwrap_or(0));
                *slot = match component {
                    0 => r
                        .saturating_mul(77)
                        .saturating_add(g.saturating_mul(150))
                        .saturating_add(b.saturating_mul(29))
                        .saturating_add(128)
                        .div_euclid(256)
                        .saturating_sub(128),
                    1 => r
                        .saturating_mul(-43)
                        .saturating_add(g.saturating_mul(-85))
                        .saturating_add(b.saturating_mul(128))
                        .saturating_add(128)
                        .div_euclid(256),
                    _ => r
                        .saturating_mul(128)
                        .saturating_add(g.saturating_mul(-107))
                        .saturating_add(b.saturating_mul(-21))
                        .saturating_add(128)
                        .div_euclid(256),
                };
            }
            if let Some(target) = self.data.get_mut(row) {
                *target = line;
            }
        }
    }

    /// The separable forward DCT: rows first, then columns, each pass a
    /// multiply-accumulate against [`COS_Q15`] at Q15 scale.
    fn forward(&mut self) {
        for row in 0..8usize {
            let Some(input) = self.data.get(row).copied() else {
                continue;
            };
            let mut line = [0i64; 8];
            for (u, slot) in line.iter_mut().enumerate().take(8) {
                let Some(kernel) = COS_Q15.get(u).copied() else {
                    continue;
                };
                let mut acc = 0i64;
                for (x, sample) in input.iter().enumerate().take(8) {
                    acc = acc.saturating_add(
                        sample.saturating_mul(i64::from(kernel.get(x).copied().unwrap_or(0))),
                    );
                }
                *slot = acc;
            }
            if let Some(target) = self.data.get_mut(row) {
                *target = line;
            }
        }
        for col in 0..8usize {
            let mut input = [0i64; 8];
            for (y, slot) in input.iter_mut().enumerate().take(8) {
                *slot = self
                    .data
                    .get(y)
                    .and_then(|line| line.get(col))
                    .copied()
                    .unwrap_or(0);
            }
            let mut output = [0i64; 8];
            for (v, slot) in output.iter_mut().enumerate().take(8) {
                let Some(kernel) = COS_Q15.get(v).copied() else {
                    continue;
                };
                let mut acc = 0i64;
                for (y, sample) in input.iter().enumerate().take(8) {
                    acc = acc.saturating_add(
                        sample.saturating_mul(i64::from(kernel.get(y).copied().unwrap_or(0))),
                    );
                }
                *slot = acc;
            }
            for (v, value) in output.iter().enumerate().take(8) {
                if let Some(target) = self.data.get_mut(v) {
                    if let Some(cell) = target.get_mut(col) {
                        *cell = *value;
                    }
                }
            }
        }
    }

    /// Divide each coefficient by its quantiser step, in zigzag order, rounding
    /// to nearest and saturating into the range the AC tables can name.
    fn quantize(&mut self, table: &[u8; 64]) {
        for (index, slot) in self.coeff.iter_mut().enumerate().take(64) {
            let Some(&position) = ZIGZAG.get(index) else {
                continue;
            };
            let row = position.div_euclid(8);
            let col = position.rem_euclid(8);
            let raw = self
                .data
                .get(row)
                .and_then(|line| line.get(col))
                .copied()
                .unwrap_or(0);
            let step = i64::from(table.get(index).copied().unwrap_or(1)).max(1);
            let divisor = step.saturating_mul(COEFF_SCALE);
            let value = round_div(raw, divisor).clamp(-MAX_COEFFICIENT, MAX_COEFFICIENT);
            *slot = i32::try_from(value).unwrap_or(0);
        }
    }
}

/// Round `n / d` to nearest for a signed numerator and a positive divisor,
/// without a division operator (`d` is never zero here).
#[must_use]
fn round_div(n: i64, d: i64) -> i64 {
    let quotient = n.div_euclid(d);
    let remainder = n.rem_euclid(d);
    if remainder.saturating_mul(2) >= d {
        quotient.saturating_add(1)
    } else {
        quotient
    }
}

/// The DC difference and the AC run/size symbols of one quantised block.
fn encode_block(
    writer: &mut BitWriter<'_>,
    dc: &HuffTable,
    ac: &HuffTable,
    coeff: &[i32; 64],
    prev_dc: &mut i32,
) -> Result<()> {
    let value = coeff.first().copied().unwrap_or(0);
    let diff = value.wrapping_sub(*prev_dc);
    *prev_dc = value;
    let category = bit_length(diff);
    write_symbol(writer, dc, category, 0)?;
    write_magnitude(writer, diff, category);

    let mut run = 0u8;
    for index in 1..64usize {
        let value = coeff.get(index).copied().unwrap_or(0);
        if value == 0 {
            run = run.saturating_add(1);
            continue;
        }
        while run >= 16 {
            // 0xF0 says "sixteen zeros and more coefficients follow" and carries
            // no magnitude bits: the run length is entirely in the symbol.
            write_symbol(writer, ac, 0xF0, 0)?;
            run = run.saturating_sub(16);
        }
        let size = bit_length(value);
        let symbol = run.checked_shl(4).unwrap_or(u8::MAX).saturating_add(size);
        write_symbol(writer, ac, symbol, size)?;
        write_magnitude(writer, value, size);
        run = 0;
    }
    if run > 0 {
        // Trailing zeros collapse into a single end-of-block.
        write_symbol(writer, ac, 0, 0)?;
    }
    Ok(())
}

/// Write one Huffman symbol.
///
/// `size` is the magnitude-bit count, carried only to name the offending
/// coefficient in the error. A symbol missing from its table would make
/// `write_code` emit nothing and desynchronise the stream, so the file would
/// decode into noise instead of failing: refuse to produce it instead. The
/// saturation in [`Block::quantize`] bounds AC size at ten bits and the DC
/// difference at eleven, and
/// `the_tables_cover_every_symbol_this_writer_can_emit` proves the Annex K
/// tables name that whole range, so this arm is a guard on the writer's own
/// invariant rather than an expected outcome.
fn write_symbol(writer: &mut BitWriter<'_>, table: &HuffTable, symbol: u8, size: u8) -> Result<()> {
    let Some(code) = table.lookup(symbol) else {
        return Err(err!(
            Code::ImageMalformed,
            during = "dct-encode",
            detail = format!("no Huffman code for symbol 0x{symbol:02X} (size {size})")
        ));
    };
    writer.write_code(code);
    Ok(())
}

/// Emit the `size` low bits of a signed coefficient the way Annex F asks: a
/// negative value is written as its own value plus `2^size - 1`, which lands on
/// the one's-complement bit pattern the decoder expects.
fn write_magnitude(writer: &mut BitWriter<'_>, value: i32, size: u8) {
    if size == 0 {
        return;
    }
    let span = 1i64
        .checked_shl(u32::from(size))
        .unwrap_or(i64::MAX)
        .saturating_sub(1);
    let raw = if value < 0 {
        i64::from(value).saturating_add(span)
    } else {
        i64::from(value)
    };
    writer.write_bits(u32::try_from(raw.max(0)).unwrap_or(0), u32::from(size));
}

/// The number of bits needed to write `value` as a JPEG difference.
///
/// Saturation in [`Block::quantize`] keeps every coefficient inside ten bits, so
/// a DC difference is inside eleven and an AC size inside ten — which is exactly
/// the range the Annex K tables declare. No symbol this writer emits can miss
/// its table.
#[must_use]
fn bit_length(value: i32) -> u8 {
    let magnitude = i64::from(value).wrapping_abs();
    let mut rest = u32::try_from(magnitude).unwrap_or(u32::MAX);
    let mut bits = 0u8;
    while rest > 0 {
        rest >>= 1;
        bits = bits.saturating_add(1);
    }
    bits
}

/// A Huffman codeword: `len` 0 means the symbol is not in the table.
#[derive(Default, Clone, Copy)]
struct HuffCode {
    bits: u32,
    len: u8,
}

/// Symbol -> codeword, sized by symbol value so a lookup is one index.
struct HuffTable {
    entries: Vec<HuffCode>,
}

impl HuffTable {
    /// The code for `symbol`, or `None` when the table does not carry it.
    ///
    /// An absent symbol keeps the `HuffCode` that [`build_huff`] leaves behind —
    /// `len` 0 — and a zero-length code writes no bits, which is how a silent
    /// miss turns into an unreadable file. The miss has to travel up.
    fn lookup(&self, symbol: u8) -> Option<HuffCode> {
        self.entries
            .get(usize::from(symbol))
            .copied()
            .filter(|code| code.len > 0)
    }
}

/// Build the canonical codewords for a `BITS`/`VALS` pair, the way Annex F
/// derives them: one code per symbol, ascending, left-shifted at each level.
#[must_use]
fn build_huff(bits: &[u8; 16], vals: &[u8]) -> HuffTable {
    let mut entries = vec![HuffCode::default(); 256];
    let mut code: u32 = 0;
    let mut position = 0usize;
    for level in 1..=16u8 {
        let count = usize::from(
            bits.get(usize::from(level).saturating_sub(1))
                .copied()
                .unwrap_or(0),
        );
        for _ in 0..count {
            let Some(&symbol) = vals.get(position) else {
                break;
            };
            if let Some(slot) = entries.get_mut(usize::from(symbol)) {
                *slot = HuffCode {
                    bits: code,
                    len: level,
                };
            }
            position = position.saturating_add(1);
            code = code.saturating_add(1);
        }
        code = code.wrapping_shl(1);
    }
    HuffTable { entries }
}

/// The MSB-first bit sink, with JPEG's byte stuffing: a written `0xFF` is
/// followed by a `0x00` so no marker can be mistaken inside the scan.
struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u32,
    acc_bits: u32,
}

impl BitWriter<'_> {
    fn write_bits(&mut self, value: u32, len: u32) {
        let len = len.min(24);
        if len == 0 {
            return;
        }
        // Only the live bits of the accumulator are meaningful; clearing the
        // rest keeps a stale high bit from ever reaching a byte.
        self.acc &= low_mask(self.acc_bits);
        self.acc = self
            .acc
            .wrapping_shl(len)
            .saturating_add(value & low_mask(len));
        self.acc_bits = self.acc_bits.saturating_add(len);
        while self.acc_bits >= 8 {
            self.acc_bits = self.acc_bits.saturating_sub(8);
            let byte = self.acc.wrapping_shr(self.acc_bits) & low_mask(8);
            self.emit(u8::try_from(byte).unwrap_or(0));
        }
    }

    fn write_code(&mut self, code: HuffCode) {
        self.write_bits(code.bits, u32::from(code.len));
    }

    fn emit(&mut self, byte: u8) {
        self.out.push(byte);
        if byte == 0xFF {
            self.out.push(0x00);
        }
    }

    /// Pad the final partial byte with 1 bits, as the standard directs.
    fn flush(&mut self) {
        if self.acc_bits > 0 {
            let pad = 8u32.saturating_sub(self.acc_bits);
            self.write_bits(u32::MAX, pad);
        }
    }
}

/// The value with its low `bits` bits set.
#[must_use]
fn low_mask(bits: u32) -> u32 {
    match 1u32.checked_shl(bits) {
        Some(one) => one.wrapping_sub(1),
        None => u32::MAX,
    }
}

/// The Annex K tables scaled by `quality` with the reference formula: below 50
/// the step grows, above 50 it shrinks, and every step stays in 1..=255.
#[must_use]
fn scaled_table(standard: &[u8; 64], quality: u8) -> [u8; 64] {
    let q = i64::from(quality);
    let scale = if quality < 50 {
        5000_i64.saturating_div(q.max(1))
    } else {
        200_i64.saturating_sub(q.saturating_mul(2))
    };
    let mut table = [1u8; 64];
    for (index, slot) in table.iter_mut().enumerate().take(64) {
        let step = i64::from(standard.get(index).copied().unwrap_or(1));
        let scaled = step
            .saturating_mul(scale)
            .saturating_add(50)
            .saturating_div(100);
        *slot = u8::try_from(scaled.max(1)).unwrap_or(u8::MAX);
    }
    table
}

/// Append `0xFF marker`, the 16-bit segment length (counting itself), payload.
fn push_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.push(0xFF);
    out.push(marker);
    let len = u16::try_from(payload.len().saturating_add(2)).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
}

/// A JFIF header that says nothing about print size: units 0 (aspect ratio
/// unknown), density 1x1, no thumbnail. A page rendered at 300 DPI is an image
/// of 300 dots per inch of *paper*, and the raster chunk does not guess the
/// paper — so the file does not claim a number it cannot support.
#[must_use]
fn jfif_payload() -> Vec<u8> {
    let mut payload = Vec::with_capacity(16);
    payload.extend_from_slice(b"JFIF\0");
    payload.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
    payload
}

/// One `DQT` segment carrying both tables: 0 for luminance, 1 for chrominance,
/// 8-bit precision (the `Pq` nibble is 0 in both).
fn push_quant_tables(out: &mut Vec<u8>, luma: &[u8; 64], chroma: &[u8; 64]) {
    let mut payload = Vec::with_capacity(130);
    payload.push(0x00);
    payload.extend_from_slice(luma);
    payload.push(0x01);
    payload.extend_from_slice(chroma);
    push_segment(out, DQT, &payload);
}

/// `SOF0`: 8-bit precision, the frame size, and three components at 1x1
/// sampling — Y with quantiser 0, Cb and Cr with quantiser 1.
fn push_frame(out: &mut Vec<u8>, width: u32, height: u32) {
    let mut payload = Vec::with_capacity(15);
    payload.push(8);
    payload.extend_from_slice(&u16::try_from(height).unwrap_or(u16::MAX).to_be_bytes());
    payload.extend_from_slice(&u16::try_from(width).unwrap_or(u16::MAX).to_be_bytes());
    payload.push(3);
    payload.extend_from_slice(&[1, 0x11, 0]);
    payload.extend_from_slice(&[2, 0x11, 1]);
    payload.extend_from_slice(&[3, 0x11, 1]);
    push_segment(out, SOF0, &payload);
}

/// The four `DHT` segments the scan header then refers to by table selector.
fn push_huff_tables(out: &mut Vec<u8>) {
    push_one_huff(out, 0x00, &LUMA_DC_BITS, &LUMA_DC_VALS);
    push_one_huff(out, 0x10, &LUMA_AC_BITS, &LUMA_AC_VALS);
    push_one_huff(out, 0x01, &CHROMA_DC_BITS, &CHROMA_DC_VALS);
    push_one_huff(out, 0x11, &CHROMA_AC_BITS, &CHROMA_AC_VALS);
}

fn push_one_huff(out: &mut Vec<u8>, selector: u8, bits: &[u8; 16], vals: &[u8]) {
    let mut payload = Vec::with_capacity(17usize.saturating_add(vals.len()));
    payload.push(selector);
    payload.extend_from_slice(bits);
    payload.extend_from_slice(vals);
    push_segment(out, DHT, &payload);
}

/// `SOS`: all three components, table selectors `0x00`/`0x11`/`0x11`, the whole
/// 64-coefficient block, no successive-approximation refinement.
fn push_scan_header(out: &mut Vec<u8>) {
    let mut payload = Vec::with_capacity(10);
    payload.push(3);
    payload.extend_from_slice(&[1, 0x00]);
    payload.extend_from_slice(&[2, 0x11]);
    payload.extend_from_slice(&[3, 0x11]);
    payload.extend_from_slice(&[0, 63, 0]);
    push_segment(out, SOS, &payload);
}

#[must_use]
fn px_width(width: u32) -> usize {
    usize::try_from(width).unwrap_or(usize::MAX)
}

#[must_use]
fn px_height(height: u32) -> usize {
    usize::try_from(height).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing
    )]

    use super::*;
    use crate::dct::{dct_decode, JpegCoding};
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    /// A 1px alternating black/white grid — the densest AC energy an 8-bit
    /// image can offer the codec, and the case that drives the coefficient
    /// range to the edge of what the Annex K tables can name.
    fn checkerboard(size: u32) -> Vec<u8> {
        let mut rgb = Vec::new();
        for y in 0..size {
            for x in 0..size {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        rgb
    }

    /// A solid colour, so any deviation the round trip reports is the codec's.
    fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut data = Vec::new();
        for _ in 0..(width.saturating_mul(height)) {
            data.extend_from_slice(&rgb);
        }
        data
    }

    /// A horizontal ramp, which has real AC energy in one direction.
    fn ramp(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let v = u8::try_from(x.rem_euclid(256)).unwrap_or(0);
                data.extend_from_slice(&[
                    v,
                    255u8.saturating_sub(v),
                    u8::try_from(y.rem_euclid(256)).unwrap_or(0),
                ]);
            }
        }
        data
    }

    #[test]
    fn the_reader_declines_a_frame_wider_than_its_own_limit() {
        // The raster chunk refuses JPEG output above 16384 samples per side
        // because this is what the engine's reader does with such a frame: the
        // limit belongs to the decoder and is a guard for untrusted input, so
        // writing a file the reader will not reopen would only produce an
        // unverifiable download. Thin, so the run stays a kilobyte of pixels.
        let (width, height) = (16_668u32, 40u32);
        let rgb = solid(width, height, [200, 120, 40]);
        let out = dct_encode(&rgb, width, height, 90, &mut guard()).expect("encode");
        let error = dct_decode(&out, &mut guard()).expect_err("the reader declines it");
        assert_eq!(error.code(), selis_error::Code::DctCorrupt);
        // The frame itself is sound: the decline is about size, not structure.
        assert_eq!(out.get(..2), Some([0xFF, 0xD8].as_slice()));
        assert_eq!(
            out.get(out.len().saturating_sub(2)..),
            Some([0xFF, 0xD9].as_slice())
        );
    }

    #[test]
    fn uniform_noise_is_the_harshest_thing_the_coder_gets() {
        // What is the largest *honest* error the coder produces? Random noise is
        // the harshest input a block transform gets: every 8x8 block is full of
        // AC energy, which is what a scanned page's grain looks like to the coder.
        // The raster chunk sets its JPEG verification bounds against these
        // numbers, so they are pinned here rather than left in a comment someone
        // has to re-measure by hand.
        let mut state = 0x1234_5678u32;
        let mut rgb = Vec::new();
        for _ in 0..(256usize * 256) {
            for _ in 0..3 {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                rgb.push(u8::try_from((state >> 16) & 0xFF).unwrap_or(0));
            }
        }
        for (quality, max_mean, max_worst) in [
            (1u8, 62u64, 255u64),
            (25, 48, 253),
            (50, 28, 203),
            (75, 15, 103),
            (90, 6, 45),
            (100, 0, 4),
        ] {
            let bytes = dct_encode(&rgb, 256, 256, quality, &mut guard()).expect("encode");
            let decoded = dct_decode(&bytes, &mut guard()).expect("decode");
            let mut total = 0u64;
            let mut worst = 0u64;
            let mut samples = 0u64;
            for (index, channel) in decoded.data.iter().enumerate() {
                let want = rgb.get(index).copied().unwrap_or(0);
                let difference = u64::from(
                    channel
                        .saturating_sub(want)
                        .saturating_add(want.saturating_sub(*channel)),
                );
                total = total.saturating_add(difference);
                worst = worst.max(difference);
                samples = samples.saturating_add(1);
            }
            let mean = total.saturating_div(samples.max(1));
            assert!(
                mean <= max_mean && worst <= max_worst,
                "noise at quality {quality}: mean {mean} (ceiling {max_mean}), worst {worst} (ceiling {max_worst})"
            );
        }
    }

    #[test]
    fn the_stream_is_a_baseline_jpeg_with_the_expected_markers() {
        let out =
            dct_encode(&solid(16, 16, [200, 120, 40]), 16, 16, 75, &mut guard()).expect("encode");
        assert_eq!(out.get(..2), Some([0xFF, 0xD8].as_slice()), "SOI first");
        assert_eq!(
            out.get(out.len().saturating_sub(2)..),
            Some([0xFF, 0xD9].as_slice()),
            "EOI last"
        );
        assert_eq!(out.get(2..4), Some([0xFF, APP0].as_slice()));
        assert!(out.windows(4).any(|w| *w == *b"JFIF"), "JFIF identifier");
        assert!(out.windows(2).any(|w| w == [0xFF, SOF0]), "SOF0");
        assert!(out.windows(2).any(|w| w == [0xFF, DQT]), "DQT");
        assert!(out.windows(2).any(|w| w == [0xFF, DHT]), "DHT");
        assert!(out.windows(2).any(|w| w == [0xFF, SOS]), "SOS");
        // Four Huffman tables, three components, two quantiser tables.
        let dht = out.windows(2).filter(|w| *w == [0xFF, DHT]).count();
        assert_eq!(dht, 4);
    }

    #[test]
    fn the_frame_header_carries_the_size_and_three_full_rate_components() {
        let out = dct_encode(&solid(9, 5, [10, 20, 30]), 9, 5, 50, &mut guard()).expect("encode");
        let sof = out
            .windows(2)
            .position(|w| w == [0xFF, SOF0])
            .expect("SOF0 present");
        let body = &out[sof.saturating_add(4)..];
        assert_eq!(body.first().copied(), Some(8), "8-bit precision");
        assert_eq!(
            body.get(1..3).map(|b| u16::from_be_bytes([b[0], b[1]])),
            Some(5),
            "height"
        );
        assert_eq!(
            body.get(3..5).map(|b| u16::from_be_bytes([b[0], b[1]])),
            Some(9),
            "width"
        );
        assert_eq!(body.get(5).copied(), Some(3), "three components");
        assert_eq!(
            body.get(6..9),
            Some([1, 0x11, 0].as_slice()),
            "Y at 1x1, quant 0"
        );
        assert_eq!(body.get(9..12), Some([2, 0x11, 1].as_slice()), "Cb");
        assert_eq!(body.get(12..15), Some([3, 0x11, 1].as_slice()), "Cr");
    }

    #[test]
    fn the_decoder_reads_the_same_pixels_back_within_one_quantiser_step() {
        let rgb = solid(24, 24, [200, 120, 40]);
        let out = dct_encode(&rgb, 24, 24, 90, &mut guard()).expect("encode");
        let back = dct_decode(&out, &mut guard()).expect("decode");
        assert_eq!((back.width, back.height), (24, 24));
        assert_eq!(back.channels, 3);
        assert_eq!(back.coding, JpegCoding::Baseline);
        let worst = back
            .data
            .chunks_exact(3)
            .map(|px| {
                px.iter()
                    .zip([200u8, 120, 40])
                    .map(|(&got, want)| i64::from(got).saturating_sub(i64::from(want)).abs())
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .expect("pixels");
        assert!(
            worst <= 12,
            "a solid colour must survive at high quality, got {worst}"
        );
    }

    #[test]
    fn a_ramp_round_trips_close_to_the_source() {
        let rgb = ramp(32, 16);
        let out = dct_encode(&rgb, 32, 16, 85, &mut guard()).expect("encode");
        let back = dct_decode(&out, &mut guard()).expect("decode");
        assert_eq!((back.width, back.height), (32, 16));
        let mut total = 0u64;
        for (index, px) in back.data.chunks_exact(3).enumerate() {
            let Some(got) =
                rgb.get(index.saturating_mul(3)..index.saturating_mul(3).saturating_add(3))
            else {
                continue;
            };
            for (channel, &want) in got.iter().enumerate() {
                total = total.saturating_add(
                    u64::from(u8::saturating_sub(
                        px.get(channel).copied().unwrap_or(0),
                        want,
                    ))
                    .saturating_add(u64::from(u8::saturating_sub(
                        want,
                        px.get(channel).copied().unwrap_or(0),
                    ))),
                );
            }
        }
        let samples = 32u64.saturating_mul(16).saturating_mul(3);
        let mean = total.saturating_div(samples);
        assert!(
            mean <= 12,
            "mean channel error on a smooth ramp, got {mean}"
        );
    }

    #[test]
    fn encoding_the_same_pixels_twice_gives_the_same_bytes() {
        let rgb = ramp(40, 24);
        let first = dct_encode(&rgb, 40, 24, 70, &mut guard()).expect("first");
        let second = dct_encode(&rgb, 40, 24, 70, &mut guard()).expect("second");
        assert_eq!(first, second, "the writer must not depend on call order");
    }

    #[test]
    fn higher_quality_produces_a_bigger_file_for_the_same_pixels() {
        let rgb = ramp(64, 64);
        let low = dct_encode(&rgb, 64, 64, 20, &mut guard()).expect("low");
        let mid = dct_encode(&rgb, 64, 64, 50, &mut guard()).expect("mid");
        let high = dct_encode(&rgb, 64, 64, 95, &mut guard()).expect("high");
        assert!(low.len() < mid.len(), "{} < {}", low.len(), mid.len());
        assert!(mid.len() < high.len(), "{} < {}", mid.len(), high.len());
    }

    #[test]
    fn the_edges_of_a_non_multiple_of_eight_are_replicated_not_zero_filled() {
        // 9x9: the last block is one column and one row wide. Zero-filling it
        // would ring the page with a dark band; the standard says replicate.
        let rgb = solid(9, 9, [255, 255, 255]);
        let out = dct_encode(&rgb, 9, 9, 80, &mut guard()).expect("encode");
        let back = dct_decode(&out, &mut guard()).expect("decode");
        assert_eq!((back.width, back.height), (9, 9));
        let darkest = back
            .data
            .chunks_exact(3)
            .map(|px| px.iter().copied().min().unwrap_or(255))
            .min()
            .expect("pixels");
        assert!(
            darkest > 230,
            "a white page stays white to its edge, got {darkest}"
        );
    }

    #[test]
    fn a_full_contrast_checkerboard_survives_the_coarsest_and_finest_tables() {
        // The pathological case for the coefficient range: every sample at one
        // of the two extremes, aligned with the basis so the AC terms are as
        // large as the format allows. At quality 100 every quantiser step is 1,
        // so the coefficients are the raw transform output, saturated to the
        // widest symbol the tables name — and the reconstruction is exact.
        for quality in [1, 50, 100] {
            let rgb = checkerboard(16);
            let out = dct_encode(&rgb, 16, 16, quality, &mut guard()).expect("encode");
            let back = dct_decode(&out, &mut guard()).expect("decode");
            assert_eq!((back.width, back.height), (16, 16), "q={quality}");
        }
        let rgb = checkerboard(16);
        let out = dct_encode(&rgb, 16, 16, 100, &mut guard()).expect("encode");
        let back = dct_decode(&out, &mut guard()).expect("decode");
        let worst = back
            .data
            .chunks_exact(3)
            .zip(rgb.chunks_exact(3))
            .map(|(got, want)| {
                got.iter()
                    .zip(want)
                    .map(|(&g, &w)| i64::from(g).saturating_sub(i64::from(w)).abs())
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .expect("pixels");
        assert_eq!(worst, 0, "a step-of-one table must round-trip exactly");
    }

    #[test]
    fn every_test_image_decodes_back_at_every_quality() {
        // The regression this sweep exists for: quality 99 and 100 put sixteen
        // zero runs in front of a surviving AC coefficient, and the writer used
        // to emit eleven extra bits after the 0xF0 escape symbol. The stream
        // still decoded at most qualities and turned to noise at others, so a
        // single quality is not enough to call the writer correct.
        let images: Vec<(&str, Vec<u8>, u32, u32)> = vec![
            ("solid", solid(24, 24, [200, 120, 40]), 24, 24),
            ("white", solid(9, 9, [255, 255, 255]), 9, 9),
            ("ramp16", ramp(16, 16), 16, 16),
            ("ramp64", ramp(64, 64), 64, 64),
            ("board16", checkerboard(16), 16, 16),
            ("board64", checkerboard(64), 64, 64),
        ];
        for quality in [1, 10, 25, 50, 75, 90, 95, 99, 100] {
            for (name, rgb, width, height) in &images {
                let out = dct_encode(rgb, *width, *height, quality, &mut guard())
                    .unwrap_or_else(|e| panic!("{name} at q={quality}: {e}"));
                let back = dct_decode(&out, &mut guard())
                    .unwrap_or_else(|e| panic!("{name} at q={quality} did not decode: {e}"));
                assert_eq!(
                    (back.width, back.height),
                    (*width, *height),
                    "{name} q={quality}"
                );
                assert_eq!(back.coding, JpegCoding::Baseline, "{name} q={quality}");
            }
        }
    }

    #[test]
    fn the_tables_cover_every_symbol_this_writer_can_emit() {
        // The writer cannot produce a symbol outside this set, so the tables
        // naming all of it is what makes `write_symbol` unreachable-in-practice
        // rather than merely unwritten-about:
        //   * AC magnitude size is bit_length of a coefficient saturated to
        //     ±MAX_COEFFICIENT, so 1..=10, and run is 0..=15 after ZRL folding;
        //   * DC magnitude category is bit_length of the difference of two such
        //     coefficients, so 1..=11;
        //   * plus EOB (0x00) and ZRL (0xF0), which carry no magnitude.
        assert!(
            MAX_COEFFICIENT.saturating_add(1) == 1024,
            "the saturation bound is exactly ten bits"
        );
        for (name, bits, vals) in [
            ("luma DC", &LUMA_DC_BITS, &LUMA_DC_VALS as &[u8]),
            ("chroma DC", &CHROMA_DC_BITS, &CHROMA_DC_VALS as &[u8]),
        ] {
            let table = build_huff(bits, vals);
            for category in 0..=11u8 {
                assert!(
                    table.lookup(category).is_some(),
                    "{name} has no code for DC category {category}"
                );
            }
        }
        for (name, bits, vals) in [
            ("luma AC", &LUMA_AC_BITS, &LUMA_AC_VALS as &[u8]),
            ("chroma AC", &CHROMA_AC_BITS, &CHROMA_AC_VALS as &[u8]),
        ] {
            let table = build_huff(bits, vals);
            for run in 0..16u8 {
                for size in 1..=10u8 {
                    let symbol = run.checked_shl(4).unwrap_or(u8::MAX).saturating_add(size);
                    assert!(
                        table.lookup(symbol).is_some(),
                        "{name} has no code for run {run} size {size} (0x{symbol:02X})"
                    );
                }
            }
            assert!(table.lookup(0x00).is_some(), "{name} has no EOB");
            assert!(table.lookup(0xF0).is_some(), "{name} has no ZRL");
        }
    }

    #[test]
    fn bad_input_is_a_typed_error() {
        let g = &mut guard();
        assert_eq!(
            dct_encode(&[], 0, 1, 50, g)
                .map(|_| ())
                .err()
                .map(|e| e.code()),
            Some(Code::ImageMalformed)
        );
        assert_eq!(
            dct_encode(&[0u8; 3], 1, 1, 101, g)
                .map(|_| ())
                .err()
                .map(|e| e.code()),
            Some(Code::ImageMalformed)
        );
        // A buffer that does not match the dimensions: 2x2 needs 12 bytes.
        assert!(dct_encode(&[0u8; 4], 2, 2, 50, g).is_err());
        assert!(dct_encode(&[0u8; 11], 2, 2, 50, g).is_err());
        assert!(dct_encode(&[0u8; 13], 2, 2, 50, g).is_err());
        // A page edge wider than the 16-bit frame header can name.
        assert!(dct_encode(&[], 70_000, 1, 50, g).is_err());
    }

    #[test]
    fn quality_zero_encodes_as_the_coarsest_step_instead_of_dividing_by_zero() {
        let out = dct_encode(&ramp(16, 16), 16, 16, 0, &mut guard()).expect("encode");
        let back = dct_decode(&out, &mut guard()).expect("decode");
        assert_eq!((back.width, back.height), (16, 16));
    }

    #[test]
    fn the_zigzag_order_visits_every_coefficient_once() {
        let mut seen = [false; 64];
        for &position in &ZIGZAG {
            assert!(
                !seen.get(position).copied().unwrap_or(true),
                "repeat at {position}"
            );
            if let Some(slot) = seen.get_mut(position) {
                *slot = true;
            }
        }
        assert!(seen.iter().all(|flag| *flag), "all 64 coefficients scanned");
        assert_eq!(ZIGZAG.first().copied(), Some(0), "DC first");
        assert_eq!(ZIGZAG.get(1).copied(), Some(1), "then (0,1)");
        assert_eq!(ZIGZAG.get(2).copied(), Some(8), "then (1,0)");
    }

    #[test]
    fn the_cosine_table_matches_the_standard_kernel() {
        // The table is the only float-derived constant in the writer, and this
        // is the only place a float appears: the encoder itself stays integer.
        for (u, row) in COS_Q15.iter().enumerate() {
            let normalisation = if u == 0 { 1.0 / f64::sqrt(2.0) } else { 1.0 };
            for (x, &value) in row.iter().enumerate() {
                let angle = f64::from(
                    u32::try_from(2usize.saturating_mul(x).saturating_add(1)).unwrap_or(0),
                ) * f64::from(u32::try_from(u).unwrap_or(0))
                    * std::f64::consts::PI
                    / 16.0;
                let exact = normalisation * f64::cos(angle) * 32768.0;
                let difference = f64::from(value) - exact;
                assert!(difference.abs() <= 1.0, "u={u} x={x} off by {difference}");
            }
        }
    }

    #[test]
    fn each_table_declares_exactly_as_many_codes_as_it_lists_symbols() {
        // A `DHT` segment whose counts and symbol list disagree is a corrupt
        // file: the decoder reads past the table into the next marker. This is
        // the invariant that makes the four constants above load-bearing.
        for (name, bits, vals) in [
            ("luma DC", &LUMA_DC_BITS, &LUMA_DC_VALS as &[u8]),
            ("luma AC", &LUMA_AC_BITS, &LUMA_AC_VALS as &[u8]),
            ("chroma DC", &CHROMA_DC_BITS, &CHROMA_DC_VALS as &[u8]),
            ("chroma AC", &CHROMA_AC_BITS, &CHROMA_AC_VALS as &[u8]),
        ] {
            let declared: usize = bits.iter().map(|&count| usize::from(count)).sum();
            assert_eq!(declared, vals.len(), "{name} table is self-inconsistent");
        }
    }

    #[test]
    fn the_derived_huffman_codes_are_prefix_free() {
        // Canonical codes are stored right-aligned in their own length, so one
        // code is a prefix of another exactly when the longer code's top bits
        // spell the shorter one.
        for (name, bits, vals) in [
            ("luma DC", &LUMA_DC_BITS, &LUMA_DC_VALS as &[u8]),
            ("luma AC", &LUMA_AC_BITS, &LUMA_AC_VALS as &[u8]),
            ("chroma DC", &CHROMA_DC_BITS, &CHROMA_DC_VALS as &[u8]),
            ("chroma AC", &CHROMA_AC_BITS, &CHROMA_AC_VALS as &[u8]),
        ] {
            let table = build_huff(bits, vals);
            let mut codes: Vec<(u32, u8)> = Vec::new();
            for &symbol in vals {
                let code = table
                    .lookup(symbol)
                    .unwrap_or_else(|| panic!("{name} misses {symbol}"));
                assert!(code.len > 0 && code.len <= 16, "{name} length");
                assert!(
                    code.bits <= low_mask(u32::from(code.len)),
                    "{name} code wider than its length"
                );
                codes.push((code.bits, code.len));
            }
            for (index, (bits_a, len_a)) in codes.iter().enumerate() {
                for (bits_b, len_b) in codes.iter().skip(index.saturating_add(1)) {
                    let (short, long, short_len, long_len) = if len_a <= len_b {
                        (*bits_a, *bits_b, *len_a, *len_b)
                    } else {
                        (*bits_b, *bits_a, *len_b, *len_a)
                    };
                    let top = long.wrapping_shr(u32::from(long_len.saturating_sub(short_len)));
                    assert_ne!(
                        short & low_mask(u32::from(short_len)),
                        top,
                        "prefix collision in {name}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_block_the_table_cannot_describe_is_refused_not_written_silently() {
        // The guard itself, against a table too small to name the block: a
        // missing symbol used to write zero bits, and a bit stream with a hole
        // in it decodes as noise rather than as an error.
        let tiny = build_huff(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[0x00]);
        let dc = build_huff(&LUMA_DC_BITS, &LUMA_DC_VALS);
        let mut out = Vec::new();
        let mut writer = BitWriter {
            out: &mut out,
            acc: 0,
            acc_bits: 0,
        };
        let mut coeff = [0i32; 64];
        coeff[5] = 100; // run 4, size 7 — symbol 0x47, which `tiny` does not carry
        let mut prev = 0i32;
        let error = encode_block(&mut writer, &dc, &tiny, &coeff, &mut prev).expect_err("refused");
        assert_eq!(error.code(), Code::ImageMalformed);
        assert!(
            out.is_empty(),
            "a refused block must not leave bytes behind"
        );
    }

    #[test]
    fn the_bit_writer_pads_a_part_16_run_and_drops_the_padding_byte_when_it_is_full() {
        // Two bugs the padding rule is easy to get wrong: 16 bits of all-ones
        // leave a whole byte, so `flush` must not append an eighteenth one; and
        // a short run must be filled with ones, not zeros, or the decoder's
        // last symbol reads as a different code.
        let mut short_out = Vec::new();
        {
            let mut writer = BitWriter {
                out: &mut short_out,
                acc: 0,
                acc_bits: 0,
            };
            writer.write_bits(0b1_0110, 5);
            writer.flush();
            assert_eq!(
                short_out,
                vec![0b1011_0111],
                "five bits, three one-bits of padding"
            );
        }
        let mut full_out = Vec::new();
        {
            let mut writer = BitWriter {
                out: &mut full_out,
                acc: 0,
                acc_bits: 0,
            };
            writer.write_bits(0xAAAA, 16);
            writer.flush();
            assert_eq!(
                full_out,
                vec![0xAA, 0xAA],
                "a full final byte is not padded"
            );
        }
    }
}
