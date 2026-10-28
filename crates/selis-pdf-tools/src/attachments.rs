//! Embedded-file extraction for the browser and CLI tools.
//!
//! The operation is deliberately read-only: it walks the catalog's
//! `/Names /EmbeddedFiles` name tree, decodes each file stream, and returns
//! the bytes with a sanitized, collision-free display name. Nothing is
//! executed, opened, or written to the filesystem on this path.
//!
//! Extraction refuses encrypted documents rather than returning ciphertext.
//! Removing encryption belongs to the separate unlock operation.

use std::collections::HashSet;

use selis_bytes::Bytes;
use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::reconcile::{catalog_entry, name_tree_pairs};
use selis_pdf_cos::{self, Obj, Ref};
use selis_pdf_filter::DecodeParms;
use selis_sandbox::{Budget, BudgetGuard};

use crate::{read_file, CliError, CliResult};

/// The most embedded files one request may extract.
pub const MAX_ATTACHMENTS: usize = 64;
/// The most decoded bytes one attachment may contribute.
pub const MAX_ATTACHMENT_BYTES: u64 = 256 * 1024 * 1024;
/// The most decoded bytes across all attachments in one request.
pub const MAX_TOTAL_ATTACHMENT_BYTES: u64 = 256 * 1024 * 1024;

/// One decoded embedded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAttachment {
    /// A safe, flat display/download name.
    pub name: String,
    /// The decoded file bytes.
    pub bytes: Vec<u8>,
}

/// Extract every embedded file from `path`.
///
/// Names prefer `/UF`, then `/F`; path separators and traversal markers are
/// removed. Duplicate names receive `-2`, `-3`, and so on. A document with no
/// extractable embedded files is refused rather than producing an empty
/// result.
///
/// # Errors
///
/// [`ToolError`](crate::ToolError) when the document cannot be parsed, is
/// encrypted, has no embedded files, exceeds the count/size limits, or an
/// attachment stream cannot be decoded.
pub fn extract(path: &str) -> CliResult<Vec<ExtractedAttachment>> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let revision = doc
        .revisions()
        .last()
        .ok_or_else(|| CliError(format!("{path}: no revisions")))?;
    if revision.encrypt.is_some() {
        return Err(CliError(format!(
            "{path}: document is encrypted; remove the password with unlock first"
        )));
    }

    let names = catalog_entry(&src, &doc, b"Names", &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot read /Names: {e}")))?
        .ok_or_else(|| CliError(format!("{path}: no embedded files")))?;
    let names = resolve_obj(&src, &doc, &names, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot read /Names: {e}")))?;
    let embedded = dict_get(&names, b"EmbeddedFiles")
        .ok_or_else(|| CliError(format!("{path}: no embedded files")))?;
    let entries = name_tree_pairs(embedded, &src, &doc, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot read the embedded-file tree: {e}")))?;
    if entries.is_empty() {
        return Err(CliError(format!("{path}: no embedded files")));
    }
    if entries.len() > MAX_ATTACHMENTS {
        return Err(CliError(format!(
            "{path}: {} embedded files exceed the {MAX_ATTACHMENTS}-file limit",
            entries.len()
        )));
    }

    let mut used = HashSet::new();
    let mut total = 0u64;
    let mut out = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let key_name = pdf_text(&key);
        let filespec = resolve_obj(&src, &doc, &value, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: cannot read an embedded-file entry: {e}")))?;
        let Some(stream_ref) = filespec_stream_ref(&src, &doc, &filespec, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: cannot read an embedded-file entry: {e}")))?
        else {
            // A Filespec with no /EF /F is inventory-only, not extractable.
            continue;
        };
        let stream = resolve_ref(&src, &doc, stream_ref, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: cannot resolve an embedded file: {e}")))?;
        let Obj::Stream { dict, data } = &stream else {
            continue;
        };

        let filters = filters_of(dict, &src, &doc, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: cannot read attachment filters: {e}")))?;
        let parms = parms_of(dict, &src, &doc, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: cannot read attachment parameters: {e}")))?;
        let decoded = if filters.is_empty() {
            data.to_vec()
        } else {
            selis_pdf_filter::decode_chain(
                &filters,
                &parms,
                data.as_slice(),
                MAX_ATTACHMENT_BYTES,
                &mut g,
            )
            .map_err(|e| {
                CliError(format!(
                    "{path}: cannot decode embedded file {}: {e}",
                    display_name(&filespec, &key_name)
                ))
            })?
        };
        let size = u64::try_from(decoded.len()).unwrap_or(u64::MAX);
        if size > MAX_ATTACHMENT_BYTES {
            return Err(CliError(format!(
                "{path}: an embedded file exceeds the {MAX_ATTACHMENT_BYTES}-byte limit"
            )));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_ATTACHMENT_BYTES {
            return Err(CliError(format!(
                "{path}: embedded files exceed the {MAX_TOTAL_ATTACHMENT_BYTES}-byte total limit"
            )));
        }

        let raw_name = display_name(&filespec, &key_name);
        let name = safe_name(&raw_name, &mut used);
        out.push(ExtractedAttachment {
            name,
            bytes: decoded,
        });
    }

    if out.is_empty() {
        return Err(CliError(format!(
            "{path}: no extractable embedded files (entries have no /EF /F stream)"
        )));
    }
    Ok(out)
}

fn resolve_obj(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    obj: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Obj> {
    match obj {
        Obj::Ref(reference) => {
            resolve_ref(src, doc, *reference, budget, g).map_err(|e| CliError(e.to_string()))
        }
        other => Ok(other.clone()),
    }
}

fn dict_get<'a>(obj: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match obj {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(name, _)| name.as_slice() == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn filespec_stream_ref(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    filespec: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Option<Ref>> {
    let Some(ef) = dict_get(filespec, b"EF") else {
        return Ok(None);
    };
    let ef = resolve_obj(src, doc, ef, budget, g)?;
    for key in [b"F".as_slice(), b"UF".as_slice()] {
        if let Some(Obj::Ref(reference)) = dict_get(&ef, key) {
            return Ok(Some(*reference));
        }
    }
    Ok(None)
}

fn display_name(filespec: &Obj, key: &str) -> String {
    for name in [b"UF".as_slice(), b"F".as_slice(), b"Name".as_slice()] {
        if let Some(Obj::String(value)) = dict_get(filespec, name) {
            let text = pdf_text(value);
            if !text.is_empty() {
                return text;
            }
        }
    }
    if key.is_empty() {
        "attachment".to_string()
    } else {
        key.to_string()
    }
}

fn pdf_text(value: &Bytes) -> String {
    let bytes = value.as_slice();
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| {
                let [high, low] = pair else {
                    return 0;
                };
                u16::from_be_bytes([*high, *low])
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| {
                let [low, high] = pair else {
                    return 0;
                };
                u16::from_le_bytes([*low, *high])
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn stream_dict_get<'a>(dict: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    dict.iter()
        .find(|(name, _)| name.as_slice() == key)
        .map(|(_, value)| value)
}

fn filters_of(
    dict: &[(Bytes, Obj)],
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Vec<String>> {
    let Some(filter) = stream_dict_get(dict, b"Filter") else {
        return Ok(Vec::new());
    };
    let filter = resolve_obj(src, doc, filter, budget, g)?;
    match filter {
        Obj::Name(name) => Ok(vec![String::from_utf8_lossy(name.as_slice()).into_owned()]),
        Obj::Array(items) => {
            let mut filters = Vec::with_capacity(items.len());
            for item in &items {
                let item = resolve_obj(src, doc, item, budget, g)?;
                let Obj::Name(name) = item else {
                    return Err(CliError(
                        "an embedded file has a non-name /Filter entry".to_string(),
                    ));
                };
                filters.push(String::from_utf8_lossy(name.as_slice()).into_owned());
            }
            Ok(filters)
        }
        Obj::Null => Ok(Vec::new()),
        _ => Err(CliError(
            "an embedded file has a /Filter that is not a name or array".to_string(),
        )),
    }
}

fn parms_of(
    dict: &[(Bytes, Obj)],
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Vec<DecodeParms>> {
    let Some(parms) = stream_dict_get(dict, b"DecodeParms") else {
        return Ok(Vec::new());
    };
    let parms = resolve_obj(src, doc, parms, budget, g)?;
    match parms {
        Obj::Array(items) => Ok(items.iter().map(parms_from_obj).collect()),
        other => Ok(vec![parms_from_obj(&other)]),
    }
}

fn parms_from_obj(obj: &Obj) -> DecodeParms {
    if !matches!(obj, Obj::Dict(_)) {
        return DecodeParms::default();
    }
    let int = |key: &[u8]| -> Option<i64> {
        dict_get(obj, key).and_then(|value| match value {
            Obj::Int(number) => Some(*number),
            _ => None,
        })
    };
    DecodeParms {
        predictor: u16::try_from(int(b"Predictor").unwrap_or(1)).unwrap_or(1),
        columns: u32::try_from(int(b"Columns").unwrap_or(1)).unwrap_or(1),
        colors: u32::try_from(int(b"Colors").unwrap_or(1)).unwrap_or(1),
        bits_per_component: u32::try_from(int(b"BitsPerComponent").unwrap_or(8)).unwrap_or(8),
        early_change: u8::try_from(int(b"EarlyChange").unwrap_or(1)).unwrap_or(1),
    }
}

fn safe_name(raw: &str, used: &mut HashSet<String>) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let mut cleaned = String::new();
    for ch in base.chars() {
        if ch.is_control() || matches!(ch, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            cleaned.push('_');
        } else if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.' | ' ') {
            cleaned.push(ch);
        } else {
            cleaned.push('_');
        }
    }
    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", ".");
    }
    let cleaned = cleaned.trim().trim_matches('.').trim();
    let mut stem = if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned.to_string()
    };
    let reserved = stem.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        reserved.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        stem.insert(0, '_');
    }
    let (stem, extension) = match stem.rsplit_once('.') {
        Some((base, ext)) if !base.is_empty() && !ext.is_empty() => {
            (base.to_string(), format!(".{ext}"))
        }
        _ => (stem, String::new()),
    };
    let stem = truncate_chars(&stem, 100);
    let extension = truncate_chars(&extension, 20);
    let mut attempt = 1usize;
    loop {
        let candidate = if attempt == 1 {
            format!("{stem}{extension}")
        } else {
            format!("{stem}-{attempt}{extension}")
        };
        if used.insert(candidate.to_ascii_lowercase()) {
            return candidate;
        }
        attempt = attempt.saturating_add(1);
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_input, MemorySession};

    #[test]
    fn extracts_decoded_files_with_safe_unique_names() {
        let files = extract_fixture("extract", fixture_pdf()).expect("extract");

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "evil.txt");
        assert_eq!(files[0].bytes, b"safe name");
        assert_eq!(files[1].name, "notes.txt");
        assert_eq!(files[1].bytes, b"hello attachment");
    }

    #[test]
    fn no_embedded_files_is_an_error() {
        let error = extract_fixture("empty", minimal_pdf()).expect_err("no attachments");
        assert!(error.to_string().contains("no embedded files"), "{error}");
    }

    #[test]
    fn encrypted_documents_are_refused() {
        let mut bytes = fixture_pdf();
        let trailer_at = bytes
            .windows(b"trailer".len())
            .position(|window| window == b"trailer")
            .expect("trailer");
        let insert_at = trailer_at + b"trailer\n<<".len();
        bytes.splice(insert_at..insert_at, b" /Encrypt 99 0 R".iter().copied());
        let error = extract_fixture("encrypted", bytes).expect_err("encrypted input");
        assert!(error.to_string().contains("encrypted"), "{error}");
    }

    #[test]
    fn safe_names_strip_paths_reserved_names_and_collisions() {
        let mut used = HashSet::new();
        assert_eq!(safe_name("../../evil.txt", &mut used), "evil.txt");
        assert_eq!(safe_name("evil.txt", &mut used), "evil-2.txt");
        assert_eq!(safe_name("CON.txt", &mut used), "_CON.txt");
        assert_eq!(safe_name("..", &mut used), "attachment");
        assert_eq!(safe_name("a:b?.txt", &mut used), "a_b_.txt");
    }

    fn extract_fixture(label: &str, pdf: Vec<u8>) -> CliResult<Vec<ExtractedAttachment>> {
        let _session = MemorySession::activate();
        let key = format!("attachments-test/{label}.pdf");
        insert_input(&key, pdf);
        extract(&key)
    }

    fn minimal_pdf() -> Vec<u8> {
        build_pdf(vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n".to_vec(),
        ])
    }

    fn fixture_pdf() -> Vec<u8> {
        let mut objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 4 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n"
                .to_vec(),
            b"4 0 obj\n<< /EmbeddedFiles 5 0 R >>\nendobj\n".to_vec(),
            b"5 0 obj\n<< /Names [(../../evil.txt) 6 0 R (notes.txt) 7 0 R] >>\nendobj\n"
                .to_vec(),
            b"6 0 obj\n<< /Type /Filespec /UF (../../evil.txt) /F (../../evil.txt) /EF << /F 8 0 R >> >>\nendobj\n"
                .to_vec(),
            b"7 0 obj\n<< /Type /Filespec /UF (notes.txt) /F (notes.txt) /EF << /F 9 0 R >> >>\nendobj\n"
                .to_vec(),
        ];
        let safe = b"73616665206e616d65>";
        objects.push(stream_object(
            8,
            &format!(
                "<< /Type /EmbeddedFile /Filter 10 0 R /Length {} >>",
                safe.len()
            ),
            safe,
        ));
        let notes = b"hello attachment";
        objects.push(stream_object(
            9,
            &format!("<< /Type /EmbeddedFile /Length {} >>", notes.len()),
            notes,
        ));
        objects.push(b"10 0 obj\n/ASCIIHexDecode\nendobj\n".to_vec());
        build_pdf(objects)
    }

    fn stream_object(number: usize, dict: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{number} 0 obj\n{dict}\nstream\n").as_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    fn build_pdf(objects: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(&object);
        }
        let xref = out.len();
        let size = offsets.len().saturating_add(1);
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }
}
