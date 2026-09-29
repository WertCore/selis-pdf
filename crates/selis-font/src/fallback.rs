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

/// Is this a standard-14 base font name Selis can substitute for?
///
/// **This is the policy half of [`fallback_bytes`], and it deliberately
/// returns no font data.** Two engine call sites ask exactly this question
/// about a *name* and never touch the bytes
/// (`selis-pdf-engine/src/session.rs`, `is_standard14_fallback` and the
/// resource resolver), and they used to ask it by calling `fallback_bytes(..)
/// .is_some()`.
///
/// That coupling is load-bearing in the worst way: with the byte-carrying
/// signature, whether a name counts as standard-14 depends on whether the face
/// it maps to happens to be *embedded in this build*. So a build that ships
/// fewer faces would quietly stop substituting for the faces it dropped, and
/// `is_standard14_fallback` would answer `false` for a perfectly ordinary
/// `/Helvetica` — the document would stop being recognised as a standard-14
/// case and would take the no-program path instead, painting nothing. The
/// question "is this name standard-14?" is about the spec, and the spec does
/// not change with what we ship.
///
/// Keeping the two apart is also what lets the embedded set shrink at all: the
/// predicate costs nothing, so removing a face from the binary cannot change
/// any answer it gives.
#[must_use]
pub fn is_standard14(font_name: &str) -> bool {
    resolve_family(font_name).is_some()
}

/// The standard-14 base family and the face it substitutes to, or `None` when
/// the name is not a standard-14 font.
///
/// The name→face mapping every entry point shares, so `is_standard14` and
/// `fallback_bytes` cannot drift apart.
fn resolve_family(font_name: &str) -> Option<&'static str> {
    // A subset font's /BaseFont is often `ABCDEE+Helvetica` — strip the tag.
    let tagged = font_name.rsplit('+').next().unwrap_or(font_name);
    let base = base_family(tagged)?;
    substitute(base, &style_hints(tagged))
}

/// The Liberation font bytes for a standard-14 base font name, or `None` when
/// the name is not a standard-14 font **or** the face it needs is not built
/// into this build.
///
/// `None` here is now ambiguous between "not a standard-14 name" and "a
/// standard-14 name whose face is not resident". Callers that mean the first
/// want [`is_standard14`]; callers that mean the second — the render path, which
/// genuinely needs glyphs — want this one, and must handle `None` by loading
/// the face rather than by concluding the document has no standard-14 font.
#[must_use]
pub fn fallback_bytes(font_name: &str) -> Option<&'static [u8]> {
    family_bytes(resolve_family(font_name)?)
}

/// The base standard-14 family of a variant name (e.g. `Times-BoldItalic` →
/// `Times-Roman`).
#[must_use]
fn base_family(name: &str) -> Option<&'static str> {
    let family = if name == "Courier" || name.starts_with("Courier-") {
        "Courier"
    } else if name == "Helvetica" || name.starts_with("Helvetica-") {
        "Helvetica"
    } else if name == "Times-Roman" || name.starts_with("Times-") || name == "Times" {
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

/// The Liberation font bytes for a family+suffix name, when that face is
/// **built into this build**.
///
/// ## What is built in, and why only this
///
/// The full twelve-face set was, until FONT-PAYLOAD sizing, embedded whole by
/// `include_bytes!` here: 1 625 684 B of the shipped module's 1 914 752 B
/// `$.rodata` — 84.9% of it, and 908 348 B of a 1 275 596 B brotli download,
/// for fonts that most documents never reach because a PDF normally embeds its
/// own. See `pdf-plan/32-FONT-PAYLOAD-SIZING.md` for the attribution.
///
/// Two faces are built in, both subsetted to Latin-1 plus typographic
/// punctuation:
///
/// - **Serif** is the choice because it is metric-compatible with Times New
///   Roman, and Times is the base-14 font most often referenced *without*
///   embedding (LaTeX, ReportLab and hand-rolled generators all do this).
/// - **Regular and Bold** rather than one face, because a single subset would
///   render bold text in regular metrics — wrong weight and wrong advances,
///   which reflows the line. Two faces cost 73 196 B brotli together.
///   Italic is left to the lazy path; the fourth face would add 70 kB for the
///   less common of the two.
///
/// The remaining ten faces are **not** embedded and are fetched at runtime
/// through the same chunk mechanism the CJK payload uses. Desktop keeps all
/// twelve in the binary behind the `builtin-fallback-fonts` feature; the subset
/// is built in on every target, because a fallback that is absent on a cold
/// cache paints nothing, and "paints nothing" is the one failure mode a
/// fallback font must not have.
///
/// ## Why the policy is not here
///
/// Nothing in this function decides *which* face a name maps to — that is
/// `substitute`, reached through [`fallback_bytes`], and it is identical on
/// every target. This function only answers *is the face resident*, which is a
/// delivery question. Keeping the two apart is what allows the desktop and web
/// builds to differ here without ever differing about which face substitutes
/// for a given name.
#[cfg(feature = "builtin-fallback-fonts")]
fn family_bytes(name: &str) -> Option<&'static [u8]> {
    let bytes: &'static [u8] = match name {
        // Desktop: the full upstream faces, all twelve, unsubsetted.
        //
        // The subsets are deliberately NOT used here. They exist to make the
        // *web* module small, and on desktop they are strictly worse — the
        // subset drops every glyph outside Latin-1 plus typographic
        // punctuation, so a desktop build embedding them would render a blank
        // for, say, `ā` in a Times-family document, where it used to render
        // correctly. Desktop has no size problem worth that, so it keeps the
        // real faces and stays fully offline with no lazy fetch at all.
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

/// The web/extension build: only the Serif subset, no other face.
///
/// Ten faces therefore come back `None` here and are the shell's job to fetch.
/// That is safe precisely because of the `is_standard14` / `fallback_bytes`
/// split above: the engine can still *name* the face it wants, so it can ask
/// for it, and asking does not depend on the answer being already in the
/// binary.
#[cfg(not(feature = "builtin-fallback-fonts"))]
fn family_bytes(name: &str) -> Option<&'static [u8]> {
    let bytes: &'static [u8] = match name {
        "LiberationSerif-Regular" => {
            include_bytes!("../../../assets/fonts/subset/LiberationSerif-Regular.ttf")
        }
        "LiberationSerif-Bold" => {
            include_bytes!("../../../assets/fonts/subset/LiberationSerif-Bold.ttf")
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

    /// The face `name` resolves to, or a panic naming the input.
    ///
    /// Reads through `fallback_bytes` so the assertions exercise the same path
    /// the engine uses, not a private shortcut.
    fn face_for(name: &str) -> Vec<u8> {
        fallback_bytes(name)
            .unwrap_or_else(|| panic!("{name} should resolve to a built-in face"))
            .to_vec()
    }

    /// A built-in face must be a real, parseable font with a drawable glyph.
    ///
    /// Uses Times rather than Helvetica because Times resolves in every build
    /// (Serif is the face that is always built in), so this covers the web
    /// build too — a corrupt subset is a silent blank page, not a build error.
    #[test]
    fn a_builtin_face_parses_and_has_glyphs() {
        let bytes = face_for("Times-Roman");
        assert!(units_per_em(&selis_bytes::Bytes::from(bytes.clone())).is_some());
        let mut g = guard();
        let font = selis_bytes::Bytes::from(bytes);
        let gid = glyph_id_for_char(&font, u32::from(b'A')).expect("glyph A");
        let outline = outline_glyph(&font, gid, &mut g).expect("outline ok");
        assert!(outline.is_some(), "'A' has a drawn outline");
    }

    #[test]
    fn subset_tag_is_stripped() {
        assert!(is_standard14("ABCDEE+Helvetica"));
        assert!(is_standard14("Times-BoldItalic"));
        assert!(is_standard14("Courier-BoldOblique"));
        assert!(!is_standard14("NotAStandardFont"));
    }

    #[test]
    fn style_hints_select_the_variant() {
        let hints = style_hints("Times-BoldItalic");
        assert!(hints.italic);
        assert_eq!(hints.weight, 700);
        assert_eq!(
            substitute("Times-Roman", &hints),
            Some("LiberationSerif-BoldItalic")
        );
        let hints = style_hints("Helvetica");
        assert_eq!(
            substitute("Helvetica", &hints),
            Some("LiberationSans-Regular")
        );
    }

    #[test]
    fn can_render_checks_the_cmap() {
        // Serif is the face built into every build, so the cmap check is
        // meaningful in both configurations.
        assert!(can_render("LiberationSerif-Regular", 'A'));
        assert!(!can_render("LiberationSerif-Regular", '\u{4E2D}')); // CJK not in the font
    }

    // --- the policy/delivery split -----------------------------------------
    //
    // These are the tests that would have caught the real bug, so they are
    // written to keep catching it if the two questions are ever re-merged.

    /// The regression test.
    ///
    /// Every standard-14 name must be recognised **regardless of which faces
    /// this build embeds**. Before the split, recognition was
    /// `fallback_bytes(name).is_some()`, so a build that embedded fewer faces
    /// would stop recognising the faces it dropped — and the engine's
    /// `is_standard14_fallback` and the resource resolver would take the
    /// "not a standard-14 document" path and drop the text instead of
    /// substituting it. Silently, and only on the builds that had been
    /// slimmed, which is the worst possible place to find out.
    #[test]
    fn is_standard14_does_not_depend_on_which_faces_are_embedded() {
        for name in [
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-Oblique",
            "Helvetica-BoldOblique",
            "Courier",
            "Courier-Bold",
            "Courier-Oblique",
            "Courier-BoldOblique",
            "Times-Roman",
            "Times-Bold",
            "Times-Italic",
            "Times-BoldItalic",
            // Subset-tagged spellings, which real files carry.
            "ABCDEE+Helvetica",
            "ABCDEE+Times-BoldItalic",
        ] {
            assert!(
                is_standard14(name),
                "{name} is a standard-14 name and must be recognised \
                 even when its face is not resident"
            );
        }
    }

    /// `Symbol` and `ZapfDingbats` are in the base-14 set, but this engine
    /// does not currently recognise them: `substitute` maps them to
    /// `LiberationSans`, yet `base_family` has no arm for them, so the name
    /// never reaches `substitute` and both questions answer "no".
    ///
    /// Pinned here so the gap stays visible rather than latent. It is
    /// **pre-existing** — this payload work did not introduce it — and it is
    /// left as-is because fixing it would change which documents count as
    /// standard-14, which is a substitution-policy change and not a sizing
    /// one. The two questions do at least agree here now.
    #[test]
    fn symbol_and_zapfdingbats_are_not_yet_recognised() {
        assert!(!is_standard14("Symbol"));
        assert!(!is_standard14("ZapfDingbats"));
        // The delivery answer must agree with the policy answer, whatever
        // that answer happens to be.
        assert_eq!(fallback_bytes("Symbol").is_some(), is_standard14("Symbol"));
    }

    /// The inverse: this must not become a way to classify arbitrary names.
    #[test]
    fn is_standard14_rejects_non_standard_names() {
        for name in [
            "NotAStandardFont",
            "Arial",
            "",
            "LiberationSans-Regular", // the *face* name, not a base-14 name
        ] {
            assert!(!is_standard14(name), "{name} must not be standard-14");
        }
    }

    /// Whatever the build, every resident face must be a usable font.
    #[test]
    fn every_resident_face_is_a_usable_font() {
        for name in ["Times-Roman", "Times-Bold", "Helvetica", "Courier"] {
            if let Some(bytes) = fallback_bytes(name) {
                let font = selis_bytes::Bytes::from(bytes.to_vec());
                assert!(
                    units_per_em(&font).is_some(),
                    "{name} resolves to bytes that are not a font"
                );
                assert!(
                    glyph_id_for_char(&font, u32::from(b'A')).is_some(),
                    "{name} has no glyph for 'A'"
                );
            }
        }
    }

    /// Web: only the two Serif subsets are resident, and the rest are nameable
    /// but not present — which is exactly the state the lazy path exists for.
    #[cfg(not(feature = "builtin-fallback-fonts"))]
    #[test]
    fn web_build_keeps_only_the_serif_subset() {
        assert!(fallback_bytes("Times-Roman").is_some());
        assert!(fallback_bytes("Times-Bold").is_some());
        assert!(fallback_bytes("Helvetica").is_none());
        assert!(fallback_bytes("Courier").is_none());
        assert!(fallback_bytes("Times-Italic").is_none());
        // ...and every one of them is still nameable, so the shell can ask.
        assert!(is_standard14("Helvetica"));
        assert!(is_standard14("Courier"));
        assert!(is_standard14("Times-Italic"));
    }

    /// The web subset must cover the code points it claims to on both faces.
    /// A subset missing a Latin-1 letter would render a blank where the
    /// previous build rendered correctly — a silent regression.
    #[cfg(not(feature = "builtin-fallback-fonts"))]
    #[test]
    fn web_subset_covers_the_builtin_codepoints() {
        for (face, name) in [
            ("Times-Roman", "LiberationSerif-Regular"),
            ("Times-Bold", "LiberationSerif-Bold"),
        ] {
            let bytes = fallback_bytes(face).expect("subset face");
            let font = selis_bytes::Bytes::from(bytes.to_vec());
            for c in 0x20u32..0x7F {
                assert!(
                    glyph_id_for_char(&font, c).is_some(),
                    "{name} is missing ASCII U+{c:04X}"
                );
            }
            for c in 0xA0u32..0x100 {
                if (c as u8 as char).is_ascii_control() {
                    continue; // 0x80-0x9F are C1 controls, not text
                }
                assert!(
                    glyph_id_for_char(&font, c).is_some(),
                    "{name} is missing Latin-1 U+{c:04X}"
                );
            }
            for c in [0x2018u32, 0x2019, 0x201C, 0x201D, 0x2013, 0x2014, 0x2026] {
                assert!(
                    glyph_id_for_char(&font, c).is_some(),
                    "{name} is missing punctuation U+{c:04X}"
                );
            }
        }
    }

    /// Desktop keeps all twelve, so nothing is fetched and behaviour is
    /// unchanged from before this work.
    #[cfg(feature = "builtin-fallback-fonts")]
    #[test]
    fn desktop_build_keeps_all_twelve_faces() {
        for name in [
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-Oblique",
            "Helvetica-BoldOblique",
            "Courier",
            "Courier-Bold",
            "Courier-Oblique",
            "Courier-BoldOblique",
            "Times-Roman",
            "Times-Bold",
            "Times-Italic",
            "Times-BoldItalic",
        ] {
            assert!(
                fallback_bytes(name).is_some(),
                "desktop must keep {name} built in"
            );
        }
    }
}
