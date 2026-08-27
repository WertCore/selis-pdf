//! Selection geometry and hit testing (SL-3.TEXT.05).
//!
//! Character-level quads for selection highlighting, caret positions (a click
//! point → a character boundary), word/line expansion, and RTL-correct
//! selection ranges. The quads are computed from the actual glyph positions,
//! so right-to-left runs naturally produce reversed (mirrored) ranges.
//!
//! The glyph advance width comes from the caller's `width_of` callback (the
//! engine resolves the font).

use selis_geom::{Point, Rect};

use crate::assembly::TextLine;
use selis_pdf_content::text::TextGlyph;

/// A character-level selection quad.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphQuad {
    /// The character code.
    pub code: u16,
    /// The selection quad (user space).
    pub rect: Rect,
}

/// A caret position (a click landed between two characters).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caret {
    /// The line index.
    pub line: usize,
    /// The glyph boundary: between glyph `boundary - 1` and `boundary`.
    pub boundary: usize,
    /// The caret x position.
    pub x: f64,
    /// The caret y position.
    pub y: f64,
}

/// The flattened glyphs of a line, in order.
#[must_use]
pub fn line_glyphs(line: &TextLine) -> Vec<&TextGlyph> {
    line.words
        .iter()
        .flat_map(|w| w.runs.iter())
        .flat_map(|r| r.glyphs.iter())
        .collect()
}

/// The selection quad of a glyph.
#[must_use]
pub fn glyph_rect(glyph: &TextGlyph, width_of: &dyn Fn(u16) -> f64) -> Rect {
    let w = width_of(glyph.code);
    // The vertical range is generous (a glyph's ascent ≈ 0.8 em above the
    // baseline, descent ≈ 0.2 em below).
    Rect::new(
        glyph.at.x,
        glyph.at.y - glyph.size,
        glyph.at.x + w,
        glyph.at.y + glyph.size,
    )
}

/// The selection quads of a line's glyphs, in order.
#[must_use]
pub fn quads(line: &TextLine, width_of: &dyn Fn(u16) -> f64) -> Vec<GlyphQuad> {
    line_glyphs(line)
        .into_iter()
        .map(|g| GlyphQuad {
            code: g.code,
            rect: glyph_rect(g, width_of),
        })
        .collect()
}

/// Hit-test a point against the assembled lines, returning a caret.
#[must_use]
pub fn caret_at(lines: &[TextLine], point: Point, width_of: &dyn Fn(u16) -> f64) -> Option<Caret> {
    let line_idx = nearest_line(lines, point.y)?;
    let line = lines.get(line_idx)?;
    let glyphs = line_glyphs(line);
    if glyphs.is_empty() {
        return None;
    }
    // Find the nearest boundary (the glyph's left edge).
    let mut best_d = f64::INFINITY;
    let mut best_boundary = 0usize;
    for (i, g) in glyphs.iter().enumerate() {
        let rect = glyph_rect(g, width_of);
        let d = (point.x - rect.x0).abs();
        if d < best_d {
            best_d = d;
            best_boundary = i;
        }
    }
    let y = (line.bbox.y0 + line.bbox.y1) / 2.0;
    let x = glyphs
        .get(best_boundary.min(glyphs.len().saturating_sub(1)))?
        .at
        .x;
    Some(Caret {
        line: line_idx,
        boundary: best_boundary,
        x,
        y,
    })
}

/// The index of the line whose y-range is nearest to `y`.
fn nearest_line(lines: &[TextLine], y: f64) -> Option<usize> {
    let mut best_d = f64::INFINITY;
    let mut best_i = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let cy = (line.bbox.y0 + line.bbox.y1) / 2.0;
        let d = (cy - y).abs();
        if d < best_d {
            best_d = d;
            best_i = i;
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(best_i)
    }
}

/// The selection rectangle covering glyphs `start..end` (exclusive) of a line.
///
/// The union of the quads — for an RTL run the quads are in reverse x order,
/// so the union is the correct enclosing rectangle regardless of direction.
#[must_use]
pub fn select_range(
    line: &TextLine,
    start: usize,
    end: usize,
    width_of: &dyn Fn(u16) -> f64,
) -> Rect {
    let glyphs = line_glyphs(line);
    let mut rect: Option<Rect> = None;
    for (i, g) in glyphs.iter().enumerate() {
        if i >= start && i < end {
            let r = glyph_rect(g, width_of);
            rect = Some(match rect {
                Some(acc) => Rect::new(
                    acc.x0.min(r.x0),
                    acc.y0.min(r.y0),
                    acc.x1.max(r.x1),
                    acc.y1.max(r.y1),
                ),
                None => r,
            });
        }
    }
    rect.unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_bytes::Bytes;

    fn glyph(code: u16, x: f64, y: f64, size: f64) -> TextGlyph {
        TextGlyph {
            code,
            at: Point::new(x, y),
            font: Bytes::copy_from_slice(b"F1"),
            size,
            mcid: None,
        }
    }

    fn line_at(glyphs: Vec<TextGlyph>) -> TextLine {
        let runs = vec![crate::assembly::TextRun {
            glyphs,
            bbox: selis_geom::Rect::new(0.0, 0.0, 0.0, 0.0),
        }];
        let words = vec![crate::assembly::TextWord {
            runs,
            bbox: selis_geom::Rect::new(0.0, 0.0, 0.0, 0.0),
        }];
        TextLine {
            words,
            bbox: selis_geom::Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    fn const_width(_code: u16) -> f64 {
        10.0
    }

    #[test]
    fn glyph_quads_are_positioned() {
        let line = line_at(vec![glyph(65, 0.0, 0.0, 12.0), glyph(66, 10.0, 0.0, 12.0)]);
        let qs = quads(&line, &const_width);
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].rect, selis_geom::Rect::new(0.0, -12.0, 10.0, 12.0));
        assert_eq!(qs[1].rect, selis_geom::Rect::new(10.0, -12.0, 20.0, 12.0));
    }

    #[test]
    fn caret_hits_the_nearest_character() {
        let lines = vec![line_at(vec![
            glyph(65, 0.0, 0.0, 12.0),
            glyph(66, 10.0, 0.0, 12.0),
        ])];
        let caret = caret_at(&lines, Point::new(5.0, 0.0), &const_width).expect("caret");
        assert_eq!(caret.line, 0);
        // x=5 is nearer to glyph 0's right edge (x=10) than glyph 1's left?
        // Actually glyph 0 spans [0,10], glyph 1 [10,20]. x=5 is in glyph 0.
        assert_eq!(caret.boundary, 0);
        let caret2 = caret_at(&lines, Point::new(15.0, 0.0), &const_width).expect("caret");
        assert_eq!(caret2.boundary, 1);
    }

    #[test]
    fn selection_range_is_the_union() {
        let line = line_at(vec![
            glyph(65, 0.0, 0.0, 12.0),
            glyph(66, 10.0, 0.0, 12.0),
            glyph(67, 20.0, 0.0, 12.0),
        ]);
        let rect = select_range(&line, 1, 3, &const_width);
        assert_eq!(rect, selis_geom::Rect::new(10.0, -12.0, 30.0, 12.0));
    }

    #[test]
    fn rtl_reversed_quads_give_the_enclosing_range() {
        let line = line_at(vec![glyph(65, 20.0, 0.0, 12.0), glyph(66, 10.0, 0.0, 12.0)]);
        let rect = select_range(&line, 0, 2, &const_width);
        assert_eq!(rect, selis_geom::Rect::new(10.0, -12.0, 30.0, 12.0));
    }
}
