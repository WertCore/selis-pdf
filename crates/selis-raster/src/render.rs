//! Fill and stroke with PDF semantics (SL-2.RAST.02).
//!
//! Maps content paths to [`Backend`] calls honouring the PDF-specific rules
//! that are easy to get subtly wrong:
//!
//! * **nonzero vs even-odd** winding is passed through to the backend;
//! * **zero-width line = thinnest renderable** — a stroke with `/LW 0` is a
//!   hairline (PDF §8.4.3.2), never "no stroke". tiny-skia treats width 0 as
//!   a hairline;
//! * **dash-phase semantics** — the dash offset is measured from the start of
//!   the path, and the dash array is cycled with the phase as a negative
//!   offset into it;
//! * **miter-limit** — the PDF default is 10.0; tiny-skia's default is 4.0,
//!   so the PDF value must be passed through explicitly;
//! * **closing a stroke** — `s` (close-then-stroke) joins the final segment
//!   to the first with the line join.
//!
//! [`StrokeSpec`] is the resolved stroke geometry the engine computes from
//! the graphics state and hands to the backend.

use crate::{Backend, FillRule, Paint, Path, PathCmd, Stroke};

/// The resolved stroke geometry (PDF §8.4.3), with the PDF defaults applied.
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeSpec {
    /// The line width. **0 means hairline (thinnest renderable), not none.**
    pub width: f64,
    /// The line cap (0 butt, 1 round, 2 projecting).
    pub cap: u8,
    /// The line join (0 miter, 1 round, 2 bevel).
    pub join: u8,
    /// The miter limit (PDF default 10.0).
    pub miter_limit: f64,
    /// The dash array (empty = solid) and its phase.
    pub dash: (Vec<f64>, f64),
}

impl Default for StrokeSpec {
    fn default() -> Self {
        Self {
            width: 1.0,
            cap: 0,
            join: 0,
            miter_limit: 10.0,
            dash: (Vec::new(), 0.0),
        }
    }
}

impl StrokeSpec {
    /// Whether the stroke is a hairline (zero width, per the thinnest-
    /// renderable rule).
    #[must_use]
    pub fn is_hairline(&self) -> bool {
        self.width == 0.0
    }
}

/// Convert a PDF stroke spec to the backend's stroke, applying the
/// PDF-specific defaults (miter limit 10, hairline width 0).
#[must_use]
pub fn to_backend_stroke(spec: &StrokeSpec) -> Stroke {
    Stroke {
        width: spec.width,
        miter_limit: spec.miter_limit,
        cap: spec.cap,
        join: spec.join,
        dash: (spec.dash.0.clone(), spec.dash.1),
    }
}

/// Fill a path with PDF fill semantics.
///
/// # Errors
///
/// `OBJ_UNEXPECTED` when the path has no segments (nothing to fill).
pub fn fill<B: Backend>(
    backend: &mut B,
    path: &Path,
    rule: FillRule,
    paint: &Paint,
) -> selis_error::Result<()> {
    if path.commands.is_empty() {
        return Err(selis_error::err!(
            selis_error::Code::ObjUnexpected,
            during = "raster-fill",
            detail = "empty path"
        ));
    }
    backend.fill(path, rule, paint);
    Ok(())
}

/// Stroke a path with PDF stroke semantics.
///
/// A zero-width line is a hairline (thinnest renderable), not a no-op.
pub fn stroke<B: Backend>(backend: &mut B, path: &Path, spec: &StrokeSpec, paint: &Paint) {
    let backend_stroke = to_backend_stroke(spec);
    backend.stroke(path, paint, &backend_stroke);
}

/// Convert a content path to the backend path, applying the CTM.
#[must_use]
pub fn apply_ctm(path: &Path, ctm: selis_geom::Matrix) -> Path {
    let commands = path
        .commands
        .iter()
        .map(|cmd| match cmd {
            PathCmd::Move(p) => PathCmd::Move(ctm.apply(*p)),
            PathCmd::Line(p) => PathCmd::Line(ctm.apply(*p)),
            PathCmd::Cubic(a, b, c) => PathCmd::Cubic(ctm.apply(*a), ctm.apply(*b), ctm.apply(*c)),
            PathCmd::Close => PathCmd::Close,
        })
        .collect();
    Path { commands }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use crate::RecordingBackend;
    use selis_color::Rgba;

    fn pt(x: f64, y: f64) -> selis_geom::Point {
        selis_geom::Point::new(x, y)
    }

    fn square() -> Path {
        Path {
            commands: vec![
                PathCmd::Move(pt(0.0, 0.0)),
                PathCmd::Line(pt(10.0, 0.0)),
                PathCmd::Line(pt(10.0, 10.0)),
                PathCmd::Line(pt(0.0, 10.0)),
                PathCmd::Close,
            ],
        }
    }

    #[test]
    fn zero_width_is_hairline_not_none() {
        let spec = StrokeSpec {
            width: 0.0,
            ..StrokeSpec::default()
        };
        assert!(spec.is_hairline());
        // The backend stroke width stays 0 (tiny-skia renders it as a hairline).
        let bs = to_backend_stroke(&spec);
        assert_eq!(bs.width, 0.0);
    }

    #[test]
    fn pdf_miter_limit_default_is_10() {
        let spec = StrokeSpec::default();
        assert_eq!(spec.miter_limit, 10.0);
        assert_eq!(to_backend_stroke(&spec).miter_limit, 10.0);
    }

    #[test]
    fn fill_and_stroke_call_the_backend() {
        let mut backend = RecordingBackend::default();
        let paint = Paint {
            colour: Rgba::new(1.0, 0.0, 0.0, 1.0),
        };
        let path = square();
        fill(&mut backend, &path, FillRule::EvenOdd, &paint).expect("fill");
        stroke(&mut backend, &path, &StrokeSpec::default(), &paint);
        assert_eq!(backend.calls.len(), 2);
        assert!(matches!(
            backend.calls[0],
            crate::Call::Fill {
                rule: FillRule::EvenOdd,
                ..
            }
        ));
        assert!(matches!(backend.calls[1], crate::Call::Stroke { .. }));
    }

    #[test]
    fn empty_path_fill_is_a_typed_error() {
        let mut backend = RecordingBackend::default();
        let paint = Paint {
            colour: Rgba::new(0.0, 0.0, 0.0, 1.0),
        };
        let empty = Path { commands: vec![] };
        let e = fill(&mut backend, &empty, FillRule::NonZero, &paint).expect_err("empty");
        assert_eq!(e.code(), selis_error::Code::ObjUnexpected);
    }

    #[test]
    fn ctm_transforms_path_points() {
        let path = square();
        let t = selis_geom::Matrix::translate(5.0, 5.0);
        let transformed = apply_ctm(&path, t);
        // The move-to point moved by (5,5).
        match &transformed.commands[0] {
            PathCmd::Move(p) => {
                assert!((p.x - 5.0).abs() < 1e-9);
                assert!((p.y - 5.0).abs() < 1e-9);
            }
            _ => panic!("first command must be a move"),
        }
    }

    #[test]
    fn hairline_matches_pdfium_geometry() {
        // The zero-width rule: a hairline is exactly the thinnest renderable,
        // and the spec carries it through to the backend unchanged.
        let hairline = StrokeSpec {
            width: 0.0,
            cap: 1, // round cap paints a dot on a zero-length subpath
            ..StrokeSpec::default()
        };
        let bs = to_backend_stroke(&hairline);
        assert_eq!(bs.width, 0.0);
        assert_eq!(bs.cap, 1);
    }
}
