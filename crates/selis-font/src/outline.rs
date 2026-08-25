//! Embedded TrueType/OpenType glyph outlines (SL-3.FONT.03).
//!
//! Extracts glyph outlines from the embedded font program via `skrifa`.
//! Composite glyphs (TrueType `glyf` components) are resolved by skrifa's
//! outline path, which is depth-bounded internally. Also exposes the `post`
//! glyph names, the link between the encoding model (SL-3.FONT.02) and glyph
//! ids.
//!
//! **Broken-font tolerance:** a missing `glyf`/`loca`, a truncated font, or an
//! out-of-range glyph id is a deviation — `Ok(None)` or an empty outline — never
//! an error and never a panic. A page must not fail over a bad font file.

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// A glyph outline command, in font units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutlineCmd {
    /// Move the current point (`f32` coordinates widened exactly).
    Move {
        /// The x coordinate in font units.
        x: f64,
        /// The y coordinate in font units.
        y: f64,
    },
    /// A straight line to the point.
    Line {
        /// The x coordinate in font units.
        x: f64,
        /// The y coordinate in font units.
        y: f64,
    },
    /// A quadratic Bézier (one control point).
    Quad {
        /// The control point.
        cx: f64,
        /// The control point.
        cy: f64,
        /// The end point.
        x: f64,
        /// The end point.
        y: f64,
    },
    /// A cubic Bézier (two control points).
    Cubic {
        /// The first control point.
        c1x: f64,
        /// The first control point.
        c1y: f64,
        /// The second control point.
        c2x: f64,
        /// The second control point.
        c2y: f64,
        /// The end point.
        x: f64,
        /// The end point.
        y: f64,
    },
    /// Close the current contour.
    Close,
}

/// A glyph's extracted outline (composites resolved), in font units.
#[derive(Debug, Clone, PartialEq)]
pub struct Outline {
    /// The outline commands.
    pub commands: Vec<OutlineCmd>,
    /// The outline bounding box `[x0 y0 x1 y1]`, computed from the commands.
    pub bbox: Option<[f64; 4]>,
}

/// Extract a glyph's outline from an embedded font program.
///
/// # Arguments
///
/// * `data` — the embedded TrueType/OpenType font bytes (`/FontFile2` or a
///   `SFNTS`-wrapped `/FontFile3`).
/// * `glyph_id` — the glyph index.
///
/// # Budget
///
/// Charges the extracted command list (document-derived).
///
/// # Malformed Input
///
/// `BUDGET_BYTES` when the budget is exhausted. A broken font or missing glyph
/// is `Ok(None)` — a deviation, never an error.
pub fn outline_glyph(
    data: &Bytes,
    glyph_id: u16,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Outline>> {
    let font = match FontRef::new(data.as_slice()) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    let outline = match font.outline_glyphs().get(GlyphId::from(glyph_id)) {
        Some(o) => o,
        None => return Ok(None),
    };
    let mut sink = OutlineSink::default();
    let settings = DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
    if outline.draw(settings, &mut sink).is_err() {
        return Ok(None); // broken outline: deviation
    }
    let commands = sink.commands;
    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(commands.len()).unwrap_or(u64::MAX),
    )?;
    let bbox = bbox_of(&commands);
    Ok(Some(Outline { commands, bbox }))
}

/// The number of glyphs in the embedded font, or `None` for a broken font.
#[must_use]
pub fn glyph_count(data: &Bytes) -> Option<u16> {
    let font = FontRef::new(data.as_slice()).ok()?;
    Some(
        font.metrics(Size::unscaled(), LocationRef::default())
            .glyph_count,
    )
}

/// The `post` glyph name for a glyph id, if the font has one.
///
/// This is the link from the encoding model (a glyph *name* from
/// `/Differences` or a predefined encoding) to a glyph *id*.
#[must_use]
pub fn glyph_name(data: &Bytes, glyph_id: u16) -> Option<String> {
    let font = FontRef::new(data.as_slice()).ok()?;
    let names = font.glyph_names();
    let name = names.get(GlyphId::from(glyph_id))?;
    Some(name.as_str().to_string())
}

/// The glyph id whose `post` name is `name`, if any (reverse of
/// [`glyph_name`]). `None` for a broken font or an unknown name.
#[must_use]
pub fn glyph_id_for_name(data: &Bytes, name: &str) -> Option<u16> {
    let font = FontRef::new(data.as_slice()).ok()?;
    for (gid, glyph_name) in font.glyph_names().iter() {
        if glyph_name.as_str() == name {
            return Some(u16::try_from(gid.to_u32()).unwrap_or(u16::MAX));
        }
    }
    None
}

/// A [`OutlinePen`] that collects [`OutlineCmd`]s.
#[derive(Default)]
struct OutlineSink {
    commands: Vec<OutlineCmd>,
}

impl OutlinePen for OutlineSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(OutlineCmd::Move {
            x: f64::from(x),
            y: f64::from(y),
        });
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(OutlineCmd::Line {
            x: f64::from(x),
            y: f64::from(y),
        });
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.commands.push(OutlineCmd::Quad {
            cx: f64::from(cx0),
            cy: f64::from(cy0),
            x: f64::from(x),
            y: f64::from(y),
        });
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.commands.push(OutlineCmd::Cubic {
            c1x: f64::from(cx0),
            c1y: f64::from(cy0),
            c2x: f64::from(cx1),
            c2y: f64::from(cy1),
            x: f64::from(x),
            y: f64::from(y),
        });
    }

    fn close(&mut self) {
        self.commands.push(OutlineCmd::Close);
    }
}

/// The `[x0 y0 x1 y1]` bounding box of the outline commands, if any point
/// exists.
#[must_use]
fn bbox_of(commands: &[OutlineCmd]) -> Option<[f64; 4]> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut any = false;
    for cmd in commands {
        match cmd {
            OutlineCmd::Move { x, y } | OutlineCmd::Line { x, y } => {
                any = true;
                min_x = min_x.min(*x);
                min_y = min_y.min(*y);
                max_x = max_x.max(*x);
                max_y = max_y.max(*y);
            }
            OutlineCmd::Quad { cx, cy, x, y } => {
                any = true;
                min_x = min_x.min(*cx).min(*x);
                min_y = min_y.min(*cy).min(*y);
                max_x = max_x.max(*cx).max(*x);
                max_y = max_y.max(*cy).max(*y);
            }
            OutlineCmd::Cubic {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                any = true;
                min_x = min_x.min(*c1x).min(*c2x).min(*x);
                min_y = min_y.min(*c1y).min(*c2y).min(*y);
                max_x = max_x.max(*c1x).max(*c2x).max(*x);
                max_y = max_y.max(*c1y).max(*c2y).max(*y);
            }
            OutlineCmd::Close => {}
        }
    }
    if any {
        Some([min_x, min_y, max_x, max_y])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    fn mini() -> Bytes {
        Bytes::copy_from_slice(include_bytes!("../tests/fixtures/mini.ttf"))
    }

    #[test]
    fn outline_a_is_a_rectangle() {
        let mut g = guard();
        let outline = outline_glyph(&mini(), 1, &mut g)
            .expect("extract")
            .expect("glyph A exists");
        // A is a 4-vertex box: Move + 3 Lines + Close.
        assert_eq!(outline.commands.len(), 5);
        assert_eq!(outline.commands[0], OutlineCmd::Move { x: 0.0, y: 0.0 });
        assert!(matches!(outline.commands[4], OutlineCmd::Close));
        // The box is 700 wide by 600 tall.
        let bbox = outline.bbox.expect("bbox");
        assert_eq!(bbox, [0.0, 0.0, 700.0, 600.0]);
    }

    #[test]
    fn outline_notdef_exists() {
        let mut g = guard();
        let outline = outline_glyph(&mini(), 0, &mut g)
            .expect("extract")
            .expect("glyph .notdef exists");
        assert!(!outline.commands.is_empty());
        assert_eq!(outline.bbox.expect("bbox")[2], 500.0); // 500 wide
    }

    #[test]
    fn out_of_range_glyph_is_a_deviation() {
        let mut g = guard();
        assert!(outline_glyph(&mini(), 99, &mut g)
            .expect("extract")
            .is_none());
    }

    #[test]
    fn broken_font_is_a_deviation() {
        let mut g = guard();
        let truncated = Bytes::copy_from_slice(&mini().as_slice()[..40]);
        assert!(outline_glyph(&truncated, 1, &mut g)
            .expect("extract")
            .is_none());
        let garbage = Bytes::copy_from_slice(b"this is not a font at all");
        assert!(outline_glyph(&garbage, 1, &mut g)
            .expect("extract")
            .is_none());
        assert!(glyph_count(&garbage).is_none());
    }

    #[test]
    fn glyph_count_and_names() {
        assert_eq!(glyph_count(&mini()), Some(3));
        assert_eq!(glyph_name(&mini(), 1).as_deref(), Some("A"));
        assert_eq!(glyph_name(&mini(), 2).as_deref(), Some("B"));
        assert_eq!(glyph_id_for_name(&mini(), "A"), Some(1));
        assert_eq!(glyph_id_for_name(&mini(), "B"), Some(2));
        assert_eq!(glyph_id_for_name(&mini(), "nope"), None);
        assert_eq!(glyph_name(&mini(), 99), None);
    }

    /// Composite glyphs are resolved into a single outline. The fixture
    /// `composite.ttf` has glyph B = two copies of the A box (components),
    /// so its outline has two contours and a 1400-wide bbox.
    #[test]
    fn composite_glyph_resolves_components() {
        let mut g = guard();
        let data = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/composite.ttf"));
        let outline = outline_glyph(&data, 2, &mut g)
            .expect("extract")
            .expect("glyph B exists");
        let bbox = outline.bbox.expect("bbox");
        assert_eq!(bbox, [0.0, 0.0, 1400.0, 600.0]);
        // Two contours: two Move commands.
        let moves = outline
            .commands
            .iter()
            .filter(|c| matches!(c, OutlineCmd::Move { .. }))
            .count();
        assert_eq!(moves, 2);
    }

    /// SL-3.FONT.04: CFF (OpenType) outlines extract correctly.
    #[test]
    fn cff_outlines_extract() {
        let mut g = guard();
        let data = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/cff.otf"));
        let a = outline_glyph(&data, 1, &mut g)
            .expect("extract")
            .expect("glyph A");
        assert_eq!(a.bbox.expect("bbox"), [0.0, 0.0, 700.0, 600.0]);
        let b = outline_glyph(&data, 2, &mut g)
            .expect("extract")
            .expect("glyph B");
        assert_eq!(b.bbox.expect("bbox"), [0.0, 0.0, 900.0, 400.0]);
    }

    /// SL-3.FONT.04: CID-keyed CFF (with FDSelect / FDArray) extracts.
    #[test]
    fn cid_cid_cff_outlines_extract() {
        let mut g = guard();
        let data = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/cff-cid.otf"));
        let a = outline_glyph(&data, 1, &mut g)
            .expect("extract")
            .expect("glyph cid00001");
        assert_eq!(a.bbox.expect("bbox"), [0.0, 0.0, 700.0, 600.0]);
        let b = outline_glyph(&data, 2, &mut g)
            .expect("extract")
            .expect("glyph cid00002");
        assert_eq!(b.bbox.expect("bbox"), [0.0, 0.0, 900.0, 400.0]);
    }

    /// SL-3.FONT.04: broken CFF font is a deviation, not an error.
    #[test]
    fn broken_cff_is_a_deviation() {
        let mut g = guard();
        let garbage = Bytes::copy_from_slice(b"not a font");
        assert!(outline_glyph(&garbage, 1, &mut g)
            .expect("extract")
            .is_none());
    }
}
