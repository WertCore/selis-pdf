//! Run, word, and line assembly (SL-3.TEXT.03).
//!
//! Groups the positioned glyphs from the content interpreter into:
//!
//! * **runs** — consecutive glyphs sharing a font and size;
//! * **words** — runs split at space characters (code 0x20);
//! * **lines** — words grouped by y-position within a tolerance.
//!
//! The output feeds reading-order inference (SL-3.TEXT.04), selection
//! (SL-3.TEXT.05), and search (SL-3.TEXT.06).

use selis_geom::{Point, Rect};

/// A positioned glyph (the content interpreter's `TextGlyph`).
pub use selis_pdf_content::text::TextGlyph;

/// A text run: positioned glyphs sharing a font and size.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// The positioned glyphs, in order.
    pub glyphs: Vec<TextGlyph>,
    /// The run's bounding box (user space).
    pub bbox: Rect,
}

/// A word: glyphs delimited by spaces.
#[derive(Debug, Clone, PartialEq)]
pub struct TextWord {
    /// The runs that make up the word.
    pub runs: Vec<TextRun>,
    /// The word's bounding box (user space).
    pub bbox: Rect,
}

/// A line: words grouped by y-position.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    /// The words, in reading order (left-to-right within the line).
    pub words: Vec<TextWord>,
    /// The line's bounding box (user space).
    pub bbox: Rect,
}

/// The tolerance for grouping glyphs into lines, as a fraction of the font
/// size (a glyph on a neighbouring line differs by at least the leading).
const LINE_TOLERANCE_FRACTION: f64 = 0.5;

/// The horizontal gap (as a fraction of the font size) that separates words.
const WORD_GAP_FRACTION: f64 = 0.5;

/// Assemble positioned glyphs into runs, then words, then lines.
#[must_use]
pub fn assemble(glyphs: Vec<TextGlyph>) -> Vec<TextLine> {
    assemble_lines(assemble_words(assemble_runs(glyphs)))
}

/// Group consecutive glyphs with the same font and size into runs.
#[must_use]
pub fn assemble_runs(glyphs: Vec<TextGlyph>) -> Vec<TextRun> {
    let mut out: Vec<TextRun> = Vec::new();
    for g in glyphs {
        let g_font = g.font.as_slice().to_vec();
        let g_size = g.size;
        let tol = g_size * LINE_TOLERANCE_FRACTION;
        let same_style = out.last().is_some_and(|r| {
            r.glyphs.first().is_some_and(|first| {
                // Same font/size AND the glyph continues the line's baseline.
                first.font.as_slice() == g_font.as_slice()
                    && first.size == g_size
                    && r.glyphs
                        .last()
                        .is_some_and(|last| (last.at.y - g.at.y).abs() <= tol)
            })
        });
        if same_style {
            if let Some(run) = out.last_mut() {
                let at = g.at;
                run.glyphs.push(g);
                run.bbox = grow_bbox(run.bbox, at);
            }
        } else {
            let at = g.at;
            out.push(TextRun {
                glyphs: vec![g],
                bbox: point_bbox(at),
            });
        }
    }
    out
}

/// Split runs at space characters (code 0x20) into words.
#[must_use]
pub fn assemble_words(runs: Vec<TextRun>) -> Vec<TextWord> {
    let mut out: Vec<TextWord> = Vec::new();
    let mut word_glyphs: Vec<TextGlyph> = Vec::new();
    for run in runs {
        // A run starting on a different baseline breaks the current word.
        let size = word_glyphs.last().map(|g| g.size).unwrap_or(12.0);
        let same_baseline = match (run.glyphs.first(), word_glyphs.last()) {
            (Some(first), Some(last)) => {
                (first.at.y - last.at.y).abs() <= size * LINE_TOLERANCE_FRACTION
            }
            _ => true,
        };
        if !same_baseline && !word_glyphs.is_empty() {
            out.push(word_from_glyphs(std::mem::take(&mut word_glyphs)));
        }
        for g in run.glyphs {
            if g.code == 0x20 {
                if !word_glyphs.is_empty() {
                    out.push(word_from_glyphs(std::mem::take(&mut word_glyphs)));
                }
            } else if let Some(last) = word_glyphs.last() {
                // Advance-gap word inference: a horizontal gap much larger
                // than half an em indicates a space even without a space
                // glyph (common in PDFs).
                let gap = (g.at.x - last.at.x).abs();
                if gap > last.size * WORD_GAP_FRACTION {
                    out.push(word_from_glyphs(std::mem::take(&mut word_glyphs)));
                    word_glyphs.push(g);
                } else {
                    word_glyphs.push(g);
                }
            } else {
                word_glyphs.push(g);
            }
        }
    }
    if !word_glyphs.is_empty() {
        out.push(word_from_glyphs(word_glyphs));
    }
    out
}

/// Group words into lines by y-position.
#[must_use]
pub fn assemble_lines(words: Vec<TextWord>) -> Vec<TextLine> {
    let mut out: Vec<TextLine> = Vec::new();
    for w in words {
        let size = w
            .runs
            .first()
            .and_then(|r| r.glyphs.first())
            .map(|g| g.size)
            .unwrap_or(12.0);
        let tol = size * LINE_TOLERANCE_FRACTION;
        let w_bbox = w.bbox;
        let same_line = out.last().is_some_and(|l| {
            let ly = (l.bbox.y0 + l.bbox.y1) / 2.0;
            let wy = (w_bbox.y0 + w_bbox.y1) / 2.0;
            (ly - wy).abs() <= tol
        });
        if same_line {
            if let Some(line) = out.last_mut() {
                line.words.push(w);
                line.bbox = grow_bbox(line.bbox, Point::new(w_bbox.x1, w_bbox.y1));
            }
        } else {
            out.push(TextLine {
                words: vec![w],
                bbox: w_bbox,
            });
        }
    }
    out
}

/// Build a word from its glyphs (grouping into runs by font/size).
fn word_from_glyphs(glyphs: Vec<TextGlyph>) -> TextWord {
    let runs = assemble_runs(glyphs);
    let mut bbox: Option<Rect> = None;
    for run in &runs {
        for g in &run.glyphs {
            bbox = Some(match bbox {
                Some(b) => grow_bbox(b, g.at),
                None => point_bbox(g.at),
            });
        }
    }
    TextWord {
        runs,
        bbox: bbox.unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0)),
    }
}

/// The bounding box of a point.
fn point_bbox(p: Point) -> Rect {
    Rect::new(p.x, p.y, p.x, p.y)
}

/// Grow a bounding box to include a point.
fn grow_bbox(b: Rect, p: Point) -> Rect {
    Rect::new(b.x0.min(p.x), b.y0.min(p.y), b.x1.max(p.x), b.y1.max(p.y))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_bytes::Bytes;

    fn glyph(code: u16, x: f64, y: f64, font: &str, size: f64) -> TextGlyph {
        TextGlyph {
            code,
            at: Point::new(x, y),
            font: Bytes::copy_from_slice(font.as_bytes()),
            size,
        }
    }

    #[test]
    fn runs_group_same_font_and_size() {
        let glyphs = vec![
            glyph(65, 0.0, 0.0, "F1", 12.0),
            glyph(66, 6.0, 0.0, "F1", 12.0),
            glyph(67, 12.0, 0.0, "F2", 14.0), // different font
            glyph(68, 18.0, 0.0, "F2", 14.0),
        ];
        let runs = assemble_runs(glyphs);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].glyphs.len(), 2);
        assert_eq!(runs[1].glyphs.len(), 2);
    }

    #[test]
    fn spaces_split_words() {
        let glyphs = vec![
            glyph(65, 0.0, 0.0, "F1", 12.0),   // A
            glyph(0x20, 6.0, 0.0, "F1", 12.0), // space
            glyph(66, 12.0, 0.0, "F1", 12.0),  // B
        ];
        let words = assemble_words(assemble_runs(glyphs));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].runs[0].glyphs.len(), 1);
        assert_eq!(words[1].runs[0].glyphs.len(), 1);
    }

    #[test]
    fn y_positions_group_lines() {
        let glyphs = vec![
            glyph(65, 0.0, 0.0, "F1", 12.0), // line 1
            glyph(66, 6.0, 0.0, "F1", 12.0),
            glyph(67, 0.0, -14.0, "F1", 12.0), // line 2
        ];
        let lines = assemble(glyphs);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].runs[0].glyphs.len(), 2);
        assert_eq!(lines[1].words[0].runs[0].glyphs.len(), 1);
    }

    /// Advance-gap word inference: a wide horizontal gap separates words even
    /// without a space glyph.
    #[test]
    fn gaps_infer_word_boundaries() {
        let glyphs = vec![
            glyph(65, 0.0, 0.0, "F1", 12.0),  // A
            glyph(66, 6.0, 0.0, "F1", 12.0),  // B (tight)
            glyph(67, 30.0, 0.0, "F1", 12.0), // C (gap > 6 = half an em)
        ];
        let words = assemble_words(assemble_runs(glyphs));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].runs[0].glyphs.len(), 2);
        assert_eq!(words[1].runs[0].glyphs.len(), 1);
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(assemble(Vec::new()).is_empty());
    }
}
