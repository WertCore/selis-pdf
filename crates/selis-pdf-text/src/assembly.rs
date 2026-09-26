//! Run, word, and line assembly (SL-3.TEXT.03).
//!
//! Groups the positioned glyphs from the content interpreter into:
//!
//! * **runs** — consecutive glyphs sharing a font and size;
//! * **words** — runs split at space characters (code 0x20) and at advance
//!   gaps exceeding half of the font's own space width (SL-3.TEXT.09);
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

/// How much of a *space width* the excess advance gap over the previous
/// glyph's own pen step must reach before the gap counts as a word break.
/// Half a space forgives wide side-bearing, italic kerning, and the ±0.1 em
/// `TJ` adjustments inside a word while catching the full-space jumps
/// generated without space characters. (SL-3.TEXT.09 replaced an
/// `0.5 × font-size` text-space threshold that split nearly every glyph,
/// because a normal glyph advance *is* about half an em.)
const WORD_GAP_SPACE_FRACTION: f64 = 0.5;

/// Assemble positioned glyphs into runs, then words, then lines.
#[must_use]
pub fn assemble(glyphs: Vec<TextGlyph>) -> Vec<TextLine> {
    assemble_lines(assemble_words(assemble_runs(glyphs)))
}

/// The positioned glyphs a display list shows, in content order — one per
/// shown code, carrying the interpreter's pen step and space width
/// (SL-3.TEXT.09). Shared by every extractor surface (CLI, WASM worker) so
/// run/word/line assembly always sees the same glyph stream.
#[must_use]
pub fn gather_glyphs(dl: &selis_pdf_content::display_list::DisplayList) -> Vec<TextGlyph> {
    use selis_pdf_content::display_list::Op;
    let mut out = Vec::new();
    for op in &dl.ops {
        if let Op::Text {
            at,
            tm,
            state,
            runs,
        } = op
        {
            let mcid = state.mcid;
            for run in runs {
                for &code in &run.glyphs {
                    out.push(TextGlyph {
                        code,
                        at: *at,
                        tm: *tm,
                        advance: run.advance,
                        font: run.font.clone(),
                        size: run.size,
                        space: run.space,
                        mcid,
                    });
                }
            }
        }
    }
    out
}

/// Replace glyph codes with their recovered Unicode scalars in place, so
/// downstream readers (word assembly included) see characters, not codes
/// (SL-3.TEXT.08 / SL-3.SHAPE.04).
///
/// `resolve(font_name, code)` is the caller's document-backed lookup (the
/// engine's `Session::text_unicode`: `/ToUnicode` then the encoding/glyph-name
/// recovery chain); resolution is cached per (font, code). A recovered scalar
/// above `u16::MAX` cannot be carried in the glyph code and keeps the raw
/// code (documented limitation).
///
/// # Malformed Input
///
/// A `None` resolution keeps the legacy byte-as-character reading — recovery
/// failures never drop text, and never guess.
pub fn apply_unicode_recovery(
    glyphs: &mut [TextGlyph],
    resolve: &mut dyn FnMut(&selis_bytes::Bytes, u16) -> Option<u32>,
) {
    let mut cache: std::collections::HashMap<(Vec<u8>, u16), Option<u32>> =
        std::collections::HashMap::new();
    for glyph in glyphs.iter_mut() {
        let key = (glyph.font.as_slice().to_vec(), glyph.code);
        let cached = cache
            .entry(key)
            .or_insert_with(|| resolve(&glyph.font, glyph.code));
        if let Some(uni) = *cached {
            if let Ok(narrow) = u16::try_from(uni) {
                glyph.code = narrow;
            }
        }
    }
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
                // Advance-gap word inference (SL-3.TEXT.09): the next glyph's
                // origin sits one pen step away when the word continues. A gap
                // of more than half an *extra* space beyond that step — in the
                // same (user-space) units as both positions — indicates a space
                // even without a space glyph, common in generated PDFs whose
                // content stream has no space characters at all.
                let gap = g.at.x - last.at.x;
                let over_space = gap - last.advance;
                if last.space > 0.0 && over_space > last.space * WORD_GAP_SPACE_FRACTION {
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

    /// A glyph with a typical proportionally-spaced pen model: advance half an
    /// em, space a quarter em (matches what the content interpreter records for
    /// the standard-14 fonts), so gap inference behaves as it does downstream.
    fn glyph(code: u16, x: f64, y: f64, font: &str, size: f64) -> TextGlyph {
        TextGlyph {
            code,
            at: Point::new(x, y),
            tm: selis_geom::Matrix::IDENTITY,
            advance: size * 0.5,
            font: Bytes::copy_from_slice(font.as_bytes()),
            size,
            space: size * 0.25,
            mcid: None,
        }
    }

    /// A glyph with an explicit pen advance and space width.
    fn glyph_pen(code: u16, x: f64, y: f64, size: f64, advance: f64, space: f64) -> TextGlyph {
        TextGlyph {
            code,
            at: Point::new(x, y),
            tm: selis_geom::Matrix::IDENTITY,
            advance,
            font: Bytes::copy_from_slice(b"F1"),
            size,
            space,
            mcid: None,
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

    /// The SL-3.TEXT.03 DoD case, tracked to its root cause by SL-3.TEXT.09:
    /// a generated content stream with **no space characters at all** still
    /// yields two words — the a→b→c→d run has no `0x20` code anywhere, the
    /// only word signal is the advance-gap jump before `c`.
    #[test]
    fn no_space_characters_in_content_stream_yet_two_words() {
        let glyphs = vec![
            glyph_pen(97, 0.0, 0.0, 12.0, 5.0, 3.0),   // a
            glyph_pen(98, 5.0, 0.0, 12.0, 5.0, 3.0),   // b
            glyph_pen(99, 16.0, 0.0, 12.0, 5.0, 3.0),  // c: 6 past b's step
            glyph_pen(100, 21.0, 0.0, 12.0, 5.0, 3.0), // d (tight)
        ];
        let words = assemble_words(assemble_runs(glyphs));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].runs[0].glyphs.len(), 2);
        assert_eq!(words[1].runs[0].glyphs.len(), 2);
    }

    /// The regression the SL-3.CONF.01 sweep made headline: glyph advances of
    /// about half an em must NOT each trip a word break. Before the fix the
    /// threshold was `0.5 × font size` against the origin gap — a normal 'e'
    /// advance meets it exactly at 12pt and exceeds it at larger sizes, and
    /// nearly every glyph extracted as its own word.
    #[test]
    fn half_em_glyph_advances_never_split() {
        let size = 24.0;
        // Helvetica widths (per 1000 em) for "Selisoracle" — every consecutive
        // origin gap is the previous glyph's advance; none is a space jump.
        let widths = [
            667.0, 556.0, 222.0, 556.0, 222.0, 556.0, 556.0, 333.0, 556.0, 500.0, 222.0,
        ];
        let mut x = 0.0;
        let mut glyphs = Vec::new();
        for (i, w) in widths.iter().enumerate() {
            let code = [83, 101, 108, 105, 115, 111, 114, 97, 99, 108, 101][i];
            let advance = *w / 1000.0 * size;
            glyphs.push(glyph_pen(
                code,
                x,
                0.0,
                size,
                advance,
                278.0 / 1000.0 * size,
            ));
            x += advance;
        }
        let words = assemble_words(assemble_runs(glyphs));
        // All advances except the very last remain *inside* the word: no gap
        // exceeds its own step by even a tenth of a space.
        assert_eq!(words.len(), 1);
    }

    /// Metrics-free glyphs (no advance/space recorded — a display list built
    /// outside the interpreter) merge rather than fragment: a zero space width
    /// disables gap inference, and never splits per glyph.
    #[test]
    fn unknown_space_width_merges_not_splits() {
        let glyphs = vec![
            glyph_pen(65, 0.0, 0.0, 12.0, 0.0, 0.0),
            glyph_pen(66, 9.0, 0.0, 12.0, 0.0, 0.0),
            glyph_pen(67, 60.0, 0.0, 12.0, 0.0, 0.0),
        ];
        let words = assemble_words(assemble_runs(glyphs));
        assert_eq!(words.len(), 1, "a space width of 0 must not invent breaks");
    }
}

/// GAP-INVARIANT PROPERTIES (SL-3.TEXT.09 DoD): word-gap inference is a
/// function of the pen step vs space width — never of font size alone, and
/// never of coordinate drift.
#[cfg(test)]
mod gap_properties {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )]

    use super::{assemble_runs, assemble_words};
    use proptest::prelude::*;
    use selis_bytes::Bytes;
    use selis_geom::Point;
    use selis_pdf_content::text::TextGlyph;

    fn glyph(code: u16, x: f64, advance: f64, space: f64) -> TextGlyph {
        TextGlyph {
            code,
            at: Point::new(x, 0.0),
            tm: selis_geom::Matrix::IDENTITY,
            advance,
            font: Bytes::copy_from_slice(b"F1"),
            size: 24.0,
            space,
            mcid: None,
        }
    }

    /// `advance/step` in (0, 50) and word length 1..40.
    /// Uniform tight layout: every gap equals the previous advance → exactly
    /// one word, whatever the (mutually independent) size, advance, and space
    /// width. The half-em split bug (SL-3.TEXT.09) violated this whenever the
    /// advance reached ~0.5 of the font *size* — an accident of the metrics,
    /// since a 0x20-free tight stream must never split.
    ///
    /// A jump of `jump_factor` extra space widths at position `jump_at` must
    /// split (factor ≥ 0.51) and must split exactly once.
    #[derive(Debug, Clone)]
    struct Layout {
        n: usize,
        advance: f64,
        space: f64,
        jump_at: Option<usize>,
        jump_factor: f64,
    }

    fn arbitrary_layout() -> impl Strategy<Value = Layout> {
        (1usize..40, 1.0f64..50.0, 0.5f64..30.0).prop_map(|(n, advance, space)| Layout {
            n,
            advance,
            space,
            jump_at: None,
            jump_factor: 0.0,
        })
    }

    fn build(l: &Layout) -> Vec<TextGlyph> {
        let mut out = Vec::with_capacity(l.n);
        let mut x = 0.0_f64;
        for i in 0..l.n {
            out.push(glyph(65 + (i % 26) as u16, x, l.advance, l.space));
            let mut step = l.advance;
            if l.jump_at == Some(i) {
                step += l.space * l.jump_factor;
            }
            x += step;
        }
        out
    }

    proptest! {
        #[test]
        fn uniform_steps_never_split(l in arbitrary_layout()) {
            let words = assemble_words(assemble_runs(build(&l)));
            prop_assert_eq!(words.len(), 1);
        }

        #[test]
        fn jump_beyond_half_space_always_splits(
            l in arbitrary_layout(),
            idx_factor in 0usize..40,
            jump in 0.51f64..12.0,
        ) {
            let mut l = l;
            prop_assume!(l.n > 1);
            // Place the jump at an interior position whose successor exists.
            l.jump_at = Some(idx_factor % (l.n - 1));
            l.jump_factor = jump;
            let words = assemble_words(assemble_runs(build(&l)));
            prop_assert_eq!(words.len(), 2);
        }
    }
}
