//! Type 3 fonts (SL-3.FONT.06) — ISO 32000-2:2020 §9.6.5.
//!
//! A Type 3 font's glyphs are **content streams**, not charstrings: each
//! entry of `/CharProcs` is a glyph procedure that begins with the `d0` or
//! `d1` operator (the glyph metrics) followed by ordinary painting operators
//! (`re`, `m`, `f`, ...). This module owns the COS-free model:
//!
//! * the `/FontMatrix` (glyph space → text space), `/FontBBox`, `/CharProcs`,
//!   `/Encoding`, and `/Widths`;
//! * [`glyph_metrics`] — reading the leading `d0`/`d1` from a glyph procedure
//!   for its width (and the `d1` colour-mask bounding box);
//! * the code → glyph-procedure lookup through the encoding.
//!
//! **Depth budgeting:** a Type 3 glyph procedure may reference another Type 3
//! font from its own `/Resources` (a Type 3 font used as a Type 3 glyph), so
//! rendering is recursive. The interpreter enforces the bound with the
//! worklist/depth pattern of SL-2.CONT.04 (`DEPTH_EXCEEDED`); the caller owns
//! that — this module only supplies the model and metrics.

use selis_bytes::Bytes;

use crate::encoding::Encoding;

/// A Type 3 font.
#[derive(Debug, Clone, PartialEq)]
pub struct Type3Font {
    /// The `/FontMatrix` (glyph space → text space), six numbers.
    pub font_matrix: [f64; 6],
    /// The `/FontBBox`, `[llx lly urx ury]`, in glyph space.
    pub font_bbox: [f64; 4],
    /// The `/CharProcs`: glyph name → content-stream bytes.
    pub char_procs: Vec<(String, Bytes)>,
    /// The resolved `/Encoding`.
    pub encoding: Encoding,
    /// The `/Widths`: character code → glyph-space width.
    pub widths: Vec<(u8, f64)>,
}

impl Type3Font {
    /// The glyph procedure for a character code, resolved through the
    /// encoding (`code → glyph name → /CharProcs`).
    #[must_use]
    pub fn glyph_procedure(&self, code: u8) -> Option<&Bytes> {
        let name = self.encoding.code_to_glyph(code)?;
        self.char_procs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, data)| data)
    }

    /// The `/Widths` entry for a character code.
    #[must_use]
    pub fn width(&self, code: u8) -> Option<f64> {
        self.widths
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, w)| *w)
    }
}

/// The result of reading a glyph procedure's leading `d0`/`d1` operator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphProcedureMetrics {
    /// The glyph width (`wx`) in glyph space.
    pub width: f64,
    /// The vertical width (`wy`); conventionally 0.
    pub height: f64,
    /// The `d1` colour-mask bounding box, `[llx lly urx ury]`.
    pub bbox: Option<[f64; 4]>,
}
/// Read the leading `d0`/`d1` operator from a Type 3 glyph content stream.
///
/// The procedure must begin with the metrics operator (its operands precede
/// it: `wx wy d0` or `wx wy llx lly urx ury d1`). Returns `None` when the
/// procedure has no `d0`/`d1` before its first drawing operator — the caller
/// then falls back to `/Widths` (a deviation, never a page failure).
///
/// # Budget
///
/// No document-derived allocation: the operand window is a fixed 6-slot
/// scratch that holds at most the six numbers of a `d1` operator.
///
/// # Malformed Input
///
/// Never fails; a stream that cannot be parsed yields `None`.
#[must_use]
pub fn glyph_metrics(content: &[u8]) -> Option<GlyphProcedureMetrics> {
    let mut window = [0.0f64; 6];
    let mut count = 0usize;
    let mut i = 0usize;
    while i < content.len() {
        while i < content.len()
            && content
                .get(i)
                .copied()
                .is_some_and(|b| b.is_ascii_whitespace())
        {
            i = i.saturating_add(1);
        }
        if i >= content.len() {
            break;
        }
        let c = content.get(i).copied()?;
        if c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+') {
            let start = i;
            while i < content.len() {
                let b = content.get(i).copied()?;
                if b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E') {
                    i = i.saturating_add(1);
                } else {
                    break;
                }
            }
            let text = std::str::from_utf8(content.get(start..i)?).ok()?;
            let value = text.parse().ok()?;
            if count >= 6 {
                // Drop the oldest operand (only d1 needs six).
                window.copy_within(1..6, 0);
            } else {
                count = count.saturating_add(1);
            }
            if let Some(slot) = window.get_mut(count.saturating_sub(1)) {
                *slot = value;
            }
        } else {
            // An operator name: read until whitespace or a delimiter.
            let start = i;
            while i < content.len() {
                let b = content.get(i).copied()?;
                if b.is_ascii_whitespace() || is_delimiter(b) {
                    break;
                }
                i = i.saturating_add(1);
            }
            let op = content.get(start..i)?;
            match op {
                b"d0" => {
                    let (wx, wy) = take2(&window, count)?;
                    return Some(GlyphProcedureMetrics {
                        width: wx,
                        height: wy,
                        bbox: None,
                    });
                }
                b"d1" => {
                    let (wx, wy, llx, lly, urx, ury) = take6(&window, count)?;
                    return Some(GlyphProcedureMetrics {
                        width: wx,
                        height: wy,
                        bbox: Some([llx, lly, urx, ury]),
                    });
                }
                // A drawing operator before the metrics operator: no d0/d1.
                _ => return None,
            }
        }
    }
    None
}

/// The last two operands, in order.
fn take2(window: &[f64; 6], count: usize) -> Option<(f64, f64)> {
    if count < 2 {
        return None;
    }
    let a = *window.get(count.saturating_sub(2))?;
    let b = *window.get(count.saturating_sub(1))?;
    Some((a, b))
}

/// The last six operands, in order.
fn take6(window: &[f64; 6], count: usize) -> Option<(f64, f64, f64, f64, f64, f64)> {
    if count < 6 {
        return None;
    }
    let a = *window.get(count.saturating_sub(6))?;
    let b = *window.get(count.saturating_sub(5))?;
    let c = *window.get(count.saturating_sub(4))?;
    let d = *window.get(count.saturating_sub(3))?;
    let e = *window.get(count.saturating_sub(2))?;
    let f = *window.get(count.saturating_sub(1))?;
    Some((a, b, c, d, e, f))
}
/// The PDF delimiter characters (ISO 32000-2 §7.2.2).
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::BaseEncoding;

    #[test]
    fn d0_extracts_width() {
        let m = glyph_metrics(b"12 0 d0\n").expect("d0");
        assert_eq!(m.width, 12.0);
        assert_eq!(m.height, 0.0);
        assert_eq!(m.bbox, None);
    }

    #[test]
    fn d1_extracts_width_and_bbox() {
        let m = glyph_metrics(b"12 0 0 0 100 100 d1\n").expect("d1");
        assert_eq!(m.width, 12.0);
        assert_eq!(m.bbox, Some([0.0, 0.0, 100.0, 100.0]));
    }

    #[test]
    fn negative_and_real_operands() {
        let m = glyph_metrics(b"-3.5 0 d0\n").expect("d0");
        assert_eq!(m.width, -3.5);
        let m = glyph_metrics(b"12.5 0.25 0 0 50 60 d1\n").expect("d1");
        assert_eq!(m.width, 12.5);
        assert_eq!(m.height, 0.25);
        assert_eq!(m.bbox, Some([0.0, 0.0, 50.0, 60.0]));
    }

    #[test]
    fn full_glyph_procedure() {
        // A real glyph procedure: metrics then painting.
        let content = b"12 0 d0\n0 0 100 100 re f\n";
        let m = glyph_metrics(content).expect("d0");
        assert_eq!(m.width, 12.0);
    }

    #[test]
    fn no_d0_d1_returns_none() {
        // Painting without a metrics operator: fall back to /Widths.
        assert_eq!(glyph_metrics(b"0 0 100 100 re f\n"), None);
        assert_eq!(glyph_metrics(b""), None);
        assert_eq!(glyph_metrics(b"garbage"), None);
    }

    #[test]
    fn glyph_lookup_through_encoding() {
        let font = Type3Font {
            font_matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            font_bbox: [0.0, 0.0, 100.0, 100.0],
            char_procs: vec![
                ("space".to_string(), Bytes::copy_from_slice(b"0 0 d0\n")),
                ("A".to_string(), Bytes::copy_from_slice(b"12 0 d0\n")),
            ],
            encoding: Encoding::base(BaseEncoding::WinAnsi),
            widths: vec![(65, 12.0)],
        };
        // Code 65 = 'A' via WinAnsi.
        let proc = font.glyph_procedure(65).expect("A procedure");
        assert_eq!(proc.as_slice(), b"12 0 d0\n");
        assert_eq!(font.width(65), Some(12.0));
        // Code 32 = space.
        let space = font.glyph_procedure(32).expect("space procedure");
        assert_eq!(space.as_slice(), b"0 0 d0\n");
        // A code with no encoding entry.
        assert!(font.glyph_procedure(0).is_none());
        assert!(font.width(66).is_none());
    }

    #[test]
    fn font_matrix_is_carried() {
        let font = Type3Font {
            font_matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            font_bbox: [0.0, 0.0, 100.0, 100.0],
            char_procs: Vec::new(),
            encoding: Encoding::base(BaseEncoding::WinAnsi),
            widths: Vec::new(),
        };
        assert_eq!(font.font_matrix[0], 0.001);
        assert_eq!(font.font_bbox[2], 100.0);
    }
}
