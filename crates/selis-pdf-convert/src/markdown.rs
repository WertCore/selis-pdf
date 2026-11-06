//! The Markdown subset parser (see the crate docs for the supported subset).

use crate::blocks::{Block, Span, Style};

/// Parse Markdown (the documented subset) into layout blocks.
///
/// Every input parses: unrecognised constructs degrade to paragraph text.
pub(crate) fn parse(input: &str) -> Vec<Block> {
    let text = input.replace('\r', "");
    let lines: Vec<&str> = text.lines().collect();
    parse_lines(&lines, 0)
}

/// Parse a run of lines (used recursively for blockquotes).
fn parse_lines(lines: &[&str], base_indent: usize) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines.get(i).copied().unwrap_or_default();
        let trimmed = line.trim_start();
        if trimmed.trim().is_empty() {
            i = i.saturating_add(1);
            continue;
        }

        // Fenced code block.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let fence = if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            };
            let mut code: Vec<String> = Vec::new();
            i = i.saturating_add(1);
            while let Some(l) = lines.get(i) {
                if l.trim_start().starts_with(fence) {
                    i = i.saturating_add(1);
                    break;
                }
                code.push((*l).to_string());
                i = i.saturating_add(1);
            }
            out.push(Block::Code(code));
            continue;
        }

        // ATX heading.
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes)
            && trimmed
                .chars()
                .nth(hashes)
                .is_some_and(|c| c == ' ' || c == '\t')
        {
            let rest = trimmed.get(hashes..).unwrap_or("").trim();
            let level = u8::try_from(hashes).unwrap_or(6).min(6);
            out.push(Block::Heading {
                level,
                spans: parse_inline(rest),
            });
            i = i.saturating_add(1);
            continue;
        }

        // Horizontal rule: a line of at least three -, * or _ plus spaces.
        if is_rule(trimmed) {
            out.push(Block::Rule);
            i = i.saturating_add(1);
            continue;
        }

        // Quote.
        if trimmed.starts_with('>') {
            let mut quoted: Vec<&str> = Vec::new();
            while let Some(l) = lines.get(i) {
                let t = l.trim_start();
                if !t.starts_with('>') {
                    break;
                }
                let stripped = t
                    .get(1..)
                    .unwrap_or("")
                    .strip_prefix(' ')
                    .unwrap_or(t.get(1..).unwrap_or(""));
                quoted.push(stripped);
                i = i.saturating_add(1);
            }
            out.push(Block::Quote(parse_lines(&quoted, base_indent)));
            continue;
        }

        // Lists.
        if list_marker(trimmed).is_some() {
            let (items, consumed) = parse_list(lines.get(i..).unwrap_or_default());
            out.extend(items);
            i = i.saturating_add(consumed);
            continue;
        }

        // GFM pipe table: a header row followed by a |---|---| delimiter row.
        if trimmed.starts_with('|') {
            if let Some(delimiter) = lines.get(i.saturating_add(1)) {
                if let Some(cols) = delimiter_cell_count(delimiter.trim_start()) {
                    let header = split_pipe_cells(trimmed);
                    if header.len() == cols {
                        out.push(pipe_table(lines, &mut i, header, cols));
                        continue;
                    }
                }
            }
        }

        // Indented code block (4 spaces / 1 tab) outside lists.
        if (line.starts_with("    ") || line.starts_with('\t')) && base_indent == 0 {
            let mut code: Vec<String> = Vec::new();
            while let Some(l) = lines.get(i) {
                if l.starts_with("    ") {
                    code.push(l.get(4..).unwrap_or("").to_string());
                } else if l.starts_with('\t') {
                    code.push(l.get(1..).unwrap_or("").to_string());
                } else if l.trim().is_empty() {
                    code.push(String::new());
                } else {
                    break;
                }
                i = i.saturating_add(1);
            }
            while code.last().is_some_and(String::is_empty) {
                code.pop();
            }
            if !code.is_empty() {
                out.push(Block::Code(code));
            }
            continue;
        }

        // Paragraph: gather until a blank line or a new block start.
        let mut para = String::new();
        while let Some(l) = lines.get(i) {
            let t = l.trim_start();
            let heading_here = {
                let h = t.chars().take_while(|c| *c == '#').count();
                (1..=6).contains(&h) && t.chars().nth(h).is_some_and(|c| c == ' ' || c == '\t')
            };
            if t.trim().is_empty()
                || t.starts_with("```")
                || t.starts_with("~~~")
                || t.starts_with('>')
                || (t.starts_with('|')
                    && !para.is_empty()
                    && lines
                        .get(i.saturating_add(1))
                        .is_some_and(|next| delimiter_cell_count(next.trim_start()).is_some()))
                || is_rule(t)
                || list_marker(t).is_some()
                || heading_here
                || ((t.starts_with("    ") || t.starts_with('\t')) && para.is_empty())
            {
                break;
            }
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(t.trim());
            i = i.saturating_add(1);
        }
        if !para.trim().is_empty() {
            out.push(Block::Paragraph(parse_inline(&para)));
        }
    }
    out
}

/// Whether a trimmed line is a horizontal rule.
fn is_rule(trimmed: &str) -> bool {
    for ch in ['-', '*', '_'] {
        let only = trimmed.chars().all(|c| c == ch || c == ' ');
        let count = trimmed.chars().filter(|c| *c == ch).count();
        if only && count >= 3 {
            return true;
        }
    }
    false
}

/// Split one pipe-table line into its cells, honouring `\|` as an escaped
/// pipe. Leading and trailing pipes are dropped.
fn split_pipe_cells(line: &str) -> Vec<String> {
    let mut cells: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            if ch == '|' {
                buf.push('|');
            } else {
                buf.push('\\');
                buf.push(ch);
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => {
                cells.push(buf.trim().to_string());
                buf.clear();
            }
            _ => buf.push(ch),
        }
    }
    if escaped {
        buf.push('\\');
    }
    cells.push(buf.trim().to_string());
    // A leading or trailing pipe contributes an empty outer cell.
    if cells.first().is_some_and(|cell| cell.is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|cell| cell.is_empty()) {
        cells.pop();
    }
    cells
}

/// If `trimmed` is a pipe-table delimiter row (`| --- | :-: |`), return the
/// number of cells it declares.
fn delimiter_cell_count(trimmed: &str) -> Option<usize> {
    if !trimmed.contains('|') || !trimmed.contains('-') {
        return None;
    }
    let cells = split_pipe_cells(trimmed);
    if cells.is_empty() {
        return None;
    }
    for cell in &cells {
        let core = cell.trim().trim_start_matches(':').trim_end_matches(':');
        if core.len() < 2 || !core.chars().all(|ch| ch == '-') {
            return None;
        }
    }
    Some(cells.len())
}

/// A cell as span lists: pipe-table cells carry plain text, not inline markup.
fn table_cell(text: &str) -> Vec<crate::blocks::Span> {
    if text.is_empty() {
        return Vec::new();
    }
    vec![crate::blocks::Span {
        style: crate::blocks::Style::Regular,
        text: text.to_string(),
    }]
}

/// Consume the delimiter and the body rows starting at `*i + 1` and build the
/// table block; `i` is advanced past the last row.
fn pipe_table(
    lines: &[&str],
    i: &mut usize,
    header: Vec<String>,
    cols: usize,
) -> crate::blocks::Block {
    let mut rows: Vec<Vec<Vec<crate::blocks::Span>>> = Vec::new();
    let mut j = i.saturating_add(2);
    while let Some(l) = lines.get(j) {
        let trimmed = l.trim_start();
        if trimmed.trim().is_empty() || !trimmed.contains('|') {
            break;
        }
        if delimiter_cell_count(trimmed).is_some() {
            // A second delimiter is not a row; stop before it.
            break;
        }
        let mut cells = split_pipe_cells(trimmed);
        cells.resize(cols, String::new());
        cells.truncate(cols);
        rows.push(cells.iter().map(|cell| table_cell(cell)).collect());
        j = j.saturating_add(1);
    }
    *i = j;
    let head = header.iter().map(|cell| table_cell(cell)).collect();
    crate::blocks::Block::Table { head, rows }
}

/// The list marker at the start of a trimmed line, if any:
/// `Some((ordered, marker_len))`.
fn list_marker(trimmed: &str) -> Option<(bool, usize)> {
    let chars: Vec<char> = trimmed.chars().collect();
    match chars.first() {
        Some('-') | Some('*') | Some('+') => {
            if chars.get(1) == Some(&' ') {
                return Some((false, 2));
            }
        }
        Some(c) if c.is_ascii_digit() => {
            let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
            if let Some(after) = chars.get(digits) {
                if (*after == '.' || *after == ')')
                    && chars.get(digits.saturating_add(1)) == Some(&' ')
                {
                    return Some((true, digits.saturating_add(2)));
                }
            }
        }
        _ => {}
    }
    None
}

/// Parse consecutive list lines into `ListItem` blocks. Returns the items and
/// the number of consumed lines.
fn parse_list(lines: &[&str]) -> (Vec<Block>, usize) {
    let mut items: Vec<Block> = Vec::new();
    let mut i = 0usize;
    let mut ordinal = 0usize;
    let mut ordered: Option<bool> = None;

    while let Some(line) = lines.get(i) {
        let leading = line.chars().take_while(|c| *c == ' ').count();
        let trimmed = line.trim_start();
        if trimmed.trim().is_empty() {
            // A blank line ends the list in this subset.
            break;
        }
        let Some((is_ordered, marker_len)) = list_marker(trimmed) else {
            // A continuation line (indented under the item).
            if leading >= 2 && !items.is_empty() {
                if let Some(Block::ListItem { spans, .. }) = items.last_mut() {
                    if let Some(last) = spans.last_mut() {
                        last.text.push(' ');
                    } else {
                        spans.push(Span {
                            style: Style::Regular,
                            text: String::new(),
                        });
                    }
                    for span in parse_inline(trimmed) {
                        spans.push(span);
                    }
                }
                i = i.saturating_add(1);
                continue;
            }
            break;
        };
        match ordered {
            None => ordered = Some(is_ordered),
            Some(o) if o != is_ordered => break, // marker kind change ends the list
            Some(_) => {}
        }
        ordinal = ordinal.saturating_add(1);
        let depth = leading.checked_div(2).unwrap_or(0).min(3);
        let text = trimmed.get(marker_len..).unwrap_or("").trim();
        items.push(Block::ListItem {
            ordered: is_ordered,
            ordinal,
            depth,
            spans: parse_inline(text),
        });
        i = i.saturating_add(1);
    }
    (items, i)
}

/// Parse inline markup into styled spans.
pub(crate) fn parse_inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut code = false;
    let mut i = 0usize;

    let flush = |out: &mut Vec<Span>, buf: &mut String, bold: bool, italic: bool, code: bool| {
        if buf.is_empty() {
            return;
        }
        let style = if code {
            Style::Code
        } else {
            let mut s = Style::Regular;
            if bold {
                s = s.with_bold();
            }
            if italic {
                s = s.with_italic();
            }
            s
        };
        out.push(Span {
            style,
            text: std::mem::take(buf),
        });
    };

    while i < chars.len() {
        let c = chars.get(i).copied().unwrap_or_default();
        if code {
            if c == '`' {
                flush(&mut out, &mut buf, bold, italic, code);
                code = false;
            } else {
                buf.push(c);
            }
            i = i.saturating_add(1);
            continue;
        }
        match c {
            '`' => {
                flush(&mut out, &mut buf, bold, italic, code);
                code = true;
                i = i.saturating_add(1);
            }
            '!' if chars.get(i.saturating_add(1)) == Some(&'[') => {
                // Images are not rendered in this subset; skip ![alt](url).
                if let Some(end) = skip_image(&chars, i) {
                    i = end;
                } else {
                    buf.push(c);
                    i = i.saturating_add(1);
                }
            }
            '[' => {
                if let Some((text_end, url, end)) = parse_link(&chars, i) {
                    flush(&mut out, &mut buf, bold, italic, code);
                    let inner: String = chars
                        .get(i.saturating_add(1)..text_end)
                        .map_or_else(String::new, |s| s.iter().collect());
                    // Link text may carry its own inline markup.
                    for span in parse_inline(&inner) {
                        out.push(span);
                    }
                    let url_str: String = url;
                    if !url_str.is_empty() && url_str != inner {
                        out.push(Span {
                            style: Style::Italic,
                            text: format!(" ({url_str})"),
                        });
                    }
                    i = end;
                } else {
                    buf.push(c);
                    i = i.saturating_add(1);
                }
            }
            '*' | '_' => {
                let doubled = chars.get(i.saturating_add(1)) == Some(&c);
                // Underscore emphasis only at word boundaries.
                if c == '_' && !doubled {
                    let before_ok = i == 0
                        || chars
                            .get(i.saturating_sub(1))
                            .is_some_and(|p| p.is_whitespace() || *p == '(' || *p == '[');
                    if !before_ok {
                        buf.push(c);
                        i = i.saturating_add(1);
                        continue;
                    }
                }
                flush(&mut out, &mut buf, bold, italic, code);
                if doubled {
                    bold = !bold;
                    i = i.saturating_add(2);
                } else {
                    italic = !italic;
                    i = i.saturating_add(1);
                }
            }
            _ => {
                buf.push(c);
                i = i.saturating_add(1);
            }
        }
    }
    flush(&mut out, &mut buf, bold, italic, code);
    out
}

/// Skip an `![alt](url)` image starting at `i`; returns the index after it.
fn skip_image(chars: &[char], i: usize) -> Option<usize> {
    let (_, _, end) = parse_link(chars, i.saturating_add(1))?;
    Some(end)
}

/// Parse `[text](url)` starting at `i` (which holds `[`).
/// Returns `(text_end_index, url, index_after_close_paren)`.
fn parse_link(chars: &[char], i: usize) -> Option<(usize, String, usize)> {
    let mut depth = 0usize;
    let mut j = i;
    let mut text_end = None;
    while let Some(&c) = chars.get(j) {
        match c {
            '[' => depth = depth.saturating_add(1),
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    text_end = Some(j);
                    break;
                }
            }
            _ => {}
        }
        j = j.saturating_add(1);
    }
    let text_end = text_end?;
    if chars.get(text_end.saturating_add(1)) != Some(&'(') {
        return None;
    }
    let url_start = text_end.saturating_add(2);
    let mut k = url_start;
    while let Some(&c) = chars.get(k) {
        if c == ')' {
            let url: String = chars
                .get(url_start..k)
                .map_or_else(String::new, |s| s.iter().collect());
            return Some((text_end, url.trim().to_string(), k.saturating_add(1)));
        }
        k = k.saturating_add(1);
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn headings_and_paragraphs() {
        let blocks = parse("# Title\n\nHello *world* and **bold**.\n");
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
        let Block::Paragraph(spans) = &blocks[1] else {
            panic!("expected paragraph");
        };
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[1].style, Style::Italic);
        assert_eq!(spans[1].text, "world");
        assert_eq!(spans[3].style, Style::Bold);
    }

    #[test]
    fn fenced_code_blocks() {
        let blocks = parse("```rust\nfn main() {}\n```\n");
        let Block::Code(lines) = &blocks[0] else {
            panic!("expected code block");
        };
        assert_eq!(lines, &["fn main() {}".to_string()]);
    }

    #[test]
    fn nested_lists() {
        let blocks = parse("- one\n- two\n  - inner\n\n1. first\n2. second\n");
        assert_eq!(blocks.len(), 5);
        let Block::ListItem { depth, .. } = &blocks[2] else {
            panic!("expected item");
        };
        assert_eq!(*depth, 1);
        let Block::ListItem {
            ordered, ordinal, ..
        } = &blocks[4]
        else {
            panic!("expected item");
        };
        assert!(*ordered);
        assert_eq!(*ordinal, 2);
    }

    #[test]
    fn blockquotes_and_rules() {
        let blocks = parse("> quoted text\n\n---\n");
        assert!(matches!(&blocks[0], Block::Quote(_)));
        assert!(matches!(&blocks[1], Block::Rule));
    }

    #[test]
    fn links_render_text_and_url() {
        let spans = parse_inline("see [the site](https://example.com) now");
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts.join(""), "see the site (https://example.com) now");
    }

    #[test]
    fn inline_code_spans() {
        let spans = parse_inline("run `cargo test` here");
        assert_eq!(spans[1].style, Style::Code);
        assert_eq!(spans[1].text, "cargo test");
    }

    #[test]
    fn indented_code_block() {
        let blocks = parse("intro paragraph\n\n    line one\n    line two\n");
        assert!(matches!(&blocks[1], Block::Code(_)));
    }

    #[test]
    fn pipe_tables_parse_with_a_header() {
        let blocks = parse("| Name | Amount |\n| --- | ---: |\n| Ada | 1200 |\n| Grace | 900 |\n");
        assert_eq!(blocks.len(), 1);
        let Block::Table { head, rows } = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(
            head.iter()
                .map(|cell| cell.iter().map(|s| s.text.as_str()).collect::<String>())
                .collect::<Vec<_>>(),
            vec!["Name".to_string(), "Amount".to_string()]
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0][1]
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "1200"
        );
    }

    #[test]
    fn pipe_tables_escape_pipes_and_ignore_alignment() {
        let blocks = parse("| A \\| B | C |\n| :--- | ---: |\n| a | b |\n");
        let Block::Table { head, rows } = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(
            head[0].iter().map(|s| s.text.as_str()).collect::<String>(),
            "A | B"
        );
        assert_eq!(
            rows[0][0]
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "a"
        );
    }

    #[test]
    fn a_table_without_a_delimiter_is_a_paragraph() {
        let blocks = parse("| just | text |\n");
        assert!(matches!(&blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn short_rows_are_padded() {
        let blocks = parse("| A | B | C |\n| --- | --- | --- |\n| only |\n");
        let Block::Table { rows, .. } = &blocks[0] else {
            panic!("expected a table");
        };
        assert_eq!(rows[0].len(), 3);
        assert!(rows[0][2].is_empty());
    }
}
