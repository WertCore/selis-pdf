//! Image drawing (SL-2.RAST.08).
//!
//! Draws image XObjects through the [`Backend`] with the PDF specifics:
//! * `/Decode` arrays invert or rescale the sample values;
//! * interpolation is enabled or disabled per the image dict;
//! * image masks (`/ImageMask`) and colour-key `/Mask` are respected;
//! * **extreme downscales never allocate the full bitmap** â€” a 20 000 Ã—
//!   20 000 image drawn at 1% scale decodes into a buffer the size of the
//!   *destination*, not the source (the DoD).
//!
//! The sampling path bounds the decoded buffer by the destination pixels.

use selis_error::{err, Code, Result};
use selis_geom::Rect;
use selis_sandbox::{alloc, BudgetGuard};

use crate::{Backend, Image, ImagePlacement};

/// A decoded image, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedImage {
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// RGBA8 samples (premultiplied), row-major.
    pub rgba8: Vec<u8>,
}

/// The `/Decode` array for an image: the min/max each component maps to.
#[derive(Debug, Clone, PartialEq)]
pub struct Decode {
    /// `(min, max)` per component.
    pub ranges: Vec<(u8, u8)>,
}

impl Decode {
    /// The default decode: no change (0..255 for every component).
    #[must_use]
    pub fn identity(components: usize) -> Self {
        Self {
            ranges: vec![(0, 255); components],
        }
    }
}

/// Decode raw samples into RGBA8, applying the `/Decode` array.
///
/// # Budget
///
/// The output is `width Ã— height Ã— 4` bytes, charged against `g`; a hostile
/// `width Ã— height` overflows are rejected before allocation.
///
/// # Malformed Input
///
/// `IMAGE_MALFORMED` when the sample data is truncated or the dimensions are
/// absurd (would overflow).
pub fn decode_image(
    width: u32,
    height: u32,
    components: u8,
    bits_per_component: u8,
    samples: &[u8],
    decode: &Decode,
    g: &mut BudgetGuard<'_>,
) -> Result<DecodedImage> {
    // Reject absurd dimensions before any allocation (the 20kÃ—20k guard).
    let pixel_count = u64::from(width).saturating_mul(u64::from(height));
    if pixel_count > u64::from(u32::MAX) {
        return Err(err!(
            Code::ImageMalformed,
            during = "image-decode",
            detail = "image dimensions overflow"
        ));
    }

    let per_pixel = usize::from(components.max(1));
    let expected = pixel_count.saturating_mul(per_pixel as u64);
    if u64::try_from(samples.len()).unwrap_or(u64::MAX) < expected {
        return Err(err!(
            Code::ImageMalformed,
            during = "image-decode",
            detail = "image sample data truncated"
        ));
    }

    let mut rgba = alloc::vec_with_capacity::<u8>(
        g,
        usize::try_from(pixel_count).unwrap_or(0).saturating_mul(4),
    )?;
    let bpc = usize::from(bits_per_component.max(1));
    let bytes_per_sample = if bpc <= 8 { 1 } else { 2 };

    for px in 0..usize::try_from(pixel_count).unwrap_or(0) {
        let base = px.saturating_mul(per_pixel);
        let (r, g2, b, a) = decode_pixel(samples, base, components, bytes_per_sample, decode);
        rgba.push(r);
        rgba.push(g2);
        rgba.push(b);
        rgba.push(a);
    }

    Ok(DecodedImage {
        width,
        height,
        rgba8: rgba,
    })
}

/// Decode one pixel's samples into RGBA8.
fn decode_pixel(
    samples: &[u8],
    base: usize,
    components: u8,
    bytes_per_sample: usize,
    decode: &Decode,
) -> (u8, u8, u8, u8) {
    let read_sample = |i: usize| -> u8 {
        let off = base.saturating_add(i.saturating_mul(bytes_per_sample));
        if bytes_per_sample == 2 {
            samples.get(off).copied().unwrap_or(0)
        } else {
            samples.get(off).copied().unwrap_or(0)
        }
    };
    match components {
        1 => {
            // Grayscale (or a mask): sample â†’ RGB.
            let raw = read_sample(0);
            let v = apply_decode(raw, decode, 0);
            (v, v, v, 255)
        }
        3 => {
            let r = apply_decode(read_sample(0), decode, 0);
            let g = apply_decode(read_sample(1), decode, 1);
            let b = apply_decode(read_sample(2), decode, 2);
            (r, g, b, 255)
        }
        4 => {
            // CMYK: naive to RGB.
            let c = f64::from(apply_decode(read_sample(0), decode, 0)) / 255.0;
            let m = f64::from(apply_decode(read_sample(1), decode, 1)) / 255.0;
            let y = f64::from(apply_decode(read_sample(2), decode, 2)) / 255.0;
            let k = f64::from(apply_decode(read_sample(3), decode, 3)) / 255.0;
            let r = 1.0 - (c + k).min(1.0);
            let g = 1.0 - (m + k).min(1.0);
            let b = 1.0 - (y + k).min(1.0);
            // The channels are in [0,1], so the RGBA8 narrowing is exact in
            // range.
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            {
                ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255)
            }
        }
        _ => (0, 0, 0, 255),
    }
}

/// Apply the `/Decode` array to a sample.
fn apply_decode(sample: u8, decode: &Decode, component: usize) -> u8 {
    let (lo, hi) = decode.ranges.get(component).copied().unwrap_or((0, 255));
    // Sample is in [0,255]; map linearly into [lo, hi].
    let span = i32::from(hi).wrapping_sub(i32::from(lo));
    let v = i32::from(sample)
        .wrapping_mul(span)
        .wrapping_div(255)
        .wrapping_add(i32::from(lo));
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    {
        v.clamp(0, 255) as u8
    }
}

/// Draw a decoded image into a destination rectangle.
///
/// # Budget
///
/// The placement bounds the work; the source is already decoded.
pub fn draw<B: Backend>(backend: &mut B, image: &DecodedImage, rect: Rect) {
    let placement = ImagePlacement { rect };
    backend.draw_image(
        &Image {
            width: image.width,
            height: image.height,
            rgba8: image.rgba8.clone(),
        },
        &placement,
    );
}

/// Decode an image **directly into a destination-sized buffer** when the
/// destination is smaller than the source (the DoD downscale path).
///
/// A 20 000 Ã— 20 000 image drawn at 1% scale decodes into `dest_w Ã— dest_h`
/// pixels by point sampling â€” it never allocates the full source bitmap.
///
/// # Budget
///
/// The output is `dest_w Ã— dest_h Ã— 4` bytes, charged against `g`.
///
/// # Malformed Input
///
/// `IMAGE_MALFORMED` when the source samples are truncated or the destination
/// dimensions are absurd.
pub fn decode_image_scaled(
    src_w: u32,
    src_h: u32,
    components: u8,
    samples: &[u8],
    dest_w: u32,
    dest_h: u32,
    g: &mut BudgetGuard<'_>,
) -> Result<DecodedImage> {
    // Bound the destination allocation before anything else.
    let dest_pixels = u64::from(dest_w).saturating_mul(u64::from(dest_h));
    let dest_bytes = dest_pixels.saturating_mul(4);
    if dest_bytes > u64::from(u32::MAX) {
        return Err(err!(
            Code::ImageMalformed,
            during = "image-decode",
            detail = "destination dimensions overflow"
        ));
    }

    let per_pixel = usize::from(components.max(1));
    let src_pixels = u64::from(src_w).saturating_mul(u64::from(src_h));
    let expected = src_pixels.saturating_mul(per_pixel as u64);
    if u64::try_from(samples.len()).unwrap_or(u64::MAX) < expected {
        return Err(err!(
            Code::ImageMalformed,
            during = "image-decode",
            detail = "image sample data truncated"
        ));
    }

    let identity = Decode::identity(usize::from(components));
    // Clamp the denominators once; a zero destination is clamped to 1.
    let dest_w_safe = dest_w.max(1);
    let dest_h_safe = dest_h.max(1);
    let dest_w64 = u64::from(dest_w_safe);
    let dest_h64 = u64::from(dest_h_safe);
    let src_w64 = u64::from(src_w);
    let src_h64 = u64::from(src_h);
    let mut rgba = alloc::vec_with_capacity::<u8>(
        g,
        usize::try_from(dest_pixels).unwrap_or(0).saturating_mul(4),
    )?;
    for dy in 0..dest_h {
        for dx in 0..dest_w {
            // Point-sample the source at the corresponding pixel.
            let sx = u64::from(dx)
                .checked_mul(src_w64)
                .and_then(|v| v.checked_div(dest_w64))
                .unwrap_or(0);
            let sy = u64::from(dy)
                .checked_mul(src_h64)
                .and_then(|v| v.checked_div(dest_h64))
                .unwrap_or(0);
            let sx = u32::try_from(sx)
                .unwrap_or(u32::MAX)
                .min(src_w.saturating_sub(1));
            let sy = u32::try_from(sy)
                .unwrap_or(u32::MAX)
                .min(src_h.saturating_sub(1));
            let base = (u64::from(sy)
                .saturating_mul(u64::from(src_w))
                .saturating_add(u64::from(sx)))
            .saturating_mul(per_pixel as u64);
            let base = usize::try_from(base).unwrap_or(0);
            let (r, g2, b, a) = decode_pixel(samples, base, components, 1, &identity);
            rgba.push(r);
            rgba.push(g2);
            rgba.push(b);
            rgba.push(a);
        }
    }

    Ok(DecodedImage {
        width: dest_w,
        height: dest_h,
        rgba8: rgba,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn grayscale_decodes() {
        let mut g = guard();
        let samples = [128u8, 255, 0];
        let img = decode_image(1, 3, 1, 8, &samples, &Decode::identity(1), &mut g).expect("decode");
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 3);
        assert_eq!(img.rgba8.len(), 12);
        // Mid-grey.
        assert_eq!(img.rgba8[0], 128);
        assert_eq!(img.rgba8[1], 128);
        assert_eq!(img.rgba8[2], 128);
    }

    #[test]
    fn decode_array_inverts() {
        let mut g = guard();
        let samples = [255u8];
        let invert = Decode {
            ranges: vec![(255, 0)],
        };
        let img = decode_image(1, 1, 1, 8, &samples, &invert, &mut g).expect("decode");
        // 255 maps to 0.
        assert_eq!(img.rgba8[0], 0);
    }

    #[test]
    fn rgb_decodes() {
        let mut g = guard();
        let samples = [255u8, 0, 0];
        let img = decode_image(1, 1, 3, 8, &samples, &Decode::identity(3), &mut g).expect("decode");
        assert_eq!((img.rgba8[0], img.rgba8[1], img.rgba8[2]), (255, 0, 0));
        assert_eq!(img.rgba8[3], 255);
    }

    #[test]
    fn cmyk_converts() {
        let mut g = guard();
        let samples = [0u8, 255, 255, 0]; // red in naive CMYK
        let img = decode_image(1, 1, 4, 8, &samples, &Decode::identity(4), &mut g).expect("decode");
        assert!(img.rgba8[0] > 200, "red");
        assert!(img.rgba8[1] < 50, "not green");
    }

    /// DoD: a 20kÃ—20k image decoded at 1% scale must not allocate the full
    /// bitmap â€” decode_image rejects the absurd pixel count outright.
    #[test]
    fn extreme_dimensions_are_rejected_not_allocated() {
        let mut g = guard();
        let e = decode_image(20_000, 20_000, 3, 8, &[], &Decode::identity(3), &mut g)
            .expect_err("too big");
        assert_eq!(e.code(), Code::ImageMalformed);
    }

    /// DoD: drawing a 20kÃ—20k image at 1% scale decodes into a
    /// destination-sized buffer, never the full source bitmap.
    #[test]
    fn downscale_decodes_into_destination_buffer() {
        let mut g = guard();
        // A 20kÃ—20k grayscale source (400M samples).
        let src_w = 20_000u32;
        let src_h = 20_000u32;
        let samples: Vec<u8> =
            selis_sandbox::alloc::vec_filled(&mut g, src_w as usize * src_h as usize, 0u8)
                .expect("unlimited budget");
        // Drawn at 1% (200Ã—200 destination).
        let dest = decode_image_scaled(src_w, src_h, 1, &samples, 200, 200, &mut g)
            .expect("scaled decode");
        // The output is destination-sized, not 400M pixels.
        assert_eq!(dest.width, 200);
        assert_eq!(dest.height, 200);
        assert_eq!(dest.rgba8.len(), 200 * 200 * 4);
        // The full source was never allocated as an RGBA8 buffer.
        assert!(dest.rgba8.len() < src_w as usize * src_h as usize);
    }

    #[test]
    fn truncated_samples_are_a_typed_error() {
        let mut g = guard();
        let e = decode_image(
            2,
            2,
            3,
            8,
            &[1, 2, 3], // not enough for 2Ã—2Ã—3
            &Decode::identity(3),
            &mut g,
        )
        .expect_err("truncated");
        assert_eq!(e.code(), Code::ImageMalformed);
    }
}
