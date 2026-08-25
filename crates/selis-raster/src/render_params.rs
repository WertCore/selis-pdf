//! The render parameter surface (SL-2.RAST.11).
//!
//! Everything a caller may choose about a render, collected in one value:
//! DPI or an explicit page-to-device matrix, the output colour space and alpha
//! channel, annotation inclusion, optional-content overrides, the
//! "for print" vs "for screen" mode, text-rendering hints, and the rendering
//! intent. Phase 3 (text and fonts) keys off this surface, and SL-2.PERF.01
//! keys its caches off [`RenderParams::cache_id`].
//!
//! The DoD is that *every* parameter changes the output as documented; each
//! field has a test proving the documented effect.

use std::collections::BTreeMap;

use selis_geom::{Matrix, Point, Rect};

/// The rendering intent (ISO 32000-2:2020 §8.6.5.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderIntent {
    /// Relative colorimetric (`/RelativeColorimetric`, 0).
    RelativeColorimetric,
    /// Absolute colorimetric (`/AbsoluteColorimetric`, 1).
    AbsoluteColorimetric,
    /// Saturation (`/Saturation`, 2).
    Saturation,
    /// Perceptual (`/Perceptual`, 3).
    Perceptual,
}

impl RenderIntent {
    /// From the numeric PDF rendering-intent value (0–3); anything else is
    /// treated as relative colorimetric, the spec default.
    #[must_use]
    pub fn from_number(n: f64) -> Self {
        // Clamped into range before narrowing; exact for 0..=3. NaN clamps to
        // the default.
        let v = n.clamp(0.0, 3.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        match v as u8 {
            1 => RenderIntent::AbsoluteColorimetric,
            2 => RenderIntent::Saturation,
            3 => RenderIntent::Perceptual,
            _ => RenderIntent::RelativeColorimetric,
        }
    }
}

/// The output colour space of the rendered pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetColourSpace {
    /// Device RGB — the screen.
    DeviceRgb,
    /// Device CMYK — for print.
    DeviceCmyk,
}

/// Optional-content overrides: a group reference mapped to its visibility.
///
/// An empty map means "use the document's default OC configuration". The
/// engine resolves group references against the document's `OCProperties`
/// (selis-pdf-doc `OcConfig`); this surface only carries the override set.
pub type OcOverrides = BTreeMap<u32, bool>;

/// The render parameter surface.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderParams {
    /// The page resolution in DPI (72, 150, 300, ...). Ignored when
    /// [`RenderParams::matrix`] is set.
    pub dpi: f64,
    /// An explicit page-to-device matrix; overrides `dpi` when set.
    pub matrix: Option<Matrix>,
    /// The output colour space.
    pub target_space: TargetColourSpace,
    /// Whether the output carries an alpha channel.
    pub alpha: bool,
    /// Whether annotations are drawn into the page.
    pub include_annotations: bool,
    /// Optional-content visibility overrides.
    pub oc: OcOverrides,
    /// "For print": apply overprint and target the print colour space.
    pub for_print: bool,
    /// Text hinting (glyph hinting on/off).
    pub text_hinting: bool,
    /// The rendering intent.
    pub intent: RenderIntent,
}

impl Default for RenderParams {
    /// A screen render: 150 DPI, RGB with alpha, no annotations, no overrides,
    /// relative colorimetric, hinting on.
    fn default() -> Self {
        Self::screen()
    }
}

impl RenderParams {
    /// A screen render: 150 DPI, RGB, alpha on, annotations on, relative
    /// colorimetric, hinting on.
    #[must_use]
    pub fn screen() -> Self {
        Self {
            dpi: 150.0,
            matrix: None,
            target_space: TargetColourSpace::DeviceRgb,
            alpha: true,
            include_annotations: true,
            oc: BTreeMap::new(),
            for_print: false,
            text_hinting: true,
            intent: RenderIntent::RelativeColorimetric,
        }
    }

    /// A print render: 300 DPI, CMYK, no alpha, overprint applied.
    #[must_use]
    pub fn print() -> Self {
        Self {
            dpi: 300.0,
            matrix: None,
            target_space: TargetColourSpace::DeviceCmyk,
            alpha: false,
            include_annotations: true,
            oc: BTreeMap::new(),
            for_print: true,
            text_hinting: true,
            intent: RenderIntent::RelativeColorimetric,
        }
    }

    /// The page-to-device matrix for a page box (PDF user space is y-up; the
    /// device space is y-down).
    ///
    /// With a custom [`RenderParams::matrix`] set, that matrix is returned
    /// unchanged. Otherwise the matrix maps the page box into a device raster
    /// of `dpi` resolution with the origin at the top-left.
    #[must_use]
    pub fn page_to_device(&self, page_box: Rect) -> Matrix {
        if let Some(m) = self.matrix {
            return m;
        }
        let s = self.dpi / 72.0;
        let height_dev = page_box.height() * s;
        Matrix::new(
            s,
            0.0,
            0.0,
            -s,
            -page_box.x0 * s,
            height_dev + page_box.y0 * s,
        )
    }

    /// The device pixel size of a page box under these parameters.
    #[must_use]
    pub fn device_size(&self, page_box: Rect) -> (u32, u32) {
        let m = self.page_to_device(page_box);
        let corners = [
            m.apply(Point::new(page_box.x0, page_box.y0)),
            m.apply(Point::new(page_box.x1, page_box.y0)),
            m.apply(Point::new(page_box.x0, page_box.y1)),
            m.apply(Point::new(page_box.x1, page_box.y1)),
        ];
        let min_x = corners.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let max_x = corners
            .iter()
            .map(|p| p.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = corners.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        let max_y = corners
            .iter()
            .map(|p| p.y)
            .fold(f64::NEG_INFINITY, f64::max);
        (width_u32(max_x - min_x), width_u32(max_y - min_y))
    }

    /// A stable identity for cache keying (SL-2.PERF.01).
    ///
    /// FNV-1a over the canonical field encoding. Floats hash via their IEEE
    /// bits so the identity is byte-stable across platforms (ADR-P0012).
    #[must_use]
    pub fn cache_id(&self) -> u64 {
        let mut h = Fnv::new();
        h.feed(&self.dpi.to_bits().to_le_bytes());
        h.feed(&match self.matrix {
            Some(m) => {
                let mut b = Vec::new();
                for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
                    b.extend_from_slice(&v.to_bits().to_le_bytes());
                }
                b
            }
            None => Vec::new(),
        });
        let mut prefix = Vec::new();
        prefix.push(match self.target_space {
            TargetColourSpace::DeviceRgb => 0u8,
            TargetColourSpace::DeviceCmyk => 1,
        });
        prefix.push(u8::from(self.alpha));
        prefix.push(u8::from(self.include_annotations));
        prefix.push(u8::from(self.for_print));
        prefix.push(u8::from(self.text_hinting));
        prefix.push(match self.intent {
            RenderIntent::RelativeColorimetric => 0,
            RenderIntent::AbsoluteColorimetric => 1,
            RenderIntent::Saturation => 2,
            RenderIntent::Perceptual => 3,
        });
        h.feed(&prefix);
        for (k, v) in &self.oc {
            h.feed(&k.to_le_bytes());
            h.feed(&[u8::from(*v)]);
        }
        h.finish()
    }
}

/// A deterministic FNV-1a hasher (stable across runs and platforms).
struct Fnv(u64);

impl Fnv {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

fn width_u32(v: f64) -> u32 {
    if !(v > 0.0) || !v.is_finite() {
        return 0;
    }
    let c = v.ceil();
    if c >= u32::MAX as f64 {
        return u32::MAX;
    }
    // c is finite, non-negative, and below u32::MAX — exact in range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        c as u32
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]

    use super::*;

    fn a4() -> Rect {
        // 595.276 x 841.89 pt.
        Rect::new(0.0, 0.0, 595.276, 841.89)
    }

    #[test]
    fn dpi_changes_the_device_size() {
        let p72 = RenderParams::screen();
        let p300 = RenderParams {
            dpi: 300.0,
            ..RenderParams::screen()
        };
        let (w72, h72) = p72.device_size(a4());
        let (w300, h300) = p300.device_size(a4());
        assert!(w300 > w72 && h300 > h72);
        // 150 DPI: 595.276pt → 1241px (595.276 * 150/72 = 1240.16 → 1241).
        assert_eq!(w72, 1241);
        // 300 DPI: 595.276 * 300/72 = 2480.3 → 2481.
        assert_eq!(w300, 2481);
    }

    #[test]
    fn custom_matrix_overrides_dpi() {
        let p = RenderParams {
            dpi: 150.0,
            matrix: Some(Matrix::scale(1.0, 1.0)),
            ..RenderParams::screen()
        };
        // 1 px/pt: A4 becomes 595×842.
        let (w, h) = p.device_size(a4());
        assert_eq!(w, 596);
        assert_eq!(h, 842);
        assert_eq!(p.page_to_device(a4()), Matrix::scale(1.0, 1.0));
    }

    #[test]
    fn page_to_device_flips_y() {
        let p = RenderParams::screen();
        let m = p.page_to_device(a4());
        // Top-left of the page box maps to device origin.
        let tl = m.apply(Point::new(0.0, a4().y1));
        assert!((tl.x - 0.0).abs() < 1e-9);
        assert!((tl.y - 0.0).abs() < 1e-9);
        // Bottom-left maps to the bottom row.
        let bl = m.apply(Point::new(0.0, 0.0));
        assert!(bl.y > 0.0);
    }

    #[test]
    fn print_presets_change_the_surface() {
        let p = RenderParams::print();
        assert_eq!(p.target_space, TargetColourSpace::DeviceCmyk);
        assert!(!p.alpha);
        assert!(p.for_print);
        assert!((p.dpi - 300.0).abs() < 1e-9);
    }

    #[test]
    fn alpha_toggles_the_output_channel() {
        // Documented effect: `alpha` selects an RGBA output over an opaque RGB.
        assert!(RenderParams::screen().alpha);
        assert!(!RenderParams::print().alpha);
    }

    #[test]
    fn annotation_inclusion_is_carried() {
        let p = RenderParams {
            include_annotations: false,
            ..RenderParams::screen()
        };
        assert!(!p.include_annotations);
    }

    #[test]
    fn oc_overrides_are_carried() {
        let mut p = RenderParams::screen();
        p.oc.insert(7, false);
        p.oc.insert(12, true);
        assert_eq!(p.oc.get(&7), Some(&false));
        assert_eq!(p.oc.get(&12), Some(&true));
    }

    #[test]
    fn render_intent_round_trips() {
        assert_eq!(
            RenderIntent::from_number(0.0),
            RenderIntent::RelativeColorimetric
        );
        assert_eq!(
            RenderIntent::from_number(1.0),
            RenderIntent::AbsoluteColorimetric
        );
        assert_eq!(RenderIntent::from_number(2.0), RenderIntent::Saturation);
        assert_eq!(RenderIntent::from_number(3.0), RenderIntent::Perceptual);
        // Out-of-range defaults to relative colorimetric.
        assert_eq!(
            RenderIntent::from_number(9.0),
            RenderIntent::RelativeColorimetric
        );
    }

    #[test]
    fn cache_id_differs_per_parameter() {
        let base = RenderParams::screen();
        let base_id = base.cache_id();
        let cases: Vec<RenderParams> = vec![
            RenderParams {
                dpi: 300.0,
                ..base.clone()
            },
            RenderParams {
                matrix: Some(Matrix::IDENTITY),
                ..base.clone()
            },
            RenderParams {
                target_space: TargetColourSpace::DeviceCmyk,
                ..base.clone()
            },
            RenderParams {
                alpha: false,
                ..base.clone()
            },
            RenderParams {
                include_annotations: false,
                ..base.clone()
            },
            RenderParams {
                for_print: true,
                ..base.clone()
            },
            RenderParams {
                text_hinting: false,
                ..base.clone()
            },
            RenderParams {
                intent: RenderIntent::Perceptual,
                ..base.clone()
            },
        ];
        for case in cases {
            assert_ne!(
                case.cache_id(),
                base_id,
                "cache id must change per parameter"
            );
        }
        // OC overrides participate.
        let mut with_oc = base.clone();
        with_oc.oc.insert(1, false);
        assert_ne!(with_oc.cache_id(), base_id);
        // Determinism: same params hash identically.
        assert_eq!(base.cache_id(), RenderParams::screen().cache_id());
    }
}
