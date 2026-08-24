//! Geometry: PDF's coordinate model, matrices, rectangles, and the fixed-point
//! type that makes ADR-P0012 (deterministic rasterisation) achievable.
//!
//! # Why fixed point
//!
//! Rule 3 of `00-INDEX.md §5` requires byte-identical output across platforms and
//! runs. `f64` arithmetic is deterministic *given the same operation order*, but
//! coordinate rounding at tile boundaries is where platform differences leak in.
//! [`Fixed`] gives an exact, portable coordinate representation so that "which
//! pixel does this edge land on" has one answer everywhere.
//!
//! # PDF's matrix form
//!
//! PDF writes a transform as six numbers `[a b c d e f]`, meaning
//!
//! ```text
//! | a  b  0 |
//! | c  d  0 |
//! | e  f  1 |
//! ```
//!
//! with row vectors: `[x y 1] × M`. [`Matrix`] stores exactly those six in that
//! order, so a reader with the spec open can compare field-for-field
//! (03-CONVENTIONS.md §4).

#![forbid(unsafe_code)]

extern crate alloc;

use core::fmt;

/// A fixed-point coordinate: 1/256 of a unit, stored in an `i32`.
///
/// Range is roughly ±8.3 million units — far beyond PDF's ±32 767 user-space
/// limit — with 1/256 resolution, which is finer than a 300 DPI device pixel at
/// any reasonable zoom.
///
/// Every operation saturates rather than wrapping or panicking, because the
/// inputs are document-derived (`arithmetic_side_effects` is denied for exactly
/// this reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Fixed(i32);

impl Fixed {
    /// Fractional bits.
    pub const SHIFT: u32 = 8;
    /// The value 1.0.
    pub const ONE: Fixed = Fixed(256);
    /// The value 0.0.
    pub const ZERO: Fixed = Fixed(0);
    /// The largest representable value.
    pub const MAX: Fixed = Fixed(i32::MAX);
    /// The smallest representable value.
    pub const MIN: Fixed = Fixed(i32::MIN);

    /// From a whole number of units, saturating.
    #[must_use]
    pub const fn from_int(n: i32) -> Self {
        Fixed(n.saturating_mul(256))
    }

    /// From an `f64`, rounding half away from zero and saturating.
    ///
    /// This is the *only* float→fixed conversion in the engine. Concentrating it
    /// here is what makes the determinism claim auditable.
    #[must_use]
    pub fn from_f64(v: f64) -> Self {
        if v.is_nan() {
            return Fixed::ZERO;
        }
        let scaled = v * 256.0;
        let rounded = if scaled >= 0.0 {
            (scaled + 0.5).floor()
        } else {
            (scaled - 0.5).ceil()
        };
        if rounded >= f64::from(i32::MAX) {
            Fixed::MAX
        } else if rounded <= f64::from(i32::MIN) {
            Fixed::MIN
        } else {
            // In range by the guards above, so the cast is exact.
            #[allow(clippy::cast_possible_truncation)]
            Fixed(rounded as i32)
        }
    }

    /// The raw 1/256 units.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// From raw 1/256 units.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Self {
        Fixed(raw)
    }

    /// As an `f64`. Exact: every `Fixed` is representable.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from(self.0) / 256.0
    }

    /// Truncate toward negative infinity to a whole unit.
    #[must_use]
    pub const fn floor_int(self) -> i32 {
        self.0 >> Self::SHIFT
    }

    /// Round up to a whole unit.
    #[must_use]
    pub const fn ceil_int(self) -> i32 {
        self.0.saturating_add(255) >> Self::SHIFT
    }

    /// Saturating addition.
    #[must_use]
    pub const fn add(self, o: Fixed) -> Fixed {
        Fixed(self.0.saturating_add(o.0))
    }

    /// Saturating subtraction.
    #[must_use]
    pub const fn sub(self, o: Fixed) -> Fixed {
        Fixed(self.0.saturating_sub(o.0))
    }

    /// Saturating multiplication, exact in `i64` before narrowing.
    #[must_use]
    pub const fn mul(self, o: Fixed) -> Fixed {
        let product = (self.0 as i64).saturating_mul(o.0 as i64);
        let scaled = product >> Self::SHIFT;
        if scaled > i32::MAX as i64 {
            Fixed::MAX
        } else if scaled < i32::MIN as i64 {
            Fixed::MIN
        } else {
            Fixed(scaled as i32)
        }
    }

    /// Division. Returns `None` on divide-by-zero rather than panicking, because
    /// the divisor can come from a document.
    #[must_use]
    pub const fn div(self, o: Fixed) -> Option<Fixed> {
        if o.0 == 0 {
            return None;
        }
        let numerator = (self.0 as i64) << Self::SHIFT;
        let quotient = numerator / (o.0 as i64);
        if quotient > i32::MAX as i64 {
            Some(Fixed::MAX)
        } else if quotient < i32::MIN as i64 {
            Some(Fixed::MIN)
        } else {
            Some(Fixed(quotient as i32))
        }
    }

    /// Absolute value, saturating.
    #[must_use]
    pub const fn abs(self) -> Fixed {
        Fixed(self.0.saturating_abs())
    }

    /// Negation, saturating.
    #[must_use]
    pub const fn neg(self) -> Fixed {
        Fixed(self.0.saturating_neg())
    }

    /// The smaller of two values.
    #[must_use]
    pub fn min(self, o: Fixed) -> Fixed {
        Fixed(self.0.min(o.0))
    }

    /// The larger of two values.
    #[must_use]
    pub fn max(self, o: Fixed) -> Fixed {
        Fixed(self.0.max(o.0))
    }
}

impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Rendered exactly, without going through a float, so a log line and a
        // golden file agree byte-for-byte.
        let neg = self.0 < 0;
        let mag = self.0.unsigned_abs();
        let whole = mag >> Self::SHIFT;
        let frac = mag & 0xff;
        // frac/256 in thousandths, exact: 1000*frac/256 == 125*frac/32.
        let milli = frac.saturating_mul(1000).checked_div(256).unwrap_or(0);
        if neg {
            f.write_str("-")?;
        }
        write!(f, "{whole}.{milli:03}")
    }
}

/// A point in some coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate. PDF user space is y-up.
    pub y: f64,
}

impl Point {
    /// A point.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// The origin.
    pub const ORIGIN: Point = Point { x: 0.0, y: 0.0 };
}

/// PDF's 6-element transformation matrix, `[a b c d e f]`.
///
/// PDF 32000-2:2020 §8.3.3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    /// `a` — x scale / row 0 col 0.
    pub a: f64,
    /// `b` — y shear / row 0 col 1.
    pub b: f64,
    /// `c` — x shear / row 1 col 0.
    pub c: f64,
    /// `d` — y scale / row 1 col 1.
    pub d: f64,
    /// `e` — x translation.
    pub e: f64,
    /// `f` — y translation.
    pub f: f64,
}

impl Matrix {
    /// The identity transform.
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// From the six numbers in PDF order.
    #[must_use]
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// A scale.
    #[must_use]
    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// A translation.
    #[must_use]
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// A rotation by whole degrees.
    ///
    /// Restricted to multiples of 90° because that is the only rotation PDF page
    /// `/Rotate` permits, and because exact values keep [`Matrix`] composition
    /// free of transcendental-function drift across platforms (ADR-P0012). For
    /// arbitrary content-stream rotation, the matrix arrives from the document
    /// already computed.
    #[must_use]
    pub fn rotate_quadrant(quarter_turns: i32) -> Self {
        match quarter_turns.rem_euclid(4) {
            1 => Self::new(0.0, 1.0, -1.0, 0.0, 0.0, 0.0),
            2 => Self::new(-1.0, 0.0, 0.0, -1.0, 0.0, 0.0),
            3 => Self::new(0.0, -1.0, 1.0, 0.0, 0.0, 0.0),
            _ => Self::IDENTITY,
        }
    }

    /// `self` then `other` — i.e. `other × self` in PDF's row-vector convention,
    /// which is the order the `cm` operator concatenates in.
    #[must_use]
    pub fn then(self, other: Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Transform a point.
    #[must_use]
    pub fn apply(self, p: Point) -> Point {
        Point {
            x: self.a * p.x + self.c * p.y + self.e,
            y: self.b * p.x + self.d * p.y + self.f,
        }
    }

    /// The determinant.
    #[must_use]
    pub fn determinant(self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// The inverse, or `None` for a singular matrix.
    ///
    /// Singular transforms occur in real documents (a zero-scale `cm`); returning
    /// `None` rather than producing infinities is what keeps them from becoming
    /// NaN coordinates deep in the raster path.
    #[must_use]
    pub fn invert(self) -> Option<Matrix> {
        let det = self.determinant();
        if !det.is_finite() || det == 0.0 {
            return None;
        }
        let inv = 1.0 / det;
        Some(Matrix {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }

    /// Whether every element is finite. A document may supply NaN or infinity.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.c.is_finite()
            && self.d.is_finite()
            && self.e.is_finite()
            && self.f.is_finite()
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// An axis-aligned rectangle.
///
/// PDF rectangle arrays are `[x0 y0 x1 y1]` with **no guarantee of ordering** —
/// `/MediaBox [612 792 0 0]` is legal and common. [`Rect::from_pdf_array`]
/// normalises, which is why every box in the engine is already `x0 <= x1`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Lower-left x.
    pub x0: f64,
    /// Lower-left y.
    pub y0: f64,
    /// Upper-right x.
    pub x1: f64,
    /// Upper-right y.
    pub y1: f64,
}

impl Rect {
    /// A rectangle from already-ordered corners.
    #[must_use]
    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    /// A rectangle from a PDF `[x0 y0 x1 y1]` array, normalising the corners.
    #[must_use]
    pub fn from_pdf_array(v: [f64; 4]) -> Self {
        let [a, b, c, d] = v;
        Self {
            x0: a.min(c),
            y0: b.min(d),
            x1: a.max(c),
            y1: b.max(d),
        }
    }

    /// Back to a PDF array, in the normalised order.
    #[must_use]
    pub const fn to_pdf_array(self) -> [f64; 4] {
        [self.x0, self.y0, self.x1, self.y1]
    }

    /// Width.
    #[must_use]
    pub fn width(self) -> f64 {
        self.x1 - self.x0
    }

    /// Height.
    #[must_use]
    pub fn height(self) -> f64 {
        self.y1 - self.y0
    }

    /// Whether the rectangle has zero or negative area.
    #[must_use]
    pub fn is_empty(self) -> bool {
        !(self.width() > 0.0 && self.height() > 0.0)
    }

    /// Whether every corner is finite.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x0.is_finite() && self.y0.is_finite() && self.x1.is_finite() && self.y1.is_finite()
    }

    /// The overlap of two rectangles, or `None` if they do not overlap.
    #[must_use]
    pub fn intersect(self, o: Rect) -> Option<Rect> {
        let r = Rect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        if r.is_empty() {
            None
        } else {
            Some(r)
        }
    }

    /// The smallest rectangle containing both.
    #[must_use]
    pub fn union(self, o: Rect) -> Rect {
        Rect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    /// Whether a point is inside, boundary included.
    #[must_use]
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }

    /// Whether `self` fully contains `o`.
    #[must_use]
    pub fn contains_rect(self, o: Rect) -> bool {
        o.x0 >= self.x0 && o.y0 >= self.y0 && o.x1 <= self.x1 && o.y1 <= self.y1
    }

    /// The axis-aligned bounding box of this rectangle after a transform.
    #[must_use]
    pub fn transform(self, m: Matrix) -> Rect {
        let corners = [
            m.apply(Point::new(self.x0, self.y0)),
            m.apply(Point::new(self.x1, self.y0)),
            m.apply(Point::new(self.x0, self.y1)),
            m.apply(Point::new(self.x1, self.y1)),
        ];
        let mut r = Rect {
            x0: f64::INFINITY,
            y0: f64::INFINITY,
            x1: f64::NEG_INFINITY,
            y1: f64::NEG_INFINITY,
        };
        for c in corners {
            r.x0 = r.x0.min(c.x);
            r.y0 = r.y0.min(c.y);
            r.x1 = r.x1.max(c.x);
            r.y1 = r.y1.max(c.y);
        }
        r
    }

    /// US Letter, 8.5 × 11 inches at 72 units per inch.
    pub const LETTER: Rect = Rect::new(0.0, 0.0, 612.0, 792.0);
    /// ISO A4, 210 × 297 mm rounded to whole points as producers write it.
    pub const A4: Rect = Rect::new(0.0, 0.0, 595.0, 842.0);
}

/// Page rotation. PDF `/Rotate` must be a multiple of 90.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Rotation {
    /// No rotation.
    #[default]
    None,
    /// 90° clockwise.
    Clockwise90,
    /// 180°.
    Half,
    /// 270° clockwise (i.e. 90° counter-clockwise).
    Clockwise270,
}

impl Rotation {
    /// From a `/Rotate` value. Non-multiples of 90 are a document error; the spec
    /// says the value *shall* be a multiple of 90, so we round to the nearest
    /// quadrant and let the caller record a deviation rather than refuse the page.
    #[must_use]
    pub fn from_degrees(deg: i64) -> Self {
        let quadrant = deg.div_euclid(90).rem_euclid(4);
        match quadrant {
            1 => Rotation::Clockwise90,
            2 => Rotation::Half,
            3 => Rotation::Clockwise270,
            _ => Rotation::None,
        }
    }

    /// Whether a `/Rotate` value is spec-conformant.
    #[must_use]
    pub fn is_conformant(deg: i64) -> bool {
        deg.rem_euclid(90) == 0
    }

    /// As degrees clockwise, normalised to 0/90/180/270.
    #[must_use]
    pub const fn degrees(self) -> i64 {
        match self {
            Rotation::None => 0,
            Rotation::Clockwise90 => 90,
            Rotation::Half => 180,
            Rotation::Clockwise270 => 270,
        }
    }

    /// This rotation composed with another.
    #[must_use]
    pub fn then(self, o: Rotation) -> Rotation {
        Rotation::from_degrees(self.degrees().saturating_add(o.degrees()))
    }

    /// Whether the rotation swaps width and height.
    #[must_use]
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Clockwise90 | Rotation::Clockwise270)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_round_trips_and_saturates() {
        assert_eq!(Fixed::from_f64(1.0), Fixed::ONE);
        assert_eq!(Fixed::from_f64(0.5).raw(), 128);
        assert_eq!(Fixed::from_f64(-0.5).raw(), -128);
        assert_eq!(Fixed::from_f64(1.0).to_f64(), 1.0);
        // Hostile inputs must not panic and must not produce garbage.
        assert_eq!(Fixed::from_f64(f64::NAN), Fixed::ZERO);
        assert_eq!(Fixed::from_f64(f64::INFINITY), Fixed::MAX);
        assert_eq!(Fixed::from_f64(f64::NEG_INFINITY), Fixed::MIN);
        assert_eq!(Fixed::from_f64(1e300), Fixed::MAX);
    }

    #[test]
    fn fixed_arithmetic_saturates_rather_than_wrapping() {
        assert_eq!(Fixed::MAX.add(Fixed::ONE), Fixed::MAX);
        assert_eq!(Fixed::MIN.sub(Fixed::ONE), Fixed::MIN);
        assert_eq!(Fixed::MAX.mul(Fixed::from_int(2)), Fixed::MAX);
        assert_eq!(Fixed::ONE.div(Fixed::ZERO), None);
        assert_eq!(
            Fixed::from_int(6).div(Fixed::from_int(2)),
            Some(Fixed::from_int(3))
        );
        assert_eq!(Fixed::from_int(3).mul(Fixed::from_int(4)), Fixed::from_int(12));
    }

    #[test]
    fn fixed_display_is_exact_and_float_free() {
        assert_eq!(alloc::format!("{}", Fixed::from_f64(1.5)), "1.500");
        assert_eq!(alloc::format!("{}", Fixed::from_f64(-2.25)), "-2.250");
        assert_eq!(alloc::format!("{}", Fixed::ZERO), "0.000");
    }

    #[test]
    fn fixed_is_ordered_for_deterministic_sorting() {
        // Sort stability in the raster path depends on a total order.
        let mut v = [Fixed::from_int(3), Fixed::from_int(-1), Fixed::from_int(2)];
        v.sort_unstable();
        assert_eq!(v, [Fixed::from_int(-1), Fixed::from_int(2), Fixed::from_int(3)]);
    }

    #[test]
    fn matrix_composition_matches_the_cm_operator() {
        // Translate then scale: PDF's `cm` concatenates onto the CTM, so the
        // translation is scaled by the later scale.
        let m = Matrix::translate(10.0, 20.0).then(Matrix::scale(2.0, 3.0));
        let p = m.apply(Point::ORIGIN);
        assert_eq!(p.x, 20.0);
        assert_eq!(p.y, 60.0);
    }

    #[test]
    fn matrix_inverse_round_trips_and_refuses_singular() {
        let m = Matrix::new(2.0, 0.0, 0.0, 4.0, 5.0, 6.0);
        let inv = m.invert().expect("non-singular");
        let p = Point::new(3.0, 7.0);
        let back = inv.apply(m.apply(p));
        assert!((back.x - p.x).abs() < 1e-12);
        assert!((back.y - p.y).abs() < 1e-12);

        assert!(Matrix::scale(0.0, 1.0).invert().is_none());
        assert!(Matrix::new(f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0).invert().is_none());
    }

    #[test]
    fn identity_is_neutral() {
        let m = Matrix::new(1.5, 2.5, 3.5, 4.5, 5.5, 6.5);
        assert_eq!(m.then(Matrix::IDENTITY), m);
        assert_eq!(Matrix::IDENTITY.then(m), m);
    }

    #[test]
    fn rect_normalises_a_reversed_mediabox() {
        // /MediaBox [612 792 0 0] is legal and appears in the wild.
        let r = Rect::from_pdf_array([612.0, 792.0, 0.0, 0.0]);
        assert_eq!(r.to_pdf_array(), [0.0, 0.0, 612.0, 792.0]);
        assert_eq!(r.width(), 612.0);
        assert_eq!(r.height(), 792.0);
    }

    #[test]
    fn rect_intersection_and_union() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 15.0, 15.0);
        assert_eq!(a.intersect(b), Some(Rect::new(5.0, 5.0, 10.0, 10.0)));
        assert_eq!(a.union(b), Rect::new(0.0, 0.0, 15.0, 15.0));
        assert_eq!(a.intersect(Rect::new(20.0, 20.0, 30.0, 30.0)), None);
        // Touching edges are not an overlap.
        assert_eq!(a.intersect(Rect::new(10.0, 0.0, 20.0, 10.0)), None);
        assert!(a.contains_rect(Rect::new(1.0, 1.0, 2.0, 2.0)));
        assert!(!a.contains_rect(b));
    }

    #[test]
    fn rect_transform_bounds_a_rotation() {
        let r = Rect::new(0.0, 0.0, 10.0, 20.0);
        let t = r.transform(Matrix::rotate_quadrant(1));
        assert_eq!(t.width(), 20.0);
        assert_eq!(t.height(), 10.0);
    }

    #[test]
    fn rotation_normalises_and_composes() {
        assert_eq!(Rotation::from_degrees(0), Rotation::None);
        assert_eq!(Rotation::from_degrees(90), Rotation::Clockwise90);
        assert_eq!(Rotation::from_degrees(450), Rotation::Clockwise90);
        assert_eq!(Rotation::from_degrees(-90), Rotation::Clockwise270);
        assert_eq!(Rotation::from_degrees(-360), Rotation::None);
        assert_eq!(
            Rotation::Clockwise90.then(Rotation::Clockwise270),
            Rotation::None
        );
        assert!(Rotation::Clockwise90.swaps_axes());
        assert!(!Rotation::Half.swaps_axes());
        // Non-conformant values are detected, not rejected.
        assert!(!Rotation::is_conformant(45));
        assert!(Rotation::is_conformant(270));
        assert_eq!(Rotation::from_degrees(45), Rotation::None);
    }
}
