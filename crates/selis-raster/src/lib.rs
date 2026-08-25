//! The rasteriser Backend abstraction (SL-2.RAST.01, ADR-P0008).
//!
//! The deliberately narrow surface every rasteriser implements: fill path,
//! stroke path, draw image, push/pop layer, set clip, set blend. A replacement
//! backend is contained because the contract is small and explicit.
//!
//! The [`RecordingBackend`] records the call sequence for tests that assert
//! *what the rasteriser was asked to do* rather than pixel output.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_color::{BlendMode, Rgba};
use selis_geom::Rect;

#[cfg(feature = "tiny-skia")]
pub mod tiny_skia;

pub mod aa;
pub mod clip;
pub mod group;
pub mod image;
pub mod render;
pub mod shading;
pub mod soft_mask;
pub mod tile;

pub use aa::{renders_match, stable_hash, AaPolicy, Determinism, DeterminismProof};
pub use clip::ClipStack;
pub use group::{composite, group_backdrop, Backdrop, GroupParams};
pub use image::{decode_image, decode_image_scaled, draw, Decode, DecodedImage};
pub use render::{apply_ctm, fill, stroke, to_backend_stroke, StrokeSpec};
pub use shading::{
    axial_colour, barycentric, radial_colour, AxialShading, FunctionShading, GouraudShading,
    LatticeShading, RadialShading, Shading, ShadingPoint,
};
pub use soft_mask::{build_mask, Mask, MaskGroup, SoftMask, SoftMaskType};
pub use tile::{render_sequential, stitch, tile_decompose, Tile};
#[cfg(feature = "tiny-skia")]
pub use tiny_skia::TinySkiaBackend;

/// A colour for the backend: resolved RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    /// The colour.
    pub colour: Rgba,
}

/// The fill rule for a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    /// Nonzero winding.
    NonZero,
    /// Even-odd.
    EvenOdd,
}

/// A path in backend terms: a sequence of commands.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    /// The commands.
    pub commands: Vec<PathCmd>,
}

/// A path command.
#[derive(Debug, Clone, PartialEq)]
pub enum PathCmd {
    /// Move to.
    Move(selis_geom::Point),
    /// Line to.
    Line(selis_geom::Point),
    /// Cubic to.
    Cubic(selis_geom::Point, selis_geom::Point, selis_geom::Point),
    /// Close.
    Close,
}

/// An image to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// RGBA8 samples, row-major.
    pub rgba8: Vec<u8>,
}

/// A drawn image region (the destination rectangle).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImagePlacement {
    /// The destination rectangle in device space.
    pub rect: Rect,
}

/// The canvas a backend draws into.
pub trait Backend {
    /// Fill a path with a colour.
    fn fill(&mut self, path: &Path, rule: FillRule, paint: &Paint);

    /// Stroke a path with a colour and line parameters.
    fn stroke(&mut self, path: &Path, paint: &Paint, stroke: &Stroke);

    /// Draw an image into a destination rectangle.
    fn draw_image(&mut self, image: &Image, placement: &ImagePlacement);

    /// Push a transparency group (an isolated layer).
    fn push_layer(&mut self, blend: BlendMode, alpha: f64);

    /// Pop the most recent transparency group.
    fn pop_layer(&mut self);

    /// Intersect the clip with a path.
    fn clip(&mut self, path: &Path, rule: FillRule);

    /// Set the blend mode for subsequent ops.
    fn set_blend(&mut self, blend: BlendMode);

    /// Set the active soft mask for subsequent ops; `None` clears it.
    ///
    /// The mask is in device space, `width × height` alpha bytes. Every op
    /// painted while the mask is active has its alpha modulated by the mask
    /// value at each device pixel (SL-2.RAST.05).
    fn set_soft_mask(&mut self, mask: Option<&Mask>);
}

/// Stroke parameters (PDF §8.4.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    /// The line width.
    pub width: f64,
    /// The line cap (0 butt, 1 round, 2 projecting).
    pub cap: u8,
    /// The line join (0 miter, 1 round, 2 bevel).
    pub join: u8,
    /// The miter limit.
    pub miter_limit: f64,
    /// The dash pattern: `(array, phase)`.
    pub dash: (Vec<f64>, f64),
}

impl Default for Stroke {
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

/// A backend that records its call sequence instead of drawing.
///
/// Tests assert on the sequence ("fill then clip then stroke") rather than on
/// pixels, so a backend behaviour change is caught as a contract change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordingBackend {
    /// The recorded calls.
    pub calls: Vec<Call>,
}

/// One recorded backend call.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `fill`.
    Fill {
        /// The fill rule.
        rule: FillRule,
        /// The paint colour.
        colour: Rgba,
    },
    /// `stroke`.
    Stroke {
        /// The stroke parameters.
        stroke: Stroke,
        /// The paint colour.
        colour: Rgba,
    },
    /// `draw_image`.
    DrawImage {
        /// The image size.
        size: (u32, u32),
        /// The destination.
        placement: ImagePlacement,
    },
    /// `push_layer`.
    PushLayer {
        /// The blend mode.
        blend: BlendMode,
        /// The alpha.
        alpha: f64,
    },
    /// `pop_layer`.
    PopLayer,
    /// `clip`.
    Clip {
        /// The fill rule.
        rule: FillRule,
    },
    /// `set_blend`.
    SetBlend(BlendMode),
    /// `set_soft_mask`.
    SetSoftMask(Option<Mask>),
}

impl Backend for RecordingBackend {
    fn fill(&mut self, _path: &Path, rule: FillRule, paint: &Paint) {
        self.calls.push(Call::Fill {
            rule,
            colour: paint.colour,
        });
    }

    fn stroke(&mut self, _path: &Path, paint: &Paint, stroke: &Stroke) {
        self.calls.push(Call::Stroke {
            stroke: stroke.clone(),
            colour: paint.colour,
        });
    }

    fn draw_image(&mut self, image: &Image, placement: &ImagePlacement) {
        self.calls.push(Call::DrawImage {
            size: (image.width, image.height),
            placement: *placement,
        });
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f64) {
        self.calls.push(Call::PushLayer { blend, alpha });
    }

    fn pop_layer(&mut self) {
        self.calls.push(Call::PopLayer);
    }

    fn clip(&mut self, _path: &Path, rule: FillRule) {
        self.calls.push(Call::Clip { rule });
    }

    fn set_blend(&mut self, blend: BlendMode) {
        self.calls.push(Call::SetBlend(blend));
    }

    fn set_soft_mask(&mut self, mask: Option<&Mask>) {
        self.calls.push(Call::SetSoftMask(mask.cloned()));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

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
    fn recording_backend_asserts_call_order() {
        let mut backend = RecordingBackend::default();
        let path = square();
        let paint = Paint {
            colour: Rgba::new(1.0, 0.0, 0.0, 1.0),
        };
        backend.set_blend(BlendMode::Multiply);
        backend.fill(&path, FillRule::NonZero, &paint);
        backend.clip(&path, FillRule::EvenOdd);
        backend.stroke(&path, &paint, &Stroke::default());

        // The recorded sequence proves the contract: blend, fill, clip, stroke.
        assert_eq!(backend.calls.len(), 4);
        assert_eq!(backend.calls[0], Call::SetBlend(BlendMode::Multiply));
        assert_eq!(
            backend.calls[1],
            Call::Fill {
                rule: FillRule::NonZero,
                colour: Rgba::new(1.0, 0.0, 0.0, 1.0),
            }
        );
        assert_eq!(
            backend.calls[2],
            Call::Clip {
                rule: FillRule::EvenOdd
            }
        );
        assert!(matches!(backend.calls[3], Call::Stroke { .. }));
    }

    #[test]
    fn push_pop_layers_are_recorded() {
        let mut backend = RecordingBackend::default();
        backend.push_layer(BlendMode::Screen, 0.5);
        backend.pop_layer();
        assert_eq!(
            backend.calls,
            vec![
                Call::PushLayer {
                    blend: BlendMode::Screen,
                    alpha: 0.5,
                },
                Call::PopLayer,
            ]
        );
    }

    #[test]
    fn draw_image_records_size_and_placement() {
        let mut backend = RecordingBackend::default();
        let image = Image {
            width: 4,
            height: 4,
            rgba8: vec![0u8; 4 * 4 * 4],
        };
        let placement = ImagePlacement {
            rect: Rect::new(0.0, 0.0, 4.0, 4.0),
        };
        backend.draw_image(&image, &placement);
        assert_eq!(
            backend.calls[0],
            Call::DrawImage {
                size: (4, 4),
                placement,
            }
        );
    }

    #[test]
    fn set_soft_mask_is_recorded_and_cleared() {
        let mut backend = RecordingBackend::default();
        let mask = Mask {
            width: 2,
            height: 1,
            alpha8: vec![0, 255],
        };
        backend.set_soft_mask(Some(&mask));
        backend.set_soft_mask(None);
        assert_eq!(
            backend.calls,
            vec![Call::SetSoftMask(Some(mask)), Call::SetSoftMask(None)]
        );
    }
}
