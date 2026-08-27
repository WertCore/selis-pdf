//! The HTML subset parser (see the crate docs for the supported subset).

use crate::blocks::{Block, Span, Style};

/// An open-list context.
#[derive(Debug)]
struct ListCtx {
    ordered: bool,
    counter: usize,
}

/// The iterative HTML-subset parser.
#[derive(Debug)]
struct Parser<'a> {
    chars: &'a [char],
    pos: usize,
    blocks: Vec<Block>,
    spans: Vec<Span>,
    text_buf: String,
    bold: usize,
    italic: usize,
    code_depth: usize,
    href_depth: usize,
    heading: Option<u8>,
    lists: Vec<ListCtx>,
    in_item: bool,
    item_ordinal: usize,
}

/// Parse an HTML subset into layout blocks.
///
/// Every input parses: broken markup, unknown tags and unknown entities
/// degrade to text.
pub(crate) fn parse(input: &str) -> Vec<Block> {
    let text = input.replace('\r', "");
    let chars: Vec<char> = text.chars().collect();
    let mut p = Parser {
        chars: &chars,
        pos: 0,
        blocks: Vec::new(),
        spans: Vec::new(),
        text_buf: String::new(),
        bold: 0,
        italic: 0,
        code_depth: 0,
        href_depth: 0,
        heading: None,
        lists: Vec::new(),
        in_item: false,
        item_ordinal: 0,
    };
    p.run();
    p.finish();
    p.blocks
}

impl<'a> Parser<'a> {
    fn run(&mut self) {
        while self.pos < self.chars.len() {
            let c = self.chars.get(self.pos).copied().unwrap_or_default();
            match c {
                '<' => self.handle_tag(),
                '&' => self.handle_entity(),
                _ => {
                    self.text_buf.push(c);
                    self.pos = self.pos.saturating_add(1);
                }
            }
        }
    }

    /// End-of-input: close any open list item or paragraph.
    fn finish(&mut self) {
        if self.in_item {
            self.close_item();
        }
        self.flush_paragraph();
    }

    fn cur_style(&self) -> Style {
        let mut s = Style::Regular;
        if self.bold > 0 {
            s = s.with_bold();
        }
        if self.italic > 0 {
            s = s.with_italic();
        }
        if self.code_depth > 0 {
            s = Style::Code;
        }
        if self.href_depth > 0 && matches!(s, Style::Regular) {
            s = Style::Italic;
        }
        s
    }

    /// Collapse whitespace runs (HTML text semantics) and flush the text
    /// buffer as a span.
    fn flush_text(&mut self) {
        if self.text_buf.is_empty() {
            return;
        }
        let collapsed: String = self
            .text_buf
            .chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        self.text_buf.clear();
        if collapsed.is_empty() {
            return;
        }
        let style = self.cur_style();
        if let Some(last) = self.spans.last_mut() {
            if last.style == style {
                last.text.push_str(&collapsed);
                return;
            }
        }
        self.spans.push(Span {
            style,
            text: collapsed,
        });
    }

    fn spans_are_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }

    /// Flush the current spans as a paragraph (or heading, when one is open).
    /// When a list item is open, the spans belong to it instead.
    fn flush_paragraph(&mut self) {
        if self.in_item {
            self.close_item();
            return;
        }
        self.flush_text();
        if self.spans_are_empty() {
            self.spans.clear();
            self.heading = None;
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        if let Some(level) = self.heading.take() {
            self.blocks.push(Block::Heading { level, spans });
        } else {
            self.blocks.push(Block::Paragraph(spans));
        }
    }

    /// Close the current list item, emitting a `ListItem`.
    fn close_item(&mut self) {
        self.flush_text();
        if self.spans_are_empty() {
            self.spans.clear();
            self.in_item = false;
            return;
        }
        let (ordered, depth) = match self.lists.last() {
            Some(ctx) => (ctx.ordered, self.lists.len().saturating_sub(1)),
            None => (false, 0),
        };
        let ordinal = self.item_ordinal;
        self.blocks.push(Block::ListItem {
            ordered,
            ordinal,
            depth: depth.min(3),
            spans: std::mem::take(&mut self.spans),
        });
        self.in_item = false;
    }

    fn handle_entity(&mut self) {
        // self.pos is at '&'
        let start = self.pos.saturating_add(1);
        let mut end = start;
        while let Some(&c) = self.chars.get(end) {
            if c == ';' || end.saturating_sub(start) > 31 {
                break;
            }
            end = end.saturating_add(1);
        }
        let found = self.chars.get(end) == Some(&';');
        let name: String = self
            .chars
            .get(start..end)
            .map_or_else(String::new, |s| s.iter().collect());
        if found {
            if let Some(replacement) = decode_entity(&name) {
                self.text_buf.push_str(&replacement);
                self.pos = end.saturating_add(1);
                return;
            }
        }
        // Unknown or unterminated: emit literally.
        self.text_buf.push('&');
        self.pos = self.pos.saturating_add(1);
    }

    fn handle_tag(&mut self) {
        // self.pos is at '<'
        if starts_with_ci(self.chars, self.pos, "<!--") {
            self.pos = find_ci(self.chars, "-->", self.pos)
                .map(|i| i.saturating_add(3))
                .unwrap_or(self.chars.len());
            return;
        }
        if starts_with_ci(self.chars, self.pos, "<!") || starts_with_ci(self.chars, self.pos, "<?")
        {
            self.pos = find_char(self.chars, '>', self.pos)
                .map(|i| i.saturating_add(1))
                .unwrap_or(self.chars.len());
            return;
        }

        let closing = self.chars.get(self.pos.saturating_add(1)) == Some(&'/');
        let name_start = if closing {
            self.pos.saturating_add(2)
        } else {
            self.pos.saturating_add(1)
        };
        let mut p = name_start;
        while let Some(&c) = self.chars.get(p) {
            if c.is_ascii_alphanumeric() {
                p = p.saturating_add(1);
            } else {
                break;
            }
        }
        let name: String = self
            .chars
            .get(name_start..p)
            .map_or_else(String::new, |s| s.iter().collect())
            .to_ascii_lowercase();
        if name.is_empty() {
            // A stray '<': emit literally.
            self.text_buf.push('<');
            self.pos = self.pos.saturating_add(1);
            return;
        }

        // Read attributes (only href is kept) and find the tag's end.
        let mut href: Option<String> = None;
        let mut self_closing = false;
        let mut q = p;
        loop {
            match self.chars.get(q).copied() {
                None => {
                    self.pos = self.chars.len();
                    return;
                }
                Some('>') => {
                    q = q.saturating_add(1);
                    break;
                }
                Some('/') => {
                    if self.chars.get(q.saturating_add(1)) == Some(&'>') {
                        self_closing = true;
                        q = q.saturating_add(2);
                        break;
                    }
                    q = q.saturating_add(1);
                }
                Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':' => {
                    let attr_start = q;
                    while let Some(&ac) = self.chars.get(q) {
                        if ac.is_ascii_alphanumeric() || ac == '-' || ac == '_' || ac == ':' {
                            q = q.saturating_add(1);
                        } else {
                            break;
                        }
                    }
                    let attr: String = self
                        .chars
                        .get(attr_start..q)
                        .map_or_else(String::new, |s| s.iter().collect())
                        .to_ascii_lowercase();
                    // Skip whitespace, expect '='.
                    while self.chars.get(q) == Some(&' ') {
                        q = q.saturating_add(1);
                    }
                    if self.chars.get(q) == Some(&'=') {
                        q = q.saturating_add(1);
                        while self.chars.get(q) == Some(&' ') {
                            q = q.saturating_add(1);
                        }
                        let quoted = matches!(self.chars.get(q), Some('"') | Some('\''));
                        if quoted {
                            let quote = self.chars.get(q).copied().unwrap_or('"');
                            q = q.saturating_add(1);
                            let vstart = q;
                            while let Some(&vc) = self.chars.get(q) {
                                if vc == quote {
                                    break;
                                }
                                q = q.saturating_add(1);
                            }
                            let value: String = self
                                .chars
                                .get(vstart..q)
                                .map_or_else(String::new, |s| s.iter().collect());
                            if quoted {
                                q = q.saturating_add(1);
                            }
                            if attr == "href" {
                                href = Some(value);
                            }
                        } else {
                            let vstart = q;
                            while let Some(&vc) = self.chars.get(q) {
                                if vc.is_whitespace() || vc == '>' {
                                    break;
                                }
                                q = q.saturating_add(1);
                            }
                            let value: String = self
                                .chars
                                .get(vstart..q)
                                .map_or_else(String::new, |s| s.iter().collect());
                            if attr == "href" {
                                href = Some(value);
                            }
                        }
                    }
                }
                Some(_) => {
                    q = q.saturating_add(1);
                }
            }
        }
        self.pos = q;

        if closing {
            self.close_tag(&name);
        } else {
            self.open_tag(&name, href.as_deref(), self_closing);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn open_tag(&mut self, name: &str, href: Option<&str>, self_closing: bool) {
        match name {
            "br" => {
                self.flush_paragraph();
            }
            "hr" => {
                self.flush_paragraph();
                self.blocks.push(Block::Rule);
            }
            "script" | "style" => {
                let close = format!("</{name}");
                self.pos = find_ci(self.chars, &close, self.pos).unwrap_or(self.chars.len());
            }
            "pre" => {
                self.flush_paragraph();
                let (lines, new_pos) = extract_pre(self.chars, self.pos);
                self.pos = new_pos;
                if !lines.is_empty() {
                    self.blocks.push(Block::CodeBlock(lines));
                }
            }
            "blockquote" => {
                self.flush_paragraph();
                let (inner, new_pos) = extract_balanced(self.chars, self.pos, "blockquote");
                self.pos = new_pos;
                self.blocks.push(Block::Blockquote(parse(&inner)));
            }
            "ul" | "ol" => {
                self.flush_paragraph();
                self.lists.push(ListCtx {
                    ordered: name == "ol",
                    counter: 0,
                });
            }
            "li" => {
                if self.in_item {
                    self.close_item();
                }
                self.in_item = true;
                if let Some(ctx) = self.lists.last_mut() {
                    ctx.counter = ctx.counter.saturating_add(1);
                    self.item_ordinal = ctx.counter;
                } else {
                    // A stray <li> outside a list: render as an unordered item.
                    self.lists.push(ListCtx {
                        ordered: false,
                        counter: 1,
                    });
                    self.item_ordinal = 1;
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_paragraph();
                let level = name
                    .get(1..2)
                    .and_then(|d| d.parse::<u8>().ok())
                    .unwrap_or(6);
                self.heading = Some(level);
            }
            "b" | "strong" => {
                self.flush_text();
                self.bold = self.bold.saturating_add(1);
            }
            "i" | "em" => {
                self.flush_text();
                self.italic = self.italic.saturating_add(1);
            }
            "code" => {
                self.flush_text();
                self.code_depth = self.code_depth.saturating_add(1);
            }
            "a" => {
                self.flush_text();
                if href.is_some() {
                    self.href_depth = self.href_depth.saturating_add(1);
                }
            }
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav"
            | "aside" | "figure" | "figcaption" | "table" | "thead" | "tbody" | "tfoot" | "tr"
            | "td" | "th" | "caption" | "form" | "fieldset" | "center" | "details" | "summary" => {
                self.flush_paragraph();
            }
            _ => {
                // Unknown tags unwrap: their text flows into the current block.
            }
        }
        let _ = self_closing;
    }

    fn close_tag(&mut self, name: &str) {
        match name {
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav"
            | "aside" | "figure" | "figcaption" | "table" | "thead" | "tbody" | "tfoot" | "tr"
            | "td" | "th" | "caption" | "form" | "fieldset" | "center" | "details" | "summary" => {
                self.flush_paragraph();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_paragraph();
            }
            "li" => {
                if self.in_item {
                    self.close_item();
                }
            }
            "ul" | "ol" => {
                if self.in_item {
                    self.close_item();
                }
                self.lists.pop();
                if self.lists.is_empty() {
                    // A paragraph break after the outermost list.
                    self.flush_paragraph();
                }
            }
            "b" | "strong" => {
                self.flush_text();
                self.bold = self.bold.saturating_sub(1);
            }
            "i" | "em" => {
                self.flush_text();
                self.italic = self.italic.saturating_sub(1);
            }
            "code" => {
                self.flush_text();
                self.code_depth = self.code_depth.saturating_sub(1);
            }
            "a" => {
                self.flush_text();
                self.href_depth = self.href_depth.saturating_sub(1);
            }
            _ => {}
        }
    }
}

/// Extract a `<pre>` body: raw text until `</pre>`, with inner tags stripped
/// and entities decoded. Returns the code lines and the position after the
/// closing tag.
fn extract_pre(chars: &[char], from: usize) -> (Vec<String>, usize) {
    let end = find_ci(chars, "</pre", from).unwrap_or(chars.len());
    let inner: String = chars
        .get(from..end)
        .map_or_else(String::new, |s| s.iter().collect());
    // Strip inner tags (typically <code>).
    let mut stripped = String::new();
    let mut in_tag = false;
    for c in inner.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {
                if !in_tag {
                    stripped.push(c);
                }
            }
        }
    }
    let decoded = decode_all_entities(&stripped);
    let mut lines: Vec<String> = decoded.lines().map(str::to_string).collect();
    // Drop a leading and trailing empty line (markup whitespace).
    if lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    // Position after "</pre" plus its '>' if present.
    let after = if end < chars.len() {
        find_char(chars, '>', end)
            .map(|i| i.saturating_add(1))
            .unwrap_or(chars.len())
    } else {
        chars.len()
    };
    (lines, after)
}

/// Extract the inner text of a balanced container element starting right after
/// its opening tag. Returns the inner text and the position after the matching
/// closing tag.
fn extract_balanced(chars: &[char], from: usize, tag: &str) -> (String, usize) {
    let mut depth = 1usize;
    let mut p = from;
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}");
    let mut inner_end = chars.len();
    while p < chars.len() {
        if starts_with_ci(chars, p, &close_pat) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                inner_end = p;
                p = find_char(chars, '>', p)
                    .map(|i| i.saturating_add(1))
                    .unwrap_or(chars.len());
                break;
            }
            p = find_char(chars, '>', p)
                .map(|i| i.saturating_add(1))
                .unwrap_or(chars.len());
        } else if starts_with_ci(chars, p, &open_pat) {
            // Only count genuine opening tags (followed by space or '>').
            let after = p.saturating_add(open_pat.len());
            if matches!(
                chars.get(after),
                Some(' ') | Some('>') | Some('\t') | Some('\n')
            ) {
                depth = depth.saturating_add(1);
            }
            p = p.saturating_add(1);
        } else {
            p = p.saturating_add(1);
        }
    }
    let inner: String = chars
        .get(from..inner_end)
        .map_or_else(String::new, |s| s.iter().collect());
    (inner, p)
}

/// Decode every `&entity;` in `text`.
fn decode_all_entities(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while let Some(&c) = chars.get(i) {
        if c == '&' {
            let start = i.saturating_add(1);
            let mut end = start;
            while let Some(&e) = chars.get(end) {
                if e == ';' || end.saturating_sub(start) > 31 {
                    break;
                }
                end = end.saturating_add(1);
            }
            if chars.get(end) == Some(&';') {
                let name: String = chars
                    .get(start..end)
                    .map_or_else(String::new, |s| s.iter().collect());
                if let Some(replacement) = decode_entity(&name) {
                    out.push_str(&replacement);
                    i = end.saturating_add(1);
                    continue;
                }
            }
            out.push('&');
            i = i.saturating_add(1);
        } else {
            out.push(c);
            i = i.saturating_add(1);
        }
    }
    out
}

/// Decode one entity name (without the `&` and `;`).
fn decode_entity(name: &str) -> Option<String> {
    if let Some(num) = name.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "shy" => '\u{00AD}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "hellip" => '\u{2026}',
        "laquo" => '\u{00AB}',
        "raquo" => '\u{00BB}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "sbquo" => '\u{201A}',
        "bdquo" => '\u{201E}',
        "bull" | "bullet" => '\u{2022}',
        "middot" => '\u{00B7}',
        "copy" => '\u{00A9}',
        "reg" => '\u{00AE}',
        "trade" => '\u{2122}',
        "times" => '\u{00D7}',
        "divide" => '\u{00F7}',
        "plusmn" => '\u{00B1}',
        "deg" => '\u{00B0}',
        "micro" => '\u{00B5}',
        "para" => '\u{00B6}',
        "sect" => '\u{00A7}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "permil" => '\u{2030}',
        "euro" => '\u{20AC}',
        "pound" => '\u{00A3}',
        "yen" => '\u{00A5}',
        "cent" => '\u{00A2}',
        "curren" => '\u{00A4}',
        "szlig" => '\u{00DF}',
        "agrave" => '\u{00E0}',
        "aacute" => '\u{00E1}',
        "acirc" => '\u{00E2}',
        "atilde" => '\u{00E3}',
        "auml" => '\u{00E4}',
        "aring" => '\u{00E5}',
        "aelig" => '\u{00E6}',
        "ccedil" => '\u{00E7}',
        "egrave" => '\u{00E8}',
        "eacute" => '\u{00E9}',
        "ecirc" => '\u{00EA}',
        "euml" => '\u{00EB}',
        "igrave" => '\u{00EC}',
        "iacute" => '\u{00ED}',
        "icirc" => '\u{00EE}',
        "iuml" => '\u{00EF}',
        "eth" => '\u{00F0}',
        "ntilde" => '\u{00F1}',
        "ograve" => '\u{00F2}',
        "oacute" => '\u{00F3}',
        "ocirc" => '\u{00F4}',
        "otilde" => '\u{00F5}',
        "ouml" => '\u{00F6}',
        "oslash" => '\u{00F8}',
        "ugrave" => '\u{00F9}',
        "uacute" => '\u{00FA}',
        "ucirc" => '\u{00FB}',
        "uuml" => '\u{00FC}',
        "yacute" => '\u{00FD}',
        "thorn" => '\u{00FE}',
        "yuml" => '\u{00FF}',
        "Agrave" => '\u{00C0}',
        "Aacute" => '\u{00C1}',
        "Acirc" => '\u{00C2}',
        "Atilde" => '\u{00C3}',
        "Auml" => '\u{00C4}',
        "Aring" => '\u{00C5}',
        "AElig" => '\u{00C6}',
        "Ccedil" => '\u{00C7}',
        "Egrave" => '\u{00C8}',
        "Eacute" => '\u{00C9}',
        "Ecirc" => '\u{00CA}',
        "Euml" => '\u{00CB}',
        "Igrave" => '\u{00CC}',
        "Iacute" => '\u{00CD}',
        "Icirc" => '\u{00CE}',
        "Iuml" => '\u{00CF}',
        "ETH" => '\u{00D0}',
        "Ntilde" => '\u{00D1}',
        "Ograve" => '\u{00D2}',
        "Oacute" => '\u{00D3}',
        "Ocirc" => '\u{00D4}',
        "Otilde" => '\u{00D5}',
        "Ouml" => '\u{00D6}',
        "Oslash" => '\u{00D8}',
        "Ugrave" => '\u{00D9}',
        "Uacute" => '\u{00DA}',
        "Ucirc" => '\u{00DB}',
        "Uuml" => '\u{00DC}',
        "Yacute" => '\u{00DD}',
        "THORN" => '\u{00DE}',
        _ => return None,
    };
    Some(c.to_string())
}

/// Case-insensitive prefix match at `pos`.
fn starts_with_ci(chars: &[char], pos: usize, needle: &str) -> bool {
    let mut p = pos;
    for n in needle.chars() {
        let Some(&c) = chars.get(p) else {
            return false;
        };
        if c.to_ascii_lowercase() != n.to_ascii_lowercase() {
            return false;
        }
        p = p.saturating_add(1);
    }
    true
}

/// Find the first occurrence of `needle` (case-insensitive) at or after `from`.
fn find_ci(chars: &[char], needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from);
    }
    let mut p = from;
    while p < chars.len() {
        if starts_with_ci(chars, p, needle) {
            return Some(p);
        }
        p = p.saturating_add(1);
    }
    None
}

/// Find the first occurrence of `c` at or after `from`.
fn find_char(chars: &[char], c: char, from: usize) -> Option<usize> {
    let mut p = from;
    while p < chars.len() {
        if chars.get(p) == Some(&c) {
            return Some(p);
        }
        p = p.saturating_add(1);
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn headings_paragraphs_and_styles() {
        let blocks = parse("<h1>Title</h1><p>Hello <b>bold</b> and <i>italic</i>.</p>");
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
        let Block::Paragraph(spans) = &blocks[1] else {
            panic!("expected paragraph");
        };
        assert!(spans
            .iter()
            .any(|s| s.style == Style::Bold && s.text.contains("bold")));
        assert!(spans
            .iter()
            .any(|s| s.style == Style::Italic && s.text.contains("italic")));
    }

    #[test]
    fn lists_keep_order_and_nesting() {
        let blocks = parse("<ol><li>one</li><li>two<ul><li>inner</li></ul></li></ol>");
        let items: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 3);
        let Block::ListItem {
            ordered, ordinal, ..
        } = items[1]
        else {
            panic!("expected item");
        };
        assert!(*ordered);
        assert_eq!(*ordinal, 2);
        let Block::ListItem { ordered, depth, .. } = items[2] else {
            panic!("expected item");
        };
        assert!(!*ordered);
        assert_eq!(*depth, 1);
    }

    #[test]
    fn pre_blocks_keep_text_verbatim() {
        let blocks = parse("<pre><code>let x = 1;\nlet y = 2;</code></pre>");
        let Block::CodeBlock(lines) = &blocks[0] else {
            panic!("expected code block");
        };
        assert_eq!(lines, &["let x = 1;".to_string(), "let y = 2;".to_string()]);
    }

    #[test]
    fn entities_decode() {
        let blocks = parse("<p>AT&amp;T &mdash; caf&#233; &#x2014; done</p>");
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(joined.contains("AT&T"));
        assert!(joined.contains("caf\u{00E9}"));
        assert!(joined.contains('\u{2014}'));
    }

    #[test]
    fn scripts_and_unknown_tags_are_dropped_or_unwrapped() {
        let blocks = parse("<script>var x = 1;</script><div><span>visible</span></div>");
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(joined.contains("visible"));
        assert!(!joined.contains("var x"));
    }

    #[test]
    fn blockquotes_nest_blocks() {
        let blocks = parse("<blockquote><p>quoted</p></blockquote>");
        let Block::Blockquote(inner) = &blocks[0] else {
            panic!("expected blockquote");
        };
        assert!(matches!(&inner[0], Block::Paragraph(_)));
    }

    #[test]
    fn broken_markup_degrades_to_text() {
        let blocks = parse("<p>unclosed <b>bold runs on");
        assert_eq!(blocks.len(), 1);
        let Block::Paragraph(spans) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(joined.contains("bold runs on"));
    }
}
