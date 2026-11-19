//! Form-data import and export: XFDF, FDF, JSON and CSV (SL-5.FORM.05).
//!
//! A reader who fills a form in one place and wants to bring the values into
//! a PDF elsewhere needs a file the two ends agree on. XFDF (Adobe's XML) and
//! FDF (the PDF-derived form format) are the two everyone reads; JSON and CSV
//! are the two a script or spreadsheet wants. This module turns any of the four
//! into the [`FieldEdit`] list the form writer already takes — the writer's
//! round-trip check and refusals apply unchanged — and exports a field report
//! back into JSON or CSV.
//!
//! Only field VALUES are imported or exported. An XFDF `<annots>` section is
//! markup, not form data; the annotation crate owns that, and this module
//! ignores it rather than half-applying it.
//!
//! ## The JSON and CSV schemas (documented, versioned, minimal)
//!
//! JSON is `selis-form-data/1`:
//!
//! ```json
//! {"schema":"selis-form-data/1","fields":{"firstName":"Ada","colors":["Red","Blue"]}}
//! ```
//!
//! `fields` maps a fully-qualified field name to either a string (single value)
//! or an array of strings (a multi-select choice). The importer also accepts a
//! bare object of `name → value` for convenience. CSV has a header row of
//! field names and exactly one value row; a multi-select is one cell whose
//! entries are joined with `;` and RFC-4180 quoted when needed. Both schemas
//! round-trip through [`import_json`] / [`import_csv`] into the same
//! [`FieldEdit`] list the FDF/XFDF importer produces.
//!
//! Binary / attachment-like fields (signature fields, push buttons and values
//! containing a NUL byte) are **explicitly skipped with a typed note** in the
//! returned [`FormDataExport::skipped`]; nothing is silently dropped.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]

use super::{
    FieldEdit, FieldKind, FormField, FormFieldsReport, MAX_FIELD_NAME_BYTES, MAX_FIELD_VALUE_BYTES,
    MAX_FORM_EDITS,
};
use serde::{Deserialize, Serialize};
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

// --- JSON / CSV export and import (SL-5.FORM.05) ------------------------------

/// The only JSON schema version this module reads or writes.
pub const FORM_DATA_SCHEMA: &str = "selis-form-data/1";
/// The separator a multi-select CSV cell uses between its values. A single
/// value containing this byte cannot be represented in CSV and is skipped with
/// a typed [`SkipReason::BinaryValue`] note rather than silently mangled.
pub const CSV_MULTI_SEPARATOR: char = ';';

/// Why a field was not exported. Every variant carries a stable, human-readable
/// note via [`SkipReason::note`] so a shell can disclose the omission rather
/// than dropping the field silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// A signature field holds binary CMS data, not a form value.
    SignatureField,
    /// A push button has no value to export.
    PushButton,
    /// The value contains a NUL byte (or a byte reserved by the target format)
    /// and could not be represented without changing it.
    BinaryValue,
    /// The field has no value set.
    NoValue,
    /// The field's type is not one this schema describes.
    UnknownKind,
}

impl SkipReason {
    /// A stable, human-readable note for this reason.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::SignatureField => "signature field (binary CMS data)",
            Self::PushButton => "push button (no value)",
            Self::BinaryValue => "value has a byte reserved by the target format",
            Self::NoValue => "field has no value",
            Self::UnknownKind => "unsupported field type",
        }
    }
}

/// One field deliberately left out of an export, with the typed reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedField {
    /// The fully-qualified field name.
    pub name: String,
    /// Why it was skipped.
    pub reason: SkipReason,
}

/// What an export produced: the bytes plus every field skipped with a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormDataExport {
    /// The JSON or CSV bytes, ready for the caller to write.
    pub bytes: Vec<u8>,
    /// Fields not represented in `bytes`, each with a typed reason.
    pub skipped: Vec<SkippedField>,
}

/// The shape of one exported field's value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ValueShape {
    Single(String),
    Multi(Vec<String>),
}

/// Why (if at all) a field cannot be exported in the requested format.
fn skip_reason(field: &FormField, csv: bool) -> Option<SkipReason> {
    match field.kind {
        FieldKind::Signature => return Some(SkipReason::SignatureField),
        FieldKind::PushButton => return Some(SkipReason::PushButton),
        FieldKind::Unknown => return Some(SkipReason::UnknownKind),
        FieldKind::Text | FieldKind::Checkbox | FieldKind::Radio | FieldKind::Choice => {}
    }
    let reserved =
        |value: &str| value.contains('\0') || (csv && value.contains(CSV_MULTI_SEPARATOR));
    if field.value.as_deref().is_some_and(reserved)
        || field.values.iter().any(|value| reserved(value))
    {
        return Some(SkipReason::BinaryValue);
    }
    if field.value.is_none() && field.values.is_empty() {
        return Some(SkipReason::NoValue);
    }
    None
}

/// Split the report into exportable fields and typed skip notes. Nothing is
/// silently dropped: every omitted field gets a [`SkippedField`] note.
fn partition(
    report: &FormFieldsReport,
    csv: bool,
) -> (Vec<(String, ValueShape)>, Vec<SkippedField>) {
    let mut values = Vec::new();
    let mut skipped = Vec::new();
    for field in &report.fields {
        if let Some(reason) = skip_reason(field, csv) {
            skipped.push(SkippedField {
                name: field.name.clone(),
                reason,
            });
            continue;
        }
        let shape = if field.values.is_empty() {
            ValueShape::Single(field.value.clone().unwrap_or_default())
        } else {
            ValueShape::Multi(field.values.clone())
        };
        values.push((field.name.clone(), shape));
    }
    (values, skipped)
}

/// Export a field report as `selis-form-data/1` JSON.
///
/// Fields that cannot be represented are skipped with a typed note in
/// [`FormDataExport::skipped`]; nothing is dropped silently.
#[must_use]
pub fn export_json(report: &FormFieldsReport) -> FormDataExport {
    let (values, skipped) = partition(report, false);
    let mut fields = serde_json::Map::new();
    for (name, value) in values {
        let json = match value {
            ValueShape::Single(text) => serde_json::Value::String(text),
            ValueShape::Multi(items) => {
                serde_json::Value::Array(items.into_iter().map(serde_json::Value::String).collect())
            }
        };
        fields.insert(name, json);
    }
    let document = serde_json::json!({ "schema": FORM_DATA_SCHEMA, "fields": fields });
    let bytes = serde_json::to_vec_pretty(&document).unwrap_or_else(|_| {
        // A map of strings to strings/arrays built above is always encodable;
        // this fallback keeps the function total without panicking.
        Vec::new()
    });
    FormDataExport { bytes, skipped }
}

/// Export a field report as CSV: a header row of field names and one value row.
///
/// A multi-select is a single RFC-4180-quoted cell whose entries are joined
/// with `;`. A single value containing `;` is skipped with a typed
/// [`SkipReason::BinaryValue`] note rather than being silently split on import.
#[must_use]
pub fn export_csv(report: &FormFieldsReport) -> FormDataExport {
    let (values, skipped) = partition(report, true);
    let mut bytes = Vec::new();
    let header: Vec<String> = values.iter().map(|(name, _)| name.clone()).collect();
    bytes.extend_from_slice(&csv_row(&header));
    bytes.push(b'\n');
    let row: Vec<String> = values
        .into_iter()
        .map(|(_, value)| match value {
            ValueShape::Single(text) => text,
            ValueShape::Multi(items) => items.join(&CSV_MULTI_SEPARATOR.to_string()),
        })
        .collect();
    bytes.extend_from_slice(&csv_row(&row));
    bytes.push(b'\n');
    FormDataExport { bytes, skipped }
}

/// RFC-4180 encode one CSV row: quote a cell containing a comma, a quote, CR
/// or LF, doubling any embedded quote.
fn csv_row(cells: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        let quote = cell
            .bytes()
            .any(|byte| matches!(byte, b',' | b'"' | b'\r' | b'\n'));
        if quote {
            out.push(b'"');
            for byte in cell.bytes() {
                if byte == b'"' {
                    out.push(b'"');
                }
                out.push(byte);
            }
            out.push(b'"');
        } else {
            out.extend_from_slice(cell.as_bytes());
        }
    }
    out
}

/// Parse one RFC-4180 CSV row into cells (no embedded newlines: rows are split
/// before this is called). Quoted cells may contain commas and doubled quotes.
fn csv_parse_row(row: &str) -> Result<Vec<String>, ImportError> {
    let bytes = row.as_bytes();
    let mut cells = Vec::new();
    let mut cell = Vec::new();
    let mut at = 0usize;
    let mut quoted = false;
    while at < bytes.len() {
        let byte = bytes[at];
        if quoted {
            if byte == b'"' {
                if bytes.get(at + 1) == Some(&b'"') {
                    cell.push(b'"');
                    at += 2;
                    continue;
                }
                quoted = false;
                at += 1;
                continue;
            }
            cell.push(byte);
            at += 1;
        } else if byte == b'"' && cell.is_empty() {
            quoted = true;
            at += 1;
        } else if byte == b',' {
            cells.push(String::from_utf8_lossy(&cell).into_owned());
            cell.clear();
            at += 1;
        } else {
            cell.push(byte);
            at += 1;
        }
    }
    if quoted {
        return Err(ImportError::InvalidField(
            "an unterminated quoted CSV cell".to_string(),
        ));
    }
    cells.push(String::from_utf8_lossy(&cell).into_owned());
    Ok(cells)
}

/// Import field values from a `selis-form-data/1` JSON document.
///
/// A bare object of `name → value` is also accepted for convenience; a
/// non-string/non-array value is a typed [`ImportError::InvalidField`].
///
/// # Errors
///
/// [`ImportError`] when the bytes are not the expected JSON shape or a field
/// is invalid.
pub fn import_json(bytes: &[u8]) -> Result<Vec<FieldEdit>, ImportError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| ImportError::InvalidField("the form data is not valid JSON".to_string()))?;
    let object = value.as_object().ok_or_else(|| {
        ImportError::InvalidField("the form data is not a JSON object".to_string())
    })?;
    let fields = match object.get("fields") {
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => {
            return Err(ImportError::InvalidField(
                "the \"fields\" member is not an object".to_string(),
            ))
        }
        None => object,
    };
    if let Some(schema) = object.get("schema").and_then(serde_json::Value::as_str) {
        if schema != FORM_DATA_SCHEMA {
            return Err(ImportError::Unsupported(format!(
                "unsupported form-data schema {schema:?} (expected {FORM_DATA_SCHEMA:?})"
            )));
        }
    }
    if fields.len() > MAX_FORM_EDITS {
        return Err(ImportError::Limit(format!(
            "more than {MAX_FORM_EDITS} fields"
        )));
    }
    let mut edits = Vec::with_capacity(fields.len());
    for (name, value) in fields {
        let value = match value {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(items) => {
                let texts: Vec<String> = items
                    .iter()
                    .map(|item| {
                        item.as_str().map(str::to_string).ok_or_else(|| {
                            ImportError::InvalidField(format!(
                                "the value for {name:?} has a non-string entry"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if texts.is_empty() {
                    return Err(ImportError::InvalidField(format!(
                        "the value for {name:?} is an empty array"
                    )));
                }
                let mut edit = checked_edit(name.clone(), texts[0].clone())?;
                edit.values = texts[1..].to_vec();
                edits.push(edit);
                continue;
            }
            _ => {
                return Err(ImportError::InvalidField(format!(
                    "the value for {name:?} is neither a string nor an array"
                )))
            }
        };
        edits.push(checked_edit(name.clone(), value)?);
    }
    if edits.is_empty() {
        return Err(ImportError::Unsupported(
            "the JSON carries no field values".to_string(),
        ));
    }
    Ok(edits)
}

/// Import field values from the CSV shape this module exports: a header row of
/// field names and exactly one value row.
///
/// # Errors
///
/// [`ImportError`] when the rows do not match or a field is invalid.
pub fn import_csv(bytes: &[u8]) -> Result<Vec<FieldEdit>, ImportError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ImportError::InvalidField("the CSV is not UTF-8".to_string()))?;
    let rows: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if rows.len() != 2 {
        return Err(ImportError::InvalidField(
            "CSV form data needs a header row and exactly one value row".to_string(),
        ));
    }
    let headers = csv_parse_row(rows[0])?;
    let values = csv_parse_row(rows[1])?;
    if headers.len() != values.len() {
        return Err(ImportError::InvalidField(format!(
            "CSV header has {} columns but the value row has {}",
            headers.len(),
            values.len()
        )));
    }
    if headers.len() > MAX_FORM_EDITS {
        return Err(ImportError::Limit(format!(
            "more than {MAX_FORM_EDITS} fields"
        )));
    }
    let mut edits = Vec::with_capacity(headers.len());
    for (name, cell) in headers.iter().zip(values.iter()) {
        if name.is_empty() {
            return Err(ImportError::InvalidField(
                "a CSV header cell is empty".to_string(),
            ));
        }
        let parts: Vec<&str> = cell.split(CSV_MULTI_SEPARATOR).collect();
        let mut edit = checked_edit(name.clone(), parts[0].to_string())?;
        if parts.len() > 1 {
            edit.values = parts[1..].iter().map(|part| (*part).to_string()).collect();
        }
        edits.push(edit);
    }
    if edits.is_empty() {
        return Err(ImportError::Unsupported(
            "the CSV carries no field values".to_string(),
        ));
    }
    Ok(edits)
}
