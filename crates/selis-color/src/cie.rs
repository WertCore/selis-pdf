//! CIE spaces: CalGray, CalRGB, Lab (SL-2.COLOR.02).
//!
//! The calibrated colour-space conversions of ISO 32000-2 §8.6.4-5:
//! * **CalGray** — a single-component space with a white point and gamma;
//! * **CalRGB** — three components with a white point, gammas, and a linear
//!   conversion matrix;
//! * **Lab** — the CIE L*a*b* space with a white point and component ranges.
//!
//! All conversions are deterministic and platform-independent (ADR-P0012).

use crate::Rgb;

/// A CIE white point (X, Y, Z), defaulting to D65.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WhitePoint {
    /// X.
    pub x: f64,
    /// Y.
    pub y: f64,
    /// Z.
    pub z: f64,
}

impl WhitePoint {
    /// The D65 white point (the PDF default).
    pub const D65: WhitePoint = WhitePoint {
        x: 0.9505,
        y: 1.0,
        z: 1.0890,
    };

    /// A white point from a PDF `/WhitePoint` array.
    #[must_use]
    pub fn from_array(v: &[f64]) -> Option<Self> {
        Some(Self {
            x: *v.first()?,
            y: *v.get(1)?,
            z: *v.get(2)?,
        })
    }
}

/// A CalGray colour space.
#[derive(Debug, Clone, PartialEq)]
pub struct CalGray {
    /// The white point.
    pub white: WhitePoint,
    /// The black point (default all zero).
    pub black: (f64, f64, f64),
    /// The gamma (default 1.0).
    pub gamma: f64,
}

/// A CalRGB colour space.
#[derive(Debug, Clone, PartialEq)]
pub struct CalRgb {
    /// The white point.
    pub white: WhitePoint,
    /// The per-channel gammas `[gr gb gg]`.
    pub gamma: (f64, f64, f64),
    /// The linear conversion matrix `[a b c d e f g h i]`.
    pub matrix: [f64; 9],
}

/// A Lab colour space.
#[derive(Debug, Clone, PartialEq)]
pub struct Lab {
    /// The white point.
    pub white: WhitePoint,
    /// The black point (default all zero).
    pub black: (f64, f64, f64),
    /// The component ranges `[amin amax bmin bmax]` (default ±100).
    pub range: (f64, f64, f64, f64),
}

/// The CIE XYZ intermediate (used by all three conversions).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Xyz {
    x: f64,
    y: f64,
    z: f64,
}

/// Convert XYZ to linear sRGB (the D65 reference white).
fn xyz_to_linear_rgb(xyz: Xyz) -> (f64, f64, f64) {
    let x = xyz.x;
    let y = xyz.y;
    let z = xyz.z;
    // The sRGB matrix (linear, before the gamma curve).
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let g = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let b = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    (r, g, b)
}

/// Apply the sRGB transfer function to a linear channel.
fn srgb_gamma(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// The CIE `f` function used by Lab↔XYZ, reserved for the Lab→XYZ direction
/// (SL-1.COLOUR). The reverse transform in [`Lab::to_rgb`] inlines `f⁻¹`.
#[allow(dead_code)]
fn cie_f(t: f64) -> f64 {
    let delta = 6.0 / 29.0;
    if t > delta * delta * delta {
        t.cbrt()
    } else {
        t / (3.0 * delta * delta) + 4.0 / 29.0
    }
}

impl CalGray {
    /// Convert a grey component to RGB.
    ///
    /// `A = g^gamma`, then `XYZ = A·[Wx Wy Wz]`, then linear RGB.
    #[must_use]
    pub fn to_rgb(&self, comps: &[f64]) -> Option<Rgb> {
        let a = *comps.first()?;
        let a = a.powf(self.gamma).clamp(0.0, 1.0);
        let xyz = Xyz {
            x: self.white.x * a,
            y: self.white.y * a,
            z: self.white.z * a,
        };
        let (r, g, b) = xyz_to_linear_rgb(xyz);
        Some(Rgb::new(srgb_gamma(r), srgb_gamma(g), srgb_gamma(b)))
    }
}

impl CalRgb {
    /// Convert a CalRGB triple to RGB.
    ///
    /// Decode each component with its gamma, apply the linear matrix, then
    /// the sRGB transfer curve.
    #[must_use]
    pub fn to_rgb(&self, comps: &[f64]) -> Option<Rgb> {
        let c1 = *comps.first()?;
        let c2 = *comps.get(1)?;
        let c3 = *comps.get(2)?;
        // Linearise with the per-channel gammas.
        let l1 = c1.powf(self.gamma.0).clamp(0.0, 1.0);
        let l2 = c2.powf(self.gamma.1).clamp(0.0, 1.0);
        let l3 = c3.powf(self.gamma.2).clamp(0.0, 1.0);
        // Apply the linear conversion matrix.
        let [m11, m12, m13, m21, m22, m23, m31, m32, m33] = self.matrix;
        let r = m11 * l1 + m12 * l2 + m13 * l3;
        let g = m21 * l1 + m22 * l2 + m23 * l3;
        let b = m31 * l1 + m32 * l2 + m33 * l3;
        Some(Rgb::new(srgb_gamma(r), srgb_gamma(g), srgb_gamma(b)))
    }
}

impl Lab {
    /// Convert a Lab triple to RGB.
    ///
    /// `L ∈ [0,100]`, `a, b` in the space's range. `XYZ = f(L, a, b)`, then
    /// linear RGB.
    #[must_use]
    pub fn to_rgb(&self, comps: &[f64]) -> Option<Rgb> {
        let l = *comps.first()?;
        let a = *comps.get(1)?;
        let b = *comps.get(2)?;
        let (amin, amax, bmin, bmax) = self.range;
        let a = a.clamp(amin, amax);
        let b = b.clamp(bmin, bmax);
        // Reverse the Lab→XYZ transform.
        let fy = (l + 16.0) / 116.0;
        let fx = fy + a / 500.0;
        let fz = fy - b / 200.0;
        let delta = 6.0 / 29.0;
        let f_inv = |f: f64| {
            if f > delta {
                f * f * f
            } else {
                3.0 * delta * delta * (f - 4.0 / 29.0)
            }
        };
        let x = self.white.x * f_inv(fx);
        let y = self.white.y * f_inv(fy);
        let z = self.white.z * f_inv(fz);
        let xyz = Xyz { x, y, z };
        let (r, g, b) = xyz_to_linear_rgb(xyz);
        Some(Rgb::new(srgb_gamma(r), srgb_gamma(g), srgb_gamma(b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calgray_white_is_white() {
        let gray = CalGray {
            white: WhitePoint::D65,
            black: (0.0, 0.0, 0.0),
            gamma: 1.0,
        };
        let rgb = gray.to_rgb(&[1.0]).expect("convert");
        assert!(rgb.to_rgb8()[0] > 250);
        assert!(rgb.to_rgb8()[1] > 250);
        assert!(rgb.to_rgb8()[2] > 250);
    }

    #[test]
    fn calgray_black_is_black() {
        let gray = CalGray {
            white: WhitePoint::D65,
            black: (0.0, 0.0, 0.0),
            gamma: 1.0,
        };
        let rgb = gray.to_rgb(&[0.0]).expect("convert");
        assert_eq!(rgb.to_rgb8(), [0, 0, 0]);
    }

    #[test]
    fn calrgb_identity_gives_identity() {
        // A CalRGB with identity matrix and gamma 1 approximates sRGB.
        let rgb_space = CalRgb {
            white: WhitePoint::D65,
            gamma: (1.0, 1.0, 1.0),
            matrix: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        };
        let rgb = rgb_space.to_rgb(&[1.0, 0.0, 0.0]).expect("convert");
        // Red maps to roughly red.
        assert!(rgb.r > 0.9);
        assert!(rgb.g < 0.1);
    }

    #[test]
    fn calrgb_matrix_rows_do_not_shadow() {
        // Regression: the blue row previously read the *green channel* (a
        // destructured `g` was shadowed by the green binding), so a
        // non-identity matrix computed a wrong blue. With the permutation
        // matrix below, l1 must land on blue: b = m31·l1.
        let rgb_space = CalRgb {
            white: WhitePoint::D65,
            gamma: (1.0, 1.0, 1.0),
            matrix: [0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0],
        };
        let rgb = rgb_space.to_rgb(&[1.0, 0.0, 0.0]).expect("convert");
        assert!(rgb.b > 0.9, "l1 must reach blue, got {:?}", rgb.to_rgb8());
        assert!(rgb.r < 0.1);
        assert!(rgb.g < 0.1);
    }

    #[test]
    fn lab_d65_white_is_white() {
        let lab = Lab {
            white: WhitePoint::D65,
            black: (0.0, 0.0, 0.0),
            range: (-100.0, 100.0, -100.0, 100.0),
        };
        // L=100, a=0, b=0 is the reference white.
        let rgb = lab.to_rgb(&[100.0, 0.0, 0.0]).expect("convert");
        assert!(rgb.r > 0.95);
        assert!(rgb.g > 0.95);
        assert!(rgb.b > 0.95);
    }

    #[test]
    fn lab_short_input_is_none() {
        let lab = Lab {
            white: WhitePoint::D65,
            black: (0.0, 0.0, 0.0),
            range: (-100.0, 100.0, -100.0, 100.0),
        };
        assert!(lab.to_rgb(&[50.0]).is_none());
    }

    #[test]
    fn lab_out_of_range_is_clamped() {
        let lab = Lab {
            white: WhitePoint::D65,
            black: (0.0, 0.0, 0.0),
            range: (-100.0, 100.0, -100.0, 100.0),
        };
        let rgb = lab.to_rgb(&[50.0, 500.0, -500.0]).expect("convert");
        // Clamped: the RGB8 conversion is total and bounded.
        assert!(rgb.r >= 0.0 && rgb.r <= 1.0);
    }
}
