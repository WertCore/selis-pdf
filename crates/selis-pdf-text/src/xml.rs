//! XML text escaping shared by the OOXML package writers (Phase 3).
//!
//! The writers emit XML by string concatenation rather than through a DOM: the
//! package parts are small and fixed, and a tree would add a dependency and hide
//! which characters can reach the output. This module is the one place that
//! decides what leaves the engine as character data, so an escape fixed once is
//! fixed for Word, PowerPoint and Excel together.

/// Escape text for XML character data and replace characters XML 1.0 forbids.
#[must_use]
pub(crate) fn xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(ch),
            ch if (ch as u32) < 0x20 => out.push('\u{FFFD}'),
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn markup_is_escaped_and_controls_become_replacement_characters() {
        assert_eq!(
            xml_text("a < b & c \"d\" 'e'"),
            "a &lt; b &amp; c &quot;d&quot; &apos;e&apos;"
        );
        assert_eq!(xml_text("a\u{0}b"), "a\u{FFFD}b");
    }
}
