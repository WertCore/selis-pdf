//! Search (SL-3.TEXT.06).
//!
//! Normalised search over the recovered text: case folding, diacritics
//! decomposition (NFD + strip combining marks), soft-hyphen removal, and the
//! PDF-specific ligature decomposition (ﬁ → fi, ﬂ → fl, ﬀ → ff, ﬃ → ffi,
//! ﬄ → ffl, ﬅ → ft, ﬆ → st). Small-caps/initial-cap text is searched as its
//! uppercase form via the case folding.
//!
//! Returns the matching ranges with their document positions.

use std::ops::Range;

use selis_geom::Rect;
use unicode_normalization::UnicodeNormalization;

use crate::assembly::TextLine;

/// Normalise text for searching.
///
/// The pipeline: decompose the common PDF ligatures, case-fold (Unicode
/// lowercase), NFD-decompose (splitting base characters from combining
/// diacritics), strip the combining marks, and drop soft hyphens (U+00AD).
#[must_use]
pub fn normalize(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            '\u{00AD}' => None,                    // soft hyphen
            '\u{FB00}' => Some("ff".to_string()),  // ﬀ
            '\u{FB01}' => Some("fi".to_string()),  // ﬁ
            '\u{FB02}' => Some("fl".to_string()),  // ﬂ
            '\u{FB03}' => Some("ffi".to_string()), // ﬃ
            '\u{FB04}' => Some("ffl".to_string()), // ﬄ
            '\u{FB05}' => Some("ft".to_string()),  // ﬅ
            '\u{FB06}' => Some("st".to_string()),  // ﬆ
            c => Some(c.to_string()),
        })
        .collect::<String>()
        .to_lowercase()
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .collect()
}

/// Whether a code point is a Unicode combining mark (Mn, Mc, Me).
fn is_combining_mark(c: char) -> bool {
    let v = c as u32;
    matches!(
        v,
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
    )
}

/// Find all occurrences of `query` in `text` (both normalised), returning the
/// byte ranges in the normalised text.
#[must_use]
pub fn search(text: &str, query: &str) -> Vec<Range<usize>> {
    let hay = normalize(text);
    let needle = normalize(query);
    let mut out = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    let bytes = hay.as_bytes();
    let n = needle.len();
    let mut i = 0usize;
    while i.saturating_add(n) <= bytes.len() {
        if bytes.get(i..i.saturating_add(n)) == Some(&needle.as_bytes()[..]) {
            out.push(i..i.saturating_add(n));
            i = i.saturating_add(n);
        } else {
            i = i.saturating_add(1);
        }
    }
    out
}

/// A search match with its document position.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchMatch {
    /// The line index.
    pub line: usize,
    /// The byte range in the line's normalised text.
    pub range: Range<usize>,
    /// The matched text (the original line text, normalised).
    pub text: String,
    /// The line's bounding rectangle.
    pub rect: Rect,
}

/// Search a set of lines (with their recovered Unicode texts) for a query.
///
/// `line_texts[i]` is the Unicode text of `lines[i]`. Returns the matches
/// with their line index and the line's bounding rect.
#[must_use]
pub fn search_lines(lines: &[TextLine], line_texts: &[String], query: &str) -> Vec<SearchMatch> {
    let mut out = Vec::new();
    let n = lines.len().min(line_texts.len());
    for i in 0..n {
        let text = line_texts.get(i).cloned().unwrap_or_default();
        let ranges = search(&text, query);
        let rect = lines
            .get(i)
            .map(|l| l.bbox)
            .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
        for range in ranges {
            let matched = normalize(&text)
                .get(range.clone())
                .unwrap_or("")
                .to_string();
            out.push(SearchMatch {
                line: i,
                range,
                text: matched,
                rect,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn normalise_case_and_diacritics() {
        assert_eq!(normalize("Café"), "cafe"); // é → e (accent stripped)
        assert_eq!(normalize("STRASSE"), "strasse");
        assert_eq!(normalize("ÄÖÜ"), "aou"); // umlauts decompose to base letters
    }

    #[test]
    fn normalise_ligatures_and_soft_hyphens() {
        assert_eq!(normalize("ﬁle"), "file");
        assert_eq!(normalize("a\u{00AD}b"), "ab"); // soft hyphen
        assert_eq!(normalize("ofﬁce"), "office");
    }

    #[test]
    fn search_finds_case_insensitive_matches() {
        let ranges = search("Hello World", "hello");
        assert_eq!(ranges, vec![0..5]);
    }

    #[test]
    fn search_handles_diacritics_and_ligatures() {
        // "Café" normalises to "cafe" (4 bytes, not 5).
        let ranges = search("Café au ﬁnland", "cafe");
        assert_eq!(ranges, vec![0..4]);
        let ranges = search("Ofﬁce", "office");
        assert_eq!(ranges.len(), 1);
    }

    #[test]
    fn search_returns_all_occurrences() {
        let ranges = search("ab ab ab", "ab");
        assert_eq!(ranges, vec![0..2, 3..5, 6..8]);
    }

    #[test]
    fn no_match_returns_empty() {
        assert!(search("hello", "xyz").is_empty());
    }

    #[test]
    fn search_lines_reports_line_and_rect() {
        let line = TextLine {
            words: Vec::new(),
            bbox: Rect::new(0.0, 0.0, 100.0, 12.0),
        };
        let matches = search_lines(&[line], &["Hello".to_string()], "hello");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[0].rect, Rect::new(0.0, 0.0, 100.0, 12.0));
    }
}
