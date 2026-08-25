//! PDF text justification (SL-3.TEXT.01) — ISO 32000-2:2020 §9.2.7.
//!
//! The effective advance of a shown glyph combines character spacing, word
//! spacing, and horizontal scaling. This lives in `selis-font` (not
//! `selis-shape`) so the content interpreter — which has the `selis-font`
//! edge but must never depend on `selis-shape` (SL-3.SHAPE.01 DoD) — can use
//! it on the replay path.

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
/// The formula:
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
    use super::*;

    #[test]
    fn simple_advance() {
        let adv = justified_advance(500.0, 12.0, 0.0, 0.0, 100.0, false, false);
        assert!((adv - 6.0).abs() < 1e-9);
    }

    #[test]
    fn word_spacing_applies_to_non_cid_spaces() {
        let adv = justified_advance(250.0, 12.0, 0.0, 5.0, 100.0, false, true);
        assert!((adv - 8.0).abs() < 1e-9);
    }

    #[test]
    fn word_spacing_does_not_apply_to_cid() {
        let adv = justified_advance(250.0, 12.0, 0.0, 5.0, 100.0, true, true);
        assert!((adv - 3.0).abs() < 1e-9);
    }

    #[test]
    fn char_spacing_and_scale() {
        let adv = justified_advance(500.0, 12.0, 2.0, 0.0, 200.0, false, false);
        // (6.0 + 2.0) * 2.0 = 16.0
        assert!((adv - 16.0).abs() < 1e-9);
    }
}
