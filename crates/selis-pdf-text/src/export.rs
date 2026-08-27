//! Structured extraction output (SL-3.TEXT.07).
//!
//! Emits the assembled, ordered lines as plain text, a structured JSON
//! document (blocks/lines/spans with style and bbox), Markdown, or HTML. The
//! foundation for the convert product and for AI/RAG integration.
//!
//! The JSON schema is versioned as `selis-extract/1`.

use selis_geom::Rect;

use crate::assembly::{TextLine, TextRun, TextWord};

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
    Structured { lines: out }
}

/// Plain text: one line per newline.
#[must_use]
pub fn to_text(lines: &[TextLine], line_texts: &[String]) -> String {
    let mut out = String::new();
    let n = lines.len().min(line_texts.len());
    for i in 0..n {
        if i > 0 {
            out.push('\n');
        }
        let text = line_texts.get(i).cloned().unwrap_or_default();
        out.push_str(&text);
    }
    out
}

/// Structured JSON (schema `selis-extract/1`).
#[must_use]
pub fn to_json(s: &Structured) -> String {
    let mut out = String::from("{\n  \"schema\": \"selis-extract/1\",\n  \"lines\": [\n");
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
pub fn to_markdown(lines: &[TextLine], line_texts: &[String]) -> String {
    let text = to_text(lines, line_texts);
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
pub fn to_html(lines: &[TextLine], line_texts: &[String]) -> String {
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
            code,
            at: Point::new(0.0, 0.0),
            font: Bytes::copy_from_slice(font.as_bytes()),
            size,
            mcid: None,
        }
    }

    #[test]
    fn plain_text_joins_lines() {
        let lines = vec![
            line_at(vec![glyph(65, "F1", 12.0)]),
            line_at(vec![glyph(66, "F1", 12.0)]),
        ];
        let text = to_text(&lines, &["Hello".to_string(), "World".to_string()]);
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
        let md = to_markdown(&lines, &texts);
        assert!(md.contains("Hello & bye"));
        let html = to_html(&lines, &texts);
        assert!(html.contains("<p"));
        assert!(html.contains("Hello &amp; bye"));
    }

    #[test]
    fn json_escapes_special_chars() {
        assert_eq!(json_escape("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_escape("a\nb"), "\"a\\nb\"");
    }
}
