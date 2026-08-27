//! Overprint and overprint simulation (SL-2.COLOR.07).
//!
//! PDF 32000-2:2020 §11.6.6. Overprint controls whether a new colour paints
//! over the existing colour or replaces it component-by-component. This module
//! owns the overprint model and the compositing formulas.
//!
//! Ship at `Identify` level in Phase 2 (the conformance ladder): the engine
//! recognises the construct and can report it, and the compositing formulas
//! are code-complete for the prepress audience. Render-level promotion is
//! Phase 9 (20-CONFORMANCE-PROGRAM.md §4, PDF/X row).

use crate::alloc::vec::Vec;

/// The overprint state from the graphics state.
///
/// ExtGState keys: `/OP` (stroke), `/op` (fill), `/OPM` (mode).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverprintParams {
    /// Overprint for stroking operations (`/OP`).
    pub stroke: bool,
    /// Overprint for non-stroking operations (`/op`).
    pub fill: bool,
    /// Overprint mode (`/OPM`): 0 or 1.
    pub mode: u8,
}

impl Default for OverprintParams {
    fn default() -> Self {
        Self {
            stroke: false,
            fill: false,
            mode: 0,
        }
    }
}

impl OverprintParams {
    /// Whether overprint is active for any operation.
    #[must_use]
    pub fn any_active(&self) -> bool {
        self.stroke || self.fill
    }
}

/// Overprint compositing mode 0 (OPM = 0).
///
/// Per component: if the source component is zero (no ink), the backdrop
/// component is retained; otherwise the source is used. This is the
/// "ink-on-ink" model: new ink prints where applied, existing ink shows
/// through where no new ink is present.
///
/// `backdrop` and `source` are per-component values in `[0, 1]`, paired by
/// index. The result has the same length as the shorter input.
#[must_use]
pub fn overprint_mode0(backdrop: &[f64], source: &[f64]) -> Vec<f64> {
    let n = backdrop.len().min(source.len());
    // Component vectors are small (≤ 4); grows incrementally.
    let mut out = Vec::new();
    for i in 0..n {
        let src = source.get(i).copied().unwrap_or(0.0);
        let bkd = backdrop.get(i).copied().unwrap_or(0.0);
        if src == 0.0 {
            out.push(bkd);
        } else {
            out.push(src);
        }
    }
    out
}

/// Overprint compositing mode 1 (OPM = 1).
///
/// Per component: `result = src + bkd × (1 − src)`. This simulates the
/// overprint operator as a source-over-like blend where the source component
/// acts as the coverage of the new ink.
///
/// `backdrop` and `source` are per-component values in `[0, 1]`, paired by
/// index. The result has the same length as the shorter input.
#[must_use]
pub fn overprint_mode1(backdrop: &[f64], source: &[f64]) -> Vec<f64> {
    let n = backdrop.len().min(source.len());
    // Component vectors are small (≤ 4); grows incrementally.
    let mut out = Vec::new();
    for i in 0..n {
        let src = source.get(i).copied().unwrap_or(0.0);
        let bkd = backdrop.get(i).copied().unwrap_or(0.0);
        let v = src + bkd * (1.0 - src);
        out.push(v.clamp(0.0, 1.0));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn mode0_retains_backdrop_for_zero_source() {
        // Cyan: source has M=0, Y=0, K=0 → backdrop magenta, yellow, black
        // show through; source has C=0.5 → replaces backdrop cyan.
        let backdrop = [0.1, 0.2, 0.3, 0.4];
        let source = [0.5, 0.0, 0.0, 0.0];
        let result = overprint_mode0(&backdrop, &source);
        assert_eq!(result, [0.5, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn mode0_source_fully_ink_replaces_backdrop() {
        let result = overprint_mode0(&[0.0, 0.0], &[0.5, 0.8]);
        assert_eq!(result, [0.5, 0.8]);
    }

    #[test]
    fn mode0_source_all_zero_retains_all_backdrop() {
        let result = overprint_mode0(&[0.3, 0.7, 0.1], &[0.0; 3]);
        assert_eq!(result, [0.3, 0.7, 0.1]);
    }

    #[test]
    fn mode1_formula() {
        // src=0.5, bkd=0.2 → 0.5 + 0.2*(1-0.5) = 0.5 + 0.1 = 0.6
        let result = overprint_mode1(&[0.2], &[0.5]);
        assert!((result[0] - 0.6).abs() < 1e-12);
    }

    #[test]
    fn mode1_fully_covering_source() {
        // src=1.0 → 1.0 + bkd*(0) = 1.0 regardless of backdrop.
        let result = overprint_mode1(&[0.9], &[1.0]);
        assert!((result[0] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn mode1_no_ink_preserves_backdrop() {
        // src=0.0 → 0.0 + bkd*(1-0) = bkd.
        let result = overprint_mode1(&[0.7], &[0.0]);
        assert!((result[0] - 0.7).abs() < 1e-12);
    }

    #[test]
    fn default_params_are_inactive() {
        let p = OverprintParams::default();
        assert!(!p.any_active());
    }

    #[test]
    fn active_params_reported() {
        let p = OverprintParams {
            stroke: true,
            fill: false,
            mode: 1,
        };
        assert!(p.any_active());
        assert!(p.stroke);
        assert!(!p.fill);
    }
}
