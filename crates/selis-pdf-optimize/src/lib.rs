//! Lossy image recompression for the Compress tool's level/target tiers.
//!
//! The pass walks the collected object graph and re-encodes *simple* image
//! XObjects — 8-bit `DeviceRGB`/`DeviceGray` samples behind no filter,
//! `FlateDecode` or `DCTDecode` — as baseline JPEG at a reduced quality,
//! downsampling the pixel grid when it exceeds the pass's side cap. Only a
//! strictly smaller stream replaces the original, so a single image can never
//! grow; the caller owns the whole-document "never larger than the input"
//! guarantee.
//!
//! Deliberately left untouched: images with `/SMask`, `/Mask`, `/ImageMask`,
//! `/Decode` or `/DecodeParms`; indexed, separation, ICC or reference
//! colour spaces; non-8-bit samples; JPX, CCITT, JBIG2, LZW and filter
//! arrays; and inline images (they live in content streams, which this pass
//! never opens). Text, annotations, forms and outlines are not touched at
//! all — the pass only ever edits an image stream's own dictionary.
//!
//! Shared by the CLI, the browser tools chunk and any later desktop shell:
//! nothing here names a filesystem or a browser API.

#![forbid(unsafe_code)]

use selis_bytes::Bytes;
use selis_pdf_cos::Obj;
use selis_sandbox::{BudgetGuard, Resource};

/// The maximum bytes a single image stream may expand to while being decoded.
const DECODE_LIMIT: u64 = 256 * 1024 * 1024;

/// The maximum number of quality-lowering passes a target search may run.
pub const MAX_PASSES: usize = 3;

/// How hard the lossy tier re-encodes images. The names match the card's
/// three radios; `None` at the call site keeps the lossless-only behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionLevel {
    /// Highest quality of the three lossy tiers: gentle quality loss, a
    /// generous pixel cap, the largest output.
    Less,
    /// The balanced default: moderate quality loss and pixel cap.
    Recommended,
    /// The smallest output: aggressive quality loss and a tight pixel cap.
    Extreme,
}

impl CompressionLevel {
    /// Parse a wire level name; `None` for anything else.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "less" => Some(Self::Less),
            "recommended" => Some(Self::Recommended),
            "extreme" => Some(Self::Extreme),
            _ => None,
        }
    }

    /// The wire name of the level.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Less => "less",
            Self::Recommended => "recommended",
            Self::Extreme => "extreme",
        }
    }

    /// The parameters of pass `index` (0-based). Pass 0 is the level's base
    /// setting; every later pass lowers the JPEG quality by 15 (floor 10)
    /// and tightens the pixel cap to 4/5, so a bounded target search walks
    /// strictly harder settings. Indices at or past [`MAX_PASSES`] clamp to
    /// the last pass.
    #[must_use]
    pub fn pass(self, index: usize) -> ImagePass {
        let (mut quality, mut max_side) = match self {
            Self::Less => (80u8, 2000u32),
            Self::Recommended => (60u8, 1400u32),
            Self::Extreme => (38u8, 900u32),
        };
        for _ in 0..index.min(MAX_PASSES.saturating_sub(1)) {
            quality = quality.saturating_sub(15).max(10);
            max_side = max_side.saturating_mul(4).saturating_div(5);
        }
        ImagePass { quality, max_side }
    }
}

/// One lossy pass's settings: the JPEG quality and the longest-side pixel
/// cap. Exposed so a caller can drive a custom search; the level schedule
/// above is the product default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImagePass {
    /// Baseline JPEG quality, 1..=100.
    pub quality: u8,
    /// Longest side of the re-encoded pixel grid; larger images are
    /// box-downsampled to fit. 0 means "never downsample".
    pub max_side: u32,
}

/// What one lossy pass did to the object graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageReport {
    /// Image XObjects the pass could re-encode (simple 8-bit RGB/gray).
    pub eligible: usize,
    /// Images actually replaced by a smaller JPEG stream.
    pub reencoded: usize,
    /// Of those, the ones whose pixel grid was also downsampled.
    pub downsampled: usize,
}

/// Re-encode the eligible image XObjects of a collected object graph in
/// place. Best-effort: an image that cannot be decoded, or whose JPEG would
/// not be smaller, keeps its original bytes. Never touches a non-image
/// object, so text and content streams pass through byte-identical.
pub fn recompress_images(
    objects: &mut [(u32, Obj)],
    pass: ImagePass,
    g: &mut BudgetGuard<'_>,
) -> ImageReport {
    let mut report = ImageReport::default();
    for (_num, obj) in objects.iter_mut() {
        match recompress_one(obj, pass, g) {
            Outcome::NotEligible => {}
            Outcome::Kept => {
                report.eligible = report.eligible.saturating_add(1);
            }
            Outcome::Reencoded { downsampled } => {
                report.eligible = report.eligible.saturating_add(1);
                report.reencoded = report.reencoded.saturating_add(1);
                if downsampled {
                    report.downsampled = report.downsampled.saturating_add(1);
                }
            }
        }
    }
    report
}

/// The three outcomes of looking at one object.
enum Outcome {
    /// Not a simple image XObject; left untouched.
    NotEligible,
    /// Eligible, but the re-encode would not shrink it; original kept.
    Kept,
    /// Replaced by a smaller JPEG stream.
    Reencoded { downsampled: bool },
}

/// Re-encode one object when it is a simple image XObject and the JPEG is
/// strictly smaller than the stored stream.
fn recompress_one(obj: &mut Obj, pass: ImagePass, g: &mut BudgetGuard<'_>) -> Outcome {
    let Obj::Stream { dict, data } = obj else {
        return Outcome::NotEligible;
    };
    if !name_is(dict_get(dict, b"Subtype"), b"Image") {
        return Outcome::NotEligible;
    }
    // Masks and sample remaps change what the samples mean; a base-image
    // re-encode cannot honour them, so the whole image is left alone.
    for key in [
        b"SMask".as_slice(),
        b"Mask",
        b"ImageMask",
        b"Decode",
        b"DecodeParms",
    ] {
        if dict_get(dict, key).is_some() {
            return Outcome::NotEligible;
        }
    }
    if int_value(dict_get(dict, b"BitsPerComponent")) != Some(8) {
        return Outcome::NotEligible;
    }
    let channels: u8 = match dict_get(dict, b"ColorSpace") {
        Some(Obj::Name(n)) if n.as_slice() == b"DeviceRGB" => 3,
        Some(Obj::Name(n)) if n.as_slice() == b"DeviceGray" => 1,
        _ => return Outcome::NotEligible,
    };
    let Some(width) = side_value(dict_get(dict, b"Width")) else {
        return Outcome::NotEligible;
    };
    let Some(height) = side_value(dict_get(dict, b"Height")) else {
        return Outcome::NotEligible;
    };

    // How the stored bytes are interpreted. A broken stream of an eligible
    // profile is kept as-is, never re-interpreted through another codec.
    enum Source {
        Raw,
        Flate,
        Dct,
    }
    let source = match dict_get(dict, b"Filter") {
        None => Source::Raw,
        Some(Obj::Name(n)) if n.as_slice() == b"FlateDecode" || n.as_slice() == b"Fl" => {
            Source::Flate
        }
        Some(Obj::Name(n)) if n.as_slice() == b"DCTDecode" || n.as_slice() == b"DCT" => Source::Dct,
        // JPX, CCITT, JBIG2, LZW, arrays: not this pass's job.
        Some(_) => return Outcome::NotEligible,
    };
    let image = match source {
        Source::Raw => match raw_to_rgba(data, width, height, channels, g) {
            Some(image) => image,
            None => return Outcome::Kept,
        },
        Source::Flate => {
            let Ok(raw) = selis_pdf_filter::flate_decode_bounded(data, DECODE_LIMIT, g) else {
                return Outcome::Kept;
            };
            match raw_to_rgba(&raw, width, height, channels, g) {
                Some(image) => image,
                None => return Outcome::Kept,
            }
        }
        Source::Dct => {
            let Ok(dct) = selis_pdf_filter::dct_decode(data, g) else {
                return Outcome::Kept;
            };
            // A colourspace/sample mismatch or a 12-bit frame is outside the
            // simple profile; keep the original rather than guess.
            if dct.channels != channels || dct.precision != 8 {
                return Outcome::Kept;
            }
            match selis_image::dct_to_rgba(&dct, g) {
                Ok(image) => image,
                Err(_) => return Outcome::Kept,
            }
        }
    };

    let (image, downsampled) = match downsample_if_needed(image, pass.max_side, g) {
        Some(pair) => pair,
        None => return Outcome::Kept,
    };
    let quality = pass.quality.clamp(1, 100);
    let Ok(jpeg) = selis_image::encode_jpeg(&image, quality, g) else {
        return Outcome::Kept;
    };
    if jpeg.len() >= data.len() {
        return Outcome::Kept;
    }
    *data = Bytes::from(jpeg);
    set_key(
        dict,
        b"Length",
        Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
    );
    set_key(
        dict,
        b"Filter",
        Obj::Name(Bytes::copy_from_slice(b"DCTDecode")),
    );
    set_key(
        dict,
        b"ColorSpace",
        Obj::Name(Bytes::copy_from_slice(b"DeviceRGB")),
    );
    set_key(dict, b"BitsPerComponent", Obj::Int(8));
    set_key(dict, b"Width", Obj::Int(i64::from(image.width)));
    set_key(dict, b"Height", Obj::Int(i64::from(image.height)));
    Outcome::Reencoded { downsampled }
}

/// Box-downsample the image when its longest side exceeds the cap. Returns
/// the image to encode and whether the grid changed.
fn downsample_if_needed(
    image: selis_image::Image,
    max_side: u32,
    g: &mut BudgetGuard<'_>,
) -> Option<(selis_image::Image, bool)> {
    let longest = image.width.max(image.height);
    if max_side == 0 || longest <= max_side {
        return Some((image, false));
    }
    let new_w = scaled_side(image.width, max_side, longest);
    let new_h = scaled_side(image.height, max_side, longest);
    let out = box_downsample(&image, new_w, new_h, g)?;
    Some((out, true))
}

/// One side of the grid scaled by `max_side / longest`, at least 1 pixel.
fn scaled_side(side: u32, max_side: u32, longest: u32) -> u32 {
    let scaled = u64::from(side)
        .saturating_mul(u64::from(max_side))
        .checked_div(u64::from(longest))
        .unwrap_or(0);
    u32::try_from(scaled).unwrap_or(1).max(1)
}

/// Area-average (box) downsample of an RGBA8 grid. Integer-only, so the
/// output is byte-identical on wasm32 and natively.
fn box_downsample(
    image: &selis_image::Image,
    new_w: u32,
    new_h: u32,
    g: &mut BudgetGuard<'_>,
) -> Option<selis_image::Image> {
    let w = usize::try_from(image.width).ok()?;
    let h = usize::try_from(image.height).ok()?;
    let nw = usize::try_from(new_w).ok()?;
    let nh = usize::try_from(new_h).ok()?;
    if nw == 0 || nh == 0 {
        return None;
    }
    let out_len = nw.saturating_mul(nh).saturating_mul(4);
    g.charge(Resource::Bytes, u64::try_from(out_len).unwrap_or(u64::MAX))
        .ok()?;
    g.charge(
        Resource::Pixels,
        u64::try_from(nw.saturating_mul(nh)).unwrap_or(u64::MAX),
    )
    .ok()?;
    let mut rgba = Vec::with_capacity(out_len);
    for oy in 0..nh {
        let y0 = oy.saturating_mul(h).checked_div(nh).unwrap_or(0);
        let y1 = oy
            .saturating_add(1)
            .saturating_mul(h)
            .checked_div(nh)
            .unwrap_or(0)
            .max(y0.saturating_add(1))
            .min(h);
        for ox in 0..nw {
            let x0 = ox.saturating_mul(w).checked_div(nw).unwrap_or(0);
            let x1 = ox
                .saturating_add(1)
                .saturating_mul(w)
                .checked_div(nw)
                .unwrap_or(0)
                .max(x0.saturating_add(1))
                .min(w);
            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let at = sy.saturating_mul(w).saturating_add(sx).saturating_mul(4);
                    let px = image.rgba.get(at..at.saturating_add(4))?;
                    for (sum, sample) in sums.iter_mut().zip(px.iter()) {
                        *sum = sum.saturating_add(u32::from(*sample));
                    }
                    count = count.saturating_add(1);
                }
            }
            if count == 0 {
                return None;
            }
            for sum in sums {
                let avg = sum
                    .saturating_add(count.saturating_div(2))
                    .checked_div(count)
                    .unwrap_or(0);
                rgba.push(u8::try_from(avg).unwrap_or(255));
            }
        }
    }
    Some(selis_image::Image {
        width: new_w,
        height: new_h,
        rgba,
    })
}

/// Interpret raw 8-bit RGB/gray samples as RGBA8, refusing a buffer that
/// does not exactly match the declared grid.
fn raw_to_rgba(
    raw: &[u8],
    width: u32,
    height: u32,
    channels: u8,
    g: &mut BudgetGuard<'_>,
) -> Option<selis_image::Image> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    let expected = pixels.saturating_mul(u64::from(channels));
    if u64::try_from(raw.len()).unwrap_or(u64::MAX) != expected {
        return None;
    }
    let out_len = usize::try_from(pixels.saturating_mul(4)).ok()?;
    g.charge(Resource::Bytes, u64::try_from(out_len).unwrap_or(u64::MAX))
        .ok()?;
    g.charge(Resource::Pixels, pixels).ok()?;
    let mut rgba = Vec::with_capacity(out_len);
    match channels {
        1 => {
            for &gray in raw {
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        3 => {
            for px in raw.chunks_exact(3) {
                let r = px.first().copied().unwrap_or(0);
                let gc = px.get(1).copied().unwrap_or(0);
                let b = px.get(2).copied().unwrap_or(0);
                rgba.extend_from_slice(&[r, gc, b, 255]);
            }
        }
        _ => return None,
    }
    Some(selis_image::Image {
        width,
        height,
        rgba,
    })
}

/// The dictionary entry for `key`, if present.
fn dict_get<'a>(dict: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    dict.iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

/// Whether the entry is the name `want`.
fn name_is(value: Option<&Obj>, want: &[u8]) -> bool {
    matches!(value, Some(Obj::Name(n)) if n.as_slice() == want)
}

/// An integer entry's value.
fn int_value(value: Option<&Obj>) -> Option<i64> {
    match value {
        Some(Obj::Int(n)) => Some(*n),
        _ => None,
    }
}

/// A positive image side that fits the JPEG frame header's 16 bits.
fn side_value(value: Option<&Obj>) -> Option<u32> {
    let n = int_value(value)?;
    u32::try_from(n).ok().filter(|n| *n > 0 && *n <= 65535)
}

/// Insert or replace the pair whose key matches `key`.
fn set_key(dict: &mut Vec<(Bytes, Obj)>, key: &[u8], value: Obj) {
    if let Some(slot) = dict.iter_mut().find(|(k, _)| k.as_slice() == key) {
        slot.1 = value;
    } else {
        dict.push((Bytes::copy_from_slice(key), value));
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::cast_possible_truncation,
        clippy::arithmetic_side_effects,
        clippy::integer_division,
        clippy::panic
    )]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    /// Deterministic photographic-style noise: an LCG over the pixel index,
    /// so Flate cannot shrink it but a low-quality JPEG can.
    fn noise_rgb(width: u32, height: u32) -> Vec<u8> {
        let count = (width as usize) * (height as usize) * 3;
        let mut out = Vec::with_capacity(count);
        let mut state = 0x1234_5678u32;
        for _ in 0..count {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            out.push((state >> 24) as u8);
        }
        out
    }

    fn name(value: &[u8]) -> Obj {
        Obj::Name(Bytes::copy_from_slice(value))
    }

    fn key(key: &[u8], value: Obj) -> (Bytes, Obj) {
        (Bytes::copy_from_slice(key), value)
    }

    /// An image XObject with the given filter and stored bytes.
    fn image_xobject(
        width: u32,
        height: u32,
        gray: bool,
        filter: Option<&[u8]>,
        data: Vec<u8>,
    ) -> Obj {
        let mut dict = vec![
            key(b"Type", name(b"XObject")),
            key(b"Subtype", name(b"Image")),
            key(b"Width", Obj::Int(i64::from(width))),
            key(b"Height", Obj::Int(i64::from(height))),
            key(
                b"ColorSpace",
                name(if gray { b"DeviceGray" } else { b"DeviceRGB" }),
            ),
            key(b"BitsPerComponent", Obj::Int(8)),
            key(
                b"Length",
                Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
            ),
        ];
        if let Some(filter) = filter {
            dict.push(key(b"Filter", name(filter)));
        }
        Obj::Stream {
            dict,
            data: Bytes::from(data),
        }
    }

    fn stream_data(obj: &Obj) -> &[u8] {
        let Obj::Stream { data, .. } = obj else {
            panic!("stream");
        };
        data.as_slice()
    }

    fn dict_of(obj: &Obj) -> &[(Bytes, Obj)] {
        let Obj::Stream { dict, .. } = obj else {
            panic!("stream");
        };
        dict
    }

    #[test]
    fn noisy_raw_image_shrinks_and_becomes_dct() {
        let mut g = guard();
        let raw = noise_rgb(240, 180);
        let original_len = raw.len();
        let mut objects = vec![(1u32, image_xobject(240, 180, false, None, raw))];
        let report = recompress_images(&mut objects, CompressionLevel::Extreme.pass(0), &mut g);
        assert_eq!(report.eligible, 1);
        assert_eq!(report.reencoded, 1);
        let obj = &objects[0].1;
        assert!(
            stream_data(obj).len() < original_len / 2,
            "JPEG of noise at q38 is much smaller"
        );
        assert!(name_is(dict_get(dict_of(obj), b"Filter"), b"DCTDecode"));
        assert!(name_is(dict_get(dict_of(obj), b"ColorSpace"), b"DeviceRGB"));
        // The replacement decodes back as a valid JPEG at the same grid.
        let dct = selis_pdf_filter::dct_decode(stream_data(obj), &mut g).expect("decode");
        assert_eq!((dct.width, dct.height), (240, 180));
    }

    #[test]
    fn flate_gray_image_reencodes_to_rgb_jpeg() {
        let mut g = guard();
        let raw = noise_rgb(200, 120);
        let gray: Vec<u8> = raw.chunks_exact(3).map(|px| px[0]).collect();
        let stored = selis_pdf_filter::flate_encode(&gray, 6);
        let mut objects = vec![(
            1u32,
            image_xobject(200, 120, true, Some(b"FlateDecode"), stored),
        )];
        let report = recompress_images(&mut objects, CompressionLevel::Extreme.pass(0), &mut g);
        assert_eq!(report.reencoded, 1);
        let obj = &objects[0].1;
        assert!(name_is(dict_get(dict_of(obj), b"ColorSpace"), b"DeviceRGB"));
        let dct = selis_pdf_filter::dct_decode(stream_data(obj), &mut g).expect("decode");
        assert_eq!((dct.width, dct.height, dct.channels), (200, 120, 3));
    }

    #[test]
    fn dct_image_reencodes_at_lower_quality() {
        let mut g = guard();
        let rgba = selis_image::Image {
            width: 240,
            height: 180,
            rgba: noise_rgb(240, 180)
                .chunks_exact(3)
                .flat_map(|px| [px[0], px[1], px[2], 255])
                .collect(),
        };
        let high = selis_image::encode_jpeg(&rgba, 95, &mut g).expect("encode");
        let mut objects = vec![(
            1u32,
            image_xobject(240, 180, false, Some(b"DCTDecode"), high.clone()),
        )];
        let report = recompress_images(&mut objects, CompressionLevel::Extreme.pass(0), &mut g);
        assert_eq!(report.reencoded, 1);
        assert!(stream_data(&objects[0].1).len() < high.len());
    }

    #[test]
    fn downsample_caps_the_longest_side() {
        let mut g = guard();
        let raw = noise_rgb(200, 100);
        let mut objects = vec![(1u32, image_xobject(200, 100, false, None, raw))];
        let pass = ImagePass {
            quality: 50,
            max_side: 64,
        };
        let report = recompress_images(&mut objects, pass, &mut g);
        assert_eq!(report.reencoded, 1);
        assert_eq!(report.downsampled, 1);
        let obj = &objects[0].1;
        assert_eq!(int_value(dict_get(dict_of(obj), b"Width")), Some(64));
        assert_eq!(int_value(dict_get(dict_of(obj), b"Height")), Some(32));
        let dct = selis_pdf_filter::dct_decode(stream_data(obj), &mut g).expect("decode");
        assert_eq!((dct.width, dct.height), (64, 32));
    }

    #[test]
    fn box_downsample_keeps_a_solid_colour_exact() {
        let mut g = guard();
        let image = selis_image::Image {
            width: 100,
            height: 50,
            rgba: [10u8, 20, 30, 255].repeat(5000),
        };
        let out = box_downsample(&image, 10, 5, &mut g).expect("downsample");
        assert_eq!((out.width, out.height), (10, 5));
        assert_eq!(out.rgba, [10u8, 20, 30, 255].repeat(50));
    }

    #[test]
    fn a_stream_that_would_grow_is_kept_byte_for_byte() {
        let mut g = guard();
        // A solid colour Flates to a few hundred bytes; any JPEG of it is
        // larger, so the pass must keep the original stream untouched.
        let solid = vec![128u8; 100 * 100 * 3];
        let stored = selis_pdf_filter::flate_encode(&solid, 10);
        let mut objects = vec![(
            1u32,
            image_xobject(100, 100, false, Some(b"FlateDecode"), stored.clone()),
        )];
        let report = recompress_images(&mut objects, CompressionLevel::Extreme.pass(0), &mut g);
        assert_eq!(report.eligible, 1);
        assert_eq!(report.reencoded, 0);
        assert_eq!(stream_data(&objects[0].1), stored.as_slice());
    }

    #[test]
    fn masked_indexed_and_unsupported_images_are_left_alone() {
        let mut g = guard();
        let raw = noise_rgb(60, 40);
        let mut masked = image_xobject(60, 40, false, None, raw.clone());
        if let Obj::Stream { dict, .. } = &mut masked {
            dict.push(key(b"SMask", Obj::Ref(selis_pdf_cos::Ref::new(9, 0))));
        }
        let mut indexed = image_xobject(60, 40, false, None, raw.clone());
        if let Obj::Stream { dict, .. } = &mut indexed {
            if let Some(slot) = dict.iter_mut().find(|(k, _)| k.as_slice() == b"ColorSpace") {
                slot.1 = name(b"Indexed");
            }
        }
        let jpx = image_xobject(60, 40, false, Some(b"JPXDecode"), raw.clone());
        let mut image_mask = image_xobject(60, 40, true, None, raw.clone());
        if let Obj::Stream { dict, .. } = &mut image_mask {
            dict.push(key(b"ImageMask", Obj::Bool(true)));
        }
        let mut one_bit = image_xobject(60, 40, true, None, raw.clone());
        if let Obj::Stream { dict, .. } = &mut one_bit {
            if let Some(slot) = dict
                .iter_mut()
                .find(|(k, _)| k.as_slice() == b"BitsPerComponent")
            {
                slot.1 = Obj::Int(1);
            }
        }
        let mut objects = vec![
            (1u32, masked),
            (2u32, indexed),
            (3u32, jpx),
            (4u32, image_mask),
            (5u32, one_bit),
            (6u32, Obj::Int(42)),
        ];
        let before: Vec<Obj> = objects.iter().map(|(_, o)| o.clone()).collect();
        let report = recompress_images(&mut objects, CompressionLevel::Extreme.pass(0), &mut g);
        assert_eq!(report.eligible, 0);
        assert_eq!(report.reencoded, 0);
        for (i, (_, obj)) in objects.iter().enumerate() {
            assert_eq!(obj, &before[i], "object {} untouched", i + 1);
        }
    }

    #[test]
    fn non_image_streams_and_content_pass_through() {
        let mut g = guard();
        let content = b"BT /F1 12 Tf (Hello) Tj ET\n".to_vec();
        let mut objects = vec![(
            1u32,
            Obj::Stream {
                dict: vec![key(
                    b"Length",
                    Obj::Int(i64::try_from(content.len()).unwrap()),
                )],
                data: Bytes::from(content.clone()),
            },
        )];
        let report = recompress_images(&mut objects, CompressionLevel::Extreme.pass(0), &mut g);
        assert_eq!(report.eligible, 0);
        assert_eq!(stream_data(&objects[0].1), content.as_slice());
    }

    #[test]
    fn pass_schedule_gets_strictly_harder_and_stays_bounded() {
        for level in [
            CompressionLevel::Less,
            CompressionLevel::Recommended,
            CompressionLevel::Extreme,
        ] {
            let mut previous = level.pass(0);
            for index in 1..MAX_PASSES {
                let next = level.pass(index);
                assert!(next.quality <= previous.quality);
                assert!(next.max_side <= previous.max_side);
                assert!(next.quality >= 10);
                previous = next;
            }
            // Clamped past the last pass.
            assert_eq!(level.pass(MAX_PASSES + 5), level.pass(MAX_PASSES - 1));
        }
        assert!(CompressionLevel::Extreme.pass(0).quality < CompressionLevel::Less.pass(0).quality);
    }

    #[test]
    fn level_names_round_trip() {
        for level in [
            CompressionLevel::Less,
            CompressionLevel::Recommended,
            CompressionLevel::Extreme,
        ] {
            assert_eq!(CompressionLevel::from_name(level.name()), Some(level));
        }
        assert_eq!(CompressionLevel::from_name("lossless"), None);
        assert_eq!(CompressionLevel::from_name(""), None);
    }
}
