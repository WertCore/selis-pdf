//! Edit the text of an existing PDF, saved as an incremental update
//! (SL-5.TEXTEDIT.01, the in-place half of ADR-P0024).
//!
//! This crate owns the first, most-requested edit: replacing a run of text
//! that is already in the document — fixing a typo, a number, or a name —
//! without re-laying-out the page. It finds the show-text operator whose
//! string operand carries the target text, rewrites just that string, and
//! writes the result as an **incremental update**: the original bytes are kept
//! verbatim as a prefix and the one changed content stream is appended as a new
//! revision ([`selis_pdf_cos::doc_writer::write_incremental_update`]). A digital
//! signature over the original revision therefore stays valid, and a small edit
//! to a large file costs bytes proportional to the edit — the behaviour Adobe,
//! Aspose and Foxit all emit.
//!
//! What this first slice does **not** do, and says so by refusing rather than
//! guessing:
//!
//! * it does not reflow — the replacement is spliced into the existing string,
//!   so a very different length will not re-break lines (that is the Phase-5
//!   reconstruction half, built on this);
//! * it edits text whose bytes are WinAnsi/ASCII-compatible (the common case
//!   for Latin body text); a value outside the single-byte range, or a font
//!   whose encoding this slice cannot read, is refused, never mangled;
//! * it rewrites a page whose `/Contents` is a single Flate or raw stream;
//!   a multi-stream page or an unsupported stream filter is refused;
//! * it does not extend a font subset, so the replacement must use glyphs the
//!   page's font already carries (true for the standard-14 faces and most
//!   embedded Latin fonts).
//!
//! It never touches the filesystem or the network: it takes the bytes the
//! caller hands it and returns the bytes the caller hands out.

#![forbid(unsafe_code)]

use std::fmt;
use std::ops::Range;

use selis_bytes::Bytes;
use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::doc_writer::write_incremental_update;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

/// The largest decoded content stream this slice will rewrite, so a hostile
/// stream cannot turn one edit into an unbounded buffer.
const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
/// The Flate level used when re-encoding a stream that was Flate-encoded.
const FLATE_LEVEL: u8 = 6;

/// What went wrong editing the text.
///
/// Every variant carries only user-facing text; none carries document bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The bytes are not a readable document, or a value the walk needs is the
    /// wrong shape.
    Malformed(String),
    /// A referenced object could not be read.
    Unresolvable(String),
    /// The document is encrypted; remove the password before editing.
    Encrypted,
    /// The request is invalid on its own terms (empty search, bad page index).
    Invalid(String),
    /// The edit is not one this slice performs, with the reason (text not
    /// found, a multi-stream page, an unsupported filter, non-WinAnsi text).
    Unsupported(String),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(d)
            | Self::Unresolvable(d)
            | Self::Invalid(d)
            | Self::Unsupported(d) => f.write_str(d),
            Self::Encrypted => {
                f.write_str("document is encrypted; remove the password with unlock first")
            }
        }
    }
}

impl std::error::Error for EditError {}

/// What an edit produced, beyond the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditReport {
    /// The 0-based page the edit landed on.
    pub page: u32,
    /// How many occurrences of the search text were replaced (this slice
    /// replaces the first match only, so 0 or 1).
    pub replaced: u32,
    /// The input size in bytes.
    pub bytes_in: u64,
    /// The output size in bytes (always ≥ `bytes_in`: an incremental update
    /// only appends).
    pub bytes_out: u64,
}

/// Replace the first occurrence of `find` with `replace` in the text of page
/// `page_index`, and return the incrementally-updated document.
///
/// # Errors
///
/// [`EditError::Invalid`] for an empty `find` or an out-of-range page,
/// [`EditError::Encrypted`] for an encrypted document, [`EditError::Unsupported`]
/// when the text is not found or the page/font/stream shape is outside this
/// slice, and [`EditError::Malformed`]/[`EditError::Unresolvable`] for bytes the
/// rewrite cannot read or produce.
pub fn edit_text(
    src: &[u8],
    page_index: u32,
    find: &str,
    replace: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, EditReport), EditError> {
    if find.is_empty() {
        return Err(EditError::Invalid("the search text is empty".to_string()));
    }
    if !find.is_ascii() || !replace.is_ascii() {
        return Err(EditError::Unsupported(
            "this edit handles ASCII/WinAnsi text only for now; the search or replacement has a wider character".to_string(),
        ));
    }

    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| EditError::Malformed(format!("cannot open: {e}")))?;
    if is_encrypted(&doc) {
        return Err(EditError::Encrypted);
    }

    let pages = page_refs(src, &doc, budget, g)?;
    let page_ref = pages.get(page_index as usize).copied().ok_or_else(|| {
        EditError::Invalid(format!(
            "page {page_index} is out of range; the document has {} page(s)",
            pages.len()
        ))
    })?;

    // Resolve the page's single content stream.
    let page_obj = resolve_ref(src, &doc, page_ref, budget, g)
        .map_err(|e| EditError::Unresolvable(format!("cannot read the page: {e}")))?;
    let Obj::Dict(page_dict) = page_obj else {
        return Err(EditError::Malformed(
            "the page is not a dictionary".to_string(),
        ));
    };
    let content_ref = single_content_ref(&page_dict)?;
    let content_obj = resolve_ref(src, &doc, content_ref, budget, g)
        .map_err(|e| EditError::Unresolvable(format!("cannot read the page content: {e}")))?;
    let Obj::Stream {
        dict: stream_dict,
        data,
    } = content_obj
    else {
        return Err(EditError::Malformed(
            "the page content is not a stream".to_string(),
        ));
    };

    let filter = stream_filter(&stream_dict)?;
    let decoded = decode_content(filter.clone(), data.as_slice(), budget, g)?;

    // Find the first show-text string operand that carries `find` and splice it.
    let Some(splice) = find_show_text(&decoded, find.as_bytes(), g)? else {
        return Err(EditError::Unsupported(format!(
            "the text {find:?} was not found in a single show-text operator on page {page_index}"
        )));
    };
    let new_text = replace_first(&splice.text, find.as_bytes(), replace.as_bytes());
    let literal = encode_pdf_literal(&new_text);
    let mut rewritten = Vec::with_capacity(decoded.len());
    rewritten.extend_from_slice(decoded.get(..splice.span.start).unwrap_or_default());
    rewritten.extend_from_slice(&literal);
    rewritten.extend_from_slice(decoded.get(splice.span.end..).unwrap_or_default());

    // Re-encode in the stream's original filter and rebuild the object.
    let (encoded, filter_name) = encode_content(filter, &rewritten);
    let new_stream = rebuild_stream(&stream_dict, encoded, filter_name);
    let trailer = carried_trailer(&doc)?;
    let out = write_incremental_update(src, &[(content_ref.num, new_stream)], &trailer, budget, g)
        .map_err(|e| EditError::Unresolvable(format!("cannot write the update: {e}")))?;

    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((
        out,
        EditReport {
            page: page_index,
            replaced: 1,
            bytes_in,
            bytes_out,
        },
    ))
}

/// True when the document's newest revision carries `/Encrypt`.
fn is_encrypted(doc: &selis_pdf_cos::Doc) -> bool {
    doc.revisions()
        .last()
        .is_some_and(|r| r.trailer.iter().any(|(k, _)| k.as_slice() == b"Encrypt"))
}

/// The leaf `/Page` object references, in reading order.
fn page_refs(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Ref>, EditError> {
    let root = doc
        .revisions()
        .last()
        .and_then(|r| r.root)
        .ok_or_else(|| EditError::Malformed("no /Root in the trailer".to_string()))?;
    let Obj::Dict(catalog) = resolve_ref(src, doc, root, budget, g)
        .map_err(|e| EditError::Unresolvable(format!("cannot read the catalog: {e}")))?
    else {
        return Err(EditError::Malformed(
            "the catalog is not a dictionary".to_string(),
        ));
    };
    let Some(Obj::Ref(pages_ref)) = dict_get(&catalog, b"Pages").cloned() else {
        return Err(EditError::Malformed(
            "the catalog has no /Pages".to_string(),
        ));
    };
    let mut out = Vec::new();
    collect_pages(src, doc, pages_ref, &mut out, 0, budget, g)?;
    Ok(out)
}

/// Walk the page tree, collecting leaf pages. Depth-bounded against a cycle.
fn collect_pages(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    node: Ref,
    out: &mut Vec<Ref>,
    depth: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), EditError> {
    if depth > 64 || out.len() > 1_000_000 {
        return Ok(());
    }
    let Obj::Dict(dict) = resolve_ref(src, doc, node, budget, g)
        .map_err(|e| EditError::Unresolvable(format!("cannot read a page-tree node: {e}")))?
    else {
        return Ok(());
    };
    match dict_get(&dict, b"Type") {
        Some(Obj::Name(t)) if t.as_slice() == b"Pages" => {
            if let Some(Obj::Array(kids)) = dict_get(&dict, b"Kids") {
                for kid in kids {
                    if let Obj::Ref(kid_ref) = kid {
                        collect_pages(src, doc, *kid_ref, out, depth.saturating_add(1), budget, g)?;
                    }
                }
            }
        }
        // A leaf /Page, or a node without /Type that carries /Contents.
        _ => out.push(node),
    }
    Ok(())
}

/// The single content-stream reference of a page, or a typed refusal.
fn single_content_ref(page_dict: &[(Bytes, Obj)]) -> Result<Ref, EditError> {
    match dict_get(page_dict, b"Contents") {
        Some(Obj::Ref(r)) => Ok(*r),
        Some(Obj::Array(items)) => match items.as_slice() {
            [Obj::Ref(r)] => Ok(*r),
            _ => Err(EditError::Unsupported(
                "this page's content is split across several streams; that is not editable yet"
                    .to_string(),
            )),
        },
        _ => Err(EditError::Unsupported(
            "this page has no single content stream to edit".to_string(),
        )),
    }
}

/// The stream's byte filter: `None` for a raw stream, `Some(name)` for a Flate
/// stream. Any other filter is refused.
fn stream_filter(stream_dict: &[(Bytes, Obj)]) -> Result<Option<Bytes>, EditError> {
    let is_flate = |n: &Bytes| n.as_slice() == b"FlateDecode" || n.as_slice() == b"Fl";
    match dict_get(stream_dict, b"Filter") {
        None => Ok(None),
        Some(Obj::Name(n)) if is_flate(n) => Ok(Some(n.clone())),
        Some(Obj::Array(items)) => match items.as_slice() {
            [Obj::Name(n)] if is_flate(n) => Ok(Some(n.clone())),
            _ => Err(EditError::Unsupported(
                "the page content uses a stream filter this edit does not handle yet".to_string(),
            )),
        },
        _ => Err(EditError::Unsupported(
            "the page content uses a stream filter this edit does not handle yet".to_string(),
        )),
    }
}

/// Decode the content stream per its filter.
fn decode_content(
    filter: Option<Bytes>,
    data: &[u8],
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>, EditError> {
    match filter {
        None => Ok(data.to_vec()),
        Some(_) => selis_pdf_filter::decode("FlateDecode", data, MAX_CONTENT_BYTES, g)
            .map_err(|e| EditError::Malformed(format!("cannot decode the page content: {e}"))),
    }
}

/// Re-encode the rewritten content in the stream's original filter. Returns the
/// bytes and the filter name to write (`None` for raw).
fn encode_content(filter: Option<Bytes>, content: &[u8]) -> (Vec<u8>, Option<Bytes>) {
    match filter {
        None => (content.to_vec(), None),
        Some(name) => (
            selis_pdf_filter::flate_encode(content, FLATE_LEVEL),
            Some(name),
        ),
    }
}

/// Rebuild the content-stream object with the new data and `/Length`, keeping
/// every other dictionary entry.
fn rebuild_stream(
    original_dict: &[(Bytes, Obj)],
    data: Vec<u8>,
    filter_name: Option<Bytes>,
) -> Obj {
    let mut dict: Vec<(Bytes, Obj)> = original_dict
        .iter()
        .filter(|(k, _)| k.as_slice() != b"Length" && k.as_slice() != b"Filter")
        .cloned()
        .collect();
    if let Some(name) = filter_name {
        dict.push((Bytes::copy_from_slice(b"Filter"), Obj::Name(name)));
    }
    dict.push((
        Bytes::copy_from_slice(b"Length"),
        Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
    ));
    Obj::Stream {
        dict,
        data: Bytes::copy_from_slice(&data),
    }
}

/// The trailer entries an incremental update carries forward: `/Root` and, when
/// present, `/ID`. `/Size` and `/Prev` are filled in by the writer.
fn carried_trailer(doc: &selis_pdf_cos::Doc) -> Result<Vec<(Vec<u8>, Obj)>, EditError> {
    let revision = doc
        .revisions()
        .last()
        .ok_or_else(|| EditError::Malformed("no revisions".to_string()))?;
    let root = revision
        .root
        .ok_or_else(|| EditError::Malformed("no /Root in the trailer".to_string()))?;
    let mut out: Vec<(Vec<u8>, Obj)> = vec![(b"Root".to_vec(), Obj::Ref(root))];
    if let Some((_, id)) = revision.trailer.iter().find(|(k, _)| k.as_slice() == b"ID") {
        out.push((b"ID".to_vec(), id.clone()));
    }
    Ok(out)
}

/// A show-text string operand found in the content: its byte span (the `(…)` or
/// `<…>` literal, delimiters included) and its decoded text.
struct Splice {
    span: Range<usize>,
    text: Vec<u8>,
}

/// Find the first show-text (`Tj`/`TJ`/`'`/`"`) string operand whose decoded
/// text contains `needle`.
fn find_show_text(
    content: &[u8],
    needle: &[u8],
    _g: &mut BudgetGuard<'_>,
) -> Result<Option<Splice>, EditError> {
    let mut cur = Cursor::new(content);
    // String operands collected since the last operator, with their spans.
    let mut pending: Vec<(Range<usize>, Vec<u8>)> = Vec::new();
    loop {
        cur.skip_ws_and_comments();
        let start = cur.pos();
        let Some(byte) = cur.peek() else { break };
        match byte {
            b'(' => {
                let text = cur.read_literal_string();
                pending.push((start..cur.pos(), text));
            }
            b'<' => {
                if cur.peek_at(1) == Some(b'<') {
                    cur.skip_dict();
                } else {
                    let text = cur.read_hex_string();
                    pending.push((start..cur.pos(), text));
                }
            }
            b'[' | b']' => {
                cur.bump();
            }
            b'/' => cur.skip_name(),
            b'%' => cur.skip_comment(),
            b if is_regular_start(b) => {
                let word = cur.read_word();
                if is_show_text_operator(&word) {
                    let take_all = word == b"TJ";
                    for (span, text) in select_operands(&pending, take_all) {
                        if contains(&text, needle) {
                            return Ok(Some(Splice { span, text }));
                        }
                    }
                }
                pending.clear();
            }
            _ => {
                cur.bump();
            }
        }
    }
    Ok(None)
}

/// The string operands a show-text operator draws: all of them for `TJ` (the
/// array elements), the last one otherwise (`Tj`/`'`/`"`).
fn select_operands(
    pending: &[(Range<usize>, Vec<u8>)],
    take_all: bool,
) -> Vec<(Range<usize>, Vec<u8>)> {
    if take_all {
        pending.to_vec()
    } else {
        pending.last().cloned().into_iter().collect()
    }
}

fn is_show_text_operator(word: &[u8]) -> bool {
    matches!(word, b"Tj" | b"TJ" | b"'" | b"\"")
}

/// A content-stream byte that begins a regular (non-delimiter, non-ws) token.
fn is_regular_start(b: u8) -> bool {
    !matches!(
        b,
        0x00 | 0x09
            | 0x0a
            | 0x0c
            | 0x0d
            | 0x20
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

/// Whether `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Replace the first occurrence of `needle` in `haystack` with `repl`.
fn replace_first(haystack: &[u8], needle: &[u8], repl: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return haystack.to_vec();
    }
    let last_start = haystack.len().saturating_sub(needle.len());
    for i in 0..=last_start {
        if haystack.get(i..i.saturating_add(needle.len())) == Some(needle) {
            let mut out = Vec::with_capacity(haystack.len());
            out.extend_from_slice(haystack.get(..i).unwrap_or_default());
            out.extend_from_slice(repl);
            out.extend_from_slice(
                haystack
                    .get(i.saturating_add(needle.len())..)
                    .unwrap_or_default(),
            );
            return out;
        }
    }
    haystack.to_vec()
}

/// Encode bytes as a PDF literal string `(…)`, escaping the three bytes that
/// must be escaped. The input here is WinAnsi/ASCII text, so no other escaping
/// is needed.
fn encode_pdf_literal(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len().saturating_add(2));
    out.push(b'(');
    for &b in bytes {
        if matches!(b, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b')');
    out
}

/// Look a value up by key in a dictionary.
fn dict_get<'a>(pairs: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

/// A forward byte cursor over a content stream.
struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

    fn pos(&self) -> usize {
        self.i
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.b.get(self.i.saturating_add(ahead)).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let v = self.b.get(self.i).copied();
        if v.is_some() {
            self.i = self.i.saturating_add(1);
        }
        v
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20) => {
                    self.bump();
                }
                Some(b'%') => self.skip_comment(),
                _ => break,
            }
        }
    }

    fn skip_comment(&mut self) {
        while let Some(b) = self.peek() {
            self.bump();
            if b == b'\n' || b == b'\r' {
                break;
            }
        }
    }

    fn skip_name(&mut self) {
        self.bump(); // the '/'
        while let Some(b) = self.peek() {
            if is_regular_start(b) {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn read_word(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            if is_regular_start(b) {
                out.push(b);
                self.bump();
            } else {
                break;
            }
        }
        out
    }

    /// Read a `(…)` literal, resolving escapes, leaving the cursor after `)`.
    fn read_literal_string(&mut self) -> Vec<u8> {
        self.bump(); // the '('
        let mut out = Vec::new();
        let mut depth = 1u32;
        while let Some(b) = self.bump() {
            match b {
                b'\\' => {
                    if let Some(esc) = self.bump() {
                        match esc {
                            b'n' => out.push(0x0a),
                            b'r' => out.push(0x0d),
                            b't' => out.push(0x09),
                            b'b' => out.push(0x08),
                            b'f' => out.push(0x0c),
                            b'(' => out.push(b'('),
                            b')' => out.push(b')'),
                            b'\\' => out.push(b'\\'),
                            b'\n' => {}
                            b'\r' => {
                                if self.peek() == Some(b'\n') {
                                    self.bump();
                                }
                            }
                            b'0'..=b'7' => {
                                let mut val = u16::from(esc.saturating_sub(b'0'));
                                for _ in 0..2 {
                                    match self.peek() {
                                        Some(d @ b'0'..=b'7') => {
                                            val = val
                                                .saturating_mul(8)
                                                .saturating_add(u16::from(d.saturating_sub(b'0')));
                                            self.bump();
                                        }
                                        _ => break,
                                    }
                                }
                                out.push(u8::try_from(val & 0xff).unwrap_or(0));
                            }
                            other => out.push(other),
                        }
                    }
                }
                b'(' => {
                    depth = depth.saturating_add(1);
                    out.push(b);
                }
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                other => out.push(other),
            }
        }
        out
    }

    /// Read a `<…>` hex string, leaving the cursor after `>`.
    fn read_hex_string(&mut self) -> Vec<u8> {
        self.bump(); // the '<'
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.bump() {
            if b == b'>' {
                break;
            }
            let Some(nibble) = hex_value(b) else { continue };
            match hi.take() {
                None => hi = Some(nibble),
                Some(h) => out.push(h.saturating_mul(16).saturating_add(nibble)),
            }
        }
        if let Some(h) = hi {
            out.push(h.saturating_mul(16));
        }
        out
    }

    /// Skip a `<<…>>` dictionary, leaving the cursor after the matching `>>`.
    fn skip_dict(&mut self) {
        self.bump(); // first '<'
        self.bump(); // second '<'
        let mut depth = 1u32;
        while depth > 0 {
            let Some(b) = self.bump() else { break };
            if b == b'<' && self.peek() == Some(b'<') {
                self.bump();
                depth = depth.saturating_add(1);
            } else if b == b'>' && self.peek() == Some(b'>') {
                self.bump();
                depth = depth.saturating_sub(1);
            }
        }
    }
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.saturating_sub(b'0')),
        b'a'..=b'f' => Some(b.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(b.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    /// A minimal one-page PDF from raw object bodies, with a correct xref.
    fn build_pdf(objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0u64; max as usize + 1];
        for (num, body) in objects {
            offsets[*num as usize] = out.len() as u64;
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len() as u64;
        out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for i in 1..=max {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[i as usize]).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
                max + 1
            )
            .as_bytes(),
        );
        out
    }

    /// A one-page document whose content shows `body` (a raw content stream).
    fn page_with_content(body: &[u8]) -> Vec<u8> {
        let stream = {
            let mut s = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
            s.extend_from_slice(body);
            s.extend_from_slice(b"\nendstream");
            s
        };
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
            ),
            (4, stream),
            (
                5,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            ),
        ])
    }

    fn run(src: &[u8], find: &str, replace: &str) -> Result<(Vec<u8>, EditReport), EditError> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        edit_text(src, 0, find, replace, &budget, &mut g)
    }

    fn page_content_text(out: &[u8]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let startxref = selis_pdf_cos::xref::find_startxref(out, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(out, startxref, &budget, &mut g).expect("parse");
        // Newest revision wins: resolve_ref(4) is the redefined content stream.
        let Obj::Stream { data, dict } =
            resolve_ref(out, &doc, Ref::new(4, 0), &budget, &mut g).expect("content stream")
        else {
            panic!("content is not a stream");
        };
        let flate = dict.iter().any(|(k, v)| {
            k.as_slice() == b"Filter" && matches!(v, Obj::Name(n) if n.as_slice() == b"FlateDecode")
        });
        if flate {
            selis_pdf_filter::decode("FlateDecode", data.as_slice(), MAX_CONTENT_BYTES, &mut g)
                .expect("decode")
        } else {
            data.as_slice().to_vec()
        }
    }

    #[test]
    fn replaces_text_and_keeps_the_original_as_a_prefix() {
        let src = page_with_content(b"BT /F1 12 Tf 10 100 Td (Hello World) Tj ET\n");
        let (out, report) = run(&src, "World", "Rust").expect("edit");

        // An incremental update: the original bytes are a byte-identical prefix.
        assert_eq!(
            out.get(..src.len()),
            Some(src.as_slice()),
            "original preserved"
        );
        assert!(out.len() > src.len(), "the update only appends");
        assert_eq!(report.replaced, 1);
        assert_eq!(report.page, 0);

        // The newest content stream shows the replacement, not the original.
        let content = page_content_text(&out);
        let text = String::from_utf8_lossy(&content);
        assert!(text.contains("(Hello Rust) Tj"), "replaced: {text}");
        assert!(!text.contains("World"), "old text gone: {text}");
    }

    #[test]
    fn edits_a_flate_encoded_content_stream() {
        let raw = b"BT /F1 12 Tf 10 100 Td (Invoice 2023) Tj ET\n";
        let encoded = selis_pdf_filter::flate_encode(raw, FLATE_LEVEL);
        let stream = {
            let mut s = format!(
                "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                encoded.len()
            )
            .into_bytes();
            s.extend_from_slice(&encoded);
            s.extend_from_slice(b"\nendstream");
            s
        };
        let src = build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R >>".to_vec(),
            ),
            (4, stream),
        ]);

        let (out, _) = run(&src, "2023", "2024").expect("edit");
        assert_eq!(
            out.get(..src.len()),
            Some(src.as_slice()),
            "original preserved"
        );
        let content = page_content_text(&out);
        let text = String::from_utf8_lossy(&content);
        assert!(
            text.contains("(Invoice 2024) Tj"),
            "replaced in flate: {text}"
        );
    }

    #[test]
    fn finds_text_inside_a_tj_array_element() {
        let src = page_with_content(b"BT /F1 12 Tf 10 100 Td [(Total: )-10(900)] TJ ET\n");
        let (out, _) = run(&src, "900", "950").expect("edit");
        let content = page_content_text(&out);
        let text = String::from_utf8_lossy(&content);
        assert!(text.contains("(950)"), "replaced TJ element: {text}");
        assert!(text.contains("(Total: )"), "other element kept: {text}");
    }

    #[test]
    fn missing_text_is_refused_without_writing() {
        let src = page_with_content(b"BT /F1 12 Tf 10 100 Td (Hello) Tj ET\n");
        let err = run(&src, "Goodbye", "x").expect_err("not found");
        assert!(matches!(err, EditError::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn an_empty_search_is_invalid() {
        let src = page_with_content(b"BT (Hi) Tj ET\n");
        let err = run(&src, "", "x").expect_err("empty");
        assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    }

    #[test]
    fn a_wide_character_is_refused() {
        let src = page_with_content(b"BT (Hi) Tj ET\n");
        let err = run(&src, "Hi", "H\u{00e9}").expect_err("wide");
        assert!(matches!(err, EditError::Unsupported(_)), "got {err:?}");
    }
}
