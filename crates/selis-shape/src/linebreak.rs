//! Line breaking and justification (SL-3.SHAPE.03).
//!
//! UAX #14 via `unicode-linebreak` for break opportunities. The PDF
//! justification formula lives in `selis-font` (the content interpreter's
//! replay path uses it via the font edge, never this crate — SL-3.SHAPE.01);
//! this module re-exports it for the shaping/editing path.

pub use selis_font::justified_advance;

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
