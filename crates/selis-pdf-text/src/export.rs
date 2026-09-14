//! Structured extraction output (SL-3.TEXT.07).
//!
//! Emits the assembled, ordered lines as plain text, a structured JSON
//! document (blocks/lines/spans with style and bbox), Markdown, or HTML. The
//! foundation for the convert product and for AI/RAG integration.
//!
//! The JSON schema is versioned as `selis-extract/1`.

use selis_geom::Rect;

use crate::assembly::{TextLine, TextRun, TextWord};

/// What a consumer must see when a page's display list **drew text** yet the
/// extractor recovered no characters (SL-3.TEXT.10): a visible low-confidence
/// marker, never a silent empty string that masquerades as a blank page. Every
/// format carries it as one more line of the extraction, so the marker reaches
/// `text`, Markdown and HTML through the line model, and JSON through both the
/// line and [`Structured::low_confidence`].
pub const LOW_CONFIDENCE_MARKER: &str = "[low-confidence: text drawn, nothing recovered]";

/// A style span within a line.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    /// The recovered text of the span.
    pub text: String,
    /// The font resource name.
    pub font: String,
    /// The font size.
    pub size: f64,
    /// The span's bounding box.
    pub bbox: Rect,
}

/// The structured model of one line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// The style spans.
    pub spans: Vec<Span>,
    /// The line's bounding box.
    pub bbox: Rect,
}

/// The structured model of a document.
#[derive(Debug, Clone, PartialEq)]
pub struct Structured {
    /// The lines, in reading order.
    pub lines: Vec<Line>,
    /// SL-3.TEXT.10: the page's display list drew glyphs but the extractor
    /// recovered **no characters** (every code was dropped rather than
    /// decoded — typically a font whose codes resolve to nothing). Set so the
    /// consumer can surface a low-confidence marker instead of treating the
    /// empty output as a genuinely blank page; never silently returns empty
    /// when text was actually drawn.
    pub low_confidence: bool,
}

/// Build the structured model from assembled lines and their recovered text.
///
/// `line_texts[i]` is the Unicode text of `lines[i]`; a line's spans are its
/// runs (grouped by style) with the recovered text split proportionally is
/// not possible without a per-glyph mapping, so each run is one span whose
/// text is the concatenation of its glyphs' recovered characters. The caller
/// provides the per-run text via `run_texts` (parallel to the line's runs).
///
/// When `run_texts` is omitted, each span is the line's text repeated (the
/// caller should provide real run-level text).
#[must_use]
pub fn structured(
    lines: &[TextLine],
    line_texts: &[String],
    run_texts: &[Vec<String>],
) -> Structured {
    let mut out = Vec::new();
    let n = lines.len().min(line_texts.len());
    for i in 0..n {
        let Some(line) = lines.get(i) else {
            continue;
        };
        let runs: Vec<TextRun> = line
            .words
            .iter()
            .flat_map(|w: &TextWord| w.runs.iter().cloned())
            .collect();
        let line_text = line_texts.get(i).cloned().unwrap_or_default();
        let mut spans = Vec::new();
        for (ri, run) in runs.iter().enumerate() {
            let text = run_texts
                .get(i)
                .and_then(|rts| rts.get(ri))
                .cloned()
                .unwrap_or_else(|| line_text.clone());
            spans.push(Span {
                text,
                font: run
                    .glyphs
                    .first()
                    .map(|g| String::from_utf8_lossy(g.font.as_slice()).to_string())
                    .unwrap_or_default(),
                size: run.glyphs.first().map(|g| g.size).unwrap_or(0.0),
                bbox: run.bbox,
            });
        }
        out.push(Line {
            spans,
            bbox: line.bbox,
        });
    }
    Structured {
        lines: out,
        low_confidence: false,
    }
}

/// Plain text: one line per newline.
///
/// `low_confidence` (SL-3.TEXT.10) appends the marker line when the page drew
/// text the extractor could not recover, so a silent empty string is never
/// confused with a genuinely blank page.
#[must_use]
pub fn to_text(lines: &[TextLine], line_texts: &[String], low_confidence: bool) -> String {
    let mut out = String::new();
    let n = lines.len().min(line_texts.len());
    for i in 0..n {
        if i > 0 {
            out.push('\n');
        }
        let text = line_texts.get(i).cloned().unwrap_or_default();
        out.push_str(&text);
    }
    if low_confidence {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(LOW_CONFIDENCE_MARKER);
    }
    out
}

/// Structured JSON (schema `selis-extract/1`).
#[must_use]
pub fn to_json(s: &Structured) -> String {
    let mut out = String::from("{\n  \"schema\": \"selis-extract/1\",\n  \"low_confidence\": ");
    out.push_str(if s.low_confidence { "true" } else { "false" });
    out.push_str(",\n  \"lines\": [\n");
    let n = s.lines.len();
    for (i, line) in s.lines.iter().enumerate() {
        out.push_str("    {\n      \"bbox\": ");
        out.push_str(&bbox_json(line.bbox));
        out.push_str(",\n      \"spans\": [\n");
        let m = line.spans.len();
        for (j, span) in line.spans.iter().enumerate() {
            out.push_str("        {\n          \"text\": ");
            out.push_str(&json_escape(&span.text));
            out.push_str(",\n          \"font\": ");
            out.push_str(&json_escape(&span.font));
            out.push_str(",\n          \"size\": ");
            out.push_str(&fmt_num(span.size));
            out.push_str(",\n          \"bbox\": ");
            out.push_str(&bbox_json(span.bbox));
            out.push_str("\n        }");
            if j < m.saturating_sub(1) {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("      ]\n    }");
        if i < n.saturating_sub(1) {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

/// Markdown: each line is a paragraph.
#[must_use]
pub fn to_markdown(lines: &[TextLine], line_texts: &[String], low_confidence: bool) -> String {
    let text = to_text(lines, line_texts, low_confidence);
    let mut out = String::new();
    for para in text.split('\n') {
        let trimmed = para.trim();
        if !trimmed.is_empty() {
            out.push_str(trimmed);
            out.push_str("\n\n");
        }
    }
    out
}

/// HTML: each line is a `<p>` with the bbox as a data attribute.
#[must_use]
pub fn to_html(lines: &[TextLine], line_texts: &[String], low_confidence: bool) -> String {
    let mut out = String::from("<html><body>\n");
    let n = lines.len().min(line_texts.len());
    for i in 0..n {
        let raw = line_texts.get(i).cloned().unwrap_or_default();
        let text = html_escape(&raw);
        let bbox = lines
            .get(i)
            .map(|l| {
                format!(
                    "{:.2},{:.2},{:.2},{:.2}",
                    l.bbox.x0, l.bbox.y0, l.bbox.x1, l.bbox.y1
                )
            })
            .unwrap_or_default();
        out.push_str(&format!("  <p data-bbox=\"{bbox}\">{text}</p>\n"));
    }
    if low_confidence {
        out.push_str("  <p class=\"low-confidence\">");
        out.push_str(&html_escape(LOW_CONFIDENCE_MARKER));
        out.push_str("</p>\n");
    }
    out.push_str("</body></html>\n");
    out
}

fn bbox_json(r: Rect) -> String {
    format!("[{:.2}, {:.2}, {:.2}, {:.2}]", r.x0, r.y0, r.x1, r.y1)
}

fn fmt_num(v: f64) -> String {
    format!("{v:.2}")
}

/// Escape a string for a JSON string literal.
fn json_escape(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escape for an HTML text node.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_bytes::Bytes;
    use selis_geom::Point;
    use selis_pdf_content::text::TextGlyph;

    fn line_at(glyphs: Vec<TextGlyph>) -> TextLine {
        let run = TextRun {
            glyphs,
            bbox: selis_geom::Rect::new(0.0, 0.0, 50.0, 12.0),
        };
        let word = TextWord {
            runs: vec![run],
            bbox: selis_geom::Rect::new(0.0, 0.0, 50.0, 12.0),
        };
        TextLine {
            words: vec![word],
            bbox: selis_geom::Rect::new(0.0, 0.0, 50.0, 12.0),
        }
    }

    fn glyph(code: u16, font: &str, size: f64) -> TextGlyph {
        TextGlyph {
            advance: size * 0.5,
            code,
            at: Point::new(0.0, 0.0),
            font: Bytes::copy_from_slice(font.as_bytes()),
            size,
            space: size * 0.25,
            mcid: None,
        }
    }

    #[test]
    fn plain_text_joins_lines() {
        let lines = vec![
            line_at(vec![glyph(65, "F1", 12.0)]),
            line_at(vec![glyph(66, "F1", 12.0)]),
        ];
        let text = to_text(&lines, &["Hello".to_string(), "World".to_string()], false);
        assert_eq!(text, "Hello\nWorld");
    }

    #[test]
    fn json_is_versioned_and_has_spans() {
        let lines = vec![line_at(vec![glyph(65, "F1", 12.0)])];
        let s = structured(&lines, &["Hello".to_string()], &[vec!["Hello".to_string()]]);
        let json = to_json(&s);
        assert!(json.contains("\"schema\": \"selis-extract/1\""));
        assert!(json.contains("\"text\": \"Hello\""));
        assert!(json.contains("\"font\": \"F1\""));
        assert!(json.contains("\"size\": 12.00"));
    }

    #[test]
    fn markdown_and_html_emit_lines() {
        let lines = vec![line_at(vec![glyph(65, "F1", 12.0)])];
        let texts = vec!["Hello & bye".to_string()];
        let md = to_markdown(&lines, &texts, false);
        assert!(md.contains("Hello & bye"));
        let html = to_html(&lines, &texts, false);
        assert!(html.contains("<p"));
        assert!(html.contains("Hello &amp; bye"));
    }

    #[test]
    fn json_escapes_special_chars() {
        assert_eq!(json_escape("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_escape("a\nb"), "\"a\\nb\"");
    }

    /// SL-3.TEXT.08 DoD: every format must emit decoded Unicode (UTF-8) for
    /// e-acute (U+00E9), the ring/degree sign (U+00B0), and CJK (U+65E5)
    /// pushed through text/json/md/html — never the PDF-string octal digits.
    #[test]
    fn all_formats_emit_unicode_not_octal_escapes() {
        // Codes are already the recovered scalars (TEXT.02 ran their ToUnicode /
        // encoding chain); the formatter must render them as UTF-8, not "é".
        let lines = vec![line_at(vec![glyph(0xE9, "F1", 12.0)])];
        let texts = vec!["\u{E9}\u{B0}\u{65E5}".to_string()];
        let run_texts = vec![vec!["\u{E9}\u{B0}\u{65E5}".to_string()]];

        let text = to_text(&lines, &texts, false);
        assert_eq!(text, "\u{E9}\u{B0}\u{65E5}");
        assert!(
            !text.contains('e'),
            "plain text carries no backslash escapes"
        );

        let s = structured(&lines, &texts, &run_texts);
        let json = to_json(&s);
        assert!(json.contains("\"text\": \"\u{E9}\u{B0}\u{65E5}\""));
        assert!(
            !json.contains("\\u00e9"),
            "JSON holds literal UTF-8, not escapes"
        );

        let md = to_markdown(&lines, &texts, false);
        assert!(md.contains("\u{E9}\u{B0}\u{65E5}"));

        let html = to_html(&lines, &texts, false);
        // HTML must not escape the non-ASCII bytes into entities; the raw
        // characters pass through (only markup chars are escaped).
        assert!(html.contains("\u{E9}\u{B0}\u{65E5}"));
    }

    /// A non-ASCII character survives JSON escaping intact.
    #[test]
    fn json_keeps_non_ascii_verbatim() {
        assert_eq!(json_escape("\u{65E5}\u{E9}"), "\"\u{65E5}\u{E9}\"");
    }

    /// SL-3.TEXT.10: the JSON schema records a low-confidence flag so a page
    /// that drew text but recovered nothing is distinguishable from a blank
    /// one on the structural path too.
    #[test]
    fn json_flags_low_confidence() {
        let mut s = structured(&[], &[], &[]);
        assert!(!s.low_confidence);
        assert!(to_json(&s).contains("\"low_confidence\": false"));
        s.low_confidence = true;
        assert!(to_json(&s).contains("\"low_confidence\": true"));
    }

    /// The low-confidence marker rendered as text reads exactly the constant
    /// (the caller appends a line carrying it — asserted end-to-end in the CLI
    /// and WASM workers).
    #[test]
    fn marker_constant_is_stable() {
        assert_eq!(
            LOW_CONFIDENCE_MARKER,
            "[low-confidence: text drawn, nothing recovered]"
        );
    }

    /// SL-3.TEXT.10: text/markdown/html all carry the marker when the flag is
    /// set (and never when it is not) — an empty page is not a low-confidence
    /// page, and a low-confidence page is never silently empty.
    #[test]
    fn low_confidence_marker_in_all_line_formats() {
        let lines: Vec<TextLine> = Vec::new();
        let texts: Vec<String> = Vec::new();
        assert_eq!(to_text(&lines, &texts, false), "");
        assert_eq!(to_text(&lines, &texts, true), LOW_CONFIDENCE_MARKER);
        assert!(to_markdown(&lines, &texts, true).contains(LOW_CONFIDENCE_MARKER));
        let html = to_html(&lines, &texts, true);
        assert!(html.contains(LOW_CONFIDENCE_MARKER));
        assert!(!to_html(&lines, &texts, false).contains(LOW_CONFIDENCE_MARKER));
        // With real content the marker rides alongside, not instead of it.
        let with_text = vec![line_at(vec![glyph(65, "F1", 12.0)])];
        let text = to_text(&with_text, &["A".to_string()], true);
        assert_eq!(text, format!("A\n{LOW_CONFIDENCE_MARKER}"));
    }
}
