//! A minimal XML scanner for the package and vector formats this crate reads
//! (EPUB's container and OPF parts, and the documented SVG subset).
//!
//! This is not a DOM: it tokenizes a byte slice into a flat list of elements
//! and text, without namespaces, DTD validation or schema checks. An EPUB a
//! user supplied is malformed XML often enough that a strict parser would
//! refuse books a reader opens without complaint, so every recovery rule here
//! is deliberate: comments, processing instructions and `DOCTYPE` are skipped,
//! entity references are decoded, and everything else surfaces to the caller
//! as an error with the line it stopped at.

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

/// One parsed XML element.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Element {
    /// The tag name, lower-cased for matching convenience. SVG and the OPF are
    /// both case-sensitive in their grammars, so both the original and the
    /// lowered form are carried; callers pick.
    pub tag: String,
    /// The attribute map, keys lower-cased (SVG presentation attributes and
    /// OPF attributes are all case-insensitive in practice), values decoded.
    pub attrs: Vec<(String, String)>,
    /// Text directly inside the element (entity-decoded).
    pub text: String,
    /// Child elements, in document order.
    pub children: Vec<Element>,
}

impl Element {
    /// The value of one attribute, matched by lower-cased name.
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Descendants whose tag equals `tag`, in document order.
    pub(crate) fn find_all<'a>(&'a self, tag: &str) -> Vec<&'a Element> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            if node.tag.eq_ignore_ascii_case(tag) {
                out.push(node);
            }
            for child in node.children.iter().rev() {
                stack.push(child);
            }
        }
        out
    }

    /// The first descendant whose tag equals `tag`.
    pub(crate) fn find_first<'a>(&'a self, tag: &'a str) -> Option<&'a Element> {
        self.find_all(tag).into_iter().next()
    }
}

/// Decode the standard XML entities and numeric references.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let Some(end) = tail.find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..end];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                } else if let Some(dec) = name.strip_prefix('#') {
                    dec.parse::<u32>().ok().and_then(char::from_u32)
                } else {
                    None
                }
            }
        };
        match decoded {
            Some(ch) => out.push(ch),
            None => {
                // An unknown entity is kept verbatim rather than dropped.
                out.push('&');
                out.push_str(name);
                out.push(';');
            }
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Parse an attribute list written inside a tag: `a="1" b='x' c=y`.
fn parse_attrs(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        // Skip separators.
        while at < bytes.len() && (bytes[at] as char).is_ascii_whitespace() {
            at += 1;
        }
        if at >= bytes.len() {
            break;
        }
        // Read the name.
        let name_start = at;
        while at < bytes.len() {
            let byte = bytes[at];
            if byte == b'=' || (byte as char).is_ascii_whitespace() {
                break;
            }
            at += 1;
        }
        let name = raw[name_start..at].trim().to_ascii_lowercase();
        if name.is_empty() {
            at += 1;
            continue;
        }
        // Skip whitespace and an optional `=`.
        while at < bytes.len() && (bytes[at] as char).is_ascii_whitespace() {
            at += 1;
        }
        if at >= bytes.len() || bytes[at] != b'=' {
            continue;
        }
        at += 1;
        while at < bytes.len() && (bytes[at] as char).is_ascii_whitespace() {
            at += 1;
        }
        // Read the quoted (or bare) value.
        let value = if at < bytes.len() && (bytes[at] == b'"' || bytes[at] == b'\'') {
            let quote = bytes[at];
            at += 1;
            let value_start = at;
            while at < bytes.len() && bytes[at] != quote {
                at += 1;
            }
            let value = &raw[value_start..at];
            at = at.saturating_add(1).min(bytes.len());
            value.to_string()
        } else {
            let value_start = at;
            while at < bytes.len() && !(bytes[at] as char).is_ascii_whitespace() {
                at += 1;
            }
            raw[value_start..at].to_string()
        };
        out.push((name, decode_entities(&value)));
    }
    out
}

/// Parse a whole document, returning the root element.
///
/// # Errors
///
/// A sentence naming the line at which the document stopped making sense.
pub(crate) fn parse(input: &str) -> Result<Element, String> {
    let mut root: Option<Element> = None;
    let mut stack: Vec<Element> = Vec::new();
    let mut line = 1usize;
    let bytes = input.as_bytes();
    let mut at = 0usize;

    let take_text =
        |input: &str, from: usize, to: usize| -> String { decode_entities(&input[from..to]) };

    while at < bytes.len() {
        if bytes[at] == b'<' {
            // Comments, declarations and processing instructions are skipped.
            if input[at..].starts_with("<!--") {
                let Some(end) = input[at..].find("-->") else {
                    return Err(format!("line {line}: an unterminated comment"));
                };
                line += input[at..at + end].matches('\n').count();
                at += end + 3;
                continue;
            }
            if input[at..].starts_with("<?") {
                let Some(end) = input[at..].find("?>") else {
                    return Err(format!(
                        "line {line}: an unterminated processing instruction"
                    ));
                };
                line += input[at..at + end].matches('\n').count();
                at += end + 2;
                continue;
            }
            if input[at..].starts_with("<!") {
                // DOCTYPE or a declaration; skip to the next `>`.
                let Some(end) = input[at..].find('>') else {
                    return Err(format!("line {line}: an unterminated declaration"));
                };
                line += input[at..at + end].matches('\n').count();
                at += end + 1;
                continue;
            }
            if input[at..].starts_with("</") {
                let Some(end) = input[at..].find('>') else {
                    return Err(format!("line {line}: an unterminated closing tag"));
                };
                let name = input[at + 2..at + end]
                    .trim()
                    .rsplit(':')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let Some(open) = stack.pop() else {
                    return Err(format!("line {line}: </{name}> closes nothing"));
                };
                if !open.tag.eq_ignore_ascii_case(&name) {
                    return Err(format!("line {line}: </{name}> closes <{}>", open.tag));
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(open),
                    None => {
                        if root.is_some() {
                            return Err(format!("line {line}: more than one root element"));
                        }
                        root = Some(open);
                    }
                }
                line += input[at..at + end].matches('\n').count();
                at += end + 1;
                continue;
            }
            // An opening or self-closing tag.
            let Some(end) = input[at..].find('>') else {
                return Err(format!("line {line}: an unterminated tag"));
            };
            let mut body = input[at + 1..at + end].trim().to_string();
            let self_closing = body.ends_with('/');
            if self_closing {
                body.pop();
            }
            let mut parts = body.splitn(2, char::is_whitespace);
            let raw_tag = parts.next().unwrap_or("");
            // Namespace prefixes are dropped: this engine matches by local
            // name, and neither the OPF nor the SVG subsets it reads ever
            // key on a prefix.
            let tag = raw_tag
                .rsplit(':')
                .next()
                .unwrap_or(raw_tag)
                .to_ascii_lowercase();
            if tag.is_empty() {
                return Err(format!("line {line}: an empty tag name"));
            }
            let attr_source = parts.next().unwrap_or("");
            let element = Element {
                tag,
                attrs: parse_attrs(attr_source),
                text: String::new(),
                children: Vec::new(),
            };
            if self_closing {
                match stack.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None => {
                        if root.is_some() {
                            return Err(format!("line {line}: more than one root element"));
                        }
                        root = Some(element);
                    }
                }
            } else {
                stack.push(element);
            }
            line += input[at..at + end].matches('\n').count();
            at += end + 1;
            continue;
        }
        // Text content.
        let next_lt = input[at..]
            .find('<')
            .map(|offset| at + offset)
            .unwrap_or(bytes.len());
        let text = take_text(input, at, next_lt);
        if let Some(open) = stack.last_mut() {
            open.text.push_str(&text);
        }
        line += input[at..next_lt].matches('\n').count();
        at = next_lt;
    }
    if let Some(open) = stack.pop() {
        return Err(format!("unterminated <{}> (line {line})", open.tag));
    }
    root.ok_or_else(|| "no root element found".to_string())
}

/// Percent-decode one URL path segment (%XX), as an OPF `href` carries it.
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[at + 1..at + 3], 16) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve `href` against the directory of the package part that names it.
pub(crate) fn resolve_href(base_dir: &str, href: &str) -> String {
    let decoded = percent_decode(href);
    if base_dir.is_empty() {
        return normalize_path(&decoded);
    }
    normalize_path(&format!("{base_dir}/{decoded}"))
}

fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                let _ = parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn attributes_and_entities_round_trip() {
        let element = parse("<root a=\"1 &amp; 2\" b='x&lt;y'/>").expect("parse");
        assert_eq!(element.attr("a"), Some("1 & 2"));
        assert_eq!(element.attr("b"), Some("x<y"));
    }

    #[test]
    fn children_find_in_document_order() {
        let element =
            parse("<root><a x=\"1\"/><b><a x=\"2\"/></b><a x=\"3\"/></root>").expect("parse");
        let found: Vec<_> = element
            .find_all("a")
            .into_iter()
            .map(|node| node.attr("x").unwrap().to_string())
            .collect();
        assert_eq!(found, vec!["1", "2", "3"]);
    }

    #[test]
    fn a_mismatched_close_is_an_error() {
        let err = parse("<a><b></a></b>").expect_err("refused");
        assert!(err.contains("closes <b>"), "{err}");
    }

    #[test]
    fn percent_decoding_and_relative_resolution() {
        assert_eq!(percent_decode("chap%201.xhtml"), "chap 1.xhtml");
        assert_eq!(
            resolve_href("OEBPS/text", "../images/p.png"),
            "OEBPS/images/p.png"
        );
        assert_eq!(resolve_href("", "a/b.html"), "a/b.html");
    }
}
