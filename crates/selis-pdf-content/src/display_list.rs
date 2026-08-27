//! Display-list IR (SL-2.CONT.07).
//!
//! The immutable intermediate representation of `01-ARCHITECTURE.md §8`:
//! an arena-allocated, serialisable op list with **resolved** (not
//! referenced) state per op. Everything downstream — raster, text, redaction,
//! edit, print, convert — consumes this, so it must be `Send`, cacheable, and
//! diffable.
//!
//! The key property is *resolved state per op*: each paint op carries the
//! exact graphics state it was painted with, so a later op can never mutate
//! an earlier one's appearance. This is what makes the IR diffable at the
//! semantic level ("op 412 changed fill colour") rather than the pixel level.

use selis_color::BlendMode;
use selis_geom::{Matrix, Point, Rect};

use crate::gstate::GState;
use crate::path::{ClipRule, Path};

/// The graphics state snapshot an op was painted with (resolved, not
/// referenced).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedState {
    /// The CTM at the op.
    pub ctm: Matrix,
    /// The line width at the op.
    pub line_width: f64,
    /// The line cap.
    pub line_cap: u8,
    /// The line join.
    pub line_join: u8,
    /// The fill colour (device RGB, 0..1). Resolved at paint time.
    pub fill: [f64; 3],
    /// The stroke colour (device RGB, 0..1). Resolved at paint time.
    pub stroke: [f64; 3],
    /// The fill alpha.
    pub alpha_fill: f64,
    /// The stroke alpha.
    pub alpha_stroke: f64,
    /// The blend mode (from `/ExtGState /BM`).
    pub blend: BlendMode,
    /// The clip paths active at the op, innermost last (from `W`/`W*`).
    pub clip: Vec<(Path, ClipRule)>,
    /// The soft-mask key (a stable reference to the `/SMask` dict), if one is
    /// active. The engine resolves it to a per-pixel mask at render time.
    pub soft_mask: Option<selis_bytes::Bytes>,
    /// The fill pattern resource name (from `scn` with a trailing name), if
    /// the fill colour space is a pattern.
    pub fill_pattern: Option<selis_bytes::Bytes>,
    /// The stroke pattern resource name.
    pub stroke_pattern: Option<selis_bytes::Bytes>,
}

impl From<&GState> for ResolvedState {
    fn from(g: &GState) -> Self {
        Self {
            ctm: g.ctm,
            line_width: g.line_width,
            line_cap: g.line_cap,
            line_join: g.line_join,
            fill: g.fill_colour,
            stroke: g.stroke_colour,
            alpha_fill: g.alpha_fill,
            alpha_stroke: g.alpha_stroke,
            blend: BlendMode::from_name(&g.blend_mode),
            clip: g.clip.clone(),
            soft_mask: g.soft_mask.clone(),
            fill_pattern: g.fill_pattern.clone(),
            stroke_pattern: g.stroke_pattern.clone(),
        }
    }
}

/// A display-list op.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Fill a path.
    Fill {
        /// The path.
        path: Path,
        /// The resolved state at paint time.
        state: ResolvedState,
    },
    /// Stroke a path.
    Stroke {
        /// The path.
        path: Path,
        /// The resolved state at paint time.
        state: ResolvedState,
    },
    /// Fill then stroke.
    FillStroke {
        /// The path.
        path: Path,
        /// The resolved state at paint time.
        state: ResolvedState,
    },
    /// Draw text (the text model lands in SL-2.TEXT.*; this is the IR slot).
    Text {
        /// The text position.
        at: Point,
        /// The resolved state at paint time.
        state: ResolvedState,
        /// The glyph runs (resolved in SL-2.TEXT).
        runs: Vec<GlyphRun>,
    },
    /// Draw an image XObject (decoded RGBA, placed in user space).
    Image {
        /// The decoded straight-RGBA samples.
        rgba8: selis_bytes::Bytes,
        /// The image width in pixels.
        width: u32,
        /// The image height in pixels.
        height: u32,
        /// The placement rectangle in user space (the unit square under the
        /// CTM).
        rect: Rect,
        /// The resolved state at paint time.
        state: ResolvedState,
    },
    /// An inline image (`BI`…`EI`), undecoded. The engine decodes it to RGBA
    /// at render time.
    InlineImage {
        /// The image dictionary (`/W`, `/H`, `/CS`, `/BPC`, `/Filter`, …).
        dict: Vec<(selis_bytes::Bytes, selis_bytes::Bytes)>,
        /// The raw (unfiltered) image data.
        data: Vec<u8>,
        /// The resolved state at paint time.
        state: ResolvedState,
    },
    /// A shading (type 1–7), named by the `/Shading` resource. The engine
    /// resolves and rasterises it to RGBA at render time.
    Shading {
        /// The shading resource name (e.g. `GS1` for `/GS1 sh`).
        name: selis_bytes::Bytes,
        /// The resolved state at paint time.
        state: ResolvedState,
    },
    /// Push a transparency group (`BDC`/`BMC`): subsequent ops paint into a
    /// layer that is composited back with `blend` and `alpha`.
    PushLayer {
        /// The group's blend mode.
        blend: BlendMode,
        /// The group's alpha.
        alpha: f64,
    },
    /// Pop the most recent transparency group (`EMC`).
    PopLayer,
}

/// A resolved glyph run (font, size, and the glyph codes).
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphRun {
    /// The font resource name.
    pub font: selis_bytes::Bytes,
    /// The font size.
    pub size: f64,
    /// The glyph codes.
    pub glyphs: Vec<u16>,
}

/// The display list: an arena of ops with resolved state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayList {
    /// The ops, in content order.
    pub ops: Vec<Op>,
}

impl DisplayList {
    /// An empty display list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of ops.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Append an op.
    pub fn push(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// Approximate heap memory used by the ops (for the DoD budget table).
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let mut total = 0usize;
        for op in &self.ops {
            total = total.saturating_add(match op {
                Op::Fill { path, .. } | Op::Stroke { path, .. } | Op::FillStroke { path, .. } => {
                    path.segments.len().saturating_mul(16)
                }
                Op::Text { runs, .. } => {
                    runs.iter().map(|r| r.glyphs.len().saturating_mul(2)).sum()
                }
                Op::Image { rgba8, .. } => rgba8.len().saturating_mul(4),
                Op::InlineImage { data, .. } => data.len().saturating_mul(4),
                Op::Shading { .. } => 16,
                Op::PushLayer { .. } | Op::PopLayer => 0,
            });
        }
        total
    }
}

/// A structural differ: reports what changed at the semantic level ("op 412
/// changed fill colour") rather than "pixels differ".
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    /// The op index that differs.
    pub op_index: usize,
    /// A human-readable description of the difference.
    pub what: String,
}

/// Compare two display lists structurally, returning a list of differences.
///
/// The resolved-state-per-op design makes this precise: an op whose fill
/// colour changed is reported as such, even if no pixel changed.
#[must_use]
pub fn diff(a: &DisplayList, b: &DisplayList) -> Vec<Diff> {
    let mut out = Vec::new();
    let common = a.len().min(b.len());
    for i in 0..common {
        if let (Some(x), Some(y)) = (a.ops.get(i), b.ops.get(i)) {
            if let Some(what) = op_diff(x, y) {
                out.push(Diff { op_index: i, what });
            }
        }
    }
    if a.len() != b.len() {
        out.push(Diff {
            op_index: common,
            what: format!("op count differs: {} vs {}", a.len(), b.len()),
        });
    }
    out
}

fn op_diff(a: &Op, b: &Op) -> Option<String> {
    match (a, b) {
        (Op::Fill { state: x, .. }, Op::Fill { state: y, .. })
        | (Op::Stroke { state: x, .. }, Op::Stroke { state: y, .. })
        | (Op::FillStroke { state: x, .. }, Op::FillStroke { state: y, .. }) => {
            if x.fill != y.fill {
                Some(format!(
                    "fill colour changed from {:?} to {:?}",
                    x.fill, y.fill
                ))
            } else if x.stroke != y.stroke {
                Some(format!(
                    "stroke colour changed from {:?} to {:?}",
                    x.stroke, y.stroke
                ))
            } else if x.line_width != y.line_width {
                Some(format!(
                    "line width changed from {} to {}",
                    x.line_width, y.line_width
                ))
            } else {
                None
            }
        }
        (Op::Text { runs: x, .. }, Op::Text { runs: y, .. }) => {
            if x != y {
                Some("text runs changed".to_string())
            } else {
                None
            }
        }
        (
            Op::Image {
                width: x,
                height: xh,
                ..
            },
            Op::Image {
                width: y,
                height: yh,
                ..
            },
        ) => {
            if x != y || xh != yh {
                Some("image dimensions changed".to_string())
            } else {
                None
            }
        }
        (Op::InlineImage { dict: x, data: xd, .. }, Op::InlineImage { dict: y, data: yd, .. }) => {
            if x != y || xd != yd {
                Some("inline image changed".to_string())
            } else {
                None
            }
        }
        (Op::Shading { name: x, .. }, Op::Shading { name: y, .. }) => {
            if x != y {
                Some("shading changed".to_string())
            } else {
                None
            }
        }
        (Op::PushLayer { blend: x, alpha: xa }, Op::PushLayer { blend: y, alpha: ya }) => {
            if x != y || xa != ya {
                Some("group blend or alpha changed".to_string())
            } else {
                None
            }
        }
        (Op::PopLayer, Op::PopLayer) => None,
        _ => Some(format!("op kind changed: {} vs {}", op_name(a), op_name(b))),
    }
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::Fill { .. } => "fill",
        Op::Stroke { .. } => "stroke",
        Op::FillStroke { .. } => "fillstroke",
        Op::Text { .. } => "text",
        Op::Image { .. } => "image",
        Op::InlineImage { .. } => "inline-image",
        Op::Shading { .. } => "shading",
        Op::PushLayer { .. } => "push-layer",
        Op::PopLayer => "pop-layer",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    fn gstate() -> GState {
        GState::new()
    }

    fn fill_op(colour: [f64; 3]) -> Op {
        let g = gstate();
        let mut st = ResolvedState::from(&g);
        st.fill = colour;
        Op::Fill {
            path: Path::new(),
            state: st,
        }
    }

    #[test]
    fn identical_lists_have_no_diff() {
        let a = DisplayList {
            ops: vec![fill_op([1.0, 0.0, 0.0])],
        };
        let b = DisplayList {
            ops: vec![fill_op([1.0, 0.0, 0.0])],
        };
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn fill_colour_change_is_reported() {
        let a = DisplayList {
            ops: vec![fill_op([1.0, 0.0, 0.0])],
        };
        let b = DisplayList {
            ops: vec![fill_op([0.0, 0.0, 1.0])],
        };
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].what.contains("fill colour"));
        assert_eq!(diffs[0].op_index, 0);
    }

    #[test]
    fn op_count_difference_is_reported() {
        let a = DisplayList {
            ops: vec![fill_op([0.0; 3])],
        };
        let b = DisplayList {
            ops: vec![fill_op([0.0; 3]), fill_op([0.0; 3])],
        };
        let diffs = diff(&a, &b);
        assert!(diffs.iter().any(|d| d.what.contains("op count")));
    }
}
