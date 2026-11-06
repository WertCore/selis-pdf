//! Extraction quality: what a text export actually recovered, per page.
//!
//! A plain-text export that silently drops a page reads exactly like a page
//! that was blank, so the ladder reports its own confidence instead of asking
//! the reader to trust an empty string. The report is structural — characters,
//! lines, pages with text, pages that drew glyphs and recovered nothing — and
//! is exported as JSON so a CLI, a browser card and the desktop app cannot
//! disagree about what "low confidence" means.
//!
//! Page separators live here too, because the separator is part of the export
//! shape rather than a caller's decoration: the same page must be marked the
//! same way in every surface, and a separator that a page's own text could
//! forge (a bare line of dashes) would be indistinguishable from content.

use std::fmt::Write as _;

/// One page's extraction facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageQuality {
    /// The 1-based page number this record describes.
    pub number: u32,
    /// Recovered Unicode scalars on the page.
    pub characters: u64,
    /// Assembled lines on the page.
    pub lines: u64,
    /// Whether the display list drew glyphs (or drew none at all).
    pub drew_text: bool,
    /// SL-3.TEXT.10: text was drawn and nothing was recovered.
    pub low_confidence: bool,
}

impl PageQuality {
    /// One page's record, counted from its recovered line texts.
    ///
    /// `drew_text` is the display list's answer, not the text's: a page that
    /// drew no glyphs is blank, and a page that drew glyphs but recovered no
    /// characters is low confidence. Those are different facts and the record
    /// keeps them apart.
    #[must_use]
    pub fn from_lines(number: u32, line_texts: &[String], drew_text: bool) -> Self {
        let characters = line_texts
            .iter()
            .map(|text| u64::try_from(text.chars().count()).unwrap_or(u64::MAX))
            .fold(0u64, u64::saturating_add);
        let lines = u64::try_from(line_texts.len()).unwrap_or(u64::MAX);
        let recovered = line_texts.iter().any(|text| !text.trim().is_empty());
        Self {
            number,
            characters,
            lines,
            drew_text,
            low_confidence: drew_text && !recovered,
        }
    }

    /// Whether this page carries no recovered character at all.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.characters == 0
    }
}

/// The quality report of one export.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtractionQuality {
    /// How many pages the export read.
    pub pages: u64,
    /// Recovered characters across the export.
    pub characters: u64,
    /// Assembled lines across the export.
    pub lines: u64,
    /// Pages that recovered no character.
    pub blank_pages: u64,
    /// Pages that recovered at least one character.
    pub pages_with_text: u64,
    /// Pages that drew glyphs and recovered nothing (SL-3.TEXT.10).
    pub low_confidence_pages: u64,
    /// One record per page, in document order.
    pub per_page: Vec<PageQuality>,
}

impl ExtractionQuality {
    /// Aggregate the per-page records into one report.
    ///
    /// Every total is computed from the records rather than passed in, so a
    /// summary line can never disagree with the rows it summarises.
    #[must_use]
    #[allow(clippy::field_reassign_with_default)]
    pub fn from_pages(per_page: Vec<PageQuality>) -> Self {
        let mut report = Self::default();
        report.pages = u64::try_from(per_page.len()).unwrap_or(u64::MAX);
        for page in &per_page {
            report.characters = report.characters.saturating_add(page.characters);
            report.lines = report.lines.saturating_add(page.lines);
            if page.is_blank() {
                report.blank_pages = report.blank_pages.saturating_add(1);
            } else {
                report.pages_with_text = report.pages_with_text.saturating_add(1);
            }
            if page.low_confidence {
                report.low_confidence_pages = report.low_confidence_pages.saturating_add(1);
            }
        }
        report.per_page = per_page;
        report
    }

    /// The verdict a reader acts on.
    ///
    /// `ok` when the export recovered text and no page was lost to an
    /// undecodable font; `low-confidence` when at least one page drew text the
    /// extractor could not read; `empty` when nothing was recovered at all.
    #[must_use]
    pub fn verdict(&self) -> &'static str {
        if self.low_confidence_pages > 0 {
            "low-confidence"
        } else if self.characters == 0 {
            "empty"
        } else {
            "ok"
        }
    }

    /// The report as JSON (`selis-extract-quality/1`).
    ///
    /// Hand-written like the extraction JSON so the crate keeps no serializer
    /// dependency on this path, and so the schema is visible in one place.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n  \"schema\": \"selis-extract-quality/1\",\n");
        let _ = writeln!(out, "  \"verdict\": {},", json_str(self.verdict()));
        let _ = writeln!(out, "  \"pages\": {},", self.pages);
        let _ = writeln!(out, "  \"characters\": {},", self.characters);
        let _ = writeln!(out, "  \"lines\": {},", self.lines);
        let _ = writeln!(out, "  \"blank_pages\": {},", self.blank_pages);
        let _ = writeln!(out, "  \"pages_with_text\": {},", self.pages_with_text);
        let _ = writeln!(
            out,
            "  \"low_confidence_pages\": {},",
            self.low_confidence_pages
        );
        out.push_str("  \"per_page\": [\n");
        for (index, page) in self.per_page.iter().enumerate() {
            let _ = write!(
                out,
                "    {{\"page\": {}, \"characters\": {}, \"lines\": {}, \
                 \"drew_text\": {}, \"low_confidence\": {}}}",
                page.number, page.characters, page.lines, page.drew_text, page.low_confidence
            );
            if index.saturating_add(1) < self.per_page.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n}\n");
        out
    }
}

/// The separator line that opens one exported page.
///
/// The shape is fixed here rather than assembled by a caller: it names the
/// page and the total, so a reader of the plain-text file can find page 7
/// without counting blank lines, and no page's own text can be mistaken for it
/// because the marker carries the document's page count as well as its number.
#[must_use]
pub fn page_separator(number: u32, total: u32) -> String {
    format!("--- page {number} of {total} ---")
}

/// One document's worth of plain text, with a separator before every page.
///
/// Pages appear in document order, and the separator always carries the page's
/// true number — a range `2-3` starts at `--- page 2 of 3 ---`, never at
/// `--- page 1 of 3 ---`. An empty selection is an empty string, not a
/// dangling separator.
#[must_use]
pub fn to_text_with_page_separators(pages: &[(u32, String)]) -> String {
    let mut out = String::new();
    let total = u32::try_from(pages.len()).unwrap_or(u32::MAX);
    for (index, (number, text)) in pages.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&page_separator(*number, total));
        out.push('\n');
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Escape a string for a JSON string literal.
fn json_str(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch < ' ' => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn totals_are_counted_from_the_rows_not_passed_in() {
        let report = ExtractionQuality::from_pages(vec![
            PageQuality::from_lines(1, &["Hello".to_string(), "World".to_string()], true),
            PageQuality::from_lines(2, &[], false),
            PageQuality::from_lines(3, &["".to_string()], true),
        ]);
        assert_eq!(report.pages, 3);
        assert_eq!(report.characters, 10);
        assert_eq!(report.lines, 3);
        assert_eq!(report.blank_pages, 2);
        assert_eq!(report.pages_with_text, 1);
        assert_eq!(report.low_confidence_pages, 1);
        assert_eq!(report.verdict(), "low-confidence");
    }

    #[test]
    fn a_blank_page_is_not_a_low_confidence_page() {
        let blank = PageQuality::from_lines(1, &[], false);
        assert!(!blank.low_confidence);
        assert!(blank.is_blank());
        let undecodable = PageQuality::from_lines(1, &[], true);
        assert!(undecodable.low_confidence);
        assert!(undecodable.is_blank());
    }

    #[test]
    fn the_json_is_versioned_and_carries_every_row() {
        let report = ExtractionQuality::from_pages(vec![PageQuality::from_lines(
            2,
            &["Two".to_string()],
            true,
        )]);
        let json = report.to_json();
        assert!(json.contains("\"schema\": \"selis-extract-quality/1\""));
        assert!(json.contains("\"verdict\": \"ok\""));
        assert!(json.contains("\"page\": 2"));
        assert!(json.contains("\"characters\": 3"));
        assert!(json.contains("\"low_confidence\": false"));
    }

    #[test]
    fn an_empty_export_says_so() {
        assert_eq!(ExtractionQuality::default().verdict(), "empty");
    }

    #[test]
    fn separators_name_the_pages_true_numbers() {
        let text =
            to_text_with_page_separators(&[(2, "second".to_string()), (3, "third".to_string())]);
        assert_eq!(
            text,
            "--- page 2 of 2 ---\nsecond\n\n--- page 3 of 2 ---\nthird\n"
        );
    }

    #[test]
    fn an_empty_selection_writes_no_separator() {
        assert_eq!(to_text_with_page_separators(&[]), "");
    }
}
