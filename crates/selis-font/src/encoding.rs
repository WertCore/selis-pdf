//! Encoding and character mapping (SL-3.FONT.02).
//!
//! For a simple font, a character code becomes a glyph through an **encoding**:
//! a base table (one of the predefined encodings of ISO 32000-2:2020 Annex D)
//! plus the `/Differences` overrides, or the font's **built-in** encoding when
//! no `/Encoding` is present.
//!
//! The rules this module implements, with citations:
//!
//! * `/Encoding` precedence (§9.2.4, §9.6.6.4): a named or dictionary
//!   `/Encoding` wins; a dictionary without `/BaseEncoding` differs over the
//!   font's built-in encoding; no `/Encoding` means the built-in encoding.
//! * `/Differences` run structure (§9.2.4.3): `[code name1 name2 ... code2
//!   name3 ...]` — each name after a code maps to the incrementing code.
//! * The predefined tables are `[&str; 256]`, index = code; `.notdef` marks
//!   an unassigned code (which resolves to no glyph).
//!
//! The character-to-glyph *selection* (cmap rules, symbolic vs nonsymbolic)
//! lives in [`crate::cmap`].

use std::collections::BTreeMap;

use crate::tables;

/// A base predefined encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseEncoding {
    /// `/StandardEncoding` (Annex D.2).
    Standard,
    /// `/WinAnsiEncoding` (Annex D.4).
    WinAnsi,
    /// `/MacRomanEncoding` (Annex D.5).
    MacRoman,
    /// `/MacExpertEncoding` (Annex D.3).
    MacExpert,
    /// The font's built-in encoding — a Type1 font's own code, or a TrueType
    /// font's cmap. Only `/Differences` entries map codes; everything else
    /// falls to the font program (via [`crate::cmap`]).
    Builtin,
}

impl BaseEncoding {
    /// The predefined table for this base, if any.
    fn table(self) -> Option<&'static [&'static str; 256]> {
        match self {
            BaseEncoding::Standard => Some(&tables::StandardEncoding),
            BaseEncoding::WinAnsi => Some(&tables::WinAnsiEncoding),
            BaseEncoding::MacRoman => Some(&tables::MacRomanEncoding),
            BaseEncoding::MacExpert => Some(&tables::MacExpertEncoding),
            BaseEncoding::Builtin => None,
        }
    }

    /// Whether this is a named predefined encoding (not the built-in).
    #[must_use]
    pub fn is_predefined(self) -> bool {
        self.table().is_some()
    }
}

/// The `/Encoding` entry as it appears in a font dictionary (§9.2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontEncoding {
    /// `/Encoding /WinAnsiEncoding` — a predefined name.
    Named(BaseEncoding),
    /// `/Encoding << /BaseEncoding ... /Differences [...] >>`.
    ///
    /// `/BaseEncoding` is `None` when absent — the differences are then
    /// applied over the font's built-in encoding (§9.6.6.4).
    Dict {
        /// `/BaseEncoding`, if present.
        base: Option<BaseEncoding>,
        /// The `/Differences` array as resolved (code → glyph name).
        differences: Vec<(u8, String)>,
    },
    /// No `/Encoding` entry — the font's built-in encoding.
    Absent,
}

impl FontEncoding {
    /// Whether any `/Differences` overrides are present.
    #[must_use]
    pub fn has_differences(&self) -> bool {
        matches!(self, FontEncoding::Dict { differences, .. } if !differences.is_empty())
    }
}

/// A resolved encoding: a base table plus `/Differences` overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoding {
    base: BaseEncoding,
    /// code → glyph name, applied over the base table.
    differences: BTreeMap<u8, String>,
}

impl Encoding {
    /// A base encoding with no differences.
    #[must_use]
    pub fn base(base: BaseEncoding) -> Self {
        Self {
            base,
            differences: BTreeMap::new(),
        }
    }

    /// A base encoding plus pre-expanded `/Differences` entries.
    ///
    /// `differences` is the resolved `(code, glyph-name)` list; use
    /// [`expand_differences`] to build it from a raw `/Differences` array.
    #[must_use]
    pub fn with_differences(base: BaseEncoding, differences: &[(u8, &str)]) -> Self {
        let mut map = BTreeMap::new();
        for (code, name) in differences {
            map.insert(*code, (*name).to_string());
        }
        Self {
            base,
            differences: map,
        }
    }

    /// The base encoding.
    #[must_use]
    pub fn base_encoding(&self) -> BaseEncoding {
        self.base
    }

    /// The glyph name for a code, or `None` when the code is unassigned
    /// (`.notdef`) or has no entry in a built-in encoding.
    #[must_use]
    pub fn code_to_glyph(&self, code: u8) -> Option<&str> {
        if let Some(name) = self.differences.get(&code) {
            return Some(name);
        }
        let table = self.base.table()?;
        let name = table.get(code as usize).copied().unwrap_or(".notdef");
        if name == ".notdef" {
            None
        } else {
            Some(name)
        }
    }
}

/// Resolve a font's `/Encoding` entry to a [`Encoding`].
///
/// Precedence (§9.2.4, §9.6.6.4): a named `/Encoding`; a dictionary's
/// `/BaseEncoding` plus `/Differences`; a dictionary with no `/BaseEncoding`
/// differs over the built-in encoding; no `/Encoding` is the built-in
/// encoding.
#[must_use]
pub fn resolve(fe: &FontEncoding) -> Encoding {
    match fe {
        FontEncoding::Named(base) => Encoding::base(*base),
        FontEncoding::Dict { base, differences } => {
            let base = base.unwrap_or(BaseEncoding::Builtin);
            let pairs: Vec<(u8, &str)> =
                differences.iter().map(|(c, n)| (*c, n.as_str())).collect();
            Encoding::with_differences(base, &pairs)
        }
        FontEncoding::Absent => Encoding::base(BaseEncoding::Builtin),
    }
}

/// One item of a `/Differences` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DifferenceItem {
    /// A code that starts a run of glyph names.
    Code(u32),
    /// A glyph name mapping to the current code, which then increments.
    Name(String),
}

/// Expand a raw `/Differences` array into `(code, glyph name)` pairs.
///
/// The array structure (§9.2.4.3): a code starts a run; each following name
/// maps to the current code and increments it. A second code starts a new run.
/// Codes beyond 255 and duplicate codes are dropped.
#[must_use]
pub fn expand_differences(items: &[DifferenceItem]) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    let mut current: Option<u8> = None;
    for item in items {
        match item {
            DifferenceItem::Code(c) => current = u8::try_from(*c).ok(),
            DifferenceItem::Name(n) => {
                if let Some(code) = current {
                    out.push((code, n.clone()));
                    current = code.checked_add(1);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winansi_spot_checks() {
        let e = Encoding::base(BaseEncoding::WinAnsi);
        assert_eq!(e.code_to_glyph(65), Some("A"));
        assert_eq!(e.code_to_glyph(0x80), Some("Euro"));
        assert_eq!(e.code_to_glyph(0x85), Some("ellipsis"));
        assert_eq!(e.code_to_glyph(0x95), Some("bullet"));
        assert_eq!(e.code_to_glyph(0xE9), Some("eacute"));
        // 0x81, 0x8D, 0x9D are unassigned in WinAnsi (normalised from pdf.js's
        // placeholder bullets, Annex D.4).
        assert_eq!(e.code_to_glyph(0x81), None);
        assert_eq!(e.code_to_glyph(0x9D), None);
    }

    #[test]
    fn standard_encoding_spot_checks() {
        let e = Encoding::base(BaseEncoding::Standard);
        assert_eq!(e.code_to_glyph(39), Some("quoteright"));
        assert_eq!(e.code_to_glyph(96), Some("quoteleft"));
        assert_eq!(e.code_to_glyph(0xA1), Some("exclamdown"));
        assert_eq!(e.code_to_glyph(0xE1), Some("AE"));
    }

    #[test]
    fn macroman_spot_checks() {
        let e = Encoding::base(BaseEncoding::MacRoman);
        assert_eq!(e.code_to_glyph(128), Some("Adieresis"));
        assert_eq!(e.code_to_glyph(0xCA), Some("space"));
        assert_eq!(e.code_to_glyph(0xFE), Some("ogonek"));
        assert_eq!(e.code_to_glyph(0xFF), Some("caron"));
    }

    #[test]
    fn macexpert_spot_checks() {
        let e = Encoding::base(BaseEncoding::MacExpert);
        assert_eq!(e.code_to_glyph(32), Some("space"));
        assert_eq!(e.code_to_glyph(35), Some("centoldstyle"));
        assert_eq!(e.code_to_glyph(0x81), Some("asuperior"));
        assert_eq!(e.code_to_glyph(0xFF), None);
    }

    #[test]
    fn differences_override_the_base() {
        let e = Encoding::with_differences(BaseEncoding::WinAnsi, &[(65, "B"), (0xE9, "agrave")]);
        assert_eq!(e.code_to_glyph(65), Some("B")); // overridden
        assert_eq!(e.code_to_glyph(66), Some("B")); // base unchanged
        assert_eq!(e.code_to_glyph(0xE9), Some("agrave")); // overridden
        assert_eq!(e.code_to_glyph(0x80), Some("Euro")); // base unchanged
    }

    #[test]
    fn differences_run_increments_codes() {
        // [65 /X /Y 70 /Z] → 65=X, 66=Y, 70=Z.
        let pairs = expand_differences(&[
            DifferenceItem::Code(65),
            DifferenceItem::Name("X".to_string()),
            DifferenceItem::Name("Y".to_string()),
            DifferenceItem::Code(70),
            DifferenceItem::Name("Z".to_string()),
        ]);
        assert_eq!(
            pairs,
            vec![
                (65, "X".to_string()),
                (66, "Y".to_string()),
                (70, "Z".to_string()),
            ]
        );
    }

    #[test]
    fn differences_codes_above_255_are_dropped() {
        let pairs = expand_differences(&[
            DifferenceItem::Code(300),
            DifferenceItem::Name("X".to_string()),
            DifferenceItem::Code(65),
            DifferenceItem::Name("Y".to_string()),
        ]);
        assert_eq!(pairs, vec![(65, "Y".to_string())]);
    }

    #[test]
    fn builtin_encoding_maps_only_differences() {
        let e = resolve(&FontEncoding::Absent);
        assert_eq!(e.code_to_glyph(65), None);
        assert_eq!(e.base_encoding(), BaseEncoding::Builtin);
        let with_diff = resolve(&FontEncoding::Dict {
            base: None,
            differences: vec![(65, "A".to_string())],
        });
        assert_eq!(with_diff.code_to_glyph(65), Some("A"));
        assert_eq!(with_diff.code_to_glyph(66), None);
    }

    #[test]
    fn dict_without_base_differs_over_builtin() {
        let e = resolve(&FontEncoding::Dict {
            base: None,
            differences: vec![(32, "space".to_string())],
        });
        assert_eq!(e.base_encoding(), BaseEncoding::Builtin);
        assert_eq!(e.code_to_glyph(32), Some("space"));
    }

    #[test]
    fn named_encoding_resolves_directly() {
        let e = resolve(&FontEncoding::Named(BaseEncoding::WinAnsi));
        assert_eq!(e.code_to_glyph(0xE9), Some("eacute"));
    }
}
