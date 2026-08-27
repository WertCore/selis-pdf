//! Tiling patterns (SL-2.RAST.07) — ISO 32000-2:2020 §8.7.2.
//!
//! A tiling pattern fills an area by repeating a small tile. This module owns
//! the pattern model and the **bounded placement** of tile instances over a
//! paint region:
//!
//! * **Coloured (type 1)** — the tile carries its own colours;
//! * **Uncoloured (type 2)** — the tile is a stencil; the current fill colour
//!   tints it at paint time;
//! * the `/Matrix` maps **pattern space → default user space** — the common
//!   bug is applying it in the current CTM's frame. It is applied *after* the
//!   CTM (`matrix.then(ctm)`), which is the spec order;
//! * the **bounded tile cache** is the DoD: the total number of instances is
//!   charged to the pixel budget, so a pattern with a tiny `/XStep` over a
//!   large region fails with `BUDGET_PIXELS` instead of rendering for an hour.
//!
//! The tile bitmap itself is rendered by the caller (the content/engine layer
//! draws the pattern's form XObject once); this module consumes that buffer
//! and produces the placement plan.

use selis_error::{err, Code, Result};
use selis_geom::{Matrix, Point, Rect};
use selis_sandbox::{BudgetGuard, Resource};

/// The paint type of a tiling pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternType {
    /// Type 1: coloured — the tile carries its own colours.
    Coloured,
    /// Type 2: uncoloured — the tile is a stencil tinted by the fill colour.
    Uncoloured,
}

/// A rendered tiling-pattern tile (the pattern's form XObject drawn once).
#[derive(Debug, Clone, PartialEq)]
pub struct PatternTile {
    /// The tile width in pixels.
    pub width: u32,
    /// The tile height in pixels.
    pub height: u32,
    /// Straight RGBA8 samples, row-major (`width × height × 4`).
    pub rgba8: Vec<u8>,
}

impl PatternTile {
    /// Whether the tile buffer is the expected length.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let expected = u64::from(self.width)
            .saturating_mul(u64::from(self.height))
            .saturating_mul(4);
        expected <= u64::from(u32::MAX)
            && u64::try_from(self.rgba8.len()).unwrap_or(u64::MAX) == expected
    }
}

/// A resolved tiling pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct TilingPattern {
    /// The paint type.
    pub paint_type: PatternType,
    /// The rendered tile bitmap.
    pub tile: PatternTile,
    /// The horizontal step in pattern space (`/XStep`).
    pub x_step: f64,
    /// The vertical step in pattern space (`/YStep`).
    pub y_step: f64,
    /// The pattern matrix mapping pattern space → default user space (`/Matrix`).
    pub matrix: Matrix,
}

/// One placed tile instance in device space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileInstance {
    /// The device-space axis-aligned bounding box of this tile cell.
    pub rect: Rect,
    /// The lattice column index (0-based from the region's leftmost cell).
    pub ix: u64,
    /// The lattice row index (0-based from the region's topmost cell).
    pub iy: u64,
}

/// The placement plan: the bounded set of tile instances covering a region.
#[derive(Debug, Clone, PartialEq)]
pub struct TilePlan {
    /// The instances, in row-major lattice order.
    pub instances: Vec<TileInstance>,
}

/// Plan the tile instances covering `region` (a device-space rectangle).
///
/// `ctm` is the device transform in effect at paint time. The combined
/// transform is `pattern.matrix.then(ctm)` — pattern space → default user
/// space (the `/Matrix`) then default user space → device (the CTM). That
/// ordering is the spec's, and the inverse (pattern matrix applied in the
/// current CTM's frame) is the classic bug this signature rules out.
///
/// The total rasterised samples (instances × tile pixels) is charged to the
/// pixel budget, which is what makes a tiny `/XStep` fail fast.
///
/// # Budget
///
/// `BUDGET_PIXELS` when the instances needed to cover the region would exceed
/// the pixel budget — a pathological tiling pattern.
///
/// # Malformed Input
///
/// `PATTERN_MALFORMED` for a non-positive step, a non-finite transform, or an
/// invalid tile buffer. A singular transform yields an empty plan (nothing can
/// be drawn), not an error — permissive in what we accept.
pub fn plan_pattern(
    pattern: &TilingPattern,
    ctm: Matrix,
    region: Rect,
    g: &mut BudgetGuard<'_>,
) -> Result<TilePlan> {
    if !(pattern.x_step > 0.0) || !(pattern.y_step > 0.0) {
        return Err(err!(
            Code::PatternMalformed,
            during = "tiling-pattern",
            detail = "non-positive step"
        ));
    }
    if !pattern.tile.is_valid() {
        return Err(err!(
            Code::PatternMalformed,
            during = "tiling-pattern",
            detail = "tile buffer has the wrong length"
        ));
    }
    let pattern_to_device = pattern.matrix.then(ctm);
    if !pattern_to_device.is_finite() {
        return Err(err!(
            Code::PatternMalformed,
            during = "tiling-pattern",
            detail = "non-finite pattern transform"
        ));
    }
    let inv = match pattern_to_device.invert() {
        Some(m) => m,
        None => {
            return Ok(TilePlan {
                instances: Vec::new(),
            })
        }
    };

    // The region's pattern-space bounding box (conservative for skewed
    // matrices: an upper bound on the cells that can touch the region).
    let corners = [
        inv.apply(Point::new(region.x0, region.y0)),
        inv.apply(Point::new(region.x1, region.y0)),
        inv.apply(Point::new(region.x0, region.y1)),
        inv.apply(Point::new(region.x1, region.y1)),
    ];
    let px0 = corners.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let px1 = corners
        .iter()
        .map(|p| p.x)
        .fold(f64::NEG_INFINITY, f64::max);
    let py0 = corners.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let py1 = corners
        .iter()
        .map(|p| p.y)
        .fold(f64::NEG_INFINITY, f64::max);

    let nx = ceil_div(px1 - px0, pattern.x_step);
    let ny = ceil_div(py1 - py0, pattern.y_step);

    // The DoD budget: total rasterised samples. A tiny step makes this huge.
    let per_instance = u64::from(pattern.tile.width).saturating_mul(u64::from(pattern.tile.height));
    let total = nx.saturating_mul(ny).saturating_mul(per_instance);
    g.charge(Resource::Pixels, total)?;

    let x0_cell = (px0 / pattern.x_step).floor() * pattern.x_step;
    let y0_cell = (py0 / pattern.y_step).floor() * pattern.y_step;

    let mut instances = Vec::new();
    for iy in 0..ny {
        for ix in 0..nx {
            // The lattice cell's pattern-space origin.
            let cx = x0_cell + ix as f64 * pattern.x_step;
            let cy = y0_cell + iy as f64 * pattern.y_step;
            let rect = cell_aabb(pattern_to_device, cx, cy, pattern.x_step, pattern.y_step);
            if rects_intersect(region, rect) {
                instances.push(TileInstance { rect, ix, iy });
            }
        }
    }
    Ok(TilePlan { instances })
}

/// The device-space AABB of one pattern-space lattice cell.
fn cell_aabb(m: Matrix, x: f64, y: f64, sx: f64, sy: f64) -> Rect {
    let c = [
        m.apply(Point::new(x, y)),
        m.apply(Point::new(x + sx, y)),
        m.apply(Point::new(x, y + sy)),
        m.apply(Point::new(x + sx, y + sy)),
    ];
    let x0 = c.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let x1 = c.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
    let y0 = c.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let y1 = c.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
    Rect::new(x0, y0, x1, y1)
}

/// Whether two axis-aligned rectangles overlap (touching counts).
fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
}

/// Ceiling division of a non-negative span by a positive step, saturating at
/// `u64::MAX`. A non-finite ratio is the budget's problem; saturate rather
/// than wrap or loop.
#[must_use]
fn ceil_div(span: f64, step: f64) -> u64 {
    if !(span > 0.0) || !(step > 0.0) || !span.is_finite() {
        return 0;
    }
    let v = (span / step).ceil();
    if !v.is_finite() || v >= u64::MAX as f64 {
        return u64::MAX;
    }
    // v is finite, non-negative, and below u64::MAX, so the narrowing is
    // exact in range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        v as u64
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    fn tile(w: u32, h: u32) -> PatternTile {
        let mut g = guard();
        let rgba8 = selis_sandbox::alloc::vec_filled(&mut g, w as usize * h as usize * 4, 0u8)
            .expect("unlimited budget");
        PatternTile {
            width: w,
            height: h,
            rgba8,
        }
    }

    fn identity_pattern(x_step: f64, y_step: f64) -> TilingPattern {
        TilingPattern {
            paint_type: PatternType::Coloured,
            tile: tile(2, 2),
            x_step,
            y_step,
            matrix: Matrix::IDENTITY,
        }
    }

    #[test]
    fn identity_pattern_covers_the_region() {
        let mut g = guard();
        let pattern = identity_pattern(10.0, 10.0);
        let plan = plan_pattern(
            &pattern,
            Matrix::IDENTITY,
            Rect::new(0.0, 0.0, 25.0, 15.0),
            &mut g,
        )
        .expect("plan");
        // A 25×15 region with 10×10 cells needs 3×2 instances.
        assert_eq!(plan.instances.len(), 6);
        // The first instance is anchored at the origin.
        assert_eq!(plan.instances[0].rect, Rect::new(0.0, 0.0, 10.0, 10.0));
        // Row-major order: the second is one step right.
        assert_eq!(plan.instances[1].rect, Rect::new(10.0, 0.0, 20.0, 10.0));
        // Instances are clipped to the region by the caller; the plan only
        // guarantees coverage — every instance overlaps the region.
        for inst in &plan.instances {
            assert!(rects_intersect(Rect::new(0.0, 0.0, 25.0, 15.0), inst.rect));
        }
    }

    #[test]
    fn pattern_matrix_is_applied_after_the_ctm() {
        let mut g = guard();
        // /Matrix scales pattern space by 2 in x: a 10-wide pattern-space cell
        // becomes 20 wide in default user space.
        let pattern = TilingPattern {
            matrix: Matrix::scale(2.0, 1.0),
            ..identity_pattern(10.0, 10.0)
        };
        let plan = plan_pattern(
            &pattern,
            Matrix::IDENTITY,
            Rect::new(0.0, 0.0, 45.0, 10.0),
            &mut g,
        )
        .expect("plan");
        // 45-wide region / 20-wide device cells → 3 columns.
        assert_eq!(plan.instances.len(), 3);
        assert_eq!(plan.instances[0].rect.x1, 20.0);
    }

    #[test]
    fn non_positive_step_is_malformed() {
        let mut g = guard();
        let pattern = identity_pattern(0.0, 10.0);
        let e = plan_pattern(
            &pattern,
            Matrix::IDENTITY,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            &mut g,
        )
        .expect_err("zero step");
        assert_eq!(e.code(), Code::PatternMalformed);
    }

    #[test]
    fn singular_transform_yields_an_empty_plan() {
        let mut g = guard();
        let pattern = TilingPattern {
            matrix: Matrix::scale(0.0, 0.0),
            ..identity_pattern(10.0, 10.0)
        };
        let plan = plan_pattern(
            &pattern,
            Matrix::IDENTITY,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            &mut g,
        )
        .expect("plan");
        assert!(plan.instances.is_empty());
    }

    #[test]
    fn invalid_tile_buffer_is_malformed() {
        let mut g = guard();
        let short_tile =
            selis_sandbox::alloc::vec_filled(&mut g, 4, 0u8).expect("unlimited budget");
        let pattern = TilingPattern {
            tile: PatternTile {
                width: 2,
                height: 2,
                rgba8: short_tile, // needs 16
            },
            ..identity_pattern(10.0, 10.0)
        };
        let e = plan_pattern(
            &pattern,
            Matrix::IDENTITY,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            &mut g,
        )
        .expect_err("bad tile");
        assert_eq!(e.code(), Code::PatternMalformed);
    }

    /// DoD: a pattern with a tiny /XStep hits the pixel budget rather than
    /// enumerating instances for an hour.
    #[test]
    fn tiny_step_hits_the_pixel_budget() {
        // A budget with a 1_000_000-pixel cap.
        let budget = Budget {
            pixels: 1_000_000,
            ..Budget::unlimited()
        };
        let mut g = budget.guard();
        let pattern = identity_pattern(0.000_001, 0.000_001);
        // A modest 1000×1000 device region needs 10^12 instances.
        let e = plan_pattern(
            &pattern,
            Matrix::IDENTITY,
            Rect::new(0.0, 0.0, 1000.0, 1000.0),
            &mut g,
        )
        .expect_err("budget");
        assert_eq!(e.code(), Code::BudgetPixels);
    }

    #[test]
    fn uncoloured_paint_type_is_carried() {
        let pattern = TilingPattern {
            paint_type: PatternType::Uncoloured,
            ..identity_pattern(10.0, 10.0)
        };
        assert_eq!(pattern.paint_type, PatternType::Uncoloured);
    }
}
