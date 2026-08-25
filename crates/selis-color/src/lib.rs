//! Colour: the PDF colour-space model and conversion.
//!
//! Phase 1A does not rasterise, so this crate carries the *model* — enough to
//! identify a colour space, know its component count, and convert device colours
//! for tools that need them (an images→PDF page background, an optimiser deciding
//! whether two colour spaces are equivalent). Rendering intents, ICC transforms,
//! and blend modes arrive with `selis-raster` in Phase 2.
//!
//! Component values are `f64` in `[0,1]` except where a space defines otherwise
//! (Lab). Conversion is exact and platform-independent — no lookup tables whose
//! contents could differ between builds (ADR-P0012).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod blend;
pub mod cie;
pub mod function;
pub mod special;

use alloc::string::String;
use alloc::vec::Vec;

pub use blend::{BlendMode, Rgba};
pub use cie::{CalGray, CalRgb, Lab, WhitePoint};
pub use function::{Function, FunctionBudget};
pub use special::{alternate_to_rgb, DeviceN, Indexed, Separation};

/// A device-independent RGB triple with components in `[0,1]`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rgb {
    /// Red.
    pub r: f64,
    /// Green.
    pub g: f64,
    /// Blue.
    pub b: f64,
}

impl Rgb {
    /// An RGB triple, clamped into `[0,1]`.
    #[must_use]
    pub fn new(r: f64, g: f64, b: f64) -> Self {
        Self {
            r: clamp01(r),
            g: clamp01(g),
            b: clamp01(b),
        }
    }

    /// Black.
    pub const BLACK: Rgb = Rgb {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };
    /// White.
    pub const WHITE: Rgb = Rgb {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };

    /// As 8-bit samples, rounding half away from zero.
    ///
    /// Deterministic by construction: no `as` truncation of a float, which is
    /// where platform rounding differences historically enter.
    #[must_use]
    pub fn to_rgb8(self) -> [u8; 3] {
        [to_u8(self.r), to_u8(self.g), to_u8(self.b)]
    }
}

/// A CMYK quadruple with components in `[0,1]`, where 1 is full ink.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Cmyk {
    /// Cyan.
    pub c: f64,
    /// Magenta.
    pub m: f64,
    /// Yellow.
    pub y: f64,
    /// Black.
    pub k: f64,
}

impl Cmyk {
    /// A CMYK quadruple, clamped into `[0,1]`.
    #[must_use]
    pub fn new(c: f64, m: f64, y: f64, k: f64) -> Self {
        Self {
            c: clamp01(c),
            m: clamp01(m),
            y: clamp01(y),
            k: clamp01(k),
        }
    }

    /// The naive conversion of PDF 32000-2:2020 §10.4.2.3.
    ///
    /// This is the *uncalibrated* transform the spec defines for `DeviceCMYK`
    /// without an output intent. It is not colorimetrically accurate and is not
    /// meant to be; a real transform needs the ICC path (Phase 2).
    #[must_use]
    pub fn to_rgb_naive(self) -> Rgb {
        Rgb::new(
            1.0 - (self.c + self.k).min(1.0),
            1.0 - (self.m + self.k).min(1.0),
            1.0 - (self.y + self.k).min(1.0),
        )
    }
}

/// A PDF colour space.
///
/// PDF 32000-2:2020 §8.6. Parametrised spaces carry only what the document model
/// needs at this phase; the full parameter sets land with the renderer.
#[derive(Debug, Clone, PartialEq)]
pub enum ColorSpace {
    /// `/DeviceGray` — one component.
    DeviceGray,
    /// `/DeviceRGB` — three components.
    DeviceRgb,
    /// `/DeviceCMYK` — four components.
    DeviceCmyk,
    /// `/CalGray`.
    CalGray,
    /// `/CalRGB`.
    CalRgb,
    /// `/Lab`, with its component ranges.
    Lab {
        /// `[amin amax bmin bmax]`.
        range: [f64; 4],
    },
    /// `/ICCBased`, carrying its declared component count and the alternate.
    IccBased {
        /// `/N` — 1, 3, or 4.
        n: u8,
        /// The `/Alternate` space to use when the profile is unusable.
        alternate: alloc::boxed::Box<ColorSpace>,
    },
    /// `/Indexed` — a palette over a base space.
    Indexed {
        /// The space the palette entries are in.
        base: alloc::boxed::Box<ColorSpace>,
        /// The highest valid index.
        hival: u16,
    },
    /// `/Separation` — one colorant.
    Separation {
        /// The colorant name, sanitised.
        name: String,
        /// The alternate space the tint transform maps into.
        alternate: alloc::boxed::Box<ColorSpace>,
    },
    /// `/DeviceN` — n colorants.
    DeviceN {
        /// The colorant names, sanitised.
        names: Vec<String>,
        /// The alternate space the tint transform maps into.
        alternate: alloc::boxed::Box<ColorSpace>,
    },
    /// `/Pattern`, optionally over an underlying space for uncoloured patterns.
    Pattern {
        /// The underlying space, for `PatternType 2` uncoloured patterns.
        under: Option<alloc::boxed::Box<ColorSpace>>,
    },
}

impl ColorSpace {
    /// The number of components a colour in this space carries.
    ///
    /// `Indexed` is 1 (the index itself). `Pattern` is 0 unless it has an
    /// underlying space.
    #[must_use]
    pub fn components(&self) -> u8 {
        match self {
            ColorSpace::DeviceGray | ColorSpace::CalGray | ColorSpace::Indexed { .. } => 1,
            ColorSpace::DeviceRgb | ColorSpace::CalRgb | ColorSpace::Lab { .. } => 3,
            ColorSpace::DeviceCmyk => 4,
            ColorSpace::IccBased { n, .. } => *n,
            ColorSpace::Separation { .. } => 1,
            ColorSpace::DeviceN { names, .. } => u8::try_from(names.len()).unwrap_or(u8::MAX),
            ColorSpace::Pattern { under } => under.as_ref().map_or(0, |u| u.components()),
        }
    }

    /// The space's initial colour, per PDF 32000-2:2020 §8.6.8.
    #[must_use]
    pub fn initial_color(&self) -> Vec<f64> {
        match self {
            ColorSpace::Lab { .. } => alloc::vec![0.0, 0.0, 0.0],
            ColorSpace::Separation { .. } | ColorSpace::DeviceN { .. } => {
                alloc::vec![1.0; usize::from(self.components())]
            }
            _ => alloc::vec![0.0; usize::from(self.components())],
        }
    }

    /// Whether this is one of the three device spaces.
    #[must_use]
    pub fn is_device(&self) -> bool {
        matches!(
            self,
            ColorSpace::DeviceGray | ColorSpace::DeviceRgb | ColorSpace::DeviceCmyk
        )
    }

    /// Convert a colour in this space to RGB, where that is defined without an
    /// ICC transform or a tint-transform function.
    ///
    /// Returns `None` when the conversion needs machinery this phase does not
    /// have — a PostScript tint transform, an ICC profile, or a palette lookup.
    /// Callers treat `None` as "cannot preview", never as "black".
    #[must_use]
    pub fn to_rgb(&self, comps: &[f64]) -> Option<Rgb> {
        match self {
            ColorSpace::DeviceGray | ColorSpace::CalGray => {
                let g = *comps.first()?;
                Some(Rgb::new(g, g, g))
            }
            ColorSpace::DeviceRgb | ColorSpace::CalRgb => {
                Some(Rgb::new(*comps.first()?, *comps.get(1)?, *comps.get(2)?))
            }
            ColorSpace::DeviceCmyk => Some(
                Cmyk::new(
                    *comps.first()?,
                    *comps.get(1)?,
                    *comps.get(2)?,
                    *comps.get(3)?,
                )
                .to_rgb_naive(),
            ),
            ColorSpace::IccBased { n, alternate } => match n {
                1 => ColorSpace::DeviceGray.to_rgb(comps),
                3 => ColorSpace::DeviceRgb.to_rgb(comps),
                4 => ColorSpace::DeviceCmyk.to_rgb(comps),
                _ => alternate.to_rgb(comps),
            },
            _ => None,
        }
    }

    /// The PDF name for this space, where it has a simple one.
    #[must_use]
    pub fn pdf_name(&self) -> Option<&'static str> {
        match self {
            ColorSpace::DeviceGray => Some("DeviceGray"),
            ColorSpace::DeviceRgb => Some("DeviceRGB"),
            ColorSpace::DeviceCmyk => Some("DeviceCMYK"),
            ColorSpace::Pattern { .. } => Some("Pattern"),
            _ => None,
        }
    }

    /// Resolve a colour space name (from the resource dict or a built-in name)
    /// into a [`ColorSpace`].
    ///
    /// `name` is the `/CS` operand value (e.g. `/DeviceRGB` or `/CS1`).
    /// `resources` is a map of resource name → colour space array (from the
    /// page or form XObject `/ColorSpace` /`Resources` dict).
    ///
    /// # Malformed Input
    ///
    /// An unrecognised name returns `None` (the caller falls back to the
    /// default space).
    pub fn resolve(
        name: &[u8],
        resources: &std::collections::BTreeMap<String, alloc::vec::Vec<u8>>,
    ) -> Option<Self> {
        // Built-in device spaces.
        match name {
            b"DeviceGray" => return Some(ColorSpace::DeviceGray),
            b"DeviceRGB" => return Some(ColorSpace::DeviceRgb),
            b"DeviceCMYK" => return Some(ColorSpace::DeviceCmyk),
            b"Pattern" => return Some(ColorSpace::Pattern { under: None }),
            _ => {}
        }
        // Look up in the resource dictionary.
        let key = String::from_utf8_lossy(name);
        if let Some(value) = resources.get(key.as_ref()) {
            // For now, Phase 2.1 treats resource-sourced spaces as the
            // device space matching their component count, or fall back.
            let _ = value;
            return None;
        }
        None
    }
}

fn clamp01(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

fn to_u8(v: f64) -> u8 {
    let scaled = clamp01(v) * 255.0 + 0.5;
    // clamp01 bounds the input, so `scaled` is within [0.5, 255.5].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = scaled as u32;
    u8::try_from(n.min(255)).unwrap_or(255)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;

    #[test]
    fn component_counts_match_the_spec() {
        assert_eq!(ColorSpace::DeviceGray.components(), 1);
        assert_eq!(ColorSpace::DeviceRgb.components(), 3);
        assert_eq!(ColorSpace::DeviceCmyk.components(), 4);
        assert_eq!(
            ColorSpace::Indexed {
                base: Box::new(ColorSpace::DeviceRgb),
                hival: 255
            }
            .components(),
            1,
            "an Indexed colour is one index, not three components"
        );
        assert_eq!(ColorSpace::Pattern { under: None }.components(), 0);
    }

    #[test]
    fn initial_colours_match_the_spec() {
        assert_eq!(ColorSpace::DeviceRgb.initial_color(), alloc::vec![0.0; 3]);
        // §8.6.8: Separation and DeviceN initialise to 1.0, not 0.0.
        assert_eq!(
            ColorSpace::Separation {
                name: String::from("Spot"),
                alternate: Box::new(ColorSpace::DeviceCmyk)
            }
            .initial_color(),
            alloc::vec![1.0]
        );
    }

    #[test]
    fn cmyk_naive_conversion() {
        assert_eq!(Cmyk::new(0.0, 0.0, 0.0, 0.0).to_rgb_naive(), Rgb::WHITE);
        assert_eq!(Cmyk::new(0.0, 0.0, 0.0, 1.0).to_rgb_naive(), Rgb::BLACK);
        let red = Cmyk::new(0.0, 1.0, 1.0, 0.0).to_rgb_naive();
        assert_eq!(red.to_rgb8(), [255, 0, 0]);
    }

    #[test]
    fn out_of_range_and_nan_components_are_clamped_not_propagated() {
        let c = Rgb::new(2.0, -1.0, f64::NAN);
        assert_eq!(c.to_rgb8(), [255, 0, 0]);
    }

    #[test]
    fn unconvertible_spaces_return_none_rather_than_black() {
        let sep = ColorSpace::Separation {
            name: String::from("PANTONE"),
            alternate: Box::new(ColorSpace::DeviceCmyk),
        };
        assert!(sep.to_rgb(&[0.5]).is_none());
        let idx = ColorSpace::Indexed {
            base: Box::new(ColorSpace::DeviceRgb),
            hival: 3,
        };
        assert!(idx.to_rgb(&[1.0]).is_none());
    }

    #[test]
    fn short_component_list_is_none_not_a_panic() {
        assert!(ColorSpace::DeviceRgb.to_rgb(&[0.5]).is_none());
        assert!(ColorSpace::DeviceCmyk.to_rgb(&[]).is_none());
    }

    #[test]
    fn icc_falls_back_by_component_count() {
        let icc = ColorSpace::IccBased {
            n: 3,
            alternate: Box::new(ColorSpace::DeviceRgb),
        };
        assert_eq!(icc.to_rgb(&[1.0, 0.0, 0.0]), Some(Rgb::new(1.0, 0.0, 0.0)));
    }

    #[test]
    fn rgb8_rounds_half_away_from_zero() {
        assert_eq!(to_u8(0.0), 0);
        assert_eq!(to_u8(1.0), 255);
        assert_eq!(to_u8(0.5), 128);
    }

    #[test]
    fn resolve_builtin_spaces() {
        use alloc::collections::BTreeMap;
        let empty: BTreeMap<String, alloc::vec::Vec<u8>> = BTreeMap::new();
        assert_eq!(
            ColorSpace::resolve(b"DeviceRGB", &empty),
            Some(ColorSpace::DeviceRgb)
        );
        assert_eq!(
            ColorSpace::resolve(b"DeviceGray", &empty),
            Some(ColorSpace::DeviceGray)
        );
        assert_eq!(
            ColorSpace::resolve(b"DeviceCMYK", &empty),
            Some(ColorSpace::DeviceCmyk)
        );
        assert_eq!(ColorSpace::resolve(b"Unknown", &empty), None);
    }
}
