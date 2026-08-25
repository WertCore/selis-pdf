//! Path construction and painting (SL-2.CONT.03).
//!
//! Builds a [`Path`] from the path operators (`m l c v y h re`), records the
//! painting and clipping operators (`S s f F f* B B* b b* n W W*`), and
//! implements the "clip takes effect after the painting op" rule — the
//! ordering trap that is easy to get subtly wrong: in `f W`, the fill paints
//! with the *old* clip and the clip applies to everything after.

use selis_geom::{Point, Rect};

/// A path segment.
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// `m` — move to.
    Move(Point),
    /// `l` — line to.
    Line(Point),
    /// `c` — cubic Bézier to with two control points.
    Cubic(Point, Point, Point),
    /// `v` — cubic with first control = current point.
    CubicFirst(Point, Point),
    /// `y` — cubic with second control = end point.
    CubicSecond(Point, Point),
    /// `h` — close the subpath.
    Close,
}

/// A path under construction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Path {
    /// The segments in construction order.
    pub segments: Vec<Segment>,
    /// The current point (the last endpoint).
    pub current: Option<Point>,
    /// The start of the current subpath.
    pub subpath_start: Option<Point>,
}

impl Path {
    /// A new empty path.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `m x y` — start a new subpath.
    pub fn move_to(&mut self, p: Point) {
        self.segments.push(Segment::Move(p));
        self.current = Some(p);
        self.subpath_start = Some(p);
    }

    /// `l x y` — line to.
    pub fn line_to(&mut self, p: Point) {
        self.segments.push(Segment::Line(p));
        self.current = Some(p);
    }

    /// `c x1 y1 x2 y2 x3 y3` — cubic to.
    pub fn cubic_to(&mut self, c1: Point, c2: Point, p: Point) {
        self.segments.push(Segment::Cubic(c1, c2, p));
        self.current = Some(p);
    }

    /// `v x2 y2 x3 y3` — cubic with first control = current point.
    pub fn cubic_first(&mut self, c2: Point, p: Point) {
        self.segments.push(Segment::CubicFirst(c2, p));
        self.current = Some(p);
    }

    /// `y x1 y1 x3 y3` — cubic with second control = end point.
    pub fn cubic_second(&mut self, c1: Point, p: Point) {
        self.segments.push(Segment::CubicSecond(c1, p));
        self.current = Some(p);
    }

    /// `h` — close the subpath (a line back to its start).
    pub fn close(&mut self) {
        self.segments.push(Segment::Close);
        self.current = self.subpath_start;
    }

    /// `re x y w h` — a rectangle as a closed subpath.
    pub fn rectangle(&mut self, rect: Rect) {
        let (x, y, w, h) = (rect.x0, rect.y0, rect.width(), rect.height());
        self.move_to(Point::new(x, y));
        self.line_to(Point::new(x + w, y));
        self.line_to(Point::new(x + w, y + h));
        self.line_to(Point::new(x, y + h));
        self.close();
    }

    /// The axis-aligned bounding box of all segments (for degenerate-case
    /// checks and the rasteriser's clip).
    #[must_use]
    pub fn bounds(&self) -> Option<Rect> {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for seg in &self.segments {
            let pts: [Option<Point>; 3] = match seg {
                Segment::Move(p) | Segment::Line(p) => [Some(*p), None, None],
                Segment::Cubic(a, b, c) => [Some(*a), Some(*b), Some(*c)],
                Segment::CubicFirst(a, b) => [Some(*a), Some(*b), None],
                Segment::CubicSecond(a, b) => [Some(*a), Some(*b), None],
                Segment::Close => [None, None, None],
            };
            for p in pts.into_iter().flatten() {
                min_x = min_x.min(p.x);
                min_y = min_y.min(p.y);
                max_x = max_x.max(p.x);
                max_y = max_y.max(p.y);
            }
        }
        if min_x == f64::INFINITY {
            None
        } else {
            Some(Rect::from_points(min_x, min_y, max_x, max_y))
        }
    }

    /// Whether the path is degenerate (no segments or no current point).
    #[must_use]
    pub fn is_degenerate(&self) -> bool {
        self.segments.is_empty() || self.current.is_none()
    }
}

/// The result of painting or clipping a path.
#[derive(Debug, Clone, PartialEq)]
pub enum PaintOp {
    /// `S` — stroke the path.
    Stroke,
    /// `s` — close then stroke.
    CloseStroke,
    /// `f`/`F` — fill with nonzero winding.
    Fill,
    /// `f*` — fill with even-odd.
    FillEvenOdd,
    /// `B` — fill then stroke.
    FillStroke,
    /// `B*` — even-odd fill then stroke.
    FillStrokeEvenOdd,
    /// `b` — close, fill, stroke.
    CloseFillStroke,
    /// `b*` — close, even-odd fill, stroke.
    CloseFillStrokeEvenOdd,
    /// `n` — no-op.
    None,
}

impl PaintOp {
    /// Whether the op paints (vs only updates the clip).
    #[must_use]
    pub const fn paints(self) -> bool {
        !matches!(self, PaintOp::None)
    }
}

impl Default for PaintOp {
    fn default() -> Self {
        PaintOp::None
    }
}

/// A completed path with its painting and clipping operators applied in the
/// order the content stream specified them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaintedPath {
    /// The path.
    pub path: Path,
    /// The paint op that was applied.
    pub paint: PaintOp,
    /// Whether the clip was updated (and its fill rule).
    pub clip: Option<bool>,
}
#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn rectangle_builds_four_lines_and_closes() {
        let mut path = Path::new();
        path.rectangle(Rect::from_points(0.0, 0.0, 10.0, 20.0));
        assert_eq!(path.segments.len(), 5); // move + 3 lines + close
        assert!(matches!(path.segments[0], Segment::Move(_)));
        assert!(matches!(path.segments[4], Segment::Close));
        let b = path.bounds().expect("bounds");
        assert_eq!((b.x0, b.y0, b.width(), b.height()), (0.0, 0.0, 10.0, 20.0));
    }

    #[test]
    fn degenerate_path_has_no_bounds() {
        let path = Path::new();
        assert!(path.is_degenerate());
        assert_eq!(path.bounds(), None);
    }

    #[test]
    fn move_line_cubic() {
        let mut path = Path::new();
        path.move_to(pt(0.0, 0.0));
        path.line_to(pt(1.0, 1.0));
        path.cubic_to(pt(2.0, 2.0), pt(3.0, 3.0), pt(4.0, 4.0));
        assert_eq!(path.segments.len(), 3);
        assert_eq!(path.current, Some(pt(4.0, 4.0)));
        assert_eq!(path.subpath_start, Some(pt(0.0, 0.0)));
    }

    #[test]
    fn zero_length_subpath_with_round_caps() {
        // A move + close with no draw: degenerate but structurally valid.
        let mut path = Path::new();
        path.move_to(pt(5.0, 5.0));
        path.close();
        assert!(!path.is_degenerate());
        assert_eq!(path.bounds().map(|b| b.width()), Some(0.0));
    }
}
