//! Fallback fonts (SL-0.LEAD.07).
//!
//! The standard-14 base fonts (Helvetica, Times, Courier, Symbol,
//! ZapfDingbats) are usually NOT embedded. When no font program is available,
//! the renderer falls back to the bundled Liberation fonts (SIL Open Font
//! License — metric-compatible with the Adobe standard-14), committed under
//! `assets/fonts/`.

use crate::outline::glyph_id_for_char;

/// Hints about the font style the fallback should substitute (from the
/// standard-14 variant name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontMatchHints {
    /// Whether the variant is oblique/italic.
    pub italic: bool,
    /// The weight (400 = normal, 700 = bold).
    pub weight: u16,
}

impl Default for FontMatchHints {
    fn default() -> Self {
        Self {
            italic: false,
            weight: 400,
        }
    }
}

/// The Liberation font bytes for a standard-14 base font name, or `None` when
/// the name is not a standard-14 font.
#[must_use]
pub fn fallback_bytes(font_name: &str) -> Option<&'static [u8]> {
    // A subset font's /BaseFont is often `ABCDEE+Helvetica` — strip the tag.
    let tagged = font_name.rsplit('+').next().unwrap_or(font_name);
    let base = base_family(tagged)?;
    let name = substitute(base, &style_hints(tagged))?;
    family_bytes(name)
}

/// The base standard-14 family of a variant name (e.g. `Times-BoldItalic` →
/// `Times-Roman`).
#[must_use]
fn base_family(name: &str) -> Option<&'static str> {
    let family = if name == "Courier"
        || name.starts_with("Courier-")
    {
        "Courier"
    } else if name == "Helvetica" || name.starts_with("Helvetica-") {
        "Helvetica"
    } else if name == "Times-Roman"
        || name.starts_with("Times-")
        || name == "Times"
    {
        "Times-Roman"
    } else {
        return None;
    };
    Some(family)
}

/// Map a base standard-14 font name (with its style) to a Liberation family
/// name. `None` when the name is not a standard-14 base font.
#[must_use]
pub fn substitute(base: &str, hints: &FontMatchHints) -> Option<&'static str> {
    let family = match base {
        "Courier" => "LiberationMono",
        "Helvetica" => "LiberationSans",
        "Times-Roman" | "Times" => "LiberationSerif",
        // Symbol and ZapfDingbats have no Liberation equivalent; the regular
        // sans-serif is a usable approximation for fallback rendering.
        "Symbol" | "ZapfDingbats" => "LiberationSans",
        _ => return None,
    };
    let suffix = match (hints.italic, hints.weight >= 700) {
        (true, true) => "-BoldItalic",
        (true, false) => "-Italic",
        (false, true) => "-Bold",
        (false, false) => "-Regular",
    };
    // Free the temporary into a static: the match arms are all string literals.
    Some(concat_static(family, suffix))
}

/// The style hints implied by a standard-14 variant name.
#[must_use]
fn style_hints(base: &str) -> FontMatchHints {
    let italic = base.contains("Italic") || base.contains("Oblique");
    let weight = if base.contains("Bold") { 700 } else { 400 };
    FontMatchHints { italic, weight }
}

/// The Liberation font bytes for a family+suffix name.
fn family_bytes(name: &str) -> Option<&'static [u8]> {
    let bytes: &'static [u8] = match name {
        "LiberationMono-Regular" => {
            include_bytes!("../../../assets/fonts/LiberationMono-Regular.ttf")
        }
        "LiberationMono-Bold" => include_bytes!("../../../assets/fonts/LiberationMono-Bold.ttf"),
        "LiberationMono-Italic" => {
            include_bytes!("../../../assets/fonts/LiberationMono-Italic.ttf")
        }
        "LiberationMono-BoldItalic" => {
            include_bytes!("../../../assets/fonts/LiberationMono-BoldItalic.ttf")
        }
        "LiberationSans-Regular" => {
            include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf")
        }
        "LiberationSans-Bold" => include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf"),
        "LiberationSans-Italic" => {
            include_bytes!("../../../assets/fonts/LiberationSans-Italic.ttf")
        }
        "LiberationSans-BoldItalic" => {
            include_bytes!("../../../assets/fonts/LiberationSans-BoldItalic.ttf")
        }
        "LiberationSerif-Regular" => {
            include_bytes!("../../../assets/fonts/LiberationSerif-Regular.ttf")
        }
        "LiberationSerif-Bold" => include_bytes!("../../../assets/fonts/LiberationSerif-Bold.ttf"),
        "LiberationSerif-Italic" => {
            include_bytes!("../../../assets/fonts/LiberationSerif-Italic.ttf")
        }
        "LiberationSerif-BoldItalic" => {
            include_bytes!("../../../assets/fonts/LiberationSerif-BoldItalic.ttf")
        }
        _ => return None,
    };
    Some(bytes)
}

/// The preferred fallback order (sans first — the common case).
#[must_use]
pub fn fallback_order() -> &'static [&'static str] {
    &[
        "LiberationSans-Regular",
        "LiberationSerif-Regular",
        "LiberationMono-Regular",
    ]
}

/// Whether a fallback font can render a character (its cmap has the codepoint).
#[must_use]
pub fn can_render(font_name: &str, ch: char) -> bool {
    let Some(bytes) = family_bytes(font_name) else {
        return false;
    };
    glyph_id_for_char(&selis_bytes::Bytes::from(bytes.to_vec()), u32::from(ch)).is_some()
}

/// Find the first fallback font that can render `ch`, honoring style hints.
#[must_use]
pub fn match_substitute(ch: char, hints: &FontMatchHints) -> Option<&'static str> {
    for family in ["LiberationSans", "LiberationSerif", "LiberationMono"] {
        let suffix = match (hints.italic, hints.weight >= 700) {
            (true, true) => "-BoldItalic",
            (true, false) => "-Italic",
            (false, true) => "-Bold",
            (false, false) => "-Regular",
        };
        let name = concat_static(family, suffix);
        if can_render(name, ch) {
            return Some(name);
        }
    }
    None
}

/// Concatenate two &'static str into one &'static str (both are literals).
fn concat_static(a: &'static str, b: &'static str) -> &'static str {
    // The only call sites pass compile-time literals, so the boxed String is
    // guaranteed to be built from them; leak it once to get a &'static str.
    Box::leak(format!("{a}{b}").into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::{outline_glyph, units_per_em};

    fn guard() -> selis_sandbox::BudgetGuard<'static> {
        selis_sandbox::Budget::unlimited().guard()
    }

    #[test]
    fn liberation_sans_parses_and_has_glyphs() {
        let bytes = fallback_bytes("Helvetica").expect("helvetica fallback");
        assert!(units_per_em(&selis_bytes::Bytes::from(bytes.to_vec())).is_some());
        let mut g = guard();
        let font = selis_bytes::Bytes::from(bytes.to_vec());
        let gid = glyph_id_for_char(&font, u32::from(b'A')).expect("glyph A");
        let outline = outline_glyph(&font, gid, &mut g).expect("outline ok");
        assert!(outline.is_some(), "'A' has a drawn outline");
    }

    #[test]
    fn subset_tag_is_stripped() {
        assert!(fallback_bytes("ABCDEE+Helvetica").is_some());
        assert!(fallback_bytes("Times-BoldItalic").is_some());
        assert!(fallback_bytes("Courier-BoldOblique").is_some());
        assert!(fallback_bytes("NotAStandardFont").is_none());
    }

    #[test]
    fn style_hints_select_the_variant() {
        let hints = style_hints("Times-BoldItalic");
        assert_eq!(hints.italic, true);
        assert_eq!(hints.weight, 700);
        assert_eq!(substitute("Times-Roman", &hints), Some("LiberationSerif-BoldItalic"));
        let hints = style_hints("Helvetica");
        assert_eq!(substitute("Helvetica", &hints), Some("LiberationSans-Regular"));
    }

    #[test]
    fn can_render_checks_the_cmap() {
        assert!(can_render("LiberationSans-Regular", 'A'));
        assert!(!can_render("LiberationSans-Regular", '\u{4E2D}')); // CJK not in the font
    }
}