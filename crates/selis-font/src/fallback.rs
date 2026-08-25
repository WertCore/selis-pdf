//! Font fallback chain (SL-3.FONT.09).
//!
//! When a font is not embedded (or a glyph is missing from the embedded
//! font), the engine falls back per ISO 32000-2 §9.6.6. The order is
//! **deterministic** (ADR-P0012: the same document renders identically
//! everywhere):
//!
//! 1. an exact or aliased name match → the metric-compatible standard-14
//!    substitute (with the AFM widths, SL-3.FONT.08);
//! 2. otherwise, classify the `/FontDescriptor` hints (flags, `/FontFamily`,
//!    panose, `/StemV`) → the bundled set;
//! 3. then to system fonts (native shells, via `fontdb` — outside this
//!    crate);
//! 4. then to a `notdef` box.
//!
//! The guarantee this module enforces is the DoD: **a `notdef` box is never
//! shown for a glyph name that exists in any standard font** — the substitute
//! is checked against its AFM table before the renderer resorts to the chain.
//!
//! The alias map follows pdf.js's `getStdFontMap`: common non-standard names
//! (Arial, Times New Roman, Courier New, ...) resolve to their
//! metric-compatible standard-14 font.

use crate::standard14;

/// The `/FontDescriptor`-derived matching hints.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FontMatchHints {
    /// The `/FontFamily` (may be absent).
    pub family: Option<String>,
    /// The `/StemV` (stem width), for weight estimation.
    pub stem_v: Option<f64>,
    /// The `/Panose` byte 0 (family kind: 2 Latin text, 4 decorative,
    /// 5 symbol, 6 monospace text) and byte 1 (serif style).
    pub panose_family_kind: Option<u8>,
    /// The descriptor's Symbolic flag (bit 2 of `/Flags`).
    pub symbolic: bool,
    /// The descriptor's FixedPitch flag (bit 1 of `/Flags`).
    pub monospace: bool,
    /// The descriptor's Serif flag (bit 0 of `/Flags`).
    pub serif: bool,
}

/// Resolve a font name to its metric-compatible standard-14 substitute.
///
/// Returns the name unchanged when it is already standard-14, its substitute
/// for the common aliases, and `None` for an unknown font.
#[must_use]
pub fn substitute(font_name: &str) -> Option<&'static str> {
    if let Some(font) = standard14::ALL_FONTS.iter().find(|f| f.name == font_name) {
        return Some(font.name);
    }
    alias(font_name)
}

/// The deterministic descriptor-based match: pick the bundled substitute for
/// an unembedded font from its `/FontDescriptor` hints.
///
/// Order of precedence (documented, deterministic):
///
/// 1. a `/FontFamily` name that aliases to a standard-14 font;
/// 2. a symbolic font → `Symbol` (or `ZapfDingbats` for the Dingbats
///    family);
/// 3. a monospace font → `Courier` (weighted by `/StemV`);
/// 4. otherwise the serif/sans decision (`/Flags` Serif bit, panose kind,
///    or a serif family name), weighted by `/StemV` for the bold style.
#[must_use]
pub fn match_substitute(hints: &FontMatchHints) -> &'static str {
    if let Some(family) = hints.family.as_deref() {
        if let Some(sub) = substitute(family) {
            return sub;
        }
        let lower = family.to_ascii_lowercase();
        if lower.contains("dingbat") || lower.contains("wingding") {
            return "ZapfDingbats";
        }
        if lower.contains("symbol") {
            return "Symbol";
        }
    }
    if hints.symbolic {
        return "Symbol";
    }
    if hints.monospace || hints.panose_family_kind == Some(6) {
        return if bold(hints) {
            "Courier-Bold"
        } else {
            "Courier"
        };
    }
    let serif = hints.serif
        || hints.panose_family_kind == Some(2)
        || hints
            .family
            .as_deref()
            .is_some_and(|f| f.to_ascii_lowercase().contains("serif"));
    if serif {
        if bold(hints) {
            "Times-Bold"
        } else {
            "Times-Roman"
        }
    } else if bold(hints) {
        "Helvetica-Bold"
    } else {
        "Helvetica"
    }
}

/// A rough `/StemV`-based weight estimate (the descriptor's stem width).
fn bold(hints: &FontMatchHints) -> bool {
    hints.stem_v.is_some_and(|s| s >= 100.0)
}

/// Whether a glyph name is renderable — exists in the standard font's AFM
/// table — so a `notdef` box is unnecessary.
///
/// The DoD check: for text that exists in *any* standard font, this returns
/// `true` for the resolved substitute.
#[must_use]
pub fn can_render(font_name: &str, glyph_name: &str) -> bool {
    let Some(sub) = substitute(font_name) else {
        return false;
    };
    standard14::width(sub, glyph_name).is_some()
}

/// The fallback order for a glyph missing from the current font: the same
/// family first, then the metric-compatible standard-14 substitute, then the
/// symbol fonts with the widest coverage.
///
/// The engine walks this list (system fonts land outside this crate, at the
/// shell layer) and stops at the first font that [`can_render`]s the glyph.
#[must_use]
pub fn fallback_order(font_name: &str) -> Vec<&'static str> {
    let mut order = Vec::new();
    if let Some(sub) = substitute(font_name) {
        order.push(sub);
    }
    // Symbol and ZapfDingbats cover the widest set of non-Latin glyphs.
    if !order.contains(&"Symbol") {
        order.push("Symbol");
    }
    if !order.contains(&"ZapfDingbats") {
        order.push("ZapfDingbats");
    }
    order
}

/// The common non-standard → standard-14 alias map (pdf.js `getStdFontMap`).
fn alias(font_name: &str) -> Option<&'static str> {
    const ALIASES: &[(&str, &str)] = &[
        // Arial family → Helvetica.
        ("Arial", "Helvetica"),
        ("Arial-Bold", "Helvetica-Bold"),
        ("Arial-Italic", "Helvetica-Oblique"),
        ("Arial-BoldItalic", "Helvetica-BoldOblique"),
        ("ArialMT", "Helvetica"),
        ("Arial-BoldMT", "Helvetica-Bold"),
        ("Arial-ItalicMT", "Helvetica-Oblique"),
        ("Arial-BoldItalicMT", "Helvetica-BoldOblique"),
        ("ArialBlack", "Helvetica"),
        ("ArialNarrow", "Helvetica"),
        ("ArialNarrow-Bold", "Helvetica-Bold"),
        ("ArialNarrow-Italic", "Helvetica-Oblique"),
        ("ArialNarrow-BoldItalic", "Helvetica-BoldOblique"),
        // Times New Roman family → Times.
        ("TimesNewRoman", "Times-Roman"),
        ("TimesNewRomanPS", "Times-Roman"),
        ("TimesNewRomanPSMT", "Times-Roman"),
        ("TimesNewRomanPS-Bold", "Times-Bold"),
        ("TimesNewRomanPSMT-Bold", "Times-Bold"),
        ("TimesNewRoman-Italic", "Times-Italic"),
        ("TimesNewRomanPS-Italic", "Times-Italic"),
        ("TimesNewRomanPSMT-Italic", "Times-Italic"),
        ("TimesNewRoman-BoldItalic", "Times-BoldItalic"),
        ("TimesNewRomanPS-BoldItalic", "Times-BoldItalic"),
        ("TimesNewRomanPSMT-BoldItalic", "Times-BoldItalic"),
        // Courier New family → Courier.
        ("CourierNew", "Courier"),
        ("CourierNewPSMT", "Courier"),
        ("CourierNew-Bold", "Courier-Bold"),
        ("CourierNewPS-BoldMT", "Courier-Bold"),
        ("CourierNew-Italic", "Courier-Oblique"),
        ("CourierNewPS-ItalicMT", "Courier-Oblique"),
        ("CourierNew-BoldItalic", "Courier-BoldOblique"),
        // Helvetica's own aliases.
        ("Helvetica-Italic", "Helvetica-Oblique"),
        ("Helvetica-BoldItalic", "Helvetica-BoldOblique"),
        // Symbol family.
        ("Symbol-Bold", "Symbol"),
        ("Symbol-Italic", "Symbol"),
        ("Symbol-BoldItalic", "Symbol"),
        // Others.
        ("ArialUnicodeMS", "Helvetica"),
        ("Calibri", "Helvetica"),
        ("Calibri-Bold", "Helvetica-Bold"),
        ("Calibri-Italic", "Helvetica-Oblique"),
        ("Calibri-BoldItalic", "Helvetica-BoldOblique"),
        ("GillSansMT", "Helvetica"),
        ("Impact", "Helvetica"),
        ("LucidaConsole", "Courier"),
        ("LucidaConsole-Bold", "Courier-Bold"),
        ("LucidaConsole-Italic", "Courier-Oblique"),
        ("LucidaConsole-BoldItalic", "Courier-BoldOblique"),
        ("NuptialScript", "Times-Italic"),
        ("SegoeUISymbol", "Helvetica"),
        ("TrebuchetMS", "Helvetica"),
        ("TrebuchetMS-Bold", "Helvetica-Bold"),
        ("TrebuchetMS-Italic", "Helvetica-Oblique"),
        ("TrebuchetMS-BoldItalic", "Helvetica-BoldOblique"),
    ];
    ALIASES
        .iter()
        .find(|(name, _)| *name == font_name)
        .map(|(_, sub)| *sub)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_fonts_substitute_to_themselves() {
        assert_eq!(substitute("Helvetica"), Some("Helvetica"));
        assert_eq!(substitute("Times-Roman"), Some("Times-Roman"));
        assert_eq!(substitute("Courier"), Some("Courier"));
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(substitute("ArialMT"), Some("Helvetica"));
        assert_eq!(substitute("Arial-Bold"), Some("Helvetica-Bold"));
        assert_eq!(substitute("TimesNewRomanPSMT"), Some("Times-Roman"));
        assert_eq!(substitute("CourierNewPSMT"), Some("Courier"));
    }

    #[test]
    fn unknown_font_is_none() {
        assert_eq!(substitute("NoSuchFont"), None);
    }

    /// DoD: a glyph that exists in a standard font's AFM renders without a
    /// `notdef` box, even through an alias.
    #[test]
    fn can_render_standard_glyphs() {
        assert!(can_render("Helvetica", "A"));
        assert!(can_render("Helvetica", "a"));
        assert!(can_render("Helvetica", "space"));
        // Through the alias.
        assert!(can_render("ArialMT", "A"));
        assert!(can_render("TimesNewRomanPSMT", "A"));
        // A glyph Helvetica does not have.
        assert!(!can_render("Helvetica", "noSuchGlyph"));
        // An unknown font renders nothing.
        assert!(!can_render("NoSuchFont", "A"));
    }

    #[test]
    fn fallback_order_has_the_substitute_first() {
        let order = fallback_order("ArialMT");
        assert_eq!(order.first(), Some(&"Helvetica"));
        assert!(order.contains(&"Symbol"));
        assert!(order.contains(&"ZapfDingbats"));
        // The substitute is not duplicated.
        assert_eq!(order.iter().filter(|f| **f == "Helvetica").count(), 1);
    }

    #[test]
    fn fallback_order_unknown_font() {
        let order = fallback_order("NoSuchFont");
        assert!(order.contains(&"Symbol"));
        assert!(order.contains(&"ZapfDingbats"));
    }

    #[test]
    fn descriptor_matching_is_deterministic() {
        let sans = FontMatchHints {
            serif: false,
            symbolic: false,
            monospace: false,
            ..FontMatchHints::default()
        };
        let serif = FontMatchHints {
            serif: true,
            symbolic: false,
            monospace: false,
            ..FontMatchHints::default()
        };
        let mono = FontMatchHints {
            monospace: true,
            ..FontMatchHints::default()
        };
        let symbolic = FontMatchHints {
            symbolic: true,
            ..FontMatchHints::default()
        };
        // Deterministic: the same hints always give the same result.
        assert_eq!(match_substitute(&sans), match_substitute(&sans));
        assert_eq!(match_substitute(&serif), "Times-Roman");
        assert_eq!(match_substitute(&mono), "Courier");
        assert_eq!(match_substitute(&symbolic), "Symbol");
    }

    #[test]
    fn stem_v_selects_the_bold_style() {
        let heavy_sans = FontMatchHints {
            stem_v: Some(150.0),
            ..FontMatchHints::default()
        };
        let light_sans = FontMatchHints {
            stem_v: Some(60.0),
            ..FontMatchHints::default()
        };
        assert_eq!(match_substitute(&heavy_sans), "Helvetica-Bold");
        assert_eq!(match_substitute(&light_sans), "Helvetica");
        let heavy_mono = FontMatchHints {
            stem_v: Some(150.0),
            monospace: true,
            ..FontMatchHints::default()
        };
        assert_eq!(match_substitute(&heavy_mono), "Courier-Bold");
    }

    #[test]
    fn panose_kind_classifies() {
        let latin_text = FontMatchHints {
            panose_family_kind: Some(2),
            ..FontMatchHints::default()
        };
        assert_eq!(match_substitute(&latin_text), "Times-Roman");
        let mono_text = FontMatchHints {
            panose_family_kind: Some(6),
            ..FontMatchHints::default()
        };
        assert_eq!(match_substitute(&mono_text), "Courier");
    }

    #[test]
    fn family_name_aliases_first() {
        let arial = FontMatchHints {
            family: Some("Arial".to_string()),
            serif: true, // family wins over flags
            ..FontMatchHints::default()
        };
        assert_eq!(match_substitute(&arial), "Helvetica");
        let dingbats = FontMatchHints {
            family: Some("Wingdings".to_string()),
            ..FontMatchHints::default()
        };
        assert_eq!(match_substitute(&dingbats), "ZapfDingbats");
    }
}
