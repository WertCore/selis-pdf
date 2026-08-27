//! The tiny-skia raster backend (SL-2.RAST.01).
//!
//! Maps the [`Backend`](crate::Backend) trait to tiny-skia canvas operations
//! (via `PixmapMut`, tiny-skia 0.11).

// tiny-skia is f32 throughout; the f64 -> f32 narrowing is inherent to the
// backend boundary and bounded by the page dimensions.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Pixmap, PixmapMut, PixmapPaint, Stroke, StrokeDash,
    Transform,
};

use crate::{Backend, Image, ImagePlacement, Mask, Path, PathCmd};

/// A backend backed by a tiny-skia pixmap.
pub struct TinySkiaBackend {
    /// The underlying pixmap.
    pixmap: Pixmap,
    /// The pixmap width.
    width: u32,
    /// The pixmap height.
    height: u32,
    /// The active soft mask (device-space alpha), if any.
    soft_mask: Option<tiny_skia::Mask>,
    /// The blend mode for subsequent ops.
    blend: crate::BlendMode,
    /// The accumulated clip mask (intersection of all `clip` calls), if any.
    clip_mask: Option<tiny_skia::Mask>,
    /// The transparency-group layer stack (RAST.04).
    layers: Vec<LayerState>,
}

/// A pushed transparency group: the parent canvas and the state to restore
/// when the group is composited back.
struct LayerState {
    /// The parent pixmap (restored on pop).
    parent: Pixmap,
    /// The soft mask saved on push (applied to the group's composite).
    soft_mask: Option<tiny_skia::Mask>,
    /// The blend mode saved on push (restored on pop).
    blend: crate::BlendMode,
    /// The group's blend mode (used to composite the layer back).
    group_blend: crate::BlendMode,
    /// The group's alpha (used to composite the layer back).
    group_alpha: f64,
}

/// The maximum group nesting depth (a hostile BDC/EMC must terminate).
const MAX_LAYER_DEPTH: usize = 32;

impl TinySkiaBackend {
    /// Create a new raster of `width × height` pixels.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Option<Self> {
        let pixmap = Pixmap::new(width, height)?;
        Some(Self {
            pixmap,
            width,
            height,
            soft_mask: None,
            blend: crate::BlendMode::Normal,
            clip_mask: None,
            layers: Vec::new(),
        })
    }

    /// The underlying pixmap (for saving).
    #[must_use]
    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }

    /// The underlying pixmap, mutable.
    #[must_use]
    pub fn pixmap_mut(&mut self) -> &mut Pixmap {
        &mut self.pixmap
    }

    /// The dimensions.
    #[must_use]
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Run a paint closure, compositing through the active soft mask when one
    /// is set. The closure draws into a temporary layer; the layer is then
    /// composited onto the canvas with the mask modulating its alpha.
    fn with_soft_mask(&mut self, paint: impl FnOnce(PixmapMut<'_>)) {
        let Some(mask) = self.soft_mask.as_ref() else {
            paint(self.pixmap.as_mut());
            return;
        };
        let Some(mut layer) = Pixmap::new(self.width, self.height) else {
            return; // zero canvas: nothing to draw
        };
        paint(layer.as_mut());
        let ts_paint = PixmapPaint {
            blend_mode: to_ts_blend(self.blend),
            ..PixmapPaint::default()
        };
        self.pixmap.as_mut().draw_pixmap(
            0,
            0,
            layer.as_ref(),
            &ts_paint,
            Transform::identity(),
            Some(mask),
        );
    }
}

/// Convert a device-space [`Mask`] to a canvas-sized `tiny_skia::Mask`,
/// resampling by nearest-neighbour when the dimensions differ. The caller is
/// expected to provide a canvas-aligned mask; this only guards against a
/// mismatch.
fn to_ts_mask(mask: &Mask, canvas_w: u32, canvas_h: u32) -> Option<tiny_skia::Mask> {
    let w = canvas_w.max(1);
    let h = canvas_h.max(1);
    let size = tiny_skia::IntSize::from_wh(w, h)?;
    let expected = (w as usize).saturating_mul(h as usize);
    if mask.width == w && mask.height == h && mask.alpha8.len() == expected {
        return tiny_skia::Mask::from_vec(mask.alpha8.clone(), size);
    }
    // Grow incrementally: the size is bounded by the canvas pixmap that this
    // backend already holds (w × h bytes ≤ the pixmap's w × h × 4), and the
    // Backend trait carries no budget guard to charge a sized pre-allocation.
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            data.push(mask.sample(x, y));
        }
    }
    tiny_skia::Mask::from_vec(data, size)
}

fn to_ts_path(path: &Path) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for cmd in &path.commands {
        match cmd {
            PathCmd::Move(p) => {
                pb.move_to(p.x as f32, p.y as f32);
            }
            PathCmd::Line(p) => {
                pb.line_to(p.x as f32, p.y as f32);
            }
            PathCmd::Cubic(c1, c2, p) => {
                pb.cubic_to(
                    c1.x as f32,
                    c1.y as f32,
                    c2.x as f32,
                    c2.y as f32,
                    p.x as f32,
                    p.y as f32,
                );
            }
            PathCmd::Close => {
                pb.close();
            }
        }
    }
    pb.finish()
}

/// The colour components are clamped in [0,1], so the RGBA8 narrowing is
/// exact in range.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn to_ts_paint_sub(paint: &crate::Paint) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(
        (paint.colour.rgb.r * 255.0) as u8,
        (paint.colour.rgb.g * 255.0) as u8,
        (paint.colour.rgb.b * 255.0) as u8,
        (paint.colour.a * 255.0) as u8,
    );
    p
}

/// Map a selis blend mode to a tiny-skia blend mode.
fn to_ts_blend(blend: crate::BlendMode) -> tiny_skia::BlendMode {
    use crate::BlendMode;
    match blend {
        BlendMode::Normal => tiny_skia::BlendMode::SourceOver,
        BlendMode::Multiply => tiny_skia::BlendMode::Multiply,
        BlendMode::Screen => tiny_skia::BlendMode::Screen,
        BlendMode::Overlay => tiny_skia::BlendMode::Overlay,
        BlendMode::Darken => tiny_skia::BlendMode::Darken,
        BlendMode::Lighten => tiny_skia::BlendMode::Lighten,
        BlendMode::ColorDodge => tiny_skia::BlendMode::ColorDodge,
        BlendMode::ColorBurn => tiny_skia::BlendMode::ColorBurn,
        BlendMode::HardLight => tiny_skia::BlendMode::HardLight,
        BlendMode::SoftLight => tiny_skia::BlendMode::SoftLight,
        BlendMode::Difference => tiny_skia::BlendMode::Difference,
        BlendMode::Exclusion => tiny_skia::BlendMode::Exclusion,
        BlendMode::Hue => tiny_skia::BlendMode::Hue,
        BlendMode::Saturation => tiny_skia::BlendMode::Saturation,
        BlendMode::Color => tiny_skia::BlendMode::Color,
        BlendMode::Luminosity => tiny_skia::BlendMode::Luminosity,
        BlendMode::Unknown => tiny_skia::BlendMode::SourceOver,
    }
}

fn to_ts_stroke(stroke: &crate::Stroke) -> Stroke {
    let dash: Vec<f32> = stroke.dash.0.iter().map(|&v| v as f32).collect();
    Stroke {
        width: stroke.width as f32,
        miter_limit: stroke.miter_limit as f32,
        line_cap: match stroke.cap {
            0 => LineCap::Butt,
            1 => LineCap::Round,
            _ => LineCap::Square,
        },
        line_join: match stroke.join {
            0 => LineJoin::Miter,
            1 => LineJoin::Round,
            _ => LineJoin::Bevel,
        },
        dash: StrokeDash::new(dash, stroke.dash.1 as f32),
    }
}

fn to_ts_fill_rule(rule: crate::FillRule) -> FillRule {
    match rule {
        crate::FillRule::NonZero => FillRule::Winding,
        crate::FillRule::EvenOdd => FillRule::EvenOdd,
    }
}

impl Backend for TinySkiaBackend {
    fn fill(&mut self, path: &Path, rule: crate::FillRule, paint: &crate::Paint) {
        let Some(ts_path) = to_ts_path(path) else {
            return;
        };
        let mut ts_paint = to_ts_paint_sub(paint);
        ts_paint.blend_mode = to_ts_blend(self.blend);
        let rule = to_ts_fill_rule(rule);
        let clip = self.clip_mask.take();
        self.with_soft_mask(|mut layer| {
            layer.fill_path(
                &ts_path,
                &ts_paint,
                rule,
                Transform::identity(),
                clip.as_ref(),
            );
        });
        self.clip_mask = clip;
    }

    fn stroke(&mut self, path: &Path, paint: &crate::Paint, stroke: &crate::Stroke) {
        let Some(ts_path) = to_ts_path(path) else {
            return;
        };
        let mut ts_paint = to_ts_paint_sub(paint);
        ts_paint.blend_mode = to_ts_blend(self.blend);
        let ts_stroke = to_ts_stroke(stroke);
        let clip = self.clip_mask.take();
        self.with_soft_mask(|mut layer| {
            layer.stroke_path(
                &ts_path,
                &ts_paint,
                &ts_stroke,
                Transform::identity(),
                clip.as_ref(),
            );
        });
        self.clip_mask = clip;
    }

    fn draw_image(&mut self, image: &Image, placement: &ImagePlacement) {
        let size = match tiny_skia::IntSize::from_wh(image.width, image.height) {
            Some(s) => s,
            None => return, // zero-size image: nothing to draw
        };
        let Some(src) = Pixmap::from_vec(image.rgba8.clone(), size) else {
            return;
        };
        let ts = Transform::from_scale(
            placement.rect.width() as f32 / image.width as f32,
            placement.rect.height() as f32 / image.height as f32,
        )
        .pre_translate(placement.rect.x0 as f32, placement.rect.y0 as f32);
        let paint = tiny_skia::PixmapPaint {
            blend_mode: to_ts_blend(self.blend),
            ..PixmapPaint::default()
        };
        let clip = self.clip_mask.take();
        self.with_soft_mask(|mut layer| {
            layer.draw_pixmap(0, 0, src.as_ref(), &paint, ts, clip.as_ref());
        });
        self.clip_mask = clip;
    }

    fn push_layer(&mut self, blend: crate::BlendMode, alpha: f64) {
        if self.layers.len() >= MAX_LAYER_DEPTH {
            return; // deviation: skip the over-deep group
        }
        let Some(layer) = Pixmap::new(self.width, self.height) else {
            return;
        };
        let parent = std::mem::replace(&mut self.pixmap, layer);
        self.layers.push(LayerState {
            parent,
            soft_mask: self.soft_mask.take(),
            blend: self.blend,
            group_blend: blend,
            group_alpha: alpha.clamp(0.0, 1.0),
        });
        // Inside the group, elements composite with source-over onto the
        // group backdrop; the group's blend applies at the composite-back.
        self.blend = crate::BlendMode::Normal;
    }

    fn pop_layer(&mut self) {
        let Some(state) = self.layers.pop() else {
            return; // deviation: EMC with no matching BDC/BMC
        };
        let layer = std::mem::replace(&mut self.pixmap, state.parent);
        let paint = PixmapPaint {
            blend_mode: to_ts_blend(state.group_blend),
            opacity: state.group_alpha.clamp(0.0, 1.0) as f32,
            ..PixmapPaint::default()
        };
        let mask = state.soft_mask.as_ref();
        self.pixmap
            .as_mut()
            .draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), mask);
        self.soft_mask = state.soft_mask;
        self.blend = state.blend;
    }

    fn clip(&mut self, path: &Path, rule: crate::FillRule) {
        let Some(ts_path) = to_ts_path(path) else {
            return;
        };
        let rule = to_ts_fill_rule(rule);
        // The clip mask is the intersection of every clip path; tiny-skia's
        // `intersect_path` keeps the white (inside) region of the new path.
        match self.clip_mask.as_mut() {
            Some(mask) => mask.intersect_path(&ts_path, rule, true, Transform::identity()),
            None => {
                if let Some(mut mask) = tiny_skia::Mask::new(self.width, self.height) {
                    mask.fill_path(&ts_path, rule, true, Transform::identity());
                    self.clip_mask = Some(mask);
                }
            }
        }
    }

    fn clear_clip(&mut self) {
        self.clip_mask = None;
    }

    fn set_blend(&mut self, blend: crate::BlendMode) {
        self.blend = blend;
    }

    fn set_soft_mask(&mut self, mask: Option<&Mask>) {
        self.soft_mask = mask.and_then(|m| to_ts_mask(m, self.width, self.height));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use crate::{FillRule, Paint, PathCmd, Stroke};

    fn pt(x: f64, y: f64) -> selis_geom::Point {
        selis_geom::Point::new(x, y)
    }

    #[test]
    fn backend_creates_and_fills() {
        let mut backend = TinySkiaBackend::new(32, 32).expect("pixmap");
        let path = Path {
            commands: vec![
                PathCmd::Move(pt(0.0, 0.0)),
                PathCmd::Line(pt(32.0, 0.0)),
                PathCmd::Line(pt(32.0, 32.0)),
                PathCmd::Line(pt(0.0, 32.0)),
                PathCmd::Close,
            ],
        };
        let paint = Paint {
            colour: selis_color::Rgba::new(1.0, 0.0, 0.0, 1.0),
        };
        backend.fill(&path, FillRule::NonZero, &paint);
        let data = backend.pixmap().data();
        assert!(data.iter().any(|&b| b != 0));
    }

    #[test]
    fn backend_strokes() {
        let mut backend = TinySkiaBackend::new(16, 16).expect("pixmap");
        let path = Path {
            commands: vec![PathCmd::Move(pt(2.0, 2.0)), PathCmd::Line(pt(14.0, 14.0))],
        };
        let paint = Paint {
            colour: selis_color::Rgba::new(0.0, 0.0, 1.0, 1.0),
        };
        backend.stroke(&path, &paint, &Stroke::default());
        let data = backend.pixmap().data();
        assert!(data.iter().any(|&b| b != 0));
    }

    #[test]
    fn backend_draws_image() {
        let mut backend = TinySkiaBackend::new(16, 16).expect("pixmap");
        let image = Image {
            width: 2,
            height: 2,
            rgba8: vec![
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ],
        };
        let placement = ImagePlacement {
            rect: selis_geom::Rect::new(0.0, 0.0, 16.0, 16.0),
        };
        backend.draw_image(&image, &placement);
        let data = backend.pixmap().data();
        assert!(data.iter().any(|&b| b != 0));
    }

    /// SL-2.RAST.05 DoD: an active soft mask modulates per-pixel alpha. A
    /// `[0, 255]` mask blocks the left half and passes the right half.
    #[test]
    fn soft_mask_modulates_paint_alpha() {
        let mut backend = TinySkiaBackend::new(2, 1).expect("pixmap");
        let mask = Mask {
            width: 2,
            height: 1,
            alpha8: vec![0, 255],
        };
        backend.set_soft_mask(Some(&mask));
        let path = Path {
            commands: vec![
                PathCmd::Move(pt(0.0, 0.0)),
                PathCmd::Line(pt(2.0, 0.0)),
                PathCmd::Line(pt(2.0, 1.0)),
                PathCmd::Line(pt(0.0, 1.0)),
                PathCmd::Close,
            ],
        };
        let paint = Paint {
            colour: selis_color::Rgba::new(1.0, 0.0, 0.0, 1.0),
        };
        backend.fill(&path, FillRule::NonZero, &paint);
        let data = backend.pixmap().data();
        // Left pixel (mask 0): stays transparent.
        assert_eq!(&data[0..4], &[0, 0, 0, 0]);
        // Right pixel (mask 255): opaque red.
        assert_eq!(&data[4..8], &[255, 0, 0, 255]);
    }

    /// Clearing the soft mask restores unmodulated painting.
    #[test]
    fn clearing_soft_mask_restores_normal_paint() {
        let mut backend = TinySkiaBackend::new(1, 1).expect("pixmap");
        let mask = Mask {
            width: 1,
            height: 1,
            alpha8: vec![0],
        };
        backend.set_soft_mask(Some(&mask));
        backend.set_soft_mask(None);
        let path = Path {
            commands: vec![
                PathCmd::Move(pt(0.0, 0.0)),
                PathCmd::Line(pt(1.0, 0.0)),
                PathCmd::Line(pt(1.0, 1.0)),
                PathCmd::Line(pt(0.0, 1.0)),
                PathCmd::Close,
            ],
        };
        let paint = Paint {
            colour: selis_color::Rgba::new(0.0, 0.0, 1.0, 1.0),
        };
        backend.fill(&path, FillRule::NonZero, &paint);
        let data = backend.pixmap().data();
        assert_eq!(&data[0..4], &[0, 0, 255, 255]);
    }
}
