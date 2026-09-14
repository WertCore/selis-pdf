//! The text state machine (SL-3.TEXT.01) — ISO 32000-2:2020 §9.4.
//!
//! Tracks the text matrix (`Tm`), text line matrix (`Tlm`), and the
//! text-state parameters (`Tc`, `Tw`, `Tz`, `TL`, `Ts`, `Tr`, `Tf`), and
//! processes the text-showing operators (`Tj`, `TJ`, `'`, `"`) into positioned
//! glyphs. The caller (the engine) merges each [`TextGlyph`] into the graphics
//! state and emits the display-list `Op::Text`.
//!
//! The effective advance of a shown glyph is the PDF justification formula
//! (word/char spacing, horizontal scaling) from `selis-font`; word spacing
//! does NOT apply to CID codes (§9.2.7).

use selis_bytes::Bytes;
use selis_font::justified_advance;
use selis_geom::{Matrix, Point};

use crate::dispatch::Operand;

/// The text state: the text-specific subset of the graphics state.
#[derive(Debug, Clone, PartialEq)]
pub struct TextState {
    /// The text matrix (`Tm`).
    pub matrix: Matrix,
    /// The text line matrix (`Tlm`).
    pub line_matrix: Matrix,
    /// `/Tc` — character spacing (text space).
    pub char_spacing: f64,
    /// `/Tw` — word spacing (text space).
    pub word_spacing: f64,
    /// `/Tz` — horizontal scaling (a percentage; 100 = 1.0).
    pub h_scale: f64,
    /// `/TL` — text leading.
    pub leading: f64,
    /// `/Ts` — text rise.
    pub rise: f64,
    /// `/Tr` — text rendering mode.
    pub render_mode: u8,
    /// `/Tf` — the font resource name.
    pub font: Option<Bytes>,
    /// `/Tf` — the font size.
    pub font_size: f64,
    /// The marked-content id (`/MCID`) of the enclosing `BDC`/`EMC` span.
    pub mcid: Option<u32>,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 100.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
            font: None,
            font_size: 0.0,
            mcid: None,
        }
    }
}

/// A positioned glyph from a show operation.
#[derive(Debug, Clone, PartialEq)]
pub struct TextGlyph {
    /// The horizontal pen step this glyph moves in user space (the
    /// justified advance mapped through the text matrix, SL-3.TEXT.09) —
    /// the reference the extractor's word-gap inference measures the next
    /// origin against.
    pub advance: f64,
    /// The character code.
    pub code: u16,
    /// The glyph origin in user space (the text matrix applied to the rise).
    pub at: Point,
    /// The font resource name.
    pub font: Bytes,
    /// The font size.
    pub size: f64,
    /// The user-space horizontal width this font's *space* glyph occupies
    /// under the current text state (SL-3.TEXT.09): the reference distance
    /// for word-gap inference in the text layer.
    pub space: f64,
    /// The marked-content id of the enclosing `BDC`/`EMC` span, if any.
    pub mcid: Option<u32>,
}

/// Process a text operator against the text state.
///
/// Returns the positioned glyphs for a show operation (`Tj`, `TJ`, `'`, `"`),
/// and an empty vec for a state-change operator. `is_cid` says whether the
/// current font is a CID font (2-byte codes; word spacing does not apply).
/// `width_of` resolves a character code to its 1000/em advance.
pub fn process(
    state: &mut TextState,
    name: &str,
    operands: &[Operand],
    is_cid: bool,
    width_of: &dyn Fn(u16) -> f64,
) -> Vec<TextGlyph> {
    match name {
        "BT" => {
            state.matrix = Matrix::IDENTITY;
            state.line_matrix = Matrix::IDENTITY;
            Vec::new()
        }
        "ET" => Vec::new(),
        "Td" => {
            let (tx, ty) = (num(operands, 0), num(operands, 1));
            state.line_matrix = state.line_matrix.then(Matrix::translate(tx, ty));
            state.matrix = state.line_matrix;
            Vec::new()
        }
        "TD" => {
            // TD sets the leading to −ty and then moves the line by (tx, ty)
            // (PDF §9.4.3: "TD = −ty TL; tx ty Td") — the move uses the
            // positive ty. Negating it mirrored TD-positioned text below the
            // page (SL-2.RAST.13).
            let (tx, ty) = (num(operands, 0), num(operands, 1));
            state.leading = -ty;
            state.line_matrix = state.line_matrix.then(Matrix::translate(tx, ty));
            state.matrix = state.line_matrix;
            Vec::new()
        }
        "Tm" => {
            let m = Matrix::new(
                num(operands, 0),
                num(operands, 1),
                num(operands, 2),
                num(operands, 3),
                num(operands, 4),
                num(operands, 5),
            );
            state.matrix = m;
            state.line_matrix = m;
            Vec::new()
        }
        "T*" => {
            newline(state);
            Vec::new()
        }
        "Tf" => {
            state.font = Some(str(operands, 0));
            state.font_size = num(operands, 1);
            Vec::new()
        }
        "Tz" => {
            state.h_scale = num(operands, 0);
            Vec::new()
        }
        "Tc" => {
            state.char_spacing = num(operands, 0);
            Vec::new()
        }
        "Tw" => {
            state.word_spacing = num(operands, 0);
            Vec::new()
        }
        "TL" => {
            state.leading = num(operands, 0);
            Vec::new()
        }
        "Ts" => {
            state.rise = num(operands, 0);
            Vec::new()
        }
        "Tr" => {
            state.render_mode = clamp_u8(num(operands, 0));
            Vec::new()
        }
        "Tj" => show_string(state, string_bytes(operands, 0), is_cid, width_of),
        "'" => {
            newline(state);
            show_string(state, string_bytes(operands, 0), is_cid, width_of)
        }
        "\"" => {
            state.word_spacing = num(operands, 0);
            state.char_spacing = num(operands, 1);
            newline(state);
            show_string(state, string_bytes(operands, 2), is_cid, width_of)
        }
        "TJ" => show_array(state, operands, is_cid, width_of),
        _ => Vec::new(),
    }
}

/// `T*`: move to the next line.
fn newline(state: &mut TextState) {
    state.line_matrix = state
        .line_matrix
        .then(Matrix::translate(0.0, -state.leading));
    state.matrix = state.line_matrix;
}

/// Show a string: decode the codes, position each glyph, advance the matrix.
///
/// Each emitted [`TextGlyph`] carries the user-space pen step (`advance`) and
/// the font's space width under the same text state (`space`) so the text
/// layer's word-gap inference compares like with like (SL-3.TEXT.09): both are
/// in the same coordinate space as `at`, never text-space units.
fn show_string(
    state: &mut TextState,
    text: &[u8],
    is_cid: bool,
    width_of: &dyn Fn(u16) -> f64,
) -> Vec<TextGlyph> {
    let Some(font) = state.font.clone() else {
        return Vec::new(); // no font set: nothing can be shown
    };
    let size = state.font_size;
    // The user-space width of the font's space glyph at this text state
    // (constant across the string: `Td`-style advances only move the origin,
    // they never change the matrix's linear part).
    let space_units = space_width_units(is_cid, width_of);
    let space_text = justified_advance(space_units, size, 0.0, 0.0, state.h_scale, false, false);
    let space = pen_x(&state.matrix, space_text);
    let mut out = Vec::new();
    for code in codes(text, is_cid) {
        let width = width_of(code);
        let advance = justified_advance(
            width,
            size,
            state.char_spacing,
            state.word_spacing,
            state.h_scale,
            is_cid,
            code == 0x20,
        );
        let at = state.matrix.apply(Point::new(0.0, state.rise));
        let advance_x = pen_x(&state.matrix, advance);
        out.push(TextGlyph {
            code,
            at,
            advance: advance_x,
            font: font.clone(),
            size,
            space,
            mcid: state.mcid,
        });
        state.matrix = state.matrix.then(Matrix::translate(advance, 0.0));
    }
    out
}

/// The user-space x the pen moves for a text-space advance of `adv`.
fn pen_x(matrix: &Matrix, adv: f64) -> f64 {
    let p0 = matrix.apply(Point::new(0.0, 0.0));
    let p1 = matrix
        .then(Matrix::translate(adv, 0.0))
        .apply(Point::new(0.0, 0.0));
    p1.x - p0.x
}

/// The width (in 1000/em glyph space) to use for the font's *space* glyph
/// when the text layer infers word gaps (SL-3.TEXT.09).
///
/// For a simple font the encoding's code 32 is the space character by
/// convention, so `/Widths` answers directly. For a CID font it does not:
/// an `Identity-*` subset maps CID 32 to an arbitrary glyph, often with the
/// default 1000-unit `/DW` width, which is no space advance at all. A width
/// outside the plausible space range (≈ 0.1–0.6 em) is therefore replaced
/// with the 0.25 em fallback shared with the common text fonts, keeping the
/// gap threshold stable regardless of the subset's glyph numbering.
fn space_width_units(is_cid: bool, width_of: &dyn Fn(u16) -> f64) -> f64 {
    const FALLBACK: f64 = 250.0;
    let w = width_of(0x20);
    if !w.is_finite() || w <= 0.0 {
        return FALLBACK;
    }
    if is_cid && (w < 100.0 || w > 600.0) {
        return FALLBACK;
    }
    w
}

/// Show a `TJ` array: strings and per-element numeric adjustments.
fn show_array(
    state: &mut TextState,
    operands: &[Operand],
    is_cid: bool,
    width_of: &dyn Fn(u16) -> f64,
) -> Vec<TextGlyph> {
    let mut out = Vec::new();
    let Some(Operand::Arr(items)) = operands.first() else {
        return out;
    };
    for item in items {
        match item {
            Operand::Str(s) => {
                out.extend(show_string(state, s.as_slice(), is_cid, width_of));
            }
            Operand::Num(n) => {
                // The adjustment is in thousandths of a text space unit, scaled
                // by the font size and horizontal scale.
                let adj = -n * state.font_size / 1000.0 * state.h_scale / 100.0;
                state.matrix = state.matrix.then(Matrix::translate(adj, 0.0));
            }
            _ => {}
        }
    }
    out
}

/// Decode a shown string into character codes (1- or 2-byte).
fn codes(text: &[u8], is_cid: bool) -> Vec<u16> {
    if is_cid {
        let mut out = Vec::new();
        let mut i = 0usize;
        while i.saturating_add(1) < text.len() {
            let hi = text.get(i).copied().unwrap_or(0);
            let lo = text.get(i.saturating_add(1)).copied().unwrap_or(0);
            out.push(u16::from_be_bytes([hi, lo]));
            i = i.saturating_add(2);
        }
        out
    } else {
        text.iter().map(|&b| u16::from(b)).collect()
    }
}

/// A numeric operand, defaulting to 0.
fn num(operands: &[Operand], i: usize) -> f64 {
    match operands.get(i) {
        Some(Operand::Num(v)) => *v,
        _ => 0.0,
    }
}

/// A name operand as bytes, defaulting to empty.
fn str(operands: &[Operand], i: usize) -> Bytes {
    match operands.get(i) {
        Some(Operand::Name(b)) => b.clone(),
        _ => Bytes::new(),
    }
}

/// A string operand as bytes, defaulting to empty.
fn string_bytes(operands: &[Operand], i: usize) -> &[u8] {
    match operands.get(i) {
        Some(Operand::Str(b)) => b.as_slice(),
        _ => &[],
    }
}

/// Clamp a render-mode number to a byte.
fn clamp_u8(v: f64) -> u8 {
    if !v.is_finite() || v < 0.0 {
        return 0;
    }
    let t = v.trunc();
    if t >= f64::from(u8::MAX) {
        return u8::MAX;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        t as u8
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use crate::dispatch::Operand;

    fn n(v: f64) -> Operand {
        Operand::Num(v)
    }
    fn s(v: &[u8]) -> Operand {
        Operand::Str(Bytes::copy_from_slice(v))
    }

    fn const_width(_code: u16) -> f64 {
        500.0
    }

    #[test]
    fn tj_shows_glyphs_at_the_text_matrix() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        let glyphs = process(&mut state, "Tj", &[s(b"ABC")], false, &const_width);
        assert_eq!(glyphs.len(), 3);
        // Widths: 500 * 12/1000 = 6 each; A at (0,0), B at (6,0), C at (12,0).
        assert_eq!(glyphs[0].at, Point::new(0.0, 0.0));
        assert_eq!(glyphs[1].at, Point::new(6.0, 0.0));
        assert_eq!(glyphs[2].at, Point::new(12.0, 0.0));
        assert_eq!(glyphs[0].code, 65); // 'A'
        assert_eq!(glyphs[0].font.as_slice(), b"F1");
    }

    #[test]
    fn word_spacing_affects_spaces() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        state.word_spacing = 5.0;
        // "A B" with const_width 500 (advance 6 each): A at 0, space at 6
        // with advance 6+5=11, B at 6+11 = 17.
        let glyphs = process(&mut state, "Tj", &[s(b"A B")], false, &const_width);
        assert_eq!(glyphs.len(), 3);
        assert_eq!(glyphs[0].at, Point::new(0.0, 0.0));
        assert_eq!(glyphs[2].at, Point::new(17.0, 0.0));
    }

    #[test]
    fn word_spacing_does_not_apply_to_cid() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        state.word_spacing = 5.0;
        // CID: 2-byte codes; "A" is 0x0041.
        let glyphs = process(&mut state, "Tj", &[s(&[0x00, 0x41])], true, &const_width);
        assert_eq!(glyphs.len(), 1);
        assert_eq!(glyphs[0].code, 0x41);
        // No word spacing for CID: advance stays 6.
        assert_eq!(glyphs[0].at, Point::new(0.0, 0.0));
    }

    #[test]
    fn tm_sets_the_text_matrix() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        // Move the text origin to (100, 200).
        process(
            &mut state,
            "Tm",
            &[n(1.0), n(0.0), n(0.0), n(1.0), n(100.0), n(200.0)],
            false,
            &const_width,
        );
        let glyphs = process(&mut state, "Tj", &[s(b"A")], false, &const_width);
        assert_eq!(glyphs[0].at, Point::new(100.0, 200.0));
    }

    #[test]
    fn td_moves_the_line_and_text_matrix() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        process(&mut state, "Td", &[n(10.0), n(20.0)], false, &const_width);
        let glyphs = process(&mut state, "Tj", &[s(b"A")], false, &const_width);
        assert_eq!(glyphs[0].at, Point::new(10.0, 20.0));
    }

    /// `TD` moves the line by the POSITIVE ty and records the leading as −ty
    /// (PDF §9.4.3: "TD = −ty TL; tx ty Td"). A negated move mirrored
    /// TD-positioned text to the wrong side of the origin (SL-2.RAST.13).
    #[test]
    fn capital_td_uses_positive_ty() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        process(&mut state, "TD", &[n(10.0), n(20.0)], false, &const_width);
        assert_eq!(state.leading, -20.0);
        let glyphs = process(&mut state, "Tj", &[s(b"A")], false, &const_width);
        assert_eq!(glyphs[0].at, Point::new(10.0, 20.0));
    }

    #[test]
    fn apostrophe_newlines_then_shows() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        state.leading = 14.0;
        // ' moves down one line then shows.
        let glyphs = process(&mut state, "'", &[s(b"A")], false, &const_width);
        assert_eq!(glyphs[0].at, Point::new(0.0, -14.0));
    }

    #[test]
    fn quote_sets_spacing_then_shows() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        state.leading = 14.0;
        // " w c string
        let glyphs = process(
            &mut state,
            "\"",
            &[n(3.0), n(2.0), s(b"A")],
            false,
            &const_width,
        );
        assert_eq!(state.word_spacing, 3.0);
        assert_eq!(state.char_spacing, 2.0);
        // Advance = 6 + 2 = 8 (word spacing not applied: 'A' is not a space).
        assert_eq!(glyphs[0].at, Point::new(0.0, -14.0));
    }

    #[test]
    fn tj_array_adjusts_positions() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        // [ "A" -250 "B" ] — the adjustment shifts forward by 250/1000*12 = 3.
        let arr = Operand::Arr(vec![s(b"A"), n(-250.0), s(b"B")]);
        let glyphs = process(&mut state, "TJ", &[arr], false, &const_width);
        assert_eq!(glyphs.len(), 2);
        // A at 0; B at 6 + 3 = 9.
        assert_eq!(glyphs[0].at, Point::new(0.0, 0.0));
        assert_eq!(glyphs[1].at, Point::new(9.0, 0.0));
    }

    #[test]
    fn no_font_set_shows_nothing() {
        let mut state = TextState::default();
        let glyphs = process(&mut state, "Tj", &[s(b"A")], false, &const_width);
        assert!(glyphs.is_empty());
    }

    /// SL-3.TEXT.09: every shown glyph carries its user-space pen step and
    /// the font's space width, so the word-gap inference compares the origin
    /// gap against the *advance* (same coordinate space) instead of a
    /// text-space fraction of the font size.
    #[test]
    fn glyph_carries_pen_advance_and_space_width() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        let glyphs = process(&mut state, "Tj", &[s(b"AB")], false, &const_width);
        assert!((glyphs[0].advance - 6.0).abs() < 1e-9, "A pen step");
        assert!((glyphs[1].advance - 6.0).abs() < 1e-9, "B pen step");
        assert!((glyphs[0].space - 6.0).abs() < 1e-9, "500/1000 * 12");
    }

    /// The invariant word-gap inference (SL-3.TEXT.09) relies on: a glyph's
    /// recorded `advance` equals the pen step between its origin and the next
    /// glyph's origin under any text matrix — the two quantities the excess
    /// compares must always live in the same coordinate space. Note this is
    /// the interpreter's raw-step composition (`Tm.advance`, not the spec's
    /// `Translate·Tm`; see the note in SL-3.TEXT.08/2.RAST scope findings):
    /// rendering positions glyphs by the same number, so extraction and
    /// display agree whatever the Tm's linear part.
    #[test]
    fn advance_tracks_the_observed_pen_step_under_any_matrix() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 2.0;
        process(
            &mut state,
            "Tm",
            &[n(10.0), n(0.0), n(0.0), n(10.0), n(0.0), n(0.0)],
            false,
            &const_width,
        );
        let glyphs = process(&mut state, "Tj", &[s(b"AB")], false, &const_width);
        // One raw justified step (500/1000 * 2) between the origins ...
        assert!(
            (glyphs[1].at.x - glyphs[0].at.x - glyphs[0].advance).abs() < 1e-9,
            "the recorded advance must equal the observed origin delta"
        );
        // ... and the space metric rides the same composition, so a real
        // word jump would still count as exactly one space.
        assert!((glyphs[0].space - glyphs[0].advance).abs() < 1e-9);
    }

    /// A CID font's code-32 entry is not a space (an `Identity-H` subset
    /// numbers glyphs arbitrarily): the width is trusted only in the plausible
    /// 0.1–0.6 em band, otherwise the 0.25 em fallback applies (SL-3.TEXT.09).
    #[test]
    fn cid_space_width_falls_back_outside_the_plausible_band() {
        assert!((space_width_units(true, &|_code| 1000.0) - 250.0).abs() < 1e-9);
        assert!((space_width_units(true, &|_code| 300.0) - 300.0).abs() < 1e-9);
        assert!((space_width_units(false, &|_code| 0.0) - 250.0).abs() < 1e-9);
        assert!((space_width_units(false, &|_code| 278.0) - 278.0).abs() < 1e-9);
    }

    /// Word spacing (`Tw`) widens only real spaces; the recorded `space`
    /// metric is the font's own width so the gap threshold doesn't inflate
    /// with the justification settings (§9.2.7).
    #[test]
    fn space_metric_excludes_word_spacing() {
        let mut state = TextState::default();
        state.font = Some(Bytes::copy_from_slice(b"F1"));
        state.font_size = 12.0;
        state.word_spacing = 5.0;
        let glyphs = process(&mut state, "Tj", &[s(b"A B")], false, &const_width);
        // 'A' and 'B' carry the plain 500-unit step (6.0); only the space's
        // own advance included Tw.
        assert!((glyphs[0].advance - 6.0).abs() < 1e-9);
        assert!((glyphs[1].advance - 11.0).abs() < 1e-9, "space: 6 + Tw 5");
        assert!((glyphs[0].space - 6.0).abs() < 1e-9);
    }
}
