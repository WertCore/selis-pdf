//! Form-data import: XFDF and FDF files into field edits.
//!
//! A reader who fills a form in one place and wants to bring the values into
//! a PDF elsewhere needs a file the two ends agree on. XFDF (Adobe's XML) and
//! FDF (the PDF-derived form format) are the two everyone reads. This module
//! turns either into the [`FieldEdit`] list the form writer already takes —
//! the writer's round-trip check and refusals apply unchanged.
//!
//! Only field VALUES are imported. An XFDF `<annots>` section is markup, not
//! form data; the annotation crate owns that, and this module ignores it
//! rather than half-applying it.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]

use super::{FieldEdit, MAX_FIELD_NAME_BYTES, MAX_FIELD_VALUE_BYTES, MAX_FORM_EDITS};
use std::fmt;

/// Why a form-data file could not be imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The bytes are not UTF-8 XML.
    NotXml,
    /// The XML is well-formed enough to read but not an XFDF document.
    NotXfdf,
    /// The bytes are not an FDF document.
    NotFdf,
    /// A field had no name or a value the writer would refuse.
    InvalidField(String),
    /// Too many fields, or one too long.
    Limit(String),
    /// An unsupported construct, named honestly.
    Unsupported(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotXml => f.write_str("the form data is not UTF-8 XML"),
            Self::NotXfdf => f.write_str("the XML is not an XFDF document"),
            Self::NotFdf => f.write_str("the file is not an FDF document"),
            Self::InvalidField(message) => write!(f, "invalid form field: {message}"),
            Self::Limit(message) => write!(f, "form-data limit: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported form data: {message}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// Decode XML character references in an attribute value (`&amp;`, `&lt;`,
/// `&gt;`, `&quot;`, `&apos;`, and numeric `&#…;`/`&#x…;`).
fn decode_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find(';') else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..end];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                if let Ok(code) = u32::from_str_radix(&entity[2..], 16) {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
            }
            _ if entity.starts_with('#') => {
                if let Ok(code) = entity[1..].parse::<u32>() {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
            }
            _ => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Read one attribute's value from a tag's raw text.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let at = tag.find(&needle)?;
    let after = &tag[at + needle.len()..];
    let after = after.trim_start();
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &after[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(decode_entities(&rest[..end]))
}

fn checked_edit(name: String, value: String) -> Result<FieldEdit, ImportError> {
    if name.is_empty() {
        return Err(ImportError::InvalidField("an empty name".to_string()));
    }
    if name.len() > MAX_FIELD_NAME_BYTES {
        return Err(ImportError::Limit(format!(
            "field name longer than {MAX_FIELD_NAME_BYTES} bytes"
        )));
    }
    if value.len() > MAX_FIELD_VALUE_BYTES {
        return Err(ImportError::Limit(format!(
            "value longer than {MAX_FIELD_VALUE_BYTES} bytes"
        )));
    }
    if value.contains('\0') {
        return Err(ImportError::InvalidField(format!(
            "the value for {name:?} contains a NUL"
        )));
    }
    Ok(FieldEdit {
        name,
        value,
        values: Vec::new(),
    })
}

/// Import field values from XFDF XML.
///
/// # Errors
///
/// [`ImportError`] when the bytes are not XFDF, or a field is invalid.
pub fn import_xfdf(xml: &str) -> Result<Vec<FieldEdit>, ImportError> {
    let trimmed = xml.trim_start();
    // An XML declaration, comments and whitespace may precede the root.
    let root_at = trimmed.find("<xfdf").ok_or(ImportError::NotXfdf)?;
    let root = &trimmed[root_at..];
    if !root.starts_with("<xfdf") {
        return Err(ImportError::NotXfdf);
    }
    // Close of the root tag.
    let head_end = root.find('>').ok_or(ImportError::NotXfdf)?;
    let head = &root[..head_end];
    if attribute(head, "xmlns").is_none() {
        return Err(ImportError::NotXfdf);
    }
    let mut edits = Vec::new();
    let mut rest = &root[head_end + 1..];
    // Walk every <field ...> element. XFDF fields can nest via <fields ...>;
    // a child's name inherits its ancestors, joined with '.', exactly as
    // Acrobat writes hierarchical field names.
    let mut stack: Vec<String> = Vec::new();
    while let Some(at) = rest.find('<') {
        rest = &rest[at..];
        let end = rest.find('>').ok_or(ImportError::NotXfdf)?;
        let tag = &rest[..end + 1];
        let name = tag.trim_start_matches('<');
        if name.starts_with("/xfdf") {
            break;
        }
        let field_tag =
            name.starts_with("field ") || name.starts_with("field>") || name.starts_with("field/>");
        if field_tag {
            let local = attribute(tag, "name").ok_or_else(|| {
                ImportError::InvalidField("an XFDF <field> without a name".to_string())
            })?;
            let full = if stack.is_empty() {
                local
            } else {
                format!("{}.{}", stack.join("."), local)
            };
            if let Some(value) = attribute(tag, "value") {
                edits.push(checked_edit(full.clone(), value)?);
            }
            // A self-closing <field .../> has no children.
            if tag.trim_end().ends_with("/>") {
                rest = &rest[end + 1..];
                continue;
            }
            stack.push(full);
        } else if name.starts_with("/field") {
            stack.pop();
        }
        rest = &rest[end + 1..];
        if edits.len() > MAX_FORM_EDITS {
            return Err(ImportError::Limit(format!(
                "more than {MAX_FORM_EDITS} fields"
            )));
        }
    }
    if edits.is_empty() {
        return Err(ImportError::Unsupported(
            "the XFDF carries no field values".to_string(),
        ));
    }
    Ok(edits)
}

/// Import field values from FDF bytes.
///
/// FDF is a PDF-like syntax: `/F (name) /V (value)` inside `/Fields [...]`.
/// This reads the value pairs the same way the form writer writes them: a
/// name, a value, and nothing else. Streams, /AP and signatures are refused
/// rather than silently ignored.
///
/// # Budget
///
/// A linear scan of the input, bounded by the field and value limits.
///
/// # Malformed Input
///
/// Bytes that are not FDF, or a field the writer would refuse, produce an
/// [`ImportError`]; nothing is partially imported.
///
/// # Errors
///
/// [`ImportError`] when the bytes are not FDF, or a field is invalid.
pub fn import_fdf(bytes: &[u8]) -> Result<Vec<FieldEdit>, ImportError> {
    if !bytes.starts_with(b"%FDF") {
        return Err(ImportError::NotFdf);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ImportError::NotFdf)?;
    if !text.contains("/FDF") {
        return Err(ImportError::NotFdf);
    }
    if text.contains("/AP ") || text.contains("/XFA") {
        return Err(ImportError::Unsupported(
            "FDF appearances and XFA packets are not imported".to_string(),
        ));
    }
    let mut edits = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0usize;
    while let Some(relative) = find_from(bytes, b"/T", at) {
        at = relative + 2;
        let Some(name) = read_pdf_string(bytes, at) else {
            continue;
        };
        at = name.end;
        // The value may come after the name, or before it; find the nearest
        // /V within this field's dictionary (before the next /T).
        let next_t = find_from(bytes, b"/T", at).unwrap_or(bytes.len());
        if let Some(value_at) = find_from(bytes, b"/V", at).filter(|v| *v < next_t) {
            if let Some(value) = read_pdf_value(bytes, value_at + 2) {
                edits.push(checked_edit(decode_entities(&name.text), value.text)?);
                if edits.len() > MAX_FORM_EDITS {
                    return Err(ImportError::Limit(format!(
                        "more than {MAX_FORM_EDITS} fields"
                    )));
                }
            }
        }
    }
    if edits.is_empty() {
        return Err(ImportError::Unsupported(
            "the FDF carries no field values".to_string(),
        ));
    }
    Ok(edits)
}

struct ParsedString {
    text: String,
    end: usize,
}

fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

fn read_pdf_value(bytes: &[u8], from: usize) -> Option<ParsedString> {
    let mut at = from;
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if at >= bytes.len() {
        return None;
    }
    if bytes[at] == b'(' {
        read_pdf_string(bytes, at)
    } else if bytes[at] == b'/' {
        at += 1;
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        Some(ParsedString {
            text: String::from_utf8_lossy(&bytes[start..at]).into_owned(),
            end: at,
        })
    } else {
        None
    }
}

fn read_pdf_string(bytes: &[u8], from: usize) -> Option<ParsedString> {
    let mut at = from;
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if bytes.get(at) != Some(&b'(') {
        return None;
    }
    at += 1;
    let mut out = Vec::new();
    let mut depth = 1usize;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if at + 1 < bytes.len() => {
                at += 1;
                match bytes[at] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'\\' | b'(' | b')' => out.push(bytes[at]),
                    other => out.push(other),
                }
                at += 1;
            }
            b'(' => {
                depth += 1;
                out.push(b'(');
                at += 1;
            }
            b')' => {
                depth -= 1;
                at += 1;
                if depth == 0 {
                    return Some(ParsedString {
                        text: String::from_utf8_lossy(&out).into_owned(),
                        end: at,
                    });
                }
                out.push(b')');
            }
            other => {
                out.push(other);
                at += 1;
            }
        }
    }
    None
}
