//! Character-to-Unicode text recovery (SL-3.TEXT.02).
//!
//! Recovers the original text of a PDF: a character code is mapped to Unicode
//! through the fallback chain (ISO 32000-2 §9.10.2), with a per-run
//! confidence:
//!
//! 1. the `/ToUnicode` CMap (`beginbfchar`/`beginbfrange`);
//! 2. the encoding's glyph name → the Adobe Glyph List;
//! 3. the `uniXXXX`/`uXXXX` glyph-name conventions;
//! 4. the font's own cmap reverse map (the character that maps to the glyph).
//!
//! The caller supplies the lookups (the font crate is COS-free); the chain and
//! confidence are what this module owns.

use crate::agl::glyph_to_unicode;
use crate::cmapfile::CMap;
use crate::encoding::Encoding;

/// The confidence of a recovered mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryConfidence {
    /// The `/ToUnicode` CMap mapped the code directly.
    ToUnicode,
    /// The encoding's glyph name resolved via the Adobe Glyph List.
    GlyphName,
    /// A `uniXXXX`/`uXXXX` glyph-name convention.
    UniName,
    /// The font's own cmap reverse map.
    CmapReverse,
    /// No mapping could be recovered.
    None,
}

/// A recovered Unicode value and its confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recovery {
    /// The Unicode scalar value, if recovered.
    pub unicode: Option<u32>,
    /// The confidence of the mapping.
    pub confidence: RecoveryConfidence,
}

/// The lookups a recovery needs (the caller resolves the document).
pub struct TextRecovery<'a> {
    /// The `/ToUnicode` CMap, if any.
    pub to_unicode: Option<&'a CMap>,
    /// The resolved encoding (for glyph-name recovery).
    pub encoding: Option<&'a Encoding>,
    /// The glyph name for a glyph id (from the font's `post` table).
    pub glyph_name: &'a dyn Fn(u16) -> Option<String>,
    /// A cmap reverse lookup: glyph id → the first character code that maps
    /// to it.
    pub cmap_reverse: &'a dyn Fn(u16) -> Option<u32>,
}

impl TextRecovery<'_> {
    /// Recover the Unicode for a character code and its glyph.
    #[must_use]
    pub fn unicode(&self, code: u32, glyph_id: u16) -> Recovery {
        // 1. The /ToUnicode CMap.
        if let Some(cmap) = self.to_unicode {
            if let Some(uni) = cmap.unicode_map(code) {
                return Recovery {
                    unicode: Some(uni),
                    confidence: RecoveryConfidence::ToUnicode,
                };
            }
        }
        // 2. The encoding's glyph name → the AGL.
        if let Some(encoding) = self.encoding {
            let code_byte = u8::try_from(code).unwrap_or(0);
            if let Some(name) = encoding.code_to_glyph(code_byte) {
                if let Some(uni) = glyph_to_unicode(name) {
                    return Recovery {
                        unicode: Some(uni),
                        confidence: RecoveryConfidence::GlyphName,
                    };
                }
                if let Some(uni) = uni_name_unicode(name) {
                    return Recovery {
                        unicode: Some(uni),
                        confidence: RecoveryConfidence::UniName,
                    };
                }
            }
        }
        // 3. The glyph's post name → the `uniXXXX`/`uXXXX` conventions.
        if let Some(name) = (self.glyph_name)(glyph_id) {
            if let Some(uni) = uni_name_unicode(&name) {
                return Recovery {
                    unicode: Some(uni),
                    confidence: RecoveryConfidence::UniName,
                };
            }
        }
        // 4. The font's own cmap reverse map.
        if let Some(uni) = (self.cmap_reverse)(glyph_id) {
            return Recovery {
                unicode: Some(uni),
                confidence: RecoveryConfidence::CmapReverse,
            };
        }
        Recovery {
            unicode: None,
            confidence: RecoveryConfidence::None,
        }
    }
}

/// Parse the `uniXXXX` (4–6 hex digits) and `uXXXX` conventions.
#[must_use]
fn uni_name_unicode(name: &str) -> Option<u32> {
    let hex = if let Some(rest) = name.strip_prefix("uni") {
        rest
    } else if let Some(rest) = name.strip_prefix('u') {
        rest
    } else {
        return None;
    };
    if hex.is_empty() || hex.len() > 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;
    use crate::encoding::{BaseEncoding, Encoding};

    fn empty_recovery() -> TextRecovery<'static> {
        TextRecovery {
            to_unicode: None,
            encoding: None,
            glyph_name: &|_gid| None,
            cmap_reverse: &|_gid| None,
        }
    }

    fn noop_cmap() -> Option<&'static CMap> {
        None
    }

    #[test]
    fn to_unicode_wins() {
        let mut cmap = CMap::default();
        cmap.bf_chars.push((0x41, 0x41));
        let r = TextRecovery {
            to_unicode: Some(&cmap),
            encoding: None,
            glyph_name: &|_gid| Some("A".to_string()),
            cmap_reverse: &|_gid| None,
        };
        let rec = r.unicode(0x41, 0);
        assert_eq!(rec.unicode, Some(0x41));
        assert_eq!(rec.confidence, RecoveryConfidence::ToUnicode);
    }

    #[test]
    fn glyph_name_resolves_via_agl() {
        let r = TextRecovery {
            to_unicode: noop_cmap(),
            encoding: Some(&Encoding::base(BaseEncoding::WinAnsi)),
            glyph_name: &|_gid| None,
            cmap_reverse: &|_gid| None,
        };
        // WinAnsi code 65 = 'A' → glyph name "A" → AGL U+0041.
        let rec = r.unicode(65, 0);
        assert_eq!(rec.unicode, Some(0x41));
        assert_eq!(rec.confidence, RecoveryConfidence::GlyphName);
    }

    #[test]
    fn uni_name_convention_parses() {
        assert_eq!(uni_name_unicode("uni0041"), Some(0x41));
        assert_eq!(uni_name_unicode("uni4E8C"), Some(0x4E8C));
        assert_eq!(uni_name_unicode("u0041"), Some(0x41));
        assert_eq!(uni_name_unicode("u1F600"), Some(0x1F600));
        assert_eq!(uni_name_unicode("uniXYZ"), None);
        assert_eq!(uni_name_unicode("A"), None);
    }

    #[test]
    fn post_name_uni_convention_is_used() {
        let r = TextRecovery {
            to_unicode: noop_cmap(),
            encoding: None,
            glyph_name: &|_gid| Some("uni4E8C".to_string()),
            cmap_reverse: &|_gid| None,
        };
        let rec = r.unicode(0, 7);
        assert_eq!(rec.unicode, Some(0x4E8C));
        assert_eq!(rec.confidence, RecoveryConfidence::UniName);
    }

    #[test]
    fn cmap_reverse_is_last_resort() {
        let r = TextRecovery {
            to_unicode: noop_cmap(),
            encoding: None,
            glyph_name: &|_gid| None,
            cmap_reverse: &|gid| if gid == 3 { Some(0x41) } else { None },
        };
        let rec = r.unicode(0, 3);
        assert_eq!(rec.unicode, Some(0x41));
        assert_eq!(rec.confidence, RecoveryConfidence::CmapReverse);
    }

    #[test]
    fn nothing_found_is_none() {
        let r = empty_recovery();
        let rec = r.unicode(0xFF, 9);
        assert_eq!(rec.unicode, None);
        assert_eq!(rec.confidence, RecoveryConfidence::None);
    }
}
