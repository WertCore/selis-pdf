//! The text layer (SL-4.UI.04's engine half).
//!
//! `selection.rs` (SL-3.TEXT.05) answers *questions* about text — where is the
//! caret, what does this range cover — and answers them from glyph quads. What
//! the viewer's text layer additionally needs is the quads **as data**, per
//! page, in reading order, with the recovered text and a per-character index
//! that lines up with that text. This module is that projection, and it is the
//! only place it is built, so the CLI, the WASM worker and a test fixture
//! cannot drift apart.
//!
//! ## Why the engine owns it, and why it is in user space
//!
//! The glyphs are the only place character geometry exists. A rasterised tile
//! has pixels and no characters; deriving a text layer from tiles would couple
//! selection to the render ladder, and the ladder deliberately presents a
//! *previous* scale's tile during a zoom (`DrawOp.provisional` in the
//! compositor's vocabulary) — selection derived from that would be wrong
//! exactly when the reader is zooming, which is the one moment it has to
//! survive. The quads are resolution-independent user-space rectangles, so one
//! fetch serves every zoom level: the viewer multiplies by the current CSS
//! scale and nothing else.
//!
//! ## The character index is the contract
//!
//! [`PageTextLayer::text`] and each line's `chars` are **index-aligned**: the
//! `i`th UTF-16 code unit of the text is described by the `i`th char entry.
//! That is what makes copy exact — a selection is a pair of integer ranges and
//! the text is a slice — and it is why `chars` is indexed by code unit rather
//! than by Unicode scalar: a JavaScript consumer's string is a UTF-16
//! sequence, and a scalar-indexed array would desynchronise from it on the
//! first astral character (an emoji, a Deseret letter). Astral scalars
//! therefore occupy two entries: the first carries the quad, and the second is
//! an uninked continuation that shares it and takes no advance.
//!
//! ## Direction is read from the geometry, not from a bidi pass
//!
//! [`TextDirection`] is derived from the sign of the step between the first two
//! inked quads of a line — a run drawn right-to-left has quads that descend in
//! x, and that descent is what selection has to mirror. This is deliberately
//! *not* a UAX #9 resolution: the plan asks that selection ranges be
//! RTL-*correct*, i.e. that a mirrored run yields mirrored ranges and a
//! mirrored caret, and the quads already say so. Paragraph base direction
//! (`selis-shape`'s first-strong) is a different question, asked by the shaper
//! and the extractor; importing it here would be both a new layer edge and the
//! wrong answer for a *visual* layer. A mixed-direction line — an Arabic word
//! inside an English sentence — is the known limit of this simplification, and
//! it is recorded as such in the viewer README rather than papered over.

use selis_geom::Rect;

use crate::assembly::TextLine;
use crate::selection::glyph_rect;

/// Which way a line's characters run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    /// Left-to-right: quads ascend in x as the character index rises.
    LeftToRight,
    /// Right-to-left: quads descend in x as the character index rises.
    RightToLeft,
}

impl TextDirection {
    /// True for [`TextDirection::RightToLeft`].
    #[must_use]
    pub fn is_rtl(self) -> bool {
        matches!(self, Self::RightToLeft)
    }
}

/// One UTF-16 code unit of a line's text, with its selection quad.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerChar {
    /// The selection quad, in PDF user space (points, y-up).
    pub rect: Rect,
    /// The pen step to the next character along the writing direction, points.
    /// Negative on a right-to-left line, zero on a continuation half.
    pub advance: f64,
    /// False for a character with no glyph of its own: a synthesised
    /// inter-word space, and the trailing half of an astral scalar.
    pub inked: bool,
}

/// One line of a page's text layer: its text, its box, and its characters.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerLine {
    /// The recovered text of the line (words joined by single spaces).
    pub text: String,
    /// The line's bounding box, user space.
    pub rect: Rect,
    /// Which way the line's characters run.
    pub direction: TextDirection,
    /// One entry per UTF-16 code unit of [`Self::text`], in order.
    pub chars: Vec<LayerChar>,
}

/// A page's text layer: the lines in **reading** order, plus the page's text.
///
/// Reading order, not visual order: the lines arrive from
/// [`order_lines`](crate::order_lines), which is structure-tree-first and
/// geometry-fallback. A two-column page's text layer is therefore column one
/// then column two, and copying it yields that — which is the whole point of
/// "copy preserves reading order, not visual order".
#[derive(Debug, Clone, PartialEq)]
pub struct PageTextLayer {
    /// Zero-based page number.
    pub page: usize,
    /// Page width in points.
    pub width: f64,
    /// Page height in points.
    pub height: f64,
    /// The lines, in reading order.
    pub lines: Vec<LayerLine>,
    /// SL-3.TEXT.10: text was drawn but nothing was recovered. The copy path
    /// surfaces the marker rather than an empty string that reads as a blank
    /// page — see [`crate::export::to_text`], which this mirrors.
    pub low_confidence: bool,
}

impl PageTextLayer {
    /// The page's text in reading order, byte-identical to what
    /// `selis extract --format=text` prints for the page (including the
    /// low-confidence marker).
    ///
    /// This is the function the copy path is specified against: copying a whole
    /// page must produce the bytes the CLI produces, and expressing it as one
    /// shared routine is what makes that checkable rather than aspirational.
    #[must_use]
    pub fn text(&self) -> String {
        let texts: Vec<String> = self.lines.iter().map(|l| l.text.clone()).collect();
        crate::export::to_text_from_line_texts(&texts, self.low_confidence)
    }
}

/// Build a page's text layer from assembled lines and their recovered texts.
///
/// `lines` and `line_texts` are index-aligned and must be the same pair the
/// extractors format from, so [`PageTextLayer::text`] reproduces
/// [`to_text`](crate::export::to_text) exactly. `width_of` resolves a character
/// code to its advance in user space and is the same callback
/// [`glyph_rect`](crate::selection::glyph_rect) takes; the caller owns font
/// resolution.
///
/// # Budget
///
/// One [`LayerChar`] (two `f64`s and a flag) per UTF-16 code unit of the page's
/// text, plus one [`LayerLine`] per line, plus the cloned line texts. The
/// caller is already holding the display list and the assembled lines, so this
/// is a constant factor on memory the page has already spent; nothing here
/// grows with the length of the document.
///
/// # Malformed Input
///
/// None. The inputs are already-parsed engine structures: a `line_texts` shorter
/// than `lines` yields no entry for the missing tail rather than a panic, and a
/// glyph code with no Unicode scalar is skipped exactly as
/// [`to_text`](crate::export::to_text) skips it, which is what keeps `chars`
/// aligned with `text`.
#[must_use]
pub fn page_layer(
    page: usize,
    size: (f64, f64),
    lines: &[TextLine],
    line_texts: &[String],
    width_of: &dyn Fn(u16) -> f64,
) -> PageTextLayer {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(text) = line_texts.get(i) else {
            continue;
        };
        // A line with no recovered text is kept, with no characters. Dropping it
        // would be tidier and would be wrong: `to_text` still emits its
        // newline, so a copy that skipped it would no longer match
        // `selis extract`, and matching it is the DoD. It contributes nothing
        // selectable, which the viewer handles by having no chars to range over.
        out.push(layer_line(line, text, width_of));
    }
    PageTextLayer {
        page,
        width: size.0,
        height: size.1,
        lines: out,
        low_confidence: false,
    }
}
/// Build one line's text layer: characters with quads, in text order.
///
/// The three things this has to get right, each of which is a way the obvious
/// implementation is wrong:
///
/// 1. **Index alignment.** `line_texts` is built by joining *words* with a
///    space, and the assembler has already stripped the space glyphs — so the
///    text has characters the glyphs do not. Each synthesised space gets the
///    quad of the gap it stands for, which is both what makes selecting across
///    a word boundary select the space and what keeps the indices in step.
/// 2. **Astral scalars.** A scalar above the BMP is two UTF-16 code units, and
///    a consumer indexing by code unit must see two entries. The second is
///    zero-width and uninked, and the *pair* shares one quad so a highlight
///    over the pair is the glyph's box rather than a doubled one.
/// 3. **Direction.** Read from the first non-zero step in x between
///    consecutive inked quads (see the module docs).
fn layer_line(line: &TextLine, text: &str, width_of: &dyn Fn(u16) -> f64) -> LayerLine {
    // Glyph quads in the line's own order, which is the order the assembler
    // emitted: the reading order within the line, which for a mirrored run is
    // right-to-left. Glyphs whose code has no Unicode scalar are dropped, which
    // is the same filter `to_text` applies and is what makes the two agree on
    // which characters exist at all.
    let inked: Vec<Rect> = crate::selection::line_glyphs(line)
        .into_iter()
        .filter(|g| char::from_u32(u32::from(g.code)).is_some())
        .map(|g| glyph_rect(g, width_of))
        .collect();

    // Direction: the first pair of consecutive inked quads that disagrees about
    // which way x runs. A one-character line, or one whose quads are all
    // vertical (a 90° `Tm` run), offers no horizontal evidence and is reported
    // left-to-right — the default, rather than a guess.
    let mut direction = TextDirection::LeftToRight;
    for pair in inked.windows(2) {
        let (Some(first), Some(second)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let dx = second.x0 - first.x0;
        if dx.abs() > f64::EPSILON {
            direction = if dx < 0.0 {
                TextDirection::RightToLeft
            } else {
                TextDirection::LeftToRight
            };
            break;
        }
    }

    // Walk the text and the inked quads together. Word boundaries in `text` are
    // where a space appears with no glyph behind it; inked characters are where
    // there is one.
    let mut chars: Vec<LayerChar> = Vec::new();
    let mut inked_at = 0usize;
    let mut previous_right: Option<f64> = None;
    for scalar in text.chars() {
        if scalar.is_whitespace() {
            // A synthesised gap. Its quad is the space between the last inked
            // character and the next one — a zero-width box at the boundary
            // when the glyphs touch, so a highlight over it is invisible rather
            // than a stray block, and never an inverted one.
            let left = previous_right.unwrap_or(line.bbox.x0);
            let right = inked.get(inked_at).map_or(left, |r| r.x0).max(left);
            chars.push(LayerChar {
                rect: Rect::new(left, line.bbox.y0, right, line.bbox.y1),
                advance: 0.0,
                inked: false,
            });
            continue;
        }
        let quad = inked.get(inked_at).copied().unwrap_or(line.bbox);
        inked_at = inked_at.saturating_add(1);
        previous_right = Some(quad.x1);
        push_scalar(&mut chars, scalar, quad, true, direction);
    }

    // The line's box is the union of what it actually covers rather than the
    // assembler's, so the box the viewer paints is the one the glyphs imply.
    let rect = chars
        .iter()
        .filter(|c| c.inked)
        .fold(None::<Rect>, |acc, c| match acc {
            None => Some(c.rect),
            Some(a) => Some(Rect::new(
                a.x0.min(c.rect.x0),
                a.y0.min(c.rect.y0),
                a.x1.max(c.rect.x1),
                a.y1.max(c.rect.y1),
            )),
        })
        .unwrap_or(line.bbox);

    LayerLine {
        text: text.to_string(),
        rect,
        direction,
        chars,
    }
}

/// Append one scalar's UTF-16 code units, sharing one quad across an astral
/// scalar's two halves.
fn push_scalar(
    chars: &mut Vec<LayerChar>,
    scalar: char,
    rect: Rect,
    inked: bool,
    direction: TextDirection,
) {
    chars.push(LayerChar {
        rect,
        advance: if direction.is_rtl() {
            -rect.width()
        } else {
            rect.width()
        },
        inked,
    });
    for _ in 1..scalar.len_utf16() {
        // The continuation half carries no ink and no advance, but it *does*
        // carry the glyph's quad. Sharing the box is what makes a consumer that
        // reads `chars[i].rect` per character — which is the whole point of the
        // index alignment — get the right answer for both halves; a zero-width
        // box here would need every consumer to learn what a continuation is
        // before it could highlight a character correctly.
        chars.push(LayerChar {
            rect,
            advance: 0.0,
            inked: false,
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use selis_bytes::Bytes;
    use selis_geom::Point;

    use super::*;
    use crate::assembly::{TextRun, TextWord};

    /// A glyph on the baseline at `y`, 10pt advance, 12pt size.
    fn glyph(code: u16, x: f64, y: f64) -> selis_pdf_content::text::TextGlyph {
        selis_pdf_content::text::TextGlyph {
            advance: 10.0,
            code,
            at: Point::new(x, y),
            tm: selis_geom::Matrix::IDENTITY,
            font: Bytes::copy_from_slice(b"F1"),
            size: 12.0,
            space: 4.0,
            dir_x: 1.0,
            dir_y: 0.0,
            mcid: None,
        }
    }

    /// One word of glyphs starting at `x`, stepping 10pt. The box runs from the
    /// baseline minus the size to the baseline plus the size, which is what
    /// `glyph_rect` produces for a 12pt glyph.
    fn word(codes: &[u16], x: f64, y: f64) -> TextWord {
        let glyphs: Vec<_> = codes
            .iter()
            .enumerate()
            .map(|(i, &code)| {
                let step = f64::from(u32::try_from(i).unwrap_or(0));
                glyph(code, x + step * 10.0, y)
            })
            .collect();
        let last = glyphs.last().map_or(x, |g| g.at.x + 10.0);
        let bbox = Rect::new(x, y - 12.0, last, y + 12.0);
        TextWord {
            runs: vec![TextRun { glyphs, bbox }],
            bbox,
        }
    }

    fn line_of(words: Vec<TextWord>) -> TextLine {
        let bbox = words.iter().skip(1).fold(words[0].bbox, |acc, w| {
            Rect::new(
                acc.x0.min(w.bbox.x0),
                acc.y0.min(w.bbox.y0),
                acc.x1.max(w.bbox.x1),
                acc.y1.max(w.bbox.y1),
            )
        });
        TextLine { words, bbox }
    }

    /// Ten points per code, the width every test glyph is built to have.
    fn const_width(_code: u16) -> f64 {
        10.0
    }

    #[test]
    fn chars_are_index_aligned_with_the_line_text() {
        let line = line_of(vec![
            word(&[72, 105], 100.0, 700.0),
            word(&[87], 200.0, 700.0),
        ]);
        let l = layer_line(&line, "Hi W", &const_width);
        assert_eq!(l.text, "Hi W");
        // Four characters, four char entries — including the space, which has
        // no glyph of its own but still occupies an index. Without that entry
        // every selection index past the first word would be off by one, and
        // the copied text would drift from the geometry that was highlighted.
        assert_eq!(l.chars.len(), 4);
        assert_eq!(l.chars[0].rect, Rect::new(100.0, 688.0, 110.0, 712.0));
        assert_eq!(l.chars[1].rect, Rect::new(110.0, 688.0, 120.0, 712.0));
        // The synthesised space sits in the gap between "Hi" and "W", so a
        // highlight across it covers the gap rather than nothing.
        assert_eq!(l.chars[2].rect, Rect::new(120.0, 688.0, 200.0, 712.0));
        assert!(!l.chars[2].inked);
        assert_eq!(l.chars[3].rect, Rect::new(200.0, 688.0, 210.0, 712.0));
    }

    #[test]
    fn a_synthesised_space_never_moves_backwards() {
        // Words that touch: the gap quad collapses to a zero-width box at the
        // boundary rather than inverting, which is what a naive
        // `next.x0 - previous.x1` produces.
        let line = line_of(vec![
            word(&[72, 105], 100.0, 700.0),
            word(&[87], 120.0, 700.0),
        ]);
        let l = layer_line(&line, "Hi W", &const_width);
        assert_eq!(l.chars[2].rect.x0, 120.0);
        assert_eq!(l.chars[2].rect.x1, 120.0);
    }

    #[test]
    fn a_descent_in_x_reads_as_right_to_left() {
        // "AB" drawn right-to-left: A at x=200, B at x=190.
        let line = line_of(vec![word(&[65], 200.0, 700.0), word(&[66], 190.0, 700.0)]);
        let l = layer_line(&line, "AB", &const_width);
        assert_eq!(l.direction, TextDirection::RightToLeft);
        assert!(l.chars[0].advance < 0.0, "an RTL pen step is negative");
        assert!(l.chars[1].advance < 0.0);
    }

    #[test]
    fn an_ascent_reads_as_left_to_right() {
        let line = line_of(vec![word(&[65, 66], 100.0, 700.0)]);
        let l = layer_line(&line, "AB", &const_width);
        assert_eq!(l.direction, TextDirection::LeftToRight);
        assert!(l.chars[0].advance > 0.0);
    }

    #[test]
    fn a_single_character_line_has_no_direction_evidence_and_defaults_ltr() {
        let line = line_of(vec![word(&[65], 100.0, 700.0)]);
        let l = layer_line(&line, "A", &const_width);
        assert_eq!(l.direction, TextDirection::LeftToRight);
    }

    #[test]
    fn a_vertical_run_reads_as_left_to_right() {
        // A 90° `Tm` run: every glyph shares an x, so there is no horizontal
        // evidence. Reporting RTL here would be a guess, and the default is the
        // honest answer.
        let mut vertical = line_of(vec![word(&[65, 66], 100.0, 700.0)]);
        for w in &mut vertical.words {
            for g in &mut w.runs[0].glyphs {
                g.at.x = 100.0;
            }
        }
        let l = layer_line(&vertical, "AB", &const_width);
        assert_eq!(l.direction, TextDirection::LeftToRight);
    }

    #[test]
    fn an_astral_scalar_occupies_two_code_units() {
        // A CID font can map a 16-bit code to a supplementary-plane scalar, so
        // the recovered text can be astral while every glyph code is `u16`. The
        // layer must then offer two entries or a UTF-16 consumer desynchronises
        // from the string on the first emoji.
        let line = line_of(vec![word(&[0x0041], 100.0, 700.0)]);
        let l = layer_line(&line, "\u{1F600}", &const_width);
        assert_eq!("\u{1F600}".encode_utf16().count(), 2);
        assert_eq!(l.chars.len(), 2);
        // The pair shares the glyph's quad: a highlight is the box, not twice it.
        assert_eq!(l.chars[0].rect, Rect::new(100.0, 688.0, 110.0, 712.0));
        assert_eq!(l.chars[0].rect, l.chars[1].rect);
        assert!(l.chars[0].inked);
        assert!(!l.chars[1].inked, "the trailing half carries no ink");
        // The continuation takes no advance, so a caret placed between the halves
        // sits at the same x as one placed at the start of the character.
        assert_eq!(l.chars[1].advance, 0.0);
        assert_eq!(l.rect, Rect::new(100.0, 688.0, 110.0, 712.0));
    }

    #[test]
    fn the_line_box_is_the_union_of_the_inked_quads() {
        let line = line_of(vec![
            word(&[72, 105], 100.0, 700.0),
            word(&[87], 300.0, 700.0),
        ]);
        let l = layer_line(&line, "Hi W", &const_width);
        assert_eq!(l.rect, Rect::new(100.0, 688.0, 310.0, 712.0));
    }

    #[test]
    fn page_text_reproduces_the_extractor_formatter_byte_for_byte() {
        // The DoD, on the engine side: the text a full-page copy produces is the
        // text `selis extract --format=text` prints. Both go through `to_text`,
        // and this asserts the layer feeds it the same strings in the same order.
        let lines = vec![
            line_of(vec![word(&[72, 105], 100.0, 700.0)]),
            line_of(vec![word(&[87, 111, 114, 108, 100], 100.0, 680.0)]),
        ];
        let texts = vec!["Hi".to_string(), "World".to_string()];
        let expected = crate::export::to_text(&lines, &texts, false);
        let layer = page_layer(0, (612.0, 792.0), &lines, &texts, &const_width);
        assert_eq!(layer.text(), expected);
        assert_eq!(layer.text(), "Hi\nWorld");
    }

    #[test]
    fn a_low_confidence_page_carries_the_marker() {
        // SL-3.TEXT.10: text drawn, nothing recovered. A copy must say so rather
        // than hand back an empty string that reads as a blank page.
        let mut layer = page_layer(0, (612.0, 792.0), &[], &[], &const_width);
        layer.low_confidence = true;
        assert_eq!(layer.text(), crate::export::LOW_CONFIDENCE_MARKER);
    }

    #[test]
    fn a_line_with_no_recovered_text_keeps_its_reading_order_slot() {
        // Dropping it would be tidier and wrong: `to_text` still emits its
        // newline, so a copy that skipped it would stop matching the CLI.
        let lines = vec![
            line_of(vec![word(&[65], 100.0, 700.0)]),
            line_of(vec![word(&[66], 100.0, 680.0)]),
        ];
        let texts = vec!["A".to_string(), String::new()];
        let layer = page_layer(0, (612.0, 792.0), &lines, &texts, &const_width);
        assert_eq!(layer.lines.len(), 2);
        assert!(layer.lines[1].chars.is_empty());
        assert_eq!(layer.text(), "A\n");
        assert_eq!(layer.text(), crate::export::to_text(&lines, &texts, false));
    }

    #[test]
    fn the_reading_order_it_receives_is_the_order_it_keeps() {
        // Two columns supplied out of visual order. The layer must not re-sort
        // them: reading order is the extractor's answer (SL-3.TEXT.04) and copy
        // has to agree with it, which is what "copy preserves reading order,
        // not visual order" means in practice.
        let lines = vec![
            line_of(vec![word(&[65], 400.0, 700.0)]),
            line_of(vec![word(&[66], 50.0, 700.0)]),
        ];
        let texts = vec!["right".to_string(), "left".to_string()];
        let layer = page_layer(0, (612.0, 792.0), &lines, &texts, &const_width);
        assert_eq!(layer.lines[0].text, "right");
        assert_eq!(layer.lines[1].text, "left");
        assert_eq!(layer.text(), "right\nleft");
    }

    #[test]
    fn a_short_line_texts_slice_does_not_panic() {
        // Hostile shape: more lines than texts. The tail contributes nothing
        // rather than crashing the page.
        let lines = vec![line_of(vec![word(&[65], 10.0, 700.0)])];
        let layer = page_layer(0, (612.0, 792.0), &lines, &[], &const_width);
        assert!(layer.lines.is_empty());
        assert_eq!(layer.text(), "");
    }

    #[test]
    fn a_glyph_with_no_unicode_scalar_is_skipped_by_both_paths() {
        // 0xD800 is a surrogate: `char::from_u32` rejects it, so `to_text` drops
        // it. The layer must drop the same one, or the two disagree about how
        // many characters exist and every index after it shifts.
        let line = line_of(vec![word(&[72, 0xD800, 105], 100.0, 700.0)]);
        let l = layer_line(&line, "Hi", &const_width);
        assert_eq!(l.chars.len(), 2);
        assert_eq!(l.chars[0].rect, Rect::new(100.0, 688.0, 110.0, 712.0));
        // The third glyph's quad is the one the dropped code occupied, so the
        // second character is highlighted where it is actually drawn rather than
        // one glyph to its left.
        assert_eq!(l.chars[1].rect, Rect::new(120.0, 688.0, 130.0, 712.0));
    }

    #[test]
    fn the_page_carries_its_own_media_size() {
        // The viewer needs the page box to place the layer; it must come from
        // the same call rather than a second metadata round-trip that could
        // disagree with it.
        let layer = page_layer(3, (595.0, 842.0), &[], &[], &const_width);
        assert_eq!(layer.page, 3);
        assert_eq!(layer.width, 595.0);
        assert_eq!(layer.height, 842.0);
    }
}
