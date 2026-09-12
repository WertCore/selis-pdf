//! Page geometry in the render path (SL-2.RAST.12).
//!
//! ISO 32000-2 §14.11.2: page `/Rotate` is part of the rendered page view —
//! a viewer shall rotate the display of the page by the given number of
//! degrees clockwise, which swaps the output bitmap's width and height for
//! 90/270. The rotation composes with the page-to-device mapping (DPI scale,
//! y-flip) and with a `/MediaBox` whose dimensions are already swapped: the
//! rendered canvas is the rotated MediaBox, whatever its aspect.
//!
//! The mapping is pure f64 matrix arithmetic (exact quadrant rotations, no
//! transcendentals), so pixel output stays deterministic across platforms
//! (RAST.09, ADR-P0012).

use selis_geom::{Matrix, Rect, Rotation};

/// The rendered view of one page: the output bitmap size and the
/// user-space → device-pixel transform that honours `/Rotate`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageView {
    /// The canvas width in pixels.
    pub width: u32,
    /// The canvas height in pixels.
    pub height: u32,
    /// The transform from PDF user space to device pixels (y-down, origin
    /// top-left), with the page's `/Rotate` applied.
    pub ctm: Matrix,
}

/// The rendered view of a page box at `dpi`: canvas dimensions swap for
/// 90/270 rotation, and `PageView::ctm` maps user space onto that canvas.
///
/// # Malformed Input
///
/// A non-finite or negative DPI, or a non-finite page box, yields a zero
/// canvas rather than an unrenderable allocation; the caller refuses a zero
/// canvas with its own typed error.
#[must_use]
pub fn page_view(page_box: Rect, rotate: Rotation, dpi: f64) -> PageView {
    let s = if dpi.is_finite() && dpi > 0.0 {
        dpi / 72.0
    } else {
        1.0
    };
    let (x0, y0, x1, y1) = (page_box.x0, page_box.y0, page_box.x1, page_box.y1);
    let (w_dev, h_dev) = ((x1 - x0) * s, (y1 - y0) * s);
    // Page → device before rotation: scale to DPI and flip y (user space is
    // y-up, the raster is y-down), with the box origin translated away.
    let base = Matrix::new(s, 0.0, 0.0, -s, -x0 * s, h_dev + y0 * s);
    // The quadrant rotation about the rotated canvas (see `rot_matrix`).
    let rot = rot_matrix(rotate, w_dev, h_dev);
    let ctm = base.then(rot);
    let (w, h) = if rotate.swaps_axes() {
        (h_dev, w_dev)
    } else {
        (w_dev, h_dev)
    };
    PageView {
        width: dim_ceil(w),
        height: dim_ceil(h),
        ctm,
    }
}

/// The rotation matrix for a page `/Rotate` quadrant, over an unrotated
/// device raster of `w × h` pixels: `/Rotate` turns the page clockwise when
/// displayed (ISO 32000-2 §14.11.2).
///
/// * 90° cw: the page's top-left corner displays at the top-right —
///   `(u, v) → (h − v, u)`;
/// * 180°: top-left → bottom-right — `(u, v) → (w − u, h − v)`;
/// * 270° cw (90° ccw): top-left → bottom-left — `(u, v) → (v, w − u)`.
fn rot_matrix(rotate: Rotation, w: f64, h: f64) -> Matrix {
    match rotate {
        Rotation::None => Matrix::IDENTITY,
        Rotation::Clockwise90 => Matrix::new(0.0, 1.0, -1.0, 0.0, h, 0.0),
        Rotation::Half => Matrix::new(-1.0, 0.0, 0.0, -1.0, w, h),
        Rotation::Clockwise270 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, w),
    }
}

/// A finite, non-negative f64 as a u32 dimension (ceil, saturate).
fn dim_ceil(v: f64) -> u32 {
    let v = if v.is_finite() { v.ceil() } else { 0.0 };
    if v <= 0.0 {
        0
    } else if v >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        // The value is bounded to [0, u32::MAX], so the narrowing is exact.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        {
            v as u32
        }
    }
}

/// The axis-aligned device-space bounding box of the unit square under `m`
/// (all four corners, so quarter-turn and negative-scale matrices normalise).
#[must_use]
pub fn unit_square_aabb(m: Matrix) -> Rect {
    let corners = [
        m.apply(selis_geom::Point::new(0.0, 0.0)),
        m.apply(selis_geom::Point::new(1.0, 0.0)),
        m.apply(selis_geom::Point::new(0.0, 1.0)),
        m.apply(selis_geom::Point::new(1.0, 1.0)),
    ];
    let mut r = Rect::new(
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for p in &corners {
        r.x0 = r.x0.min(p.x);
        r.y0 = r.y0.min(p.y);
        r.x1 = r.x1.max(p.x);
        r.y1 = r.y1.max(p.y);
    }
    r
}

/// The device-space scale factor of `m` (the square root of |det|), for
/// stroke widths and other user-space magnitudes. A non-finite or singular
/// determinant maps to 1.0 — the path collapses anyway, so the width value
/// does not matter and a NaN must not reach the rasteriser.
#[must_use]
pub fn device_scale(m: Matrix) -> f64 {
    let det = m.determinant().abs();
    if !det.is_finite() || det == 0.0 {
        return 1.0;
    }
    det.sqrt()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, clippy::float_cmp)]

    use super::*;
    use selis_geom::Point;

    fn letter() -> Rect {
        Rect::new(0.0, 0.0, 612.0, 792.0)
    }

    #[test]
    fn unrotated_view_scales_and_flips() {
        let v = page_view(letter(), Rotation::None, 72.0);
        assert_eq!((v.width, v.height), (612, 792));
        // The user-space top-left corner (0, 792) lands at the device origin.
        let tl = v.ctm.apply(Point::new(0.0, 792.0));
        assert_eq!(tl, Point::new(0.0, 0.0));
        // The bottom-left corner lands on the bottom row.
        let bl = v.ctm.apply(Point::new(0.0, 0.0));
        assert_eq!(bl, Point::new(0.0, 792.0));
    }

    #[test]
    fn dpi_150_scales_the_canvas() {
        let v = page_view(letter(), Rotation::None, 150.0);
        assert_eq!((v.width, v.height), (1275, 1651));
        // 1 pt = 150/72 px (epsilon: the scale factor is not exactly
        // representable in binary floating point).
        let p = v.ctm.apply(Point::new(72.0, 720.0));
        assert!((p.x - 150.0).abs() < 1e-6, "got {p:?}");
        assert!((p.y - 150.0).abs() < 1e-6, "got {p:?}");
    }

    #[test]
    fn rotate_90_swaps_the_canvas_and_maps_corners() {
        let v = page_view(letter(), Rotation::Clockwise90, 72.0);
        assert_eq!((v.width, v.height), (792, 612));
        // Top-left of the page displays at the top-right of the canvas.
        let tl = v.ctm.apply(Point::new(0.0, 792.0));
        assert_eq!(tl, Point::new(792.0, 0.0));
        // Top-right displays at the bottom-right.
        let tr = v.ctm.apply(Point::new(612.0, 792.0));
        assert_eq!(tr, Point::new(792.0, 612.0));
        // Bottom-left displays at the top-left.
        let bl = v.ctm.apply(Point::new(0.0, 0.0));
        assert_eq!(bl, Point::new(0.0, 0.0));
    }

    #[test]
    fn rotate_180_keeps_the_canvas_size() {
        let v = page_view(letter(), Rotation::Half, 72.0);
        assert_eq!((v.width, v.height), (612, 792));
        // Top-left of the page displays at the bottom-right.
        let tl = v.ctm.apply(Point::new(0.0, 792.0));
        assert_eq!(tl, Point::new(612.0, 792.0));
        // Bottom-right of the page displays at the top-left.
        let br = v.ctm.apply(Point::new(612.0, 0.0));
        assert_eq!(br, Point::new(0.0, 0.0));
    }

    #[test]
    fn rotate_270_swaps_and_maps_corners() {
        let v = page_view(letter(), Rotation::Clockwise270, 72.0);
        assert_eq!((v.width, v.height), (792, 612));
        // Top-left of the page displays at the bottom-left.
        let tl = v.ctm.apply(Point::new(0.0, 792.0));
        assert_eq!(tl, Point::new(0.0, 612.0));
        // Bottom-left displays at the bottom-right.
        let bl = v.ctm.apply(Point::new(0.0, 0.0));
        assert_eq!(bl, Point::new(792.0, 612.0));
    }

    #[test]
    fn rotate_composes_with_a_swapped_media_box() {
        // Landscape Letter MediaBox with /Rotate 90: the canvas is portrait.
        let landscape = Rect::new(0.0, 0.0, 792.0, 612.0);
        let v = page_view(landscape, Rotation::Clockwise90, 150.0);
        assert_eq!((v.width, v.height), (1275, 1651));
        // A point on the page's top edge lands on the canvas's right edge.
        let p = v.ctm.apply(Point::new(0.0, 612.0));
        assert!(
            (p.x - 1275.0).abs() < 1e-9,
            "top edge → right edge, got {p:?}"
        );
        assert!((p.y - 0.0).abs() < 1e-9);
    }

    #[test]
    fn non_origin_media_box_translates_away() {
        let box_ = Rect::new(10.0, 20.0, 622.0, 812.0);
        let v = page_view(box_, Rotation::None, 72.0);
        assert_eq!((v.width, v.height), (612, 792));
        let tl = v.ctm.apply(Point::new(10.0, 812.0));
        assert_eq!(tl, Point::new(0.0, 0.0));
    }

    #[test]
    fn hostile_dpi_and_box_stay_finite() {
        let nan = page_view(letter(), Rotation::None, f64::NAN);
        assert_eq!(
            (nan.width, nan.height),
            (612, 792),
            "NaN dpi falls back to 72"
        );
        let inf_box = Rect::new(0.0, 0.0, f64::INFINITY, 792.0);
        let v = page_view(inf_box, Rotation::None, 72.0);
        assert_eq!(v.width, 0, "non-finite box saturates to a zero canvas");
        let zero = page_view(letter(), Rotation::None, 0.0);
        assert_eq!((zero.width, zero.height), (612, 792));
    }

    #[test]
    fn device_scale_matches_the_dpi_factor() {
        let v = page_view(letter(), Rotation::Clockwise90, 150.0);
        assert!((device_scale(v.ctm) - 150.0 / 72.0).abs() < 1e-9);
        assert_eq!(device_scale(Matrix::IDENTITY), 1.0);
        assert_eq!(device_scale(Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0)), 1.0);
    }

    #[test]
    fn unit_square_aabb_normalises_flipped_and_rotated_squares() {
        let flipped = Matrix::new(2.0, 0.0, 0.0, -3.0, 5.0, 7.0);
        let r = unit_square_aabb(flipped);
        assert_eq!(r, Rect::new(5.0, 4.0, 7.0, 7.0));
        let rot = Matrix::new(0.0, 1.0, -1.0, 0.0, 10.0, 0.0);
        let r = unit_square_aabb(rot);
        assert_eq!(r, Rect::new(9.0, 0.0, 10.0, 1.0));
    }
}
