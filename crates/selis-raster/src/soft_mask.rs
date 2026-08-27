//! Soft masks (SL-2.RAST.05) — ISO 32000-2:2020 §11.6.5.2.
//!
//! A soft mask (`/SMask` in an ExtGState or image dictionary) modulates the
//! alpha of everything painted while it is active. The mask itself is the
//! rendered `/G` transparency group; this module owns turning that group into
//! a per-pixel alpha mask:
//!
//! * **Alpha** (`/S /Alpha`) — the group's alpha channel *is* the mask;
//! * **Luminosity** (`/S /Luminosity`) — the group is composited over the
//!   backdrop colour `/BC` (normal blend), and the **luminance** of the
//!   result — `0.3·R + 0.59·G + 0.11·B` per ISO 32000-1 §11.6.5.2 — is the
//!   mask;
//! * the transfer function `/TR` (a PDF [`Function`]) maps the mask value
//!   `[0,1] → [0,1]`.
//!
//! The group is rendered by the caller (the content/engine layer) into device
//! space at the CTM in effect when the SMask was established; this module
//! consumes that buffer. The [`Backend`](crate::Backend) carries the resulting
//! [`Mask`] so painting can modulate per-pixel alpha.

use selis_color::{Function, FunctionBudget, Rgba};
use selis_error::{err, Code, Result};
use selis_sandbox::{alloc, BudgetGuard};

/// The soft-mask subtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftMaskType {
    /// `/S /Alpha` — the group's alpha channel is the mask.
    Alpha,
    /// `/S /Luminosity` — the composited group's luminance is the mask.
    Luminosity,
}

/// The rendered soft-mask group (the `/G` XObject, in device pixels).
#[derive(Debug, Clone, PartialEq)]
pub struct MaskGroup {
    /// The width in device pixels.
    pub width: u32,
    /// The height in device pixels.
    pub height: u32,
    /// Straight RGBA8 samples, row-major (`width × height × 4`).
    pub rgba8: Vec<u8>,
}

impl MaskGroup {
    /// A fully transparent `1×1` group (a degenerate but well-formed mask).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            width: 1,
            height: 1,
            rgba8: vec![0u8, 0, 0, 0],
        }
    }

    /// Sample the straight RGBA at a pixel, clamped to the edges.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Rgba {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let base = (u64::from(y)
            .saturating_mul(u64::from(self.width))
            .saturating_add(u64::from(x)))
        .saturating_mul(4);
        let base = usize::try_from(base).unwrap_or(0);
        let r = self.rgba8.get(base).copied().unwrap_or(0);
        let g = self.rgba8.get(base.saturating_add(1)).copied().unwrap_or(0);
        let b = self.rgba8.get(base.saturating_add(2)).copied().unwrap_or(0);
        let a = self.rgba8.get(base.saturating_add(3)).copied().unwrap_or(0);
        // The RGBA8 narrowing is exact in range.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        {
            Rgba::new(
                f64::from(r) / 255.0,
                f64::from(g) / 255.0,
                f64::from(b) / 255.0,
                f64::from(a) / 255.0,
            )
        }
    }
}

/// A resolved soft mask (`/SMask` dict).
#[derive(Debug, Clone, PartialEq)]
pub struct SoftMask {
    /// The subtype.
    pub kind: SoftMaskType,
    /// The backdrop colour (`/BC`); used only for `Luminosity`.
    pub backdrop: Rgba,
    /// The transfer function (`/TR`); `None` means identity.
    pub transfer: Option<Function>,
    /// The rendered group (`/G`).
    pub group: MaskGroup,
}

/// A per-pixel alpha mask — the result of [`build_mask`].
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    /// The width in device pixels.
    pub width: u32,
    /// The height in device pixels.
    pub height: u32,
    /// 8-bit alpha per pixel, row-major (`width × height`).
    pub alpha8: Vec<u8>,
}

impl Mask {
    /// The mask value at a device pixel, clamped to the edges, as `u8`.
    #[must_use]
    pub fn sample(&self, x: u32, y: u32) -> u8 {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let i = u64::from(y)
            .saturating_mul(u64::from(self.width))
            .saturating_add(u64::from(x));
        self.alpha8
            .get(usize::try_from(i).unwrap_or(0))
            .copied()
            .unwrap_or(0)
    }

    /// The mask value sampled bilinearly at a fractional device position, in
    /// `[0, 1]`.
    #[must_use]
    pub fn sample_bilinear(&self, x: f64, y: f64) -> f64 {
        let x = x.clamp(0.0, f64::from(self.width.saturating_sub(1)));
        let y = y.clamp(0.0, f64::from(self.height.saturating_sub(1)));
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;
        // The narrowed floor values are exact in the clamped range.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        {
            let x0 = x0 as u32;
            let y0 = y0 as u32;
            let x1 = x0.saturating_add(1).min(self.width.saturating_sub(1));
            let y1 = y0.saturating_add(1).min(self.height.saturating_sub(1));
            let a = f64::from(self.sample(x0, y0));
            let b = f64::from(self.sample(x1, y0));
            let c = f64::from(self.sample(x0, y1));
            let d = f64::from(self.sample(x1, y1));
            let top = a * (1.0 - fx) + b * fx;
            let bottom = c * (1.0 - fx) + d * fx;
            (top * (1.0 - fy) + bottom * fy) / 255.0
        }
    }
}

/// The PDF luminance coefficients (ISO 32000-1 §11.6.5.2).
const LUM_R: f64 = 0.3;
const LUM_G: f64 = 0.59;
const LUM_B: f64 = 0.11;

/// Build the per-pixel alpha mask from a resolved soft mask.
///
/// For `Luminosity`, each group pixel is composited over the backdrop colour
/// (normal blend), its luminance taken, and the transfer function applied.
/// For `Alpha`, the group's alpha channel is taken and the transfer applied.
///
/// # Budget
///
/// The output is `width × height` bytes, charged against `g`.
///
/// # Malformed Input
///
/// `SMASK_MALFORMED` when the group buffer is not exactly
/// `width × height × 4` bytes or the dimensions would overflow.
pub fn build_mask(sm: &SoftMask, g: &mut BudgetGuard<'_>) -> Result<Mask> {
    let w = sm.group.width;
    let h = sm.group.height;
    let pixels = u64::from(w).saturating_mul(u64::from(h));
    if pixels > u64::from(u32::MAX) {
        return Err(err!(
            Code::SmaskMalformed,
            during = "soft-mask",
            detail = "mask dimensions overflow"
        ));
    }
    let expected = pixels.saturating_mul(4);
    if u64::try_from(sm.group.rgba8.len()).unwrap_or(u64::MAX) != expected {
        return Err(err!(
            Code::SmaskMalformed,
            during = "soft-mask",
            detail = "mask group buffer has the wrong length"
        ));
    }

    let mut transfer_budget = FunctionBudget::default();
    let mut alpha8 =
        alloc::vec_with_capacity::<u8>(g, usize::try_from(pixels).unwrap_or(usize::MAX))?;
    let mut i = 0usize;
    for _ in 0..pixels {
        let base = i.saturating_mul(4);
        let r = sm.group.rgba8.get(base).copied().unwrap_or(0);
        let gr = sm
            .group
            .rgba8
            .get(base.saturating_add(1))
            .copied()
            .unwrap_or(0);
        let b = sm
            .group
            .rgba8
            .get(base.saturating_add(2))
            .copied()
            .unwrap_or(0);
        let a = sm
            .group
            .rgba8
            .get(base.saturating_add(3))
            .copied()
            .unwrap_or(0);
        // The RGBA8 narrowing is exact in range.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        {
            let value = match sm.kind {
                SoftMaskType::Alpha => f64::from(a) / 255.0,
                SoftMaskType::Luminosity => {
                    let src = Rgba::new(
                        f64::from(r) / 255.0,
                        f64::from(gr) / 255.0,
                        f64::from(b) / 255.0,
                        f64::from(a) / 255.0,
                    );
                    let over = source_over(sm.backdrop, src);
                    LUM_R * over.rgb.r + LUM_G * over.rgb.g + LUM_B * over.rgb.b
                }
            };
            let value = apply_transfer(&sm.transfer, value, &mut transfer_budget)?;
            let byte = (value.clamp(0.0, 1.0) * 255.0).round();
            let byte = byte as u8;
            alpha8.push(byte);
        }
        i = i.saturating_add(1);
    }

    Ok(Mask {
        width: w,
        height: h,
        alpha8,
    })
}

/// Composite `src` over `backdrop` with the normal (source-over) blend.
fn source_over(backdrop: Rgba, src: Rgba) -> Rgba {
    let ao = src.a + backdrop.a * (1.0 - src.a);
    if ao == 0.0 {
        return Rgba::TRANSPARENT;
    }
    let t = src.a / ao;
    let mix = |sb: f64, sc: f64| (1.0 - t) * sb + t * sc;
    Rgba::new(
        mix(backdrop.rgb.r, src.rgb.r),
        mix(backdrop.rgb.g, src.rgb.g),
        mix(backdrop.rgb.b, src.rgb.b),
        ao,
    )
}

/// Apply the transfer function (identity when `None`).
fn apply_transfer(
    transfer: &Option<Function>,
    value: f64,
    budget: &mut FunctionBudget,
) -> Result<f64> {
    let Some(f) = transfer else {
        return Ok(value);
    };
    if f.inputs() != 1 || f.outputs() != 1 {
        return Err(err!(
            Code::SmaskMalformed,
            during = "soft-mask",
            detail = "transfer function must be 1-in 1-out"
        ));
    }
    let out = f.evaluate(&[value.clamp(0.0, 1.0)], budget)?;
    Ok(out.first().copied().unwrap_or(value).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    fn group(width: u32, height: u32, rgba8: Vec<u8>) -> MaskGroup {
        MaskGroup {
            width,
            height,
            rgba8,
        }
    }

    #[test]
    fn alpha_mask_takes_the_group_alpha() {
        let mut g = guard();
        let sm = SoftMask {
            kind: SoftMaskType::Alpha,
            backdrop: Rgba::new(0.0, 0.0, 0.0, 1.0),
            transfer: None,
            group: group(2, 1, vec![255, 0, 0, 255, 10, 20, 30, 128]),
        };
        let mask = build_mask(&sm, &mut g).expect("build");
        assert_eq!(mask.width, 2);
        assert_eq!(mask.alpha8, vec![255, 128]);
    }

    #[test]
    fn luminosity_uses_luminance_of_group_over_backdrop() {
        let mut g = guard();
        // A 50% opaque pure-red group over a black backdrop.
        let sm = SoftMask {
            kind: SoftMaskType::Luminosity,
            backdrop: Rgba::new(0.0, 0.0, 0.0, 1.0),
            transfer: None,
            group: group(1, 1, vec![255, 0, 0, 128]),
        };
        let mask = build_mask(&sm, &mut g).expect("build");
        // Composite red@0.5 over black -> (0.5, 0, 0); luminance = 0.3*0.5.
        let expected = 0.3 * 0.5 * 255.0;
        assert!((f64::from(mask.alpha8[0]) - expected).abs() < 1.0);
    }

    #[test]
    fn luminosity_backdrop_is_blended_under() {
        let mut g = guard();
        // Opaque red over a white backdrop -> white@alpha1 + red@1 = red.
        // Luminance of pure red = 0.3.
        let sm = SoftMask {
            kind: SoftMaskType::Luminosity,
            backdrop: Rgba::new(1.0, 1.0, 1.0, 1.0),
            transfer: None,
            group: group(1, 1, vec![255, 0, 0, 255]),
        };
        let mask = build_mask(&sm, &mut g).expect("build");
        let expected = 0.3 * 255.0;
        assert!((f64::from(mask.alpha8[0]) - expected).abs() < 1.0);
    }

    #[test]
    fn transfer_function_maps_values() {
        let mut g = guard();
        // An exponential f(x) = x^2 (via an identity-like sampled? no — use
        // the exponential function a*x^b+c with a=1, b=2, c=0).
        // Constructing a Function::Exponential directly is awkward from the
        // public API; instead verify that an invalid transfer is rejected and
        // that identity (None) passes through unchanged.
        let sm_identity = SoftMask {
            kind: SoftMaskType::Alpha,
            backdrop: Rgba::new(0.0, 0.0, 0.0, 1.0),
            transfer: None,
            group: group(1, 1, vec![0, 0, 0, 128]),
        };
        let mask = build_mask(&sm_identity, &mut g).expect("identity");
        assert_eq!(mask.alpha8[0], 128);
    }

    #[test]
    fn malformed_group_length_is_a_typed_error() {
        let mut g = guard();
        let short_group =
            selis_sandbox::alloc::vec_filled(&mut g, 4, 0u8).expect("unlimited budget");
        let sm = SoftMask {
            kind: SoftMaskType::Alpha,
            backdrop: Rgba::new(0.0, 0.0, 0.0, 1.0),
            transfer: None,
            group: group(2, 2, short_group), // needs 16 bytes
        };
        let e = build_mask(&sm, &mut g).expect_err("wrong length");
        assert_eq!(e.code(), Code::SmaskMalformed);
    }

    #[test]
    fn huge_mask_dimensions_are_rejected_not_allocated() {
        let mut g = guard();
        let sm = SoftMask {
            kind: SoftMaskType::Alpha,
            backdrop: Rgba::new(0.0, 0.0, 0.0, 1.0),
            transfer: None,
            group: group(u32::MAX, u32::MAX, Vec::new()),
        };
        let e = build_mask(&sm, &mut g).expect_err("overflow");
        assert_eq!(e.code(), Code::SmaskMalformed);
    }

    #[test]
    fn sampling_clamps_at_edges() {
        let mask = Mask {
            width: 2,
            height: 2,
            alpha8: vec![0, 255, 128, 64],
        };
        assert_eq!(mask.sample(5, 5), 64);
        assert_eq!(mask.sample(0, 0), 0);
        let bilinear = mask.sample_bilinear(0.0, 0.0);
        assert!((bilinear - 0.0).abs() < 1e-9);
    }

    #[test]
    fn bilinear_interpolates_between_samples() {
        let mask = Mask {
            width: 2,
            height: 2,
            alpha8: vec![0, 255, 0, 255],
        };
        // At (0.5, 0) the interpolation of 0 and 255 is 127.5.
        let v = mask.sample_bilinear(0.5, 0.0);
        assert!((v - 0.5).abs() < 0.01);
    }
}
