//! Width resolution (SL-3.FONT.01).
//!
//! The DoD is that every code resolves to a real width, because width errors
//! accumulate into visibly wrong line lengths. The precedence:
//!
//! 1. the `/Widths` array when it covers the code (and is correctly sized);
//! 2. the embedded font's own metrics (a TrueType `hmtx` advance via `skrifa`,
//!    scaled from font units to PDF glyph space — 1000 units per em) — the
//!    missing-`/Widths` fallback;
//! 3. `/MissingWidth`.
//!
//! A broken embedded font is a deviation, never a page failure: it falls
//! through to `/MissingWidth`.

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider};

use crate::model::{FontDict, FontFile};

/// A resolved font: a width for every code.
#[derive(Debug, Clone)]
pub struct ResolvedFont {
    /// The `/Widths` array, when present and correctly sized.
    widths: Option<WidthsArray>,
    /// The embedded font's own metrics (the missing-`/Widths` fallback).
    embedded: Option<EmbeddedMetrics>,
    /// The `/MissingWidth` (0 by default).
    missing_width: f64,
}

#[derive(Debug, Clone)]
struct WidthsArray {
    first: u32,
    values: Vec<f64>,
}

impl WidthsArray {
    /// The width for `code`, or `None` outside the array.
    fn width(&self, code: u32) -> Option<f64> {
        if code < self.first {
            return None;
        }
        let idx = usize::try_from(code.saturating_sub(self.first)).unwrap_or(0);
        self.values.get(idx).copied()
    }
}

/// The embedded font's own metrics, by flavour. Type1/CFF metrics land with
/// SL-3.FONT.04/05; TrueType is available now.
#[derive(Debug, Clone)]
enum EmbeddedMetrics {
    TrueType(TtMetrics),
}

impl EmbeddedMetrics {
    fn width(&self, code: u32) -> Option<f64> {
        match self {
            EmbeddedMetrics::TrueType(m) => m.width(code),
        }
    }
}

/// The metrics extracted from a TrueType/OpenType font's `head` and `hmtx`
/// tables: the binary font data and the `unitsPerEm` (for scaling width
/// values to PDF glyph space).
#[derive(Debug, Clone)]
pub struct TtMetrics {
    /// The raw font data.
    pub data: Bytes,
    /// The font's `unitsPerEm` (from the `head` table).
    pub units_per_em: u16,
}

impl TtMetrics {
    /// The advance width for `code` in PDF glyph space (1000 units per em).
    fn width(&self, code: u32) -> Option<f64> {
        let font = FontRef::new(self.data.as_slice()).ok()?;
        let ch = char::from_u32(code)?;
        let glyph = font.charmap().map(ch)?;
        let advance = font
            .glyph_metrics(Size::unscaled(), LocationRef::default())
            .advance_width(glyph)?;
        // The advance is in font units; PDF glyph space is 1000 units per em.
        let em = f64::from(self.units_per_em.max(1));
        Some(f64::from(advance) * 1000.0 / em)
    }
}

/// Resolve a font dictionary into per-code widths.
///
/// A composite (Type0) font resolves through its first descendant CIDFont.
/// Type1/CFF embedded metrics land in SL-3.FONT.04/05; until then they fall
/// through to `/MissingWidth`.
///
/// # Budget
///
/// Charges the embedded font-program bytes when parsing metrics.
///
/// # Malformed Input
///
/// `BUDGET_BYTES` when the budget is exhausted. A broken embedded font is a
/// deviation (returns a font that falls through to `/MissingWidth`), never an
/// error — a page must not fail over a bad font file.
pub fn resolve(dict: &FontDict, g: &mut BudgetGuard<'_>) -> Result<ResolvedFont> {
    // Only the Type0 wrapper delegates: its metrics live in the descendant
    // CIDFont. CIDFontType0/2 are composite but carry their own metrics.
    let target = if dict.subtype == crate::model::FontSubtype::Type0 {
        match &dict.descendant {
            Some(desc) => desc.as_ref(),
            None => {
                // No descendant to resolve against: a font with no metrics.
                return Ok(ResolvedFont {
                    widths: None,
                    embedded: None,
                    missing_width: dict.missing_width(),
                });
            }
        }
    } else {
        dict
    };

    let widths = target
        .widths_sized()
        .map(|values| WidthsArray {
            first: target.first_char,
            values: values.to_vec(),
        })
        .or_else(|| standard14_widths(target));
    let embedded = match &target.font_file {
        Some(FontFile::TrueType(data)) => {
            parse_ttf_metrics(data, g)?.map(EmbeddedMetrics::TrueType)
        }
        Some(_) => None, // Type1 (PFB) and CFF metrics land in FONT.04/05.
        None => None,
    };

    Ok(ResolvedFont {
        widths,
        embedded,
        missing_width: target.missing_width(),
    })
}

/// Parse embedded TrueType metrics under a budget.
///
/// Returns `Ok(None)` for a broken font (deviation, fall back to
/// `/MissingWidth`), `Err` only on budget exhaustion.
///
/// # Budget
///
/// Charges the font-program bytes.
///
/// # Malformed Input
///
/// `BUDGET_BYTES` when the budget is exhausted.
pub fn parse_ttf_metrics(data: &Bytes, g: &mut BudgetGuard<'_>) -> Result<Option<TtMetrics>> {
    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(data.len()).unwrap_or(u64::MAX),
    )?;
    let font = match FontRef::new(data.as_slice()) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    let units_per_em = font
        .metrics(Size::unscaled(), LocationRef::default())
        .units_per_em;
    if units_per_em == 0 {
        return Ok(None); // a font without units-per-em cannot provide widths
    }
    Ok(Some(TtMetrics {
        data: data.clone(),
        units_per_em,
    }))
}

/// The `/MissingWidth` fallback of a font dict.
#[must_use]
fn standard14_widths(dict: &FontDict) -> Option<WidthsArray> {
    // Standard-14 Type1 fonts carry no /Widths in the dict; use the AFM
    // widths keyed by the StandardEncoding glyph name (SL-3.FONT.03).
    if dict.subtype != crate::model::FontSubtype::Type1
        || !crate::standard14::is_standard(&dict.base_font)
    {
        return None;
    }
    // Standard-14 Type1 fonts carry no /Widths in the dict; use the AFM
    // widths keyed by the StandardEncoding glyph name (SL-3.FONT.03). The
    // range covers 0–255 regardless of /FirstChar//LastChar (usually absent).
    // Glyphs absent from the AFM table (e.g. ".notdef") get a 0 width.
    let mut values = Vec::new();
    for code in 0u32..=255 {
        let idx = usize::try_from(code).unwrap_or(0);
        let width = crate::tables::StandardEncoding
            .get(idx)
            .and_then(|name| crate::standard14::width(&dict.base_font, name))
            .map(f64::from)
            .unwrap_or(0.0);
        values.push(width);
    }
    Some(WidthsArray { first: 0, values })
}

impl ResolvedFont {
    /// The resolved width for `code`.
    ///
    /// Order: `/Widths` array → embedded metrics → standard-14 AFM →
    /// `/MissingWidth`. Every code resolves (never `None`); the final
    /// fallback is `/MissingWidth`, which defaults to 0.
    #[must_use]
    pub fn width(&self, code: u32) -> f64 {
        self.widths
            .as_ref()
            .and_then(|w| w.width(code))
            .or_else(|| self.embedded.as_ref().and_then(|e| e.width(code)))
            .unwrap_or(self.missing_width)
    }

    /// The resolved `/MissingWidth`.
    #[must_use]
    pub fn missing_width(&self) -> f64 {
        self.missing_width
    }

    /// Whether the font has an embedded TrueType metric source.
    #[must_use]
    pub fn has_embedded_metrics(&self) -> bool {
        self.embedded.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FontDescriptor, FontDict, FontSubtype};
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn widths_array_resolves_in_range() {
        let mut g = guard();
        let dict = FontDict {
            first_char: 32,
            last_char: 34,
            widths: Some(vec![250.0, 500.0, 750.0]),
            ..FontDict::simple(FontSubtype::Type1)
        };
        let resolved = resolve(&dict, &mut g).expect("resolve");
        assert_eq!(resolved.width(32), 250.0);
        assert_eq!(resolved.width(33), 500.0);
        assert_eq!(resolved.width(34), 750.0);
    }

    #[test]
    fn out_of_range_code_falls_to_missing_width() {
        let mut g = guard();
        let dict = FontDict {
            first_char: 32,
            last_char: 34,
            widths: Some(vec![250.0, 500.0, 750.0]),
            descriptor: Some(FontDescriptor {
                missing_width: 600.0,
                ..FontDescriptor::default()
            }),
            ..FontDict::simple(FontSubtype::Type1)
        };
        let resolved = resolve(&dict, &mut g).expect("resolve");
        assert_eq!(resolved.width(35), 600.0);
        assert_eq!(resolved.width(0), 600.0);
    }

    #[test]
    fn missing_width_defaults_to_zero() {
        let mut g = guard();
        let dict = FontDict::simple(FontSubtype::Type1);
        let resolved = resolve(&dict, &mut g).expect("resolve");
        assert_eq!(resolved.width(65), 0.0);
        assert_eq!(resolved.missing_width(), 0.0);
    }

    #[test]
    fn mis_sized_widths_array_is_a_deviation_not_an_error() {
        let mut g = guard();
        let dict = FontDict {
            first_char: 32,
            last_char: 40,
            widths: Some(vec![1.0]), // wrong size
            ..FontDict::simple(FontSubtype::Type1)
        };
        let resolved = resolve(&dict, &mut g).expect("resolve");
        assert_eq!(resolved.width(33), 0.0); // fell through to missing width
    }

    /// DoD: a missing `/Widths` falls back to the embedded font's own metrics.
    /// The fixture TrueType has units-per-em 1000 and advance widths A=700,
    /// B=900, so the resolved widths match those numbers exactly.
    #[test]
    fn missing_widths_falls_back_to_embedded_metrics() {
        let mut g = guard();
        let ttf = include_bytes!("../tests/fixtures/mini.ttf");
        let dict = FontDict {
            subtype: FontSubtype::TrueType,
            widths: None,
            font_file: Some(crate::model::FontFile::TrueType(Bytes::copy_from_slice(
                ttf,
            ))),
            ..FontDict::simple(FontSubtype::TrueType)
        };
        let resolved = resolve(&dict, &mut g).expect("resolve");
        assert!(resolved.has_embedded_metrics());
        assert_eq!(resolved.width(0x41), 700.0); // 'A'
        assert_eq!(resolved.width(0x42), 900.0); // 'B'
                                                 // A code the font has no glyph for falls through to missing width.
        assert_eq!(resolved.width(0x43), 0.0); // 'C' not in the fixture
    }

    #[test]
    fn widths_array_wins_over_embedded_metrics() {
        let mut g = guard();
        let ttf = include_bytes!("../tests/fixtures/mini.ttf");
        let dict = FontDict {
            subtype: FontSubtype::TrueType,
            first_char: 65,
            last_char: 65,
            widths: Some(vec![123.0]),
            font_file: Some(crate::model::FontFile::TrueType(Bytes::copy_from_slice(
                ttf,
            ))),
            ..FontDict::simple(FontSubtype::TrueType)
        };
        let resolved = resolve(&dict, &mut g).expect("resolve");
        // The /Widths entry wins over the embedded advance (700).
        assert_eq!(resolved.width(0x41), 123.0);
    }

    #[test]
    fn composite_resolves_through_the_descendant() {
        let mut g = guard();
        let desc = FontDict {
            first_char: 0,
            last_char: 1,
            widths: Some(vec![333.0, 444.0]),
            ..FontDict::simple(FontSubtype::CidFontType2)
        };
        let dict = FontDict {
            subtype: FontSubtype::Type0,
            descendant: Some(Box::new(desc)),
            ..FontDict::simple(FontSubtype::Type0)
        };
        let resolved = resolve(&dict, &mut g).expect("resolve");
        assert_eq!(resolved.width(0), 333.0);
        assert_eq!(resolved.width(1), 444.0);
    }

    #[test]
    fn broken_embedded_font_is_a_deviation_not_an_error() {
        let mut g = guard();
        let dict = FontDict {
            subtype: FontSubtype::TrueType,
            font_file: Some(crate::model::FontFile::TrueType(Bytes::copy_from_slice(
                b"not a font",
            ))),
            ..FontDict::simple(FontSubtype::TrueType)
        };
        let resolved = resolve(&dict, &mut g).expect("resolve");
        assert!(!resolved.has_embedded_metrics());
        assert_eq!(resolved.width(0x41), 0.0);
    }
}
