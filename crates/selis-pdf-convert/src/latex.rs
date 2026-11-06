//! The LaTeX subset parser (see the crate docs for the supported subset).
//!
//! It parses structure, not typography: sections become headings, `\item`
//! becomes a list item, `verbatim` stays verbatim, and `\textbf`/`\textit`/
//! `\emph`/`\texttt` become styled runs. Everything else that looks like a
//! command is dropped rather than guessed at — a document that uses `tabular`
//! or `\includegraphics` produces a text-only PDF, and the crate docs say so.

use crate::blocks::{Block, Span, Style};

/// Strip `%` comments, leaving `\%` intact.
fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.replace('\r', "").lines() {
        let mut kept = String::new();
        let mut escaped = false;
        for ch in line.chars() {
            if escaped {
                kept.push(ch);
                escaped = false;
                continue;
            }
            match ch {
                '\\' => {
                    kept.push(ch);
                    escaped = true;
                }
                '%' => break,
                other => kept.push(other),
            }
        }
        out.push_str(&kept);
        out.push('\n');
    }
    out
}

/// The brace group starting at `s`, as its inner text and the bytes consumed.
fn brace_group(s: &str) -> Option<(&str, usize)> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    if first != '{' {
        return None;
    }
    let mut depth = 1usize;
    for (at, ch) in chars {
        match ch {
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = at;
                    return Some((s.get(1..end)?, end.saturating_add(1)));
                }
            }
            _ => {}
        }
    }
    None
}

/// The command name starting at `s`, and how many bytes it occupies.
fn command(s: &str) -> (String, usize) {
    let name: String = s.chars().take_while(char::is_ascii_alphabetic).collect();
    let len = name.len();
    (name, len)
}

/// Inline text with the styles of the enclosing run applied.
fn inline_with(text: &str, base: Style) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;
    fn flush(out: &mut Vec<Span>, plain: &mut String, style: Style) {
        if !plain.is_empty() {
            out.push(Span {
                style,
                text: core::mem::take(plain),
            });
        }
    }
    while i < text.len() {
        let ch = match text.get(i..).and_then(|rest| rest.chars().next()) {
            Some(ch) => ch,
            None => break,
        };
        if ch != '\\' {
            plain.push(ch);
            i = i.saturating_add(ch.len_utf8());
            continue;
        }
        let rest = text.get(i.saturating_add(1)..).unwrap_or("");
        let (name, name_len) = command(rest);
        if name.is_empty() {
            // `\\` is a line break, `\%` and friends are literal characters.
            let next = rest.chars().next();
            match next {
                Some('\\') => {
                    plain.push(' ');
                    i = i.saturating_add(2);
                }
                Some(symbol) => {
                    plain.push(symbol);
                    i = i
                        .saturating_add(1)
                        .saturating_add(symbol.len_utf8().min(rest.len()));
                }
                None => i = text.len(),
            }
            continue;
        }
        let after = rest.get(name_len..).unwrap_or("");
        let style = match name.as_str() {
            "textbf" => Some(base.with_bold()),
            "textit" | "emph" => Some(base.with_italic()),
            "texttt" => Some(Style::Code),
            _ => None,
        };
        if let (Some(style), Some((inner, consumed))) = (style, brace_group(after)) {
            flush(&mut out, &mut plain, base);
            out.extend(inline_with(inner, style));
            i = i
                .saturating_add(1)
                .saturating_add(name_len)
                .saturating_add(consumed);
            continue;
        }
        // Not a styled run: keep the command's argument as text when it has one
        // (`\section`-like commands never reach here), else drop the command.
        if let Some((inner, consumed)) = brace_group(after) {
            plain.push_str(inner);
            i = i
                .saturating_add(1)
                .saturating_add(name_len)
                .saturating_add(consumed);
        } else {
            i = i.saturating_add(1).saturating_add(name_len);
        }
    }
    flush(&mut out, &mut plain, base);
    out
}

fn parse_inline(text: &str) -> Vec<Span> {
    inline_with(text, Style::Regular)
}

/// Parse LaTeX (the documented subset) into layout blocks.
///
/// Every input parses: unknown commands lose their name and keep their
/// argument, and an unterminated environment ends at the end of the input.
pub(crate) fn parse(input: &str) -> Vec<Block> {
    let text = strip_comments(input);
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Block> = Vec::new();
    let mut paragraph = String::new();
    let mut list: Option<bool> = None;
    let mut ordinal = 0usize;
    // Each item carries the ordinal it was created with, so flushing it later
    // cannot borrow the next item's number.
    let mut item: Option<(String, usize)> = None;
    let mut title: Option<String> = None;
    let mut i = 0usize;

    fn flush_paragraph(out: &mut Vec<Block>, paragraph: &mut String) {
        let text = paragraph.trim().to_string();
        paragraph.clear();
        if !text.is_empty() {
            out.push(Block::Paragraph(parse_inline(&text)));
        }
    }
    fn flush_item(out: &mut Vec<Block>, item: &mut Option<(String, usize)>, ordered: bool) {
        if let Some((text, ordinal)) = item.take() {
            let text = text.trim().to_string();
            if !text.is_empty() {
                out.push(Block::ListItem {
                    ordered,
                    ordinal,
                    depth: 0,
                    spans: parse_inline(&text),
                });
            }
        }
    }

    while i < lines.len() {
        let line = lines.get(i).copied().unwrap_or_default().trim();
        i = i.saturating_add(1);
        if line.is_empty() {
            if item.is_none() {
                flush_paragraph(&mut out, &mut paragraph);
            }
            continue;
        }
        if line.starts_with("\\begin{verbatim}") {
            flush_paragraph(&mut out, &mut paragraph);
            let mut code: Vec<String> = Vec::new();
            while let Some(next) = lines.get(i) {
                let next = *next;
                i = i.saturating_add(1);
                if next.trim_start().starts_with("\\end{verbatim}") {
                    break;
                }
                code.push(next.to_string());
            }
            out.push(Block::Code(code));
            continue;
        }
        if line.starts_with("\\begin{itemize}") || line.starts_with("\\begin{enumerate}") {
            flush_paragraph(&mut out, &mut paragraph);
            list = Some(line.starts_with("\\begin{enumerate}"));
            ordinal = 1;
            continue;
        }
        if line.starts_with("\\end{itemize}") || line.starts_with("\\end{enumerate}") {
            flush_item(&mut out, &mut item, list.unwrap_or(false));
            list = None;
            continue;
        }
        if line.starts_with("\\item") {
            if list.is_none() {
                list = Some(false);
            }
            flush_item(&mut out, &mut item, list.unwrap_or(false));
            let rest = line.get("\\item".len()..).unwrap_or("").trim();
            item = Some((rest.to_string(), ordinal));
            ordinal = ordinal.saturating_add(1);
            continue;
        }
        // Sectioning commands.
        let mut heading: Option<(u8, &str)> = None;
        for (command_name, level) in [
            ("\\chapter", 1u8),
            ("\\section", 1u8),
            ("\\subsection", 2),
            ("\\subsubsection", 3),
        ] {
            if let Some(rest) = line.strip_prefix(command_name) {
                if let Some((inner, _)) = brace_group(rest.trim_start()) {
                    heading = Some((level, inner));
                }
                break;
            }
        }
        if let Some((level, inner)) = heading {
            flush_paragraph(&mut out, &mut paragraph);
            flush_item(&mut out, &mut item, list.unwrap_or(false));
            out.push(Block::Heading {
                level,
                spans: parse_inline(inner),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("\\title") {
            if let Some((inner, _)) = brace_group(rest.trim_start()) {
                title = Some(inner.to_string());
            }
            continue;
        }
        if line.starts_with("\\maketitle") {
            flush_paragraph(&mut out, &mut paragraph);
            if let Some(title) = title.clone() {
                out.push(Block::Heading {
                    level: 1,
                    spans: parse_inline(&title),
                });
            }
            continue;
        }
        // Body text: belongs to the open item, or to the open paragraph.
        if let Some((text, _)) = item.as_mut() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(line);
        } else {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(line);
        }
    }
    flush_item(&mut out, &mut item, list.unwrap_or(false));
    flush_paragraph(&mut out, &mut paragraph);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn sections_become_headings() {
        let blocks = parse("\\section{One}\nText.\n\\subsection{Two}\nMore.\n");
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(blocks[1], Block::Paragraph(_)));
        assert!(matches!(blocks[2], Block::Heading { level: 2, .. }));
    }

    #[test]
    fn items_keep_their_order() {
        let blocks = parse("\\begin{enumerate}\n\\item First\n\\item Second\n\\end{enumerate}\n");
        match blocks.first() {
            Some(Block::ListItem {
                ordered, ordinal, ..
            }) => {
                assert!(*ordered);
                assert_eq!(*ordinal, 1);
            }
            other => panic!("expected a list item, got {other:?}"),
        }
        match blocks.get(1) {
            Some(Block::ListItem { ordinal, .. }) => assert_eq!(*ordinal, 2),
            other => panic!("expected a second item, got {other:?}"),
        }
    }

    #[test]
    fn inline_styles_are_read() {
        let blocks = parse("A \\textbf{bold} and \\emph{italic} word.\n");
        let Some(Block::Paragraph(spans)) = blocks.first() else {
            panic!("expected a paragraph");
        };
        assert!(spans
            .iter()
            .any(|s| s.style == Style::Bold && s.text == "bold"));
        assert!(spans
            .iter()
            .any(|s| s.style == Style::Italic && s.text == "italic"));
    }

    #[test]
    fn comments_are_stripped_but_escaped_percent_survives() {
        let blocks = parse("Keep % drop this\n100\\% kept\n");
        let text = match blocks.first() {
            Some(Block::Paragraph(spans)) => {
                spans.iter().map(|s| s.text.as_str()).collect::<String>()
            }
            other => panic!("expected a paragraph, got {other:?}"),
        };
        assert!(text.contains("100%"));
        assert!(!text.contains("drop this"));
    }

    #[test]
    fn verbatim_is_code() {
        let blocks = parse("\\begin{verbatim}\n  x = 1;\n\\end{verbatim}\n");
        match blocks.first() {
            Some(Block::Code(lines)) => assert_eq!(lines, &vec!["  x = 1;".to_string()]),
            other => panic!("expected code, got {other:?}"),
        }
    }

    #[test]
    fn the_title_appears_where_maketitle_is() {
        let blocks = parse("\\title{My Paper}\n\\maketitle\nBody.\n");
        match blocks.first() {
            Some(Block::Heading { level, spans }) => {
                assert_eq!(*level, 1);
                assert_eq!(spans.first().map(|s| s.text.as_str()), Some("My Paper"));
            }
            other => panic!("expected a title heading, got {other:?}"),
        }
    }
}
