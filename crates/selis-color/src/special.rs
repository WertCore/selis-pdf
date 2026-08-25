//! Indexed, Separation, DeviceN + tint transforms (SL-2.COLOR.04).
//!
//! * **Indexed** — a palette over a base space: one component is an index
//!   into a byte lookup table.
//! * **Separation** — one colourant mapped to the alternate space by a tint
//!   transform (a PDF function).
//! * **DeviceN** — n colourants mapped by a tint transform.
//!
//! The `/All` and `/None` separation names are the correctness trap: `/None`
//! paints nothing (its tint always maps to the alternate's initial colour at
//! zero opacity). Handled explicitly.

use crate::function::Function;
use crate::Rgb;

/// A Separation colour space.
#[derive(Debug, Clone, PartialEq)]
pub struct Separation {
    /// The colourant name (`All`, `None`, or a spot name).
    pub name: String,
    /// The alternate space's component count.
    pub alternate_components: u8,
    /// The tint transform (a PDF function).
    pub tint: Function,
}

impl Separation {
    /// Whether this is the `/All` separation (paints the alternate).
    #[must_use]
    pub fn is_all(&self) -> bool {
        self.name == "All"
    }

    /// Whether this is the `/None` separation (paints nothing).
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.name == "None"
    }

    /// Evaluate the tint transform on a tint value in `[0,1]`.
    ///
    /// For `/None`, the result is the alternate's initial colour (nothing).
    #[must_use]
    pub fn tint_components(&self, tint: f64) -> Vec<f64> {
        if self.is_none() {
            // Paints nothing: zero in every alternate component.
            return vec![0.0; usize::from(self.alternate_components)];
        }
        let mut budget = crate::function::FunctionBudget::default();
        self.tint
            .evaluate(&[tint], &mut budget)
            .unwrap_or_else(|_| vec![0.0; usize::from(self.alternate_components)])
    }
}

/// A DeviceN colour space.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceN {
    /// The colourant names.
    pub names: Vec<String>,
    /// The alternate space's component count.
    pub alternate_components: u8,
    /// The tint transform (a PDF function).
    pub tint: Function,
}

impl DeviceN {
    /// Evaluate the tint transform on the colourant values.
    #[must_use]
    pub fn tint_components(&self, comps: &[f64]) -> Vec<f64> {
        let mut budget = crate::function::FunctionBudget::default();
        self.tint
            .evaluate(comps, &mut budget)
            .unwrap_or_else(|_| vec![0.0; usize::from(self.alternate_components)])
    }
}

/// An Indexed colour space.
#[derive(Debug, Clone, PartialEq)]
pub struct Indexed {
    /// The base space's component count (1, 3, or 4).
    pub base_components: u8,
    /// The highest valid index.
    pub hival: u16,
    /// The palette bytes: `(hival+1) × base_components` bytes.
    pub lookup: Vec<u8>,
}

impl Indexed {
    /// The palette entry for an index, as components in `[0,1]`.
    #[must_use]
    pub fn entry(&self, index: u8) -> Vec<f64> {
        let n = usize::from(self.base_components);
        let idx = usize::from(index).min(self.hival as usize);
        let base = idx.saturating_mul(n);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let b = self
                .lookup
                .get(base.saturating_add(i))
                .copied()
                .unwrap_or(0);
            out.push(f64::from(b) / 255.0);
        }
        out
    }
}

/// Convert a DeviceN/Separation alternate-space component set to RGB via the
/// device-space naive conversions.
#[must_use]
pub fn alternate_to_rgb(components: u8, comps: &[f64]) -> Option<Rgb> {
    match components {
        1 => Some(Rgb::new(
            comps.first().copied().unwrap_or(0.0),
            comps.first().copied().unwrap_or(0.0),
            comps.first().copied().unwrap_or(0.0),
        )),
        3 => Some(Rgb::new(
            comps.first().copied().unwrap_or(0.0),
            comps.get(1).copied().unwrap_or(0.0),
            comps.get(2).copied().unwrap_or(0.0),
        )),
        4 => Some(
            crate::Cmyk::new(
                comps.first().copied().unwrap_or(0.0),
                comps.get(1).copied().unwrap_or(0.0),
                comps.get(2).copied().unwrap_or(0.0),
                comps.get(3).copied().unwrap_or(0.0),
            )
            .to_rgb_naive(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use crate::function::{CalcOp, CalculatorFunction};

    fn identity_tint() -> Function {
        // A type-4 calculator that returns its input: `cvr`.
        Function::Calculator(CalculatorFunction {
            inputs: 1,
            outputs: 1,
            domain: vec![(0.0, 1.0)],
            range: vec![(0.0, 1.0)],
            program: vec![CalcOp::Cvr],
        })
    }

    #[test]
    fn separation_all_paints() {
        // The tint maps 1 input to 3 alternate components (grey in RGB).
        let sep = Separation {
            name: "All".to_string(),
            alternate_components: 3,
            tint: Function::Calculator(CalculatorFunction {
                inputs: 1,
                outputs: 3,
                domain: vec![(0.0, 1.0)],
                range: vec![(0.0, 1.0); 3],
                program: vec![CalcOp::Dup, CalcOp::Dup],
            }),
        };
        assert!(sep.is_all());
        let comps = sep.tint_components(1.0);
        assert_eq!(comps.len(), 3);
        assert!((comps[0] - 1.0).abs() < 1e-9);
        assert!((comps[1] - 1.0).abs() < 1e-9);
    }

    /// The `/None` trap: paints nothing, not white.
    #[test]
    fn separation_none_paints_nothing() {
        let sep = Separation {
            name: "None".to_string(),
            alternate_components: 3,
            tint: identity_tint(),
        };
        assert!(sep.is_none());
        let comps = sep.tint_components(1.0);
        assert_eq!(comps, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn separation_spot_tint() {
        let sep = Separation {
            name: "PANTONE 123".to_string(),
            alternate_components: 3,
            tint: Function::Calculator(CalculatorFunction {
                inputs: 1,
                outputs: 3,
                domain: vec![(0.0, 1.0)],
                range: vec![(0.0, 1.0); 3],
                program: vec![CalcOp::Dup, CalcOp::Dup],
            }),
        };
        assert!(!sep.is_all() && !sep.is_none());
        let comps = sep.tint_components(0.5);
        assert_eq!(comps.len(), 3);
        assert!((comps[0] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn indexed_lookup() {
        let idx = Indexed {
            base_components: 3,
            hival: 1,
            lookup: vec![0, 0, 0, 255, 0, 0], // entry 0 black, entry 1 red
        };
        let black = idx.entry(0);
        assert!((black[0] - 0.0).abs() < 1e-9);
        let red = idx.entry(1);
        assert!((red[0] - 1.0).abs() < 1e-9);
        assert!((red[1] - 0.0).abs() < 1e-9);
    }

    #[test]
    fn indexed_out_of_range_clamps_to_hival() {
        let idx = Indexed {
            base_components: 1,
            hival: 0,
            lookup: vec![128],
        };
        // Any index maps to the last valid entry.
        let v = idx.entry(255);
        assert!((v[0] - 128.0 / 255.0).abs() < 1e-9);
    }

    #[test]
    fn devicen_tint() {
        let dn = DeviceN {
            names: vec!["Cyan".to_string(), "Black".to_string()],
            alternate_components: 1,
            tint: identity_tint(),
        };
        let out = dn.tint_components(&[0.25]);
        assert_eq!(out.len(), 1);
        assert!((out[0] - 0.25).abs() < 1e-9);
    }

    #[test]
    fn alternate_to_rgb_cmyk() {
        let rgb = alternate_to_rgb(4, &[0.0, 1.0, 1.0, 0.0]).expect("cmyk");
        // Red-ish.
        assert!(rgb.r > 0.9);
        assert!(rgb.g < 0.1);
    }
}
