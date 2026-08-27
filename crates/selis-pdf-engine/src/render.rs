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
use selis_pdf_content::display_list::{DisplayList, Op, ResolvedState};
use selis_pdf_content::path::{Path as ContentPath, Segment};
use selis_raster::{
    Backend, FillRule, Paint as RasterPaint, Path as RasterPath, PathCmd, StrokeSpec,
    TinySkiaBackend,
};
use selis_sandbox::BudgetGuard;

/// Render a display list onto a backend.
///
/// `font_data` resolves a font resource name to the font program bytes (the
/// engine's document layer provides this).
///
/// # Malformed Input
///
/// A degenerate or unrenderable op is skipped (a deviation), never fatal.
pub fn render_display_list(
    dl: &DisplayList,
    backend: &mut TinySkiaBackend,
    font_data: &dyn Fn(&selis_bytes::Bytes) -> Option<Vec<u8>>,
    resolve_smask: &dyn Fn(&selis_bytes::Bytes) -> Option<selis_raster::Mask>,
    resolve_inline_image: &dyn Fn(&[(selis_bytes::Bytes, selis_bytes::Bytes)], &[u8]) -> Option<(
        u32,
        u32,
        selis_bytes::Bytes,
    )>,
    resolve_shading: &dyn Fn(&selis_bytes::Bytes, &ResolvedState) -> Option<(
        u32,
        u32,
        selis_bytes::Bytes,
        selis_geom::Rect,
    )>,
    resolve_pattern: &dyn Fn(&selis_bytes::Bytes) -> Option<selis_raster::pattern::TilingPattern>,
    g: &mut BudgetGuard<'_>,
) {
    // The blend mode, clip, and soft mask are per-op resolved state; emit
    // backend state only when they change.
    let mut current_blend = selis_color::BlendMode::Normal;
    let mut current_clip: Vec<(selis_pdf_content::path::Path, selis_pdf_content::path::ClipRule)> =
        Vec::new();
    let mut current_smask: Option<selis_bytes::Bytes> = None;
    for op in &dl.ops {
        // Transparency group boundaries have no per-op paint state.
        if let Op::PushLayer { blend, alpha } = op {
            backend.push_layer(*blend, *alpha);
            continue;
        }
        if matches!(op, Op::PopLayer) {
            backend.pop_layer();
            continue;
        }
        let state = op_state(op);
        if state.blend != current_blend {
            backend.set_blend(state.blend);
            current_blend = state.blend;
        }
        if state.clip != current_clip {
            backend.clear_clip();
            for (path, rule) in &state.clip {
                if let Some(p) = to_raster_path(path) {
                    backend.clip(
                        &p,
                        match rule {
                            selis_pdf_content::path::ClipRule::NonZero => FillRule::NonZero,
                            selis_pdf_content::path::ClipRule::EvenOdd => FillRule::EvenOdd,
                        },
                    );
                }
            }
            current_clip = state.clip.clone();
        }
        if state.soft_mask != current_smask {
            let mask = match &state.soft_mask {
                Some(key) => resolve_smask(key),
                None => None,
            };
            backend.set_soft_mask(mask.as_ref());
            current_smask = state.soft_mask.clone();
        }
        match op {
            Op::Fill { path, state } => {
                let Some(p) = to_raster_path(path) else {
                    continue;
                };
                if let Some(pattern_name) = &state.fill_pattern {
                    if let Some(pattern) = resolve_pattern(pattern_name) {
                        draw_pattern(backend, &pattern, &p, state, g);
                        continue;
                    }
                }
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
            Op::Text { at, state, runs } => {
                for run in runs {
                    let Some(font_bytes) = font_data(&run.font) else {
                        continue;
                    };
                    let fb = selis_bytes::Bytes::from(font_bytes);
                    // The outline coordinates are in font units; the text
                    // transform scales them by size/upem and positions them.
                    let upem = selis_font::units_per_em(&fb)
                        .map(f64::from)
                        .unwrap_or(1000.0);
                    let scale = run.size / upem;

                    for &code in &run.glyphs {
                        let Some(gid) = selis_font::glyph_id_for_char(&fb, u32::from(code)) else {
                            continue;
                        };
                        let Some(outline) = selis_font::outline_glyph(&fb, gid, g).ok().flatten()
                        else {
                            continue;
                        };
                        let m = state
                            .ctm
                            .then(Matrix::translate(at.x, at.y))
                            .then(Matrix::scale(scale, scale));
                        let transformed = transform_outline(&outline, m);
                        let Some(p) = raster_path_from_commands(&transformed) else {
                            continue;
                        };
                        let paint = paint(&state.fill, state.alpha_fill);
                        selis_raster::render::fill(backend, &p, FillRule::NonZero, &paint);
                    }
                }
            }
            Op::Image {
                rgba8,
                width,
                height,
                rect,
                ..
            } => {
                let img = selis_raster::Image {
                    width: *width,
                    height: *height,
                    rgba8: rgba8.as_slice().to_vec(),
                };
                let placement = selis_raster::ImagePlacement { rect: *rect };
                backend.draw_image(&img, &placement);
            }
            Op::InlineImage { dict, data, state } => {
                let Some((w, h, rgba8)) = resolve_inline_image(dict, data) else {
                    continue;
                };
                let p0 = state.ctm.apply(Point::new(0.0, 0.0));
                let p1 = state.ctm.apply(Point::new(1.0, 1.0));
                let img = selis_raster::Image {
                    width: w,
                    height: h,
                    rgba8: rgba8.as_slice().to_vec(),
                };
                let placement = selis_raster::ImagePlacement {
                    rect: selis_geom::Rect::new(p0.x, p0.y, p1.x, p1.y),
                };
                backend.draw_image(&img, &placement);
            }
            Op::Shading { name, state } => {
                let Some((w, h, rgba8, rect)) = resolve_shading(name, state) else {
                    continue;
                };
                let img = selis_raster::Image {
                    width: w,
                    height: h,
                    rgba8: rgba8.as_slice().to_vec(),
                };
                let placement = selis_raster::ImagePlacement { rect };
                backend.draw_image(&img, &placement);
            }
            Op::PushLayer { .. } | Op::PopLayer => {
                // Handled before the paint dispatch; unreachable here.
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
                let a = current.unwrap_or(*b);
                commands.push(PathCmd::Cubic(a, *b, *c));
                current = Some(*c);
            }
            Segment::CubicSecond(a, c) => {
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

/// Transform a glyph outline (in font units) by a matrix, producing raster
/// path commands. Quadratic Béziers are converted to cubics.
#[must_use]
fn transform_outline(outline: &selis_font::Outline, m: Matrix) -> Vec<PathCmd> {
    let mut out = Vec::new();
    let mut current: Option<Point> = None;
    for cmd in &outline.commands {
        match cmd {
            selis_font::OutlineCmd::Move { x, y } => {
                let p = m.apply(Point::new(*x, *y));
                out.push(PathCmd::Move(p));
                current = Some(p);
            }
            selis_font::OutlineCmd::Line { x, y } => {
                let p = m.apply(Point::new(*x, *y));
                out.push(PathCmd::Line(p));
                current = Some(p);
            }
            selis_font::OutlineCmd::Quad { cx, cy, x, y } => {
                let start = current.unwrap_or(Point::new(0.0, 0.0));
                let c = m.apply(Point::new(*cx, *cy));
                let end = m.apply(Point::new(*x, *y));
                // Quadratic → cubic conversion.
                let c1 = Point::new(
                    start.x + 2.0 / 3.0 * (c.x - start.x),
                    start.y + 2.0 / 3.0 * (c.y - start.y),
                );
                let c2 = Point::new(
                    end.x + 2.0 / 3.0 * (c.x - end.x),
                    end.y + 2.0 / 3.0 * (c.y - end.y),
                );
                out.push(PathCmd::Cubic(c1, c2, end));
                current = Some(end);
            }
            selis_font::OutlineCmd::Cubic {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                let c1 = m.apply(Point::new(*c1x, *c1y));
                let c2 = m.apply(Point::new(*c2x, *c2y));
                let end = m.apply(Point::new(*x, *y));
                out.push(PathCmd::Cubic(c1, c2, end));
                current = Some(end);
            }
            selis_font::OutlineCmd::Close => {
                out.push(PathCmd::Close);
            }
        }
    }
    out
}

/// Build a raster path from a non-empty command list.
#[must_use]
fn raster_path_from_commands(commands: &[PathCmd]) -> Option<RasterPath> {
    if commands.is_empty() {
        return None;
    }
    Some(RasterPath {
        commands: commands.to_vec(),
    })
}

/// The resolved state of any paint op (the caller handles group boundaries
/// before calling this).
fn op_state(op: &Op) -> &ResolvedState {    match op {
        Op::Fill { state, .. }
        | Op::Stroke { state, .. }
        | Op::FillStroke { state, .. }
        | Op::Text { state, .. }
        | Op::Image { state, .. }
        | Op::InlineImage { state, .. }
        | Op::Shading { state, .. } => state,
        Op::PushLayer { .. } | Op::PopLayer => {
            unreachable!("group ops are handled before op_state")
        }
    }
}

/// Draw a tiling pattern over a fill region (the path's device bounding box):
/// plan the tile instances and draw each.
fn draw_pattern(
    backend: &mut TinySkiaBackend,
    pattern: &selis_raster::pattern::TilingPattern,
    path: &selis_raster::Path,
    state: &ResolvedState,
    g: &mut BudgetGuard<'_>,
) {
    let Some(user_rect) = path_bounds(path) else {
        return;
    };
    let p0 = state.ctm.apply(Point::new(user_rect.x0, user_rect.y0));
    let p1 = state.ctm.apply(Point::new(user_rect.x1, user_rect.y1));
    let region = selis_geom::Rect::new(
        p0.x.min(p1.x),
        p0.y.min(p1.y),
        p0.x.max(p1.x),
        p0.y.max(p1.y),
    );
    let Ok(plan) = selis_raster::pattern::plan_pattern(pattern, state.ctm, region, g) else {
        return;
    };
    let img = selis_raster::Image {
        width: pattern.tile.width,
        height: pattern.tile.height,
        rgba8: pattern.tile.rgba8.clone(),
    };
    for inst in &plan.instances {
        let placement = selis_raster::ImagePlacement { rect: inst.rect };
        backend.draw_image(&img, &placement);
    }
}

/// The axis-aligned bounding box of a raster path (user space).
fn path_bounds(path: &selis_raster::Path) -> Option<selis_geom::Rect> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for cmd in &path.commands {
        let p = match cmd {
            selis_raster::PathCmd::Move(p) | selis_raster::PathCmd::Line(p) => Some(*p),
            selis_raster::PathCmd::Cubic(a, b, c) => {
                min_x = min_x.min(a.x).min(b.x);
                min_y = min_y.min(a.y).min(b.y);
                Some(*c)
            }
            selis_raster::PathCmd::Close => None,
        };
        if let Some(p) = p {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
    }
    if min_x > max_x {
        return None;
    }
    Some(selis_geom::Rect::new(min_x, min_y, max_x, max_y))
}

/// The raster paint for a device-RGB colour.
fn paint(rgb: &[f64; 3], alpha: f64) -> RasterPaint {
    RasterPaint {
        colour: Rgba::new(
            rgb.first().copied().unwrap_or(0.0).clamp(0.0, 1.0),
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

    fn no_font(_font: &selis_bytes::Bytes) -> Option<Vec<u8>> {
        None
    }

    fn no_do(_name: &selis_bytes::Bytes) -> Option<selis_pdf_content::exec::DoTarget> {
        None
    }

    fn no_ext_gstate(_name: &selis_bytes::Bytes) -> Option<Vec<(selis_bytes::Bytes, selis_pdf_content::dispatch::Operand)>> {
        None
    }

    fn no_smask(_key: &selis_bytes::Bytes) -> Option<selis_raster::Mask> {
        None
    }

    fn no_inline_image(
        _dict: &[(selis_bytes::Bytes, selis_bytes::Bytes)],
        _data: &[u8],
    ) -> Option<(u32, u32, selis_bytes::Bytes)> {
        None
    }

    fn no_shading(
        _name: &selis_bytes::Bytes,
        _state: &ResolvedState,
    ) -> Option<(u32, u32, selis_bytes::Bytes, selis_geom::Rect)> {
        None
    }

    fn no_pattern(_name: &selis_bytes::Bytes) -> Option<selis_raster::pattern::TilingPattern> {
        None
    }

    /// A content stream drawing a filled red square renders red pixels.
    #[test]
    fn a_filled_rectangle_renders_pixels() {
        let mut g = guard();
        let content = b"0 0 m 0 100 l 100 100 l 100 0 l h 1 0 0 rg f";
        let dl = selis_pdf_content::exec::execute(
            content,
            &const_width,
            &no_do,
            &no_ext_gstate,
            &mut g,
        )
        .expect("execute");
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend, &no_font, &no_smask, &no_inline_image, &no_shading, &no_pattern, &mut g);
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
        let dl = selis_pdf_content::exec::execute(
            content,
            &const_width,
            &no_do,
            &no_ext_gstate,
            &mut g,
        )
        .expect("execute");
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend, &no_font, &no_smask, &no_inline_image, &no_shading, &no_pattern, &mut g);
        let data = backend.pixmap().data();
        // Centre (50,50) is red; corner (5,5) is blue.
        let centre = (50 * 100 + 50) * 4;
        let corner = (5 * 100 + 5) * 4;
        assert_eq!(&data[centre..centre + 3], &[255, 0, 0]);
        assert_eq!(&data[corner..corner + 3], &[0, 0, 255]);
    }

    /// A content stream with a `Do` for a 2×2 image renders it scaled.
    #[test]
    fn an_image_xobject_renders() {
        use selis_pdf_content::exec::DoTarget;
        let mut g = guard();
        // 2x2 RGBA: top-left red, rest blue.
        let rgba8 = vec![
            255u8, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
        ];
        let do_image = |_name: &selis_bytes::Bytes| {
            Some(DoTarget::Image {
                width: 2,
                height: 2,
                rgba8: selis_bytes::Bytes::copy_from_slice(&rgba8),
            })
        };
        // Scale the unit square to 0..100 so the image fills the canvas.
        let content = b"100 0 0 100 0 0 cm /Im1 Do";
        let dl = selis_pdf_content::exec::execute(content, &const_width, &do_image, &no_ext_gstate, &mut g)
            .expect("execute");
        assert_eq!(dl.ops.len(), 1);
        assert!(matches!(dl.ops[0], Op::Image { .. }));
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend, &no_font, &no_smask, &no_inline_image, &no_shading, &no_pattern, &mut g);
        let data = backend.pixmap().data();
        // Top-left (10,10) is red; bottom-right (90,90) is blue.
        let tl = (10 * 100 + 10) * 4;
        let br = (90 * 100 + 90) * 4;
        assert_eq!(&data[tl..tl + 3], &[255, 0, 0]);
        assert_eq!(&data[br..br + 3], &[0, 0, 255]);
    }

    /// A `/Multiply` blend darkens the fill against a gray backdrop, rather
    /// than source-over replacing it.
    #[test]
    fn multiply_blend_darkens_the_fill() {
        let mut g = guard();
        // Gray page, then red on top with /Multiply: red = 0.5 × 1.0 = 0.5.
        let content = b"0 0 m 0 100 l 100 100 l 100 0 l h 0.5 g f \
                        /GS1 gs 25 25 m 25 75 l 75 75 l 75 25 l h 1 0 0 rg f";
        let ext = |name: &selis_bytes::Bytes| {
            if name.as_slice() == b"GS1" {
                Some(vec![(
                    selis_bytes::Bytes::copy_from_slice(b"BM"),
                    selis_pdf_content::dispatch::Operand::Name(
                        selis_bytes::Bytes::copy_from_slice(b"Multiply"),
                    ),
                )])
            } else {
                None
            }
        };
        let dl = selis_pdf_content::exec::execute(content, &const_width, &no_do, &ext, &mut g)
            .expect("execute");
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend, &no_font, &no_smask, &no_inline_image, &no_shading, &no_pattern, &mut g);
        let data = backend.pixmap().data();
        let centre = (50 * 100 + 50) * 4;
        // Multiply of gray (≈128) and red (255) leaves ≈128 red, not 255, and
        // the green/blue channels are suppressed.
        assert!(
            data[centre] < 200,
            "red should be darkened by Multiply, got {}",
            data[centre]
        );
        assert_eq!(data[centre + 1], 0, "green should be zero");
    }

    /// A clipped fill only paints inside the clip rectangle; pixels outside
    /// the clip stay white.
    #[test]
    fn clip_restricts_rendered_pixels() {
        let mut g = guard();
        // Fill the whole canvas gray, then clip to a 50×50 rect and paint
        // red inside it. Pixels outside the clip should be gray, not red.
        let content = b"0 0 m 100 0 l 100 100 l 0 100 l h 0.5 g f \
                        0 0 m 50 0 l 50 50 l 0 50 l h W \
                        0 0 m 100 0 l 100 100 l 0 100 l h 1 0 0 rg f";
        let dl = selis_pdf_content::exec::execute(content, &const_width, &no_do, &no_ext_gstate, &mut g)
            .expect("execute");
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend, &no_font, &no_smask, &no_inline_image, &no_shading, &no_pattern, &mut g);
        let data = backend.pixmap().data();
        // Inside the clip (25, 25): red (the red fill covers the clip area).
        let inside = (25 * 100 + 25) * 4;
        assert_eq!(&data[inside..inside + 3], &[255, 0, 0], "inside clip should be red");
        // Outside the clip (75, 75): gray (0.5 → 127), not red.
        let outside = (75 * 100 + 75) * 4;
        assert_eq!(data[outside], 127, "outside clip should be gray");
        assert_eq!(data[outside + 1], 127);
        assert_eq!(data[outside + 2], 127);
    }

    /// A transparency group with alpha 0.5 composites its content at 50%
    /// over the backdrop: blue at 50% over red is magenta.
    #[test]
    fn group_alpha_composites_over_the_backdrop() {
        let mut g = guard();
        // Fill the canvas red, then a group tagged /GS1 (alpha 0.5) fills blue.
        let content = b"0 0 m 100 0 l 100 100 l 0 100 l h 1 0 0 rg f \
                        /GS1 BDC 0 0 m 100 0 l 100 100 l 0 100 l h 0 0 1 rg f EMC";
        let ext = |name: &selis_bytes::Bytes| {
            if name.as_slice() == b"GS1" {
                Some(vec![(
                    selis_bytes::Bytes::copy_from_slice(b"ca"),
                    selis_pdf_content::dispatch::Operand::Num(0.5),
                )])
            } else {
                None
            }
        };
        let dl = selis_pdf_content::exec::execute(content, &const_width, &no_do, &ext, &mut g)
            .expect("execute");
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend, &no_font, &no_smask, &no_inline_image, &no_shading, &no_pattern, &mut g);
        let data = backend.pixmap().data();
        let centre = (50 * 100 + 50) * 4;
        // 0.5 × blue(0,0,255) + 0.5 × red(255,0,0) = 127.5 → 128.
        assert_eq!(data[centre], 128, "red channel ≈127.5");
        assert_eq!(data[centre + 1], 0, "green channel none");
        assert_eq!(data[centre + 2], 128, "blue channel ≈127.5");
    }

    /// A soft mask (from `/GS1 gs` with an SMask key) modulates the alpha of
    /// everything painted while active: a black fill becomes 50% alpha.
    #[test]
    fn soft_mask_modulates_alpha() {
        let mut g = guard();
        let ext = |name: &selis_bytes::Bytes| {
            if name.as_slice() == b"GS1" {
                Some(vec![(
                    selis_bytes::Bytes::copy_from_slice(b"SMask"),
                    selis_pdf_content::dispatch::Operand::Name(
                        selis_bytes::Bytes::copy_from_slice(b"1 0"),
                    ),
                )])
            } else {
                None
            }
        };
        let resolve_smask = |key: &selis_bytes::Bytes| -> Option<selis_raster::Mask> {
            if key.as_slice() == b"1 0" {
                Some(selis_raster::Mask {
                    width: 1,
                    height: 1,
                    alpha8: vec![128],
                })
            } else {
                None
            }
        };
        let content = b"/GS1 gs 0 0 m 100 0 l 100 100 l 0 100 l h 0 g f";
        let dl = selis_pdf_content::exec::execute(
            content,
            &const_width,
            &no_do,
            &ext,
            &mut g,
        )
        .expect("execute");
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend, &no_font, &resolve_smask, &no_inline_image, &no_shading, &no_pattern, &mut g);
        let data = backend.pixmap().data();
        let centre = (50 * 100 + 50) * 4;
        // The black fill is 50% alpha (mask 128), not fully opaque.
        assert_eq!(data[centre], 0, "black");
        assert_eq!(data[centre + 3], 128, "alpha halved by the mask");
    }

    /// An inline image (`BI`/`ID`/`EI`) is decoded and rendered.
    #[test]
    fn inline_image_renders() {
        let mut g = guard();
        // 2x2 RGB: top-left red, rest blue, scaled to fill the canvas.
        let content = b"100 0 0 100 0 0 cm \
                        BI /W 2 /H 2 /BPC 8 /CS /RGB /L 12 ID \
                        \xff\x00\x00\x00\x00\xff\x00\x00\xff\x00\x00\xff EI";
        let dl = selis_pdf_content::exec::execute(
            content,
            &const_width,
            &no_do,
            &no_ext_gstate,
            &mut g,
        )
        .expect("execute");
let resolve_inline =
            |dict: &[(selis_bytes::Bytes, selis_bytes::Bytes)],
             data: &[u8]|
             -> Option<(u32, u32, selis_bytes::Bytes)> {
                let parse = |key: &[u8]| -> Option<u32> {
                    dict.iter()
                        .find(|(k, _)| k.as_slice() == key)
                        .and_then(|(_, v)| {
                            std::str::from_utf8(v.as_slice()).ok()?.trim().parse().ok()
                        })
                };
                let w = parse(b"W")?;
                let h = parse(b"H")?;
                // The raw data is RGB (3 bytes/pixel); pad to RGBA.
                let mut rgba = Vec::with_capacity(data.len() / 3 * 4);
                for chunk in data.chunks(3) {
                    rgba.extend_from_slice(chunk);
                    rgba.push(255);
                }
                Some((w, h, selis_bytes::Bytes::copy_from_slice(&rgba)))
            };
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(&dl, &mut backend, &no_font, &no_smask, &resolve_inline, &no_shading, &no_pattern, &mut g);
        let data = backend.pixmap().data();
        let tl = (10 * 100 + 10) * 4;
        let br = (90 * 100 + 90) * 4;
        assert_eq!(&data[tl..tl + 3], &[255, 0, 0], "top-left red");
        assert_eq!(&data[br..br + 3], &[0, 0, 255], "bottom-right blue");
    }

    /// A fill with an active tiling pattern renders the pattern's tiles over
    /// the fill region instead of a solid colour.
    #[test]
    fn pattern_fill_renders_tiles() {
        let mut g = guard();
        let content = b"/Pattern cs /Pat1 scn 0 0 m 100 0 l 100 100 l 0 100 l h f";
        let dl = selis_pdf_content::exec::execute(
            content,
            &const_width,
            &no_do,
            &no_ext_gstate,
            &mut g,
        )
        .expect("execute");
        let resolve_pattern = |name: &selis_bytes::Bytes| {
            if name.as_slice() == b"Pat1" {
                Some(selis_raster::pattern::TilingPattern {
                    paint_type: selis_raster::pattern::PatternType::Coloured,
                    tile: selis_raster::pattern::PatternTile {
                        width: 50,
                        height: 50,
                        rgba8: (0..2500).flat_map(|_| [255u8, 0, 0, 255]).collect(),
                    },
                    x_step: 50.0,
                    y_step: 50.0,
                    matrix: selis_geom::Matrix::IDENTITY,
                })
            } else {
                None
            }
        };
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(
            &dl,
            &mut backend,
            &no_font,
            &no_smask,
            &no_inline_image,
            &no_shading,
            &resolve_pattern,
            &mut g,
        );
        let data = backend.pixmap().data();
        let centre = (50 * 100 + 50) * 4;
assert_eq!(&data[centre..centre + 3], &[255, 0, 0], "pattern tile fills red");
    }

    /// A shading (`/Name sh`) is resolved and rasterised by the engine, then
    /// drawn as an image.
    #[test]
    fn shading_renders() {
        let mut g = guard();
        let content = b"/GS1 sh";
        let dl = selis_pdf_content::exec::execute(
            content,
            &const_width,
            &no_do,
            &no_ext_gstate,
            &mut g,
        )
        .expect("execute");
        assert!(matches!(dl.ops[0], Op::Shading { .. }));
        let resolve_shading =
            |name: &selis_bytes::Bytes,
             _state: &ResolvedState|
             -> Option<(u32, u32, selis_bytes::Bytes, selis_geom::Rect)> {
                if name.as_slice() == b"GS1" {
                    let rgba: Vec<u8> = (0..10_000u32)
                        .flat_map(|_| [255u8, 0, 0, 255])
                        .collect();
                    Some((
                        100,
                        100,
                        selis_bytes::Bytes::copy_from_slice(&rgba),
                        selis_geom::Rect::new(0.0, 0.0, 100.0, 100.0),
                    ))
                } else {
                    None
                }
            };
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        render_display_list(
            &dl,
            &mut backend,
            &no_font,
            &no_smask,
            &no_inline_image,
            &resolve_shading,
            &no_pattern,
            &mut g,
        );
        let data = backend.pixmap().data();
        let centre = (50 * 100 + 50) * 4;
        assert_eq!(&data[centre..centre + 3], &[255, 0, 0], "shading fills red");
    }
}
