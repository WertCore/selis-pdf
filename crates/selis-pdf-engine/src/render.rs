//! The engine's render path (SL-2.RAST.11 → the page renderer).
//!
//! Walks a [`DisplayList`] and paints each op onto a raster [`Backend`]. This
//! is the composition point the architecture (§8) describes: content streams
//! become display lists (`selis-pdf-content::exec`), which become pixels
//! here.
//!
//! Paths (fill/stroke) are fully rendered. Text ops currently record their
//! glyph positions; the glyph-outline rasterisation is the next increment.

use selis_color::Rgba;
use selis_geom::{Matrix, Point};
use selis_pdf_content::display_list::{DisplayList, Op};
use selis_pdf_content::path::{Path as ContentPath, Segment};
use selis_raster::{
    FillRule, Paint as RasterPaint, Path as RasterPath, PathCmd, StrokeSpec, TinySkiaBackend,
};

/// Render a display list onto a backend.
///
/// # Malformed Input
///
/// A degenerate or unrenderable op is skipped (a deviation), never fatal.
pub fn render_display_list(dl: &DisplayList, backend: &mut TinySkiaBackend) {
    for op in &dl.ops {
        match op {
            Op::Fill { path, state } => {
                let Some(p) = to_raster_path(path) else {
                    continue;
                };
                let paint = paint(&state.fill, state.alpha_fill);
                selis_raster::render::fill(backend, &p, FillRule::NonZero, &paint);
            }
            Op::Stroke { path, state } => {
                let Some(p) = to_raster_path(path) else {
                    continue;
                };
                let paint = paint(&state.stroke, state.alpha_stroke);
                let spec = stroke_spec(state);
                selis_raster::render::stroke(backend, &p, &spec, &paint);
            }
            Op::FillStroke { path, state } => {
                let Some(p) = to_raster_path(path) else {
                    continue;
                };
                let fill_paint = paint(&state.fill, state.alpha_fill);
                selis_raster::render::fill(backend, &p, FillRule::NonZero, &fill_paint);
                let stroke_paint = paint(&state.stroke, state.alpha_stroke);
                let spec = stroke_spec(state);
                selis_raster::render::stroke(backend, &p, &spec, &stroke_paint);
            }
            Op::Text { .. } => {
                // Glyph-outline rasterisation lands with the next increment.
            }
        }
    }
}

/// Convert a content path to a raster path, expanding the `v`/`y` shorthand
/// cubics. `None` for a degenerate path.
#[must_use]
fn to_raster_path(content: &ContentPath) -> Option<RasterPath> {
    if content.is_degenerate() {
        return None;
    }
    let mut commands = Vec::new();
    let mut current: Option<Point> = None;
    for seg in &content.segments {
        match seg {
            Segment::Move(p) => {
                commands.push(PathCmd::Move(*p));
                current = Some(*p);
            }
            Segment::Line(p) => {
                commands.push(PathCmd::Line(*p));
                current = Some(*p);
            }
            Segment::Cubic(a, b, c) => {
                commands.push(PathCmd::Cubic(*a, *b, *c));
                current = Some(*c);
            }
            Segment::CubicFirst(b, c) => {
                // `v x2 y2 x3 y3`: the first control is the current point.
                let a = current.unwrap_or(*b);
                commands.push(PathCmd::Cubic(a, *b, *c));
                current = Some(*c);
            }
            Segment::CubicSecond(a, c) => {
                // `y x1 y1 x3 y3`: the second control is the end point.
                commands.push(PathCmd::Cubic(*a, *c, *c));
                current = Some(*c);
            }
            Segment::Close => {
                commands.push(PathCmd::Close);
            }
        }
    }
    Some(RasterPath { commands })
}

/// The raster paint for a device-RGB colour.
fn paint(rgb: &[f64; 3], alpha: f64) -> RasterPaint {
    RasterPaint {
        colour: Rgba::new(
            rgb.get(0).copied().unwrap_or(0.0).clamp(0.0, 1.0),
            rgb.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0),
            rgb.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0),
            alpha.clamp(0.0, 1.0),
        ),
    }
}

/// The resolved stroke spec from the display-list state.
fn stroke_spec(state: &selis_pdf_content::display_list::ResolvedState) -> StrokeSpec {
    StrokeSpec {
        width: state.line_width,
        cap: state.line_cap,
        join: state.line_join,
        miter_limit: 10.0,
        dash: (Vec::new(), 0.0),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::{Budget, BudgetGuard};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    fn const_width(_font: &selis_bytes::Bytes, _code: u16) -> f64 {
        500.0
    }

    /// A content stream drawing a filled red square renders red pixels.
    #[test]
    fn a_filled_rectangle_renders_pixels() {
        let mut g = guard();
        let content = b"0 0 m 0 100 l 100 100 l 100 0 l h 1 0 0 rg f";
        let dl = selis_pdf_content::exec::execute(content, &const_width, &mut g).expect("execute");
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend);
        let data = backend.pixmap().data();
        // The centre pixel should be opaque red.
        let idx = (50 * 100 + 50) * 4;
        assert_eq!(&data[idx..idx + 3], &[255, 0, 0]);
    }

    /// A filled rect on a white-then-blue background: the fill covers it.
    #[test]
    fn fill_covers_the_rectangle() {
        let mut g = guard();
        // Fill the whole page blue first, then a red square in the centre.
        let content = b"0 0 m 0 100 l 100 100 l 100 0 l h 0 0 1 rg f 25 25 m 25 75 l 75 75 l 75 25 l h 1 0 0 rg f";
        let dl = selis_pdf_content::exec::execute(content, &const_width, &mut g).expect("execute");
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend);
        let data = backend.pixmap().data();
        // Centre (50,50) is red; corner (5,5) is blue.
        let centre = (50 * 100 + 50) * 4;
        let corner = (5 * 100 + 5) * 4;
        assert_eq!(&data[centre..centre + 3], &[255, 0, 0]);
        assert_eq!(&data[corner..corner + 3], &[0, 0, 255]);
    }
}
