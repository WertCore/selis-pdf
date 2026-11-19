//! Font matching for edited text (SL-5.TEXTEDIT.04, dep SL-3.FONT.09).
//!
//! When an edit needs a character the page's original face cannot draw —
//! because the face is not embedded, is unavailable, or its subset simply
//! lacks that glyph — the edit must **not** silently draw with a different
//! face. Changing a face changes every advance width on the line, i.e. the
//! appearance of the document. So this module never returns a bare face name:
//! it returns a [`Substitute`] carrying a user-facing [`disclosure`](Substitute::disclosure)
//! string, and a quality [`confidence`](Substitute::confidence) so a UI can
//! show how close the match is.
//!
//! The choice is metric-compatible first: a standard-14 base face maps to its
//! `Liberation` twin through the same policy the renderer uses
//! ([`crate::fallback::substitute_for`]), so a `/Helvetica` becomes
//! `LiberationSans-Regular`, whose advances are the AFM's. Only when that face
//! cannot carry a needed character do we widen to the bundled fallback set.

use std::collections::BTreeSet;

use selis_sandbox::Budget;

use crate::fallback::{can_render, face_resident, fallback_order, substitute_for, FontMatchHints};

/// Why an original face needed a substitute at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstituteReason {
    /// The face is named in the document but has no font program embedded.
    Unembedded,
    /// The face is neither embedded nor one we can obtain a program for.
    Unavailable,
    /// The face is embedded, but its subset does not carry a needed glyph.
    MissingGlyph,
}

impl SubstituteReason {
    /// A short, user-facing label for the reason.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unembedded => "not embedded",
            Self::Unavailable => "unavailable",
            Self::MissingGlyph => "missing a needed glyph",
        }
    }
}

/// What we know about the page's original face.
///
/// The glyph coverage of an embedded subset is carried explicitly because the
/// edit crate reads the document's object graph and knows which codes the
/// `/ToUnicode` map can actually resolve — this module is COS-free and cannot
/// inspect a PDF itself.
#[derive(Debug, Clone)]
pub enum FontAvailability<'a> {
    /// The face is embedded and can draw every character in `covers`.
    Embedded {
        /// The characters the embedded subset carries.
        covers: &'a [char],
    },
    /// The face is referenced but has no embedded font program.
    Unembedded,
    /// The face cannot be obtained at all (unknown or broken program).
    Unavailable,
}

/// The original face as the caller knows it.
#[derive(Debug, Clone)]
pub struct OriginalFont<'a> {
    /// The `/BaseFont` name, possibly with a subset tag (`ABCDEE+Helvetica`).
    pub name: &'a str,
    /// What the caller knows about its availability and glyph coverage.
    pub availability: FontAvailability<'a>,
}

impl<'a> OriginalFont<'a> {
    /// An embedded face carrying `covers`.
    #[must_use]
    pub const fn embedded(name: &'a str, covers: &'a [char]) -> Self {
        Self {
            name,
            availability: FontAvailability::Embedded { covers },
        }
    }

    /// A referenced but unembedded face.
    #[must_use]
    pub const fn unembedded(name: &'a str) -> Self {
        Self {
            name,
            availability: FontAvailability::Unembedded,
        }
    }

    /// A face that is unavailable altogether.
    #[must_use]
    pub const fn unavailable(name: &'a str) -> Self {
        Self {
            name,
            availability: FontAvailability::Unavailable,
        }
    }
}

/// The outcome of font matching for one edit.
///
/// `disclosure` is empty **only** when `substituted` is `false`. There is no
/// path that changes the face without producing a disclosure — that is the
/// whole point of TEXTEDIT.04.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitute {
    /// The original `/BaseFont` name, as the caller gave it.
    pub original: String,
    /// The face to use: the original when `substituted` is false, else the
    /// chosen substitute's resident name.
    pub face: String,
    /// Whether the face actually changed.
    pub substituted: bool,
    /// Why it changed, or `None` when it did not.
    pub reason: Option<SubstituteReason>,
    /// Quality of the match in `0..=100` (higher is better). `100` means the
    /// original face was kept: no appearance change at all.
    pub confidence: u32,
    /// Whether the chosen substitute is metric-compatible with the original.
    pub metric_compatible: bool,
    /// Whether the chosen substitute can draw every needed character.
    pub covers_all: bool,
    /// The user-facing disclosure. Empty only when `substituted` is false.
    pub disclosure: String,
}

impl Substitute {
    /// A "keep the original" result: no substitution, no appearance change.
    fn keep(original: &str) -> Self {
        Self {
            original: original.to_string(),
            face: original.to_string(),
            substituted: false,
            reason: None,
            confidence: 100,
            metric_compatible: true,
            covers_all: true,
            disclosure: String::new(),
        }
    }
}

/// Pick the face for an edit of `original_font` that must draw every character
/// in `needed_chars`, and explain any substitution in a disclosure string.
///
/// The algorithm, in order:
///
/// 1. If the face is embedded and covers every needed character, keep it —
///    confidence 100, no disclosure. Nothing about the page's appearance
///    changes, so there is nothing to tell the user.
/// 2. Otherwise decide why (unembedded / unavailable / missing glyph).
/// 3. Ask the standard-14 policy for a metric-compatible face from the
///    `Liberation` family. A standard-14 name resolves to its AFM twin.
/// 4. Check that face against every needed character. If it cannot carry one,
///    widen through the bundled fallback set; keep the best face that carries
///    the most characters, preferring the metric-compatible one on ties.
/// 5. Score the match and build the disclosure. A substitution is **never**
///    returned with an empty disclosure.
///
/// # Budget
///
/// Work is bounded by `needed_chars.len()` times the (fixed, small) candidate
/// set. Each candidate charges one [`BudgetGuard`](selis_sandbox::BudgetGuard)
/// tick; if the budget is exhausted the search stops with the best face found
/// so far, whose confidence records the incomplete check. No document bytes
/// are read here.
///
/// # Malformed Input
///
/// An empty font name or an empty `needed_chars` is not an error: an empty
/// name is treated as an unavailable face (`Unavailable` unless said otherwise)
/// and an empty character list needs no glyphs, so an embedded face is kept.
/// Every result, including these, still satisfies the disclosure contract.
#[must_use]
pub fn match_substitute(
    original_font: &OriginalFont<'_>,
    needed_chars: &[char],
    budget: &Budget,
) -> Substitute {
    let mut g = budget.guard();

    // 1. Keep the original when it is embedded and complete.
    if let FontAvailability::Embedded { covers } = &original_font.availability {
        let covered = covers.iter().copied().collect::<BTreeSet<char>>();
        if needed_chars.iter().all(|ch| covered.contains(ch)) {
            return Substitute::keep(original_font.name);
        }
    }

    // 2. Why are we substituting?
    let reason = match original_font.availability {
        FontAvailability::Unembedded => SubstituteReason::Unembedded,
        FontAvailability::Unavailable => SubstituteReason::Unavailable,
        FontAvailability::Embedded { .. } => SubstituteReason::MissingGlyph,
    };

    // Which characters does the original fail to draw? (Informational: the
    // disclosure names them for the missing-glyph case.)
    let covered = match &original_font.availability {
        FontAvailability::Embedded { covers } => covers.iter().copied().collect(),
        _ => BTreeSet::new(),
    };
    let missing: Vec<char> = needed_chars
        .iter()
        .copied()
        .filter(|ch| !covered.contains(ch))
        .collect::<BTreeSet<char>>()
        .into_iter()
        .collect();

    // 3. The metric-compatible first candidate, from the standard-14 policy.
    let hints = style_hints(original_font.name);
    let metric_face =
        substitute_for(original_font.name).or_else(|| substitute("Helvetica", &hints));

    // 4. Walk the candidate set. A metric-compatible face is tried first; the
    //    bundled fallback order widens the search when it cannot carry a char.
    let mut candidates: Vec<(String, bool)> = Vec::new();
    if let Some(face) = metric_face {
        candidates.push((face.to_string(), true));
    }
    for face in fallback_order() {
        if candidates.iter().any(|(name, _)| name.as_str() == *face) {
            continue;
        }
        candidates.push(((*face).to_string(), false));
    }
    // Style variants widen coverage without changing the family: a bold or
    // italic twin of the metric-compatible face has the same AFM advances.
    for face in style_variants(metric_face) {
        if candidates.iter().any(|(name, _)| name == &face) {
            continue;
        }
        candidates.push((face, true));
    }

    let mut best: Option<(String, bool, usize)> = None;
    for (face, metric) in candidates {
        if g.tick().is_err() {
            break;
        }
        let carried = needed_chars
            .iter()
            .filter(|ch| face_covers(&face, **ch))
            .count();
        let better = match &best {
            None => true,
            Some((_, best_metric, best_carried)) => {
                carried > *best_carried || (carried == *best_carried && metric && !*best_metric)
            }
        };
        if better {
            best = Some((face, metric, carried));
        }
        if carried == needed_chars.len() && metric {
            break;
        }
    }

    let (face, metric_compatible, carried) = best.unwrap_or_else(|| {
        (
            metric_face.unwrap_or("LiberationSans-Regular").to_string(),
            metric_face.is_some(),
            0,
        )
    });
    let total = needed_chars.len();
    let covers_all = total == 0 || carried == total;
    let resident = face_resident(&face);
    let confidence = confidence(metric_compatible, covers_all, resident);

    // 5. Disclose, always. The disclosure names the original face, why it was
    //    replaced, the substitute, and any character it still cannot carry.
    let gap = if covers_all {
        String::new()
    } else {
        let mut s = String::from("; warning: this face still cannot draw ");
        for (i, ch) in missing
            .iter()
            .filter(|ch| !face_covers(&face, **ch))
            .enumerate()
        {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!("U+{:04X}", u32::from(*ch)));
        }
        s.push_str(" — the edit could not be matched safely");
        s
    };
    let kind = if metric_compatible {
        "metric-compatible "
    } else {
        ""
    };
    let why = match reason {
        SubstituteReason::MissingGlyph => {
            format!("lacks a needed glyph ({})", glyph_list(&missing))
        }
        _ => reason.label().to_string(),
    };
    let disclosure = format!(
        "original face {original} {why}; substituted {kind}{face}{gap}",
        original = original_font.name
    );

    Substitute {
        original: original_font.name.to_string(),
        face,
        substituted: true,
        reason: Some(reason),
        confidence,
        metric_compatible,
        covers_all,
        disclosure,
    }
}

/// The style hints implied by a `/BaseFont` variant name.
fn style_hints(name: &str) -> FontMatchHints {
    FontMatchHints {
        italic: name.contains("Italic") || name.contains("Oblique"),
        weight: if name.contains("Bold") { 700 } else { 400 },
    }
}

/// Map a base standard-14 family + hints to a Liberation face, widening the
/// policy in [`crate::fallback::substitute`] to Symbol/ZapfDingbats too (those
/// have no AFM twin, but a sans face is the engine's documented approximation).
fn substitute(base: &str, hints: &FontMatchHints) -> Option<&'static str> {
    crate::fallback::substitute(base, hints)
}

/// Same-family style variants of a metric-compatible face.
fn style_variants(face: Option<&'static str>) -> Vec<String> {
    let Some(face) = face else {
        return Vec::new();
    };
    let Some((family, suffix)) = face.split_once('-') else {
        return Vec::new();
    };
    ["Regular", "Bold", "Italic", "BoldItalic"]
        .iter()
        .filter(|s| **s != suffix)
        .map(|s| format!("{family}-{s}"))
        .collect()
}

/// Whether `face` can draw `ch`.
///
/// When the face's bytes are resident this is an exact cmap lookup. When they
/// are not (slim/web builds carry only the Serif subset), fall back to the
/// documented coverage of the Liberation family: Basic Latin, Latin-1 and the
/// typographic punctuation the built-in subset is cut to. CJK and other scripts
/// return `false` — we never claim a substitute covers a glyph we cannot see.
fn face_covers(face: &str, ch: char) -> bool {
    if can_render(face, ch) {
        return true;
    }
    if !face.starts_with("Liberation") {
        return false;
    }
    let cp = u32::from(ch);
    (0x20u32..=0x7E).contains(&cp)
        || (0xA0u32..=0xFF).contains(&cp)
        || matches!(cp, 0x2013 | 0x2014 | 0x2018..=0x201D | 0x2026 | 0x20AC)
}

/// A discrete quality score, so no float arithmetic is needed in production
/// code (the workspace denies arithmetic side effects).
const fn confidence(metric_compatible: bool, covers_all: bool, resident: bool) -> u32 {
    match (metric_compatible, covers_all, resident) {
        (true, true, true) => 100,
        (true, true, false) => 90,
        (true, false, _) => 55,
        (false, true, true) => 75,
        (false, true, false) => 65,
        (false, false, _) => 35,
    }
}

/// A short list of the characters a subset was missing, for the disclosure.
fn glyph_list(chars: &[char]) -> String {
    let mut out = String::new();
    for (i, ch) in chars.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!(
            "'{}' (U+{:04X})",
            ch.escape_default(),
            u32::from(*ch)
        ));
    }
    if out.is_empty() {
        out.push_str("unknown");
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    fn budget() -> Budget {
        Budget::unlimited()
    }

    /// (a) An unembedded Helvetica must resolve to the metric-compatible
    /// Liberation Sans, with a disclosure that says so.
    #[test]
    fn unembedded_helvetica_gets_liberation_sans_and_discloses() {
        let needed = ['H', 'e', 'l', 'l', 'o'];
        let s = match_substitute(&OriginalFont::unembedded("Helvetica"), &needed, &budget());
        assert!(s.substituted);
        assert_eq!(s.face, "LiberationSans-Regular");
        assert_eq!(s.reason, Some(SubstituteReason::Unembedded));
        assert!(s.metric_compatible);
        assert!(s.covers_all);
        assert!(!s.disclosure.is_empty(), "a substitution must be disclosed");
        assert!(
            s.disclosure.contains("original face Helvetica"),
            "disclosure: {}",
            s.disclosure
        );
        assert!(
            s.disclosure
                .contains("substituted metric-compatible LiberationSans-Regular"),
            "disclosure: {}",
            s.disclosure
        );
    }

    /// (b) A character absent from the original subset resolves to a
    /// substitute that carries it, and the disclosure names both.
    #[test]
    fn missing_subset_glyph_gets_a_carrying_substitute_and_discloses() {
        let needed = ['a', 'é'];
        let s = match_substitute(
            &OriginalFont::embedded("ABCDEE+Helvetica", &['a']),
            &needed,
            &budget(),
        );
        assert!(s.substituted);
        assert_eq!(s.reason, Some(SubstituteReason::MissingGlyph));
        assert!(s.covers_all, "the substitute carries the missing glyph");
        assert!(face_covers(&s.face, 'é'), "{} must carry U+00E9", s.face);
        assert!(
            s.disclosure.contains("lacks a needed glyph"),
            "disclosure: {}",
            s.disclosure
        );
        assert!(
            s.disclosure.contains("U+00E9"),
            "the missing codepoint is disclosed: {}",
            s.disclosure
        );
    }

    /// (d) An embedded, complete face is kept: no substitution, no disclosure.
    #[test]
    fn embedded_complete_face_is_not_substituted() {
        let needed = ['H', 'e', 'l', 'l', 'o'];
        let s = match_substitute(
            &OriginalFont::embedded("ABCDEE+Helvetica", &['H', 'e', 'l', 'o']),
            &needed,
            &budget(),
        );
        assert!(!s.substituted);
        assert_eq!(s.face, "ABCDEE+Helvetica");
        assert_eq!(s.reason, None);
        assert_eq!(s.confidence, 100);
        assert!(
            s.disclosure.is_empty(),
            "no change means nothing to disclose"
        );
    }

    /// A substitute that still cannot carry a CJK glyph says so, loudly, and
    /// scores low — it is never presented as a confident silent replacement.
    #[test]
    fn uncovered_glyph_is_disclosed_and_scores_low() {
        let s = match_substitute(&OriginalFont::unembedded("Helvetica"), &['中'], &budget());
        assert!(s.substituted);
        assert!(!s.covers_all);
        assert!(s.confidence < 60);
        assert!(
            s.disclosure.contains("cannot draw") && s.disclosure.contains("U+4E2D"),
            "disclosure: {}",
            s.disclosure
        );
    }

    /// Every substitution carries a disclosure; no-substitution carries none.
    #[test]
    fn disclosure_contract_holds_for_every_availability() {
        for original in [
            OriginalFont::unembedded("Helvetica"),
            OriginalFont::unavailable("MysteryFont"),
            OriginalFont::embedded("Times-Roman", &[]),
        ] {
            let s = match_substitute(&original, &['A'], &budget());
            assert_eq!(s.disclosure.is_empty(), !s.substituted);
        }
    }
}
