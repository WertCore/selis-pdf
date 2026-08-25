//! The tiny-skia raster backend (SL-2.RAST.01).
//!
//! Maps the [`Backend`](crate::Backend) trait to tiny-skia canvas operations
//! (via `PixmapMut`, tiny-skia 0.11).

// tiny-skia is f32 throughout; the f64 -> f32 narrowing is inherent to the
// backend boundary and bounded by the page dimensions.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Pixmap, PixmapPaint, Stroke, StrokeDash, Transform,
};

use crate::{Backend, Image, ImagePlacement, Path, PathCmd};

/// A backend backed by a tiny-skia pixmap.
pub struct TinySkiaBackend {
    /// The underlying pixmap.
    pixmap: Pixmap,
    /// The pixmap width.
    width: u32,
    /// The pixmap height.
    height: u32,
}

impl TinySkiaBackend {
    /// Create a new raster of `width × height` pixels.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Option<Self> {
        let pixmap = Pixmap::new(width, height)?;
        Some(Self {
            pixmap,
            width,
            height,
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
fn to_ts_paint(paint: &crate::Paint) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(
        (paint.colour.rgb.r * 255.0) as u8,
        (paint.colour.rgb.g * 255.0) as u8,
        (paint.colour.rgb.b * 255.0) as u8,
        (paint.colour.a * 255.0) as u8,
    );
    p
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
        let ts_paint = to_ts_paint(paint);
        self.pixmap.as_mut().fill_path(
            &ts_path,
            &ts_paint,
            to_ts_fill_rule(rule),
            Transform::identity(),
            None,
        );
    }

    fn stroke(&mut self, path: &Path, paint: &crate::Paint, stroke: &crate::Stroke) {
        let Some(ts_path) = to_ts_path(path) else {
            return;
        };
        let ts_paint = to_ts_paint(paint);
        let ts_stroke = to_ts_stroke(stroke);
        self.pixmap.as_mut().stroke_path(
            &ts_path,
            &ts_paint,
            &ts_stroke,
            Transform::identity(),
            None,
        );
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
        let paint = PixmapPaint::default();
        self.pixmap
            .as_mut()
            .draw_pixmap(0, 0, src.as_ref(), &paint, ts, None);
    }

    fn push_layer(&mut self, _blend: crate::BlendMode, _alpha: f64) {
        // Transparency groups land with RAST.04.
    }

    fn pop_layer(&mut self) {}

    fn clip(&mut self, _path: &Path, _rule: crate::FillRule) {
        // tiny-skia 0.11 clips via the mask parameter on fill/stroke
        // (RAST.03 wires it through); a standalone clip op is recorded by the
        // RecordingBackend and enforced here once masks are threaded.
    }

    fn set_blend(&mut self, _blend: crate::BlendMode) {
        // Blend mode is per-paint in tiny-skia; RAST.06 wires it through.
    }
}

#[cfg(test)]
mod tests {
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
}
