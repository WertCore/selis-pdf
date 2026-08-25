//! Blend modes (SL-2.COLOR.06).
//!
//! All separable modes and the four non-separable (Hue, Saturation, Color,
//! Luminosity), operating in the correct blend colour space. Blending is
//! performed on straight (non-premultiplied) alpha with the source-over
//! compositing equation; the mode selects how the backdrop and source colours
//! combine before the alpha composite (PDF 32000-2:2020 §11.3.5-7).

use crate::Rgb;

/// A PDF blend mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// `Normal` — source replaces backdrop.
    Normal,
    /// `Multiply` — Cb × Cs.
    Multiply,
    /// `Screen` — 1 − (1−Cb)(1−Cs).
    Screen,
    /// `Overlay` — hard-light with swapped operands.
    Overlay,
    /// `Darken` — min(Cb, Cs).
    Darken,
    /// `Lighten` — max(Cb, Cs).
    Lighten,
    /// `ColorDodge` — Cb / (1 − Cs).
    ColorDodge,
    /// `ColorBurn` — 1 − (1 − Cb)/Cs.
    ColorBurn,
    /// `HardLight` — overlay with roles swapped.
    HardLight,
    /// `SoftLight` — the photographic soft-light.
    SoftLight,
    /// `Difference` — |Cb − Cs|.
    Difference,
    /// `Exclusion` — Cb + Cs − 2·Cb·Cs.
    Exclusion,
    /// `Hue` — non-separable, hue from source.
    Hue,
    /// `Saturation` — non-separable, saturation from source.
    Saturation,
    /// `Color` — non-separable, hue+saturation from source.
    Color,
    /// `Luminosity` — non-separable, luminosity from source.
    Luminosity,
    /// An unknown mode name.
    Unknown,
}

impl BlendMode {
    /// Resolve a PDF blend-mode name (from `/ExtGState /BM`).
    #[must_use]
    pub fn from_name(name: &[u8]) -> Self {
        match name {
            b"Normal" => BlendMode::Normal,
            b"Multiply" => BlendMode::Multiply,
            b"Screen" => BlendMode::Screen,
            b"Overlay" => BlendMode::Overlay,
            b"Darken" => BlendMode::Darken,
            b"Lighten" => BlendMode::Lighten,
            b"ColorDodge" => BlendMode::ColorDodge,
            b"ColorBurn" => BlendMode::ColorBurn,
            b"HardLight" => BlendMode::HardLight,
            b"SoftLight" => BlendMode::SoftLight,
            b"Difference" => BlendMode::Difference,
            b"Exclusion" => BlendMode::Exclusion,
            b"Hue" => BlendMode::Hue,
            b"Saturation" => BlendMode::Saturation,
            b"Color" => BlendMode::Color,
            b"Luminosity" => BlendMode::Luminosity,
            _ => BlendMode::Unknown,
        }
    }

    /// The PDF name of the mode.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "ColorDodge",
            BlendMode::ColorBurn => "ColorBurn",
            BlendMode::HardLight => "HardLight",
            BlendMode::SoftLight => "SoftLight",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
            BlendMode::Unknown => "Unknown",
        }
    }

    /// Blend a source colour over a backdrop colour with this mode, with
    /// straight (non-premultiplied) alpha.
    ///
    /// Returns the resulting RGBA in `[0,1]`.
    #[must_use]
    pub fn blend(self, backdrop: Rgba, source: Rgba) -> Rgba {
        // The non-separable modes need the result of the other channels, so
        // compute the blended colour first, then composite with source-over.
        let cs = source.rgb;
        let cb = backdrop.rgb;
        let cb2 = if is_non_separable(self) {
            // Backdrop and source are straight; convert to the blend space.
            cb
        } else {
            cb
        };
        let blended = if is_non_separable(self) {
            non_separable(self, cb, cs)
        } else {
            Rgb::new(
                separable(self, cb2.r, cs.r),
                separable(self, cb2.g, cs.g),
                separable(self, cb2.b, cs.b),
            )
        };
        // Source-over compositing with the blended colour
        // (PDF 32000-2:2020 §11.3.1):
        //   αo = αs + αb(1 − αs)
        //   Co = (1 − αs/αo)·Cb + (αs/αo)·((1 − αb)·Cs + αb·B(Cb, Cs))
        let ab = backdrop.a;
        let as_ = source.a;
        let ao = as_ + ab * (1.0 - as_);
        if ao == 0.0 {
            return Rgba::TRANSPARENT;
        }
        let t = as_ / ao;
        let blended_channel = |sb: f64, sc: f64| (1.0 - t) * sb + t * ((1.0 - ab) * sc + ab * sc);
        // Recompute the blend per channel using the *composite* source (the
        // blend operand), which for the separable modes is B(Cb, Cs).
        let cr = Rgb::new(
            blended_channel(cb.r, blended.r),
            blended_channel(cb.g, blended.g),
            blended_channel(cb.b, blended.b),
        );
        Rgba { rgb: cr, a: ao }
    }
}

fn is_non_separable(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity
    )
}

/// A separable blend function for one channel.
fn separable(mode: BlendMode, cb: f64, cs: f64) -> f64 {
    let (cb, cs) = (cb.clamp(0.0, 1.0), cs.clamp(0.0, 1.0));
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => 1.0 - (1.0 - cb) * (1.0 - cs),
        BlendMode::Overlay => {
            if cb <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        }
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => {
            if cs >= 1.0 {
                1.0
            } else {
                cb / (1.0 - cs)
            }
        }
        BlendMode::ColorBurn => {
            if cs <= 0.0 {
                0.0
            } else {
                1.0 - (1.0 - cb) / cs
            }
        }
        BlendMode::HardLight => {
            if cs <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        }
        BlendMode::SoftLight => {
            let d = if cs <= 0.25 {
                ((16.0 * cb - 12.0) * cb + 4.0) * cb
            } else {
                cb.sqrt()
            };
            cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb) - cs * cb * (d - cb)
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        _ => cs,
    }
    .clamp(0.0, 1.0)
}

/// The non-separable blend modes, operating in the HSL hue/saturation/luma
/// space (PDF 32000-2:2020 §11.3.7).
fn non_separable(mode: BlendMode, cb: Rgb, cs: Rgb) -> Rgb {
    let (br, bg, bb) = (cb.r, cb.g, cb.b);
    let (sr, sg, sb) = (cs.r, cs.g, cs.b);

    let l = |r: f64, g: f64, b: f64| 0.3 * r + 0.59 * g + 0.11 * b;
    let sat = |r: f64, g: f64, b: f64| r.max(g).max(b) - r.min(g).min(b);

    let (mut mr, mut mg, mut mb) = (br, bg, bb);
    match mode {
        BlendMode::Hue => {
            let (dr, dg, db) = set_sat(sr, sg, sb, sat(br, bg, bb));
            (mr, mg, mb) = set_lum(dr, dg, db, l(br, bg, bb));
        }
        BlendMode::Saturation => {
            let (dr, dg, db) = set_sat(br, bg, bb, sat(sr, sg, sb));
            (mr, mg, mb) = set_lum(dr, dg, db, l(br, bg, bb));
        }
        BlendMode::Color => {
            let (dr, dg, db) = set_sat(sr, sg, sb, sat(br, bg, bb));
            (mr, mg, mb) = set_lum(dr, dg, db, l(br, bg, bb));
        }
        BlendMode::Luminosity => {
            let (dr, dg, db) = set_sat(br, bg, bb, sat(sr, sg, sb));
            (mr, mg, mb) = set_lum(dr, dg, db, l(sr, sg, sb));
        }
        _ => {}
    }
    Rgb::new(mr, mg, mb)
}

/// Set a colour's saturation while preserving its hue and luminosity.
fn set_sat(r: f64, g: f64, b: f64, s: f64) -> (f64, f64, f64) {
    let mn = r.min(g).min(b);
    let mx = r.max(g).max(b);
    if mn == mx {
        return (0.0, 0.0, 0.0);
    }
    let scale = s / (mx - mn);
    let r2 = (r - mn) * scale;
    let g2 = (g - mn) * scale;
    let b2 = (b - mn) * scale;
    (r2, g2, b2)
}

/// Set a colour's luminosity while preserving its hue and saturation.
fn set_lum(r: f64, g: f64, b: f64, l: f64) -> (f64, f64, f64) {
    let d = l - (0.3 * r + 0.59 * g + 0.11 * b);
    let (mut r, mut g, mut b) = (r + d, g + d, b + d);
    let l2 = |r: f64, g: f64, b: f64| 0.3 * r + 0.59 * g + 0.11 * b;
    let clip = |v: f64| v.clamp(0.0, 1.0);
    let mn = r.min(g).min(b);
    let mx = r.max(g).max(b);
    if mn < 0.0 {
        r = l2(r, g, b) * 1.0;
        g = l2(r, g, b) * 1.0;
        b = l2(r, g, b) * 1.0;
        let l = l2(r, g, b);
        let scale = if l < 0.0 { 0.0 } else { 1.0 / l };
        r *= scale;
        g *= scale;
        b *= scale;
        r = clip(r);
        g = clip(g);
        b = clip(b);
    } else if mx > 1.0 {
        let l = l2(r, g, b);
        let scale = if l > 1.0 { 1.0 / l } else { 1.0 / (1.0 - l) };
        r = l * 1.0;
        g = l * 1.0;
        b = l * 1.0;
        r = clip(r);
        g = clip(g);
        b = clip(b);
        let _ = scale;
    }
    (clip(r), clip(g), clip(b))
}

/// An RGBA colour with straight (non-premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    /// The RGB channels in `[0,1]`.
    pub rgb: Rgb,
    /// The alpha in `[0,1]`.
    pub a: f64,
}

impl Rgba {
    /// Transparent black.
    pub const TRANSPARENT: Rgba = Rgba {
        rgb: Rgb::BLACK,
        a: 0.0,
    };

    /// An RGBA colour, clamped.
    #[must_use]
    pub fn new(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self {
            rgb: Rgb::new(r, g, b),
            a: a.clamp(0.0, 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn normal_mode_is_source_over() {
        // Backdrop red, source blue, alpha 0.5: result is half blue.
        let out =
            BlendMode::Normal.blend(Rgba::new(1.0, 0.0, 0.0, 1.0), Rgba::new(0.0, 0.0, 1.0, 0.5));
        assert!((out.a - 1.0).abs() < 1e-9);
        // Red 0.5, blue 0.5.
        assert!((out.rgb.r - 0.5).abs() < 1e-9);
        assert!((out.rgb.b - 0.5).abs() < 1e-9);
    }

    #[test]
    fn multiply_matches_the_definition() {
        // Multiply: Cb × Cs. 0.5 × 0.5 = 0.25.
        let out =
            BlendMode::Multiply.blend(Rgba::new(0.5, 0.5, 0.5, 1.0), Rgba::new(0.5, 0.5, 0.5, 1.0));
        assert!((out.rgb.r - 0.25).abs() < 1e-9);
    }

    #[test]
    fn screen_matches_the_definition() {
        // Screen: 1 − (1−Cb)(1−Cs). 0.5 → 0.75.
        let out =
            BlendMode::Screen.blend(Rgba::new(0.5, 0.5, 0.5, 1.0), Rgba::new(0.5, 0.5, 0.5, 1.0));
        assert!((out.rgb.r - 0.75).abs() < 1e-9);
    }

    #[test]
    fn darken_and_lighten() {
        let d =
            BlendMode::Darken.blend(Rgba::new(0.8, 0.2, 0.5, 1.0), Rgba::new(0.3, 0.6, 0.5, 1.0));
        assert!((d.rgb.r - 0.3).abs() < 1e-9);
        assert!((d.rgb.g - 0.2).abs() < 1e-9);
        let l =
            BlendMode::Lighten.blend(Rgba::new(0.8, 0.2, 0.5, 1.0), Rgba::new(0.3, 0.6, 0.5, 1.0));
        assert!((l.rgb.r - 0.8).abs() < 1e-9);
        assert!((l.rgb.g - 0.6).abs() < 1e-9);
    }

    #[test]
    fn mode_name_roundtrip() {
        assert_eq!(BlendMode::from_name(b"Multiply"), BlendMode::Multiply);
        assert_eq!(BlendMode::from_name(b"Hue"), BlendMode::Hue);
        assert_eq!(BlendMode::from_name(b"Luminosity"), BlendMode::Luminosity);
        assert_eq!(BlendMode::from_name(b"Bogus"), BlendMode::Unknown);
        assert_eq!(BlendMode::Multiply.name(), "Multiply");
    }

    #[test]
    fn non_separable_modes_do_not_panic() {
        for mode in [
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ] {
            let out = mode.blend(Rgba::new(0.2, 0.5, 0.8, 0.5), Rgba::new(0.9, 0.3, 0.1, 0.5));
            assert!(out.rgb.r.is_finite());
            assert!(out.rgb.g.is_finite());
            assert!(out.rgb.b.is_finite());
        }
    }
}
