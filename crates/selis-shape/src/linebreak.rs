//! Line breaking and justification (SL-3.SHAPE.03).
//!
//! UAX #14 via `unicode-linebreak` for break opportunities, and the PDF
//! justification formula: the effective advance of a glyph is the result of
//! combining character spacing, word spacing, and horizontal scaling.
//!
//! The PDF trap: word spacing applies to the space character (code 0x20) for
//! simple fonts, but **does not** apply to 2-byte CID codes (ISO 32000-2
//! §9.2.7). The caller passes `is_cid` to indicate whether the glyph is from
//! a CID font.

/// The byte offset of a line break opportunity in the text.
///
/// Produced by [`line_breaks`] in text order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BreakOpportunity {
    /// The byte offset of the break.
    pub byte: usize,
    /// Whether the break is mandatory (hard newline) or allowed.
    pub mandatory: bool,
}

/// Compute the line break opportunities per UAX #14 (via `unicode-linebreak`).
///
/// Returns a list of `(byte_offset, is_mandatory)` in text order.
#[must_use]
pub fn line_breaks(text: &str) -> Vec<BreakOpportunity> {
    unicode_linebreak::linebreaks(text)
        .map(|(byte, kind)| BreakOpportunity {
            byte,
            mandatory: kind == unicode_linebreak::BreakOpportunity::Mandatory,
        })
        .collect()
}

/// The PDF justification formula for a single glyph's advance.
///
/// The effective advance (in text space units) after applying character
/// spacing, word spacing, and horizontal scaling.
///
/// # Arguments
///
/// * `width` — the glyph's advance in 1000/em glyph space (from `/Widths` or
///   the embedded font).
/// * `font_size` — the current font size (text space → glyph space scale).
/// * `char_spacing` — `/Tc` (character spacing, in text space).
/// * `word_spacing` — `/Tw` (word spacing, in text space).
/// * `is_cid` — whether the glyph is from a CID font (word spacing does NOT
///   apply to CID codes per §9.2.7).
/// * `horizontal_scale` — `/Tz` (a percentage, e.g. 100 for 1.0).
/// * `is_space` — whether the code that produced this glyph is 0x20 (space).
///
/// The formula (ISO 32000-2 §9.2.7):
///
/// ```text
/// effective_advance = (width * font_size / 1000 + Tc + (is_space && !is_cid ? Tw : 0)) * Tz / 100
/// ```
#[must_use]
pub fn justified_advance(
    width: f64,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    is_cid: bool,
    is_space: bool,
) -> f64 {
    let scaled = width * font_size / 1000.0;
    let ws = if is_space && !is_cid {
        word_spacing
    } else {
        0.0
    };
    (scaled + char_spacing + ws) * horizontal_scale / 100.0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn line_breaks_at_newlines() {
        let breaks = line_breaks("abc\ndef\n");
        // "abc\ndef\n": \n at bytes 3 and 7; breaks after each at 4 and 8.
        assert_eq!(
            breaks,
            vec![
                BreakOpportunity {
                    byte: 4,
                    mandatory: true
                },
                BreakOpportunity {
                    byte: 8,
                    mandatory: true
                },
            ]
        );
    }

    #[test]
    fn line_breaks_in_paragraph() {
        let breaks = line_breaks("Hello world");
        // The break opportunity is after the space (byte 6), plus the
        // mandatory end-of-text break.
        assert!(breaks.iter().any(|b| b.byte == 6 && !b.mandatory));
        assert!(breaks.iter().any(|b| b.byte == 11 && b.mandatory));
    }

    #[test]
    fn justification_simple() {
        // A glyph with width 500, font_size 12, Tc=0, Tw=0, Tz=100, not
        // a space, not CID.
        let adv = justified_advance(500.0, 12.0, 0.0, 0.0, 100.0, false, false);
        // width * font_size / 1000 = 500 * 12 / 1000 = 6.0
        assert!((adv - 6.0).abs() < 1e-9);
    }

    #[test]
    fn word_spacing_applies_to_non_cid_spaces() {
        let adv = justified_advance(250.0, 12.0, 0.0, 5.0, 100.0, false, true);
        // (250 * 12 / 1000 + 5) * 100 / 100 = 3.0 + 5.0 = 8.0
        assert!((adv - 8.0).abs() < 1e-9);
    }

    #[test]
    fn word_spacing_does_not_apply_to_cid() {
        let adv = justified_advance(250.0, 12.0, 0.0, 5.0, 100.0, true, true);
        // (250 * 12 / 1000 + 0) * 100 / 100 = 3.0
        assert!((adv - 3.0).abs() < 1e-9);
    }

    #[test]
    fn char_spacing_modifies_all_glyphs() {
        let adv = justified_advance(500.0, 12.0, 2.0, 0.0, 100.0, false, false);
        // (500 * 12 / 1000 + 2) * 1.0 = 6.0 + 2.0 = 8.0
        assert!((adv - 8.0).abs() < 1e-9);
    }

    #[test]
    fn horizontal_scaling_scales_the_advance() {
        let adv = justified_advance(500.0, 12.0, 0.0, 0.0, 200.0, false, false);
        // (500 * 12 / 1000 + 0) * 2.0 = 6.0 * 2.0 = 12.0
        assert!((adv - 12.0).abs() < 1e-9);
    }

    #[test]
    fn empty_text_has_no_breaks() {
        assert!(line_breaks("").is_empty());
    }
}
