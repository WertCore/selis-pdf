//! Shadings 1–7 (SL-2.RAST.06).
//!
//! The seven shading types:
//! * **1 Function** — colour varies by a function of position;
//! * **2 Axial** — colour varies along a line segment;
//! * **3 Radial** — colour varies with the distance from a centre (the
//!   "extend" cone cases are a classic source of wrong output);
//! * **4 Free-form Gouraud** — a triangle mesh;
//! * **5 Lattice-form Gouraud** — a rectangular grid mesh;
//! * **6 Coons patch** — bicubic patches;
//! * **7 Tensor-product patch** — bicubic with different control structure.
//!
//! Types 4–7 need mesh decoding from a packed bit stream. This module defines
//! the shading model and the geometry; the raster (per-pixel colour) lands
//! with the engine.

use selis_color::function::{Function, FunctionBudget};
use selis_geom::{Matrix, Point};

/// A shading colour at a point.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadingPoint {
    /// The point in user space.
    pub point: Point,
    /// The colour-space components.
    pub components: Vec<f64>,
}

/// A shading type 1: function of position.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionShading {
    /// The domain rectangle `[x0 y0 x1 y1]`.
    pub domain: (f64, f64, f64, f64),
    /// The function mapping `(x, y)` to colour components.
    pub function: Function,
    /// The colour-space component count.
    pub components: u8,
    /// The matrix mapping user space into the function's domain.
    pub matrix: Matrix,
}

/// A shading type 2: axial.
#[derive(Debug, Clone, PartialEq)]
pub struct AxialShading {
    /// The start point.
    pub start: Point,
    /// The end point.
    pub end: Point,
    /// The t-domain `[t0 t1]`.
    pub domain: (f64, f64),
    /// The colour function.
    pub function: Function,
    /// Whether the shading extends beyond the end point.
    pub extend_start: bool,
    /// Whether the shading extends beyond the start point.
    pub extend_end: bool,
}

/// A shading type 3: radial.
#[derive(Debug, Clone, PartialEq)]
pub struct RadialShading {
    /// The start centre.
    pub start: Point,
    /// The start radius.
    pub start_radius: f64,
    /// The end centre.
    pub end: Point,
    /// The end radius.
    pub end_radius: f64,
    /// The t-domain.
    pub domain: (f64, f64),
    /// The colour function.
    pub function: Function,
    /// Extend beyond the end circle.
    pub extend_end: bool,
    /// Extend beyond the start circle.
    pub extend_start: bool,
}

/// A shading type 4: free-form Gouraud triangle mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct GouraudShading {
    /// The vertices: `(point, components)`.
    pub vertices: Vec<ShadingPoint>,
    /// The triangle indices into `vertices`.
    pub triangles: Vec<(u32, u32, u32)>,
}

/// A shading type 5: lattice-form Gouraud.
#[derive(Debug, Clone, PartialEq)]
pub struct LatticeShading {
    /// The vertices, row-major.
    pub vertices: Vec<ShadingPoint>,
    /// The number of vertices per row.
    pub cols: u32,
}

/// The seven shading types.
#[derive(Debug, Clone, PartialEq)]
pub enum Shading {
    /// Type 1.
    Function(FunctionShading),
    /// Type 2.
    Axial(AxialShading),
    /// Type 3.
    Radial(RadialShading),
    /// Type 4.
    Gouraud(GouraudShading),
    /// Type 5.
    Lattice(LatticeShading),
    /// Types 6/7 (Coons and tensor patches) — the control structures are
    /// decoded by the engine; the model is the same triangle-tessellated
    /// form.
    Patch(PatchShading),
}

/// A Coons or tensor-product patch shading.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchShading {
    /// The tessellated patch corners.
    pub patches: Vec<Vec<ShadingPoint>>,
}

/// Evaluate an axial shading at a point: the colour along the segment.
///
/// Returns the components at the perpendicular projection of `p` onto the
/// line `start→end`, clamped to the t-domain.
#[must_use]
pub fn axial_colour(s: &AxialShading, p: Point) -> Vec<f64> {
    let dx = s.end.x - s.start.x;
    let dy = s.end.y - s.start.y;
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        let px = p.x - s.start.x;
        let py = p.y - s.start.y;
        ((px * dx + py * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let t = t.clamp(s.domain.0, s.domain.1);
    eval_function(&s.function, &[t])
}

/// Evaluate a radial shading at a point: the colour by the distance ratio.
///
/// The "extend" cone cases: a point outside the start/end circle uses the
/// nearest endpoint colour when the corresponding `extend_*` flag is set,
/// and is not painted otherwise (the caller tests the return).
#[must_use]
pub fn radial_colour(s: &RadialShading, p: Point) -> Option<Vec<f64>> {
    let dx = s.end.x - s.start.x;
    let dy = s.end.y - s.start.y;
    let dr = s.end_radius - s.start_radius;
    // The point is on the cone if
    //   |p − (start + t·d)| = start_radius + t·dr
    // Solve the quadratic for t; the solution with the smaller |t| in
    // [0,1] wins, and the extend flags decide outside points.
    let fx = p.x - s.start.x;
    let fy = p.y - s.start.y;
    let a = dx * dx + dy * dy - dr * dr;
    let b = 2.0 * (fx * dx + fy * dy - s.start_radius * dr);
    let c = fx * fx + fy * fy - s.start_radius * s.start_radius;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        // No real intersection: outside the cone.
        return None;
    }
    let sqrt_d = disc.sqrt();
    // `a` may be zero when the cone is degenerate (start == end, equal radii);
    // the t-value is then given by the linear term alone.
    let denom = 2.0 * a;
    let (t1, t2) = if denom.abs() < 1e-12 {
        if b.abs() < 1e-12 {
            return None;
        }
        (-c / b, -c / b)
    } else {
        ((-b - sqrt_d) / denom, (-b + sqrt_d) / denom)
    };
    let (t_lo, t_hi) = (t1.min(t2), t1.max(t2));
    let t1 = t_lo;
    let t2 = t_hi;
    // Prefer the solution in [0,1]; else use the extend flags.
    let t = if (0.0..=1.0).contains(&t1) {
        t1
    } else if (0.0..=1.0).contains(&t2) {
        t2
    } else if t1 > 1.0 && s.extend_end {
        1.0
    } else if t2 < 0.0 && s.extend_start {
        0.0
    } else {
        return None;
    };
    let t = t.clamp(s.domain.0, s.domain.1);
    Some(eval_function(&s.function, &[t]))
}

/// Evaluate a function under a budget, returning zeros on failure (a broken
/// function must not fail the page).
fn eval_function(f: &Function, input: &[f64]) -> Vec<f64> {
    let mut budget = FunctionBudget::default();
    f.evaluate(input, &mut budget).unwrap_or_else(|_| vec![0.0])
}

/// Barycentric interpolation of vertex colours (types 4/5).
#[must_use]
pub fn barycentric(
    a: &ShadingPoint,
    b: &ShadingPoint,
    c: &ShadingPoint,
    u: f64,
    v: f64,
) -> Vec<f64> {
    let w = 1.0 - u - v;
    a.components
        .iter()
        .zip(&b.components)
        .zip(&c.components)
        .map(|((&ca, &cb), &cc)| ca * w + cb * u + cc * v)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_color::function::{CalcOp, CalculatorFunction};

    fn identity_fn() -> Function {
        // f(t) = t (1 input → 1 output).
        Function::Calculator(CalculatorFunction {
            inputs: 1,
            outputs: 1,
            domain: vec![(0.0, 1.0)],
            range: vec![(0.0, 1.0)],
            program: vec![CalcOp::Cvr],
        })
    }

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn axial_midpoint() {
        let s = AxialShading {
            start: pt(0.0, 0.0),
            end: pt(10.0, 0.0),
            domain: (0.0, 1.0),
            function: identity_fn(),
            extend_start: false,
            extend_end: false,
        };
        // Midpoint of the segment: t = 0.5.
        let c = axial_colour(&s, pt(5.0, 0.0));
        assert!((c[0] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn axial_beyond_end_clamps() {
        let s = AxialShading {
            start: pt(0.0, 0.0),
            end: pt(10.0, 0.0),
            domain: (0.0, 1.0),
            function: identity_fn(),
            extend_start: false,
            extend_end: false,
        };
        let c = axial_colour(&s, pt(100.0, 0.0));
        assert!((c[0] - 1.0).abs() < 1e-9);
    }

    /// The radial "extend" cone case: a point inside the start circle uses
    /// the start colour when extend_start is set.
    #[test]
    fn radial_extend_start() {
        let s = RadialShading {
            start: pt(0.0, 0.0),
            end: pt(0.0, 0.0),
            start_radius: 10.0,
            end_radius: 20.0,
            domain: (0.0, 1.0),
            function: identity_fn(),
            extend_start: true,
            extend_end: true,
        };
        // At the centre, t ~ 0.
        let c = radial_colour(&s, pt(0.0, 0.0));
        assert!(c.is_some());
        if let Some(c) = c {
            assert!(c[0].abs() < 0.01);
        }
    }

    #[test]
    fn radial_inside_without_extend_is_none() {
        let s = RadialShading {
            start: pt(0.0, 0.0),
            end: pt(0.0, 0.0),
            start_radius: 10.0,
            end_radius: 20.0,
            domain: (0.0, 1.0),
            function: identity_fn(),
            extend_start: false,
            extend_end: false,
        };
        // A point inside the start circle maps to t < 0; with no
        // extend_start the shading is not painted there.
        assert!(radial_colour(&s, pt(5.0, 0.0)).is_none());
    }

    #[test]
    fn barycentric_interpolates() {
        let a = ShadingPoint {
            point: pt(0.0, 0.0),
            components: vec![0.0],
        };
        let b = ShadingPoint {
            point: pt(1.0, 0.0),
            components: vec![1.0],
        };
        let c = ShadingPoint {
            point: pt(0.0, 1.0),
            components: vec![0.5],
        };
        // u=1, v=0: vertex b.
        let at_b = barycentric(&a, &b, &c, 1.0, 0.0);
        assert!((at_b[0] - 1.0).abs() < 1e-9);
        // u=0, v=0: vertex a.
        let at_a = barycentric(&a, &b, &c, 0.0, 0.0);
        assert!((at_a[0] - 0.0).abs() < 1e-9);
    }
}
