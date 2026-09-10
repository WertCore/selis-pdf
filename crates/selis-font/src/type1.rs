//! Embedded Type 1 fonts — PFB/PFA, eexec, Type 1 charstrings (SL-3.FONT.05).
//!
//! Delegates to `read_fonts::ps::type1::Type1Font` (re-exported by skrifa as
//! `skrifa::raw`) which handles eexec decryption, charstring evaluation,
//! subrs, flex, and seac. A broken or unsupported Type 1 font is a deviation
//! — `Ok(None)` — never an error.

use selis_bytes::Bytes;
use selis_error::Result;
use selis_sandbox::BudgetGuard;
use skrifa::raw::ps::type1::Type1Font as RawType1;
use skrifa::raw::types::GlyphId;

use crate::outline::{Outline, OutlineCmd, OutlineSink};

/// A parsed Type 1 font program (PFA or PFB).
///
/// Keeps the byte data and re-parses on each glyph outline access (the `draw`
/// call is the dominant cost; `Type1Font::new` is O(font size) but acceptable
/// for a single-page render).
pub struct Type1Font {
    data: Bytes,
}

/// Parse a Type 1 font program.
///
/// # Budget
///
/// Charges the font-program bytes.
///
/// # Malformed Input
///
/// A broken font is `Ok(None)` — a deviation. `BUDGET_BYTES` when exhausted.
pub fn parse(data: &Bytes, g: &mut BudgetGuard<'_>) -> Result<Option<Type1Font>> {
    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(data.len()).unwrap_or(u64::MAX),
    )?;
    // Panic containment: read-fonts can panic on hostile fonts
    // (SL-1.ROB.06, e.g. its Type1 real-number parser overflowing).
    if !crate::contain(|| RawType1::new(data.as_slice())).is_some_and(|parsed| parsed.is_ok()) {
        return Ok(None);
    }
    Ok(Some(Type1Font { data: data.clone() }))
}

/// The number of glyphs in the Type 1 font.
#[must_use]
pub fn glyph_count(data: &Bytes) -> Option<u32> {
    let raw = crate::contain(|| RawType1::new(data.as_slice())).and_then(|r| r.ok())?;
    Some(raw.num_glyphs())
}

/// The glyph name for a glyph id.
#[must_use]
pub fn glyph_name(data: &Bytes, glyph_id: u16) -> Option<String> {
    let raw = crate::contain(|| RawType1::new(data.as_slice())).and_then(|r| r.ok())?;
    raw.glyph_name(GlyphId::from(glyph_id))
        .map(|s| s.to_string())
}

impl Type1Font {
    /// Extract a glyph outline, in font design units (typically 1000/em).
    pub fn outline_glyph(&self, glyph_id: u16, g: &mut BudgetGuard<'_>) -> Result<Option<Outline>> {
        // Panic containment: read-fonts' charstring machinery can panic on
        // hostile fonts (SL-1.ROB.06).
        let Some(commands) = crate::contain(|| -> Option<Vec<OutlineCmd>> {
            let raw = RawType1::new(self.data.as_slice()).ok()?;
            let mut sink = OutlineSink::default();
            if raw.draw(GlyphId::from(glyph_id), None, &mut sink).is_err() {
                return None; // broken glyph: deviation
            }
            Some(sink.commands)
        })
        .flatten() else {
            return Ok(None);
        };
        g.charge(
            selis_sandbox::Resource::Bytes,
            u64::try_from(commands.len()).unwrap_or(u64::MAX),
        )?;
        let bbox = crate::outline::bbox_of(&commands);
        Ok(Some(Outline { commands, bbox }))
    }

    /// The glyph name for a glyph id.
    #[must_use]
    pub fn glyph_name(&self, glyph_id: u16) -> Option<String> {
        let raw = crate::contain(|| RawType1::new(self.data.as_slice())).and_then(|r| r.ok())?;
        raw.glyph_name(GlyphId::from(glyph_id))
            .map(|s| s.to_string())
    }

    /// The number of glyphs.
    #[must_use]
    pub fn glyph_count(&self) -> u32 {
        crate::contain(|| RawType1::new(self.data.as_slice()))
            .and_then(|r| r.ok())
            .map(|r| r.num_glyphs())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    fn pfa() -> Bytes {
        Bytes::copy_from_slice(include_bytes!("../tests/fixtures/type1.pfa"))
    }

    #[test]
    fn parse_pfa() {
        let mut g = guard();
        let font = parse(&pfa(), &mut g).expect("parse").expect("type1 font");
        assert_eq!(font.glyph_count(), 3);
        assert_eq!(font.glyph_name(1), Some("A".to_string()));
        assert_eq!(font.glyph_name(2), Some("B".to_string()));
    }

    #[test]
    fn outline_a_is_a_rectangle() {
        let mut g = guard();
        let font = parse(&pfa(), &mut g).expect("parse").expect("type1 font");
        let outline = font
            .outline_glyph(1, &mut g)
            .expect("extract")
            .expect("A exists");
        assert_eq!(outline.bbox.expect("bbox"), [0.0, 0.0, 700.0, 600.0]);
    }

    #[test]
    fn outline_b_is_a_rectangle() {
        let mut g = guard();
        let font = parse(&pfa(), &mut g).expect("parse").expect("type1 font");
        let outline = font
            .outline_glyph(2, &mut g)
            .expect("extract")
            .expect("B exists");
        assert_eq!(outline.bbox.expect("bbox"), [0.0, 0.0, 900.0, 400.0]);
    }

    #[test]
    fn broken_font_is_a_deviation() {
        let mut g = guard();
        let garbage = Bytes::copy_from_slice(b"not a type1 font");
        assert!(parse(&garbage, &mut g).expect("parse").is_none());
        assert!(glyph_count(&garbage).is_none());
    }

    /// A PFB (binary) Type 1 font parses and extracts identically.
    #[test]
    fn pfb_parses() {
        let mut g = guard();
        let data = Bytes::copy_from_slice(include_bytes!("../tests/fixtures/type1.pfb"));
        let font = parse(&data, &mut g).expect("parse").expect("pfb font");
        assert_eq!(font.glyph_count(), 3);
        assert_eq!(font.glyph_name(1), Some("A".to_string()));
        let outline = font
            .outline_glyph(1, &mut g)
            .expect("extract")
            .expect("A exists");
        assert_eq!(outline.bbox.expect("bbox"), [0.0, 0.0, 700.0, 600.0]);
    }
}
