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
//! * it edits single-byte (WinAnsi/ASCII) text directly, AND — for a Type0/CID
//!   subset whose glyph codes are not its characters (the common Chrome/Office
//!   export) — it reads and rewrites through the font's own `/ToUnicode` map
//!   (SL-5.TEXTEDIT.02): the search is matched as text, and the replacement is
//!   re-encoded into the SAME codes, so the edit draws with the glyphs the
//!   embedded subset already carries. A font this slice cannot read a code map
//!   for is refused, never mangled;
//! * it rewrites a page whose `/Contents` is a single Flate or raw stream;
//!   a multi-stream page or an unsupported stream filter is refused;
//! * it does not extend or re-subset a font, so every replacement character
//!   must be one the page's font already carries a glyph for — a character it
//!   does not (a letter the original run never used) is refused rather than
//!   drawn with a wrong or missing glyph.
//!
//! It never touches the filesystem or the network: it takes the bytes the
//! caller hands it and returns the bytes the caller hands out.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
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
    /// found, a multi-stream page, an unsupported filter, or a replacement
    /// character the embedded font has no glyph for).
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

    // A font whose glyph codes are not its characters (a Type0/CID subset, the
    // common case for Chrome/Office exports) carries a `/ToUnicode` CMap; load
    // one codec per page font so the search and the replacement can go through
    // the embedded font's own codes — no new face is shipped or embedded, the
    // glyphs the page already carries are reused (SL-5.TEXTEDIT.02).
    let fonts = resolve_page_fonts(src, &doc, &page_dict, budget, g);

    // Find the first show-text operand that carries `find` and rewrite it,
    // decoding through the active font's codec when it has one.
    let Some((span, new_operand)) = find_show_text(&decoded, &fonts, find, replace, g)? else {
        return Err(EditError::Unsupported(format!(
            "the text {find:?} was not found in a single show-text operator on page {page_index}"
        )));
    };
    let mut rewritten = Vec::with_capacity(decoded.len());
    rewritten.extend_from_slice(decoded.get(..span.start).unwrap_or_default());
    rewritten.extend_from_slice(&new_operand);
    rewritten.extend_from_slice(decoded.get(span.end..).unwrap_or_default());

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
/// Find the first show-text (`Tj`/`TJ`/`'`/`"`) string operand whose text —
/// decoded through the active font's `/ToUnicode` codec when it has one —
/// carries `find`, and return the operand's span (delimiters included) with
/// its fully re-encoded replacement.
///
/// The scanner tracks the font the most recent `Tf` selected so that a
/// subset/CID operand (2-byte codes that are not characters) can be read and
/// rewritten through the embedded font instead of matched as raw bytes.
fn find_show_text(
    content: &[u8],
    fonts: &BTreeMap<Vec<u8>, FontCodec>,
    find: &str,
    replace: &str,
    _g: &mut BudgetGuard<'_>,
) -> Result<Option<(Range<usize>, Vec<u8>)>, EditError> {
    let mut cur = Cursor::new(content);
    // String operands collected since the last operator, with their spans.
    let mut pending: Vec<(Range<usize>, Vec<u8>)> = Vec::new();
    // The most recent `/Name` token, and the font the most recent `Tf` chose.
    let mut last_name: Option<Vec<u8>> = None;
    let mut current_font: Option<Vec<u8>> = None;
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
            // A `/Name` binds the font on the next `Tf`; keep the latest.
            b'/' => last_name = Some(cur.read_name_token()),
            b'%' => cur.skip_comment(),
            b if is_regular_start(b) => {
                let word = cur.read_word();
                if word == b"Tf" {
                    current_font.clone_from(&last_name);
                }
                if is_show_text_operator(&word) {
                    let codec = current_font.as_deref().and_then(|n| fonts.get(n));
                    let take_all = word == b"TJ";
                    for (span, raw) in select_operands(&pending, take_all) {
                        let delim = content.get(span.start).copied();
                        if let Some(new_operand) =
                            rewrite_operand(&raw, delim, codec, find, replace)?
                        {
                            return Ok(Some((span, new_operand)));
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

/// Encode bytes as a PDF hex string `<…>` — the natural form for 2-byte CID
/// codes, which carry nulls and non-printables a literal would have to escape.
fn encode_pdf_hex(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len().saturating_mul(2).saturating_add(2));
    out.push(b'<');
    for &b in bytes {
        out.push(hex_digit(b >> 4));
        out.push(hex_digit(b & 0x0f));
    }
    out.push(b'>');
    out
}

/// A low nibble (0–15) as its lowercase hex ASCII digit.
fn hex_digit(nibble: u8) -> u8 {
    b"0123456789abcdef"
        .get(usize::from(nibble & 0x0f))
        .copied()
        .unwrap_or(b'0')
}

/// The most CMap entries this slice will hold for one font — a bound on a
/// hostile `/ToUnicode` stream, well above any real subset's glyph count.
const MAX_CMAP_ENTRIES: usize = 1 << 18;

/// A page font's code↔character mapping, read from its `/ToUnicode` CMap.
///
/// A simple font keys single bytes; a Type0/CID subset keys 2-byte codes
/// (`code_width`). `to_unicode` reads a show-text operand back to text;
/// `from_unicode` re-encodes replacement characters into the SAME codes, so an
/// edit draws with glyphs the embedded subset already carries — this reuses
/// the font the PDF ships, it neither adds a glyph nor embeds a new face.
#[derive(Debug, Clone, Default)]
struct FontCodec {
    code_width: usize,
    to_unicode: BTreeMap<u32, String>,
    from_unicode: BTreeMap<char, u32>,
}

impl FontCodec {
    /// Split an operand's bytes into big-endian codes of this font's width; a
    /// trailing partial code (malformed) is dropped.
    fn split_codes(&self, raw: &[u8]) -> Vec<u32> {
        let width = self.code_width.clamp(1, 4);
        let mut codes = Vec::with_capacity(raw.len().checked_div(width).unwrap_or(0));
        for chunk in raw.chunks_exact(width) {
            codes.push(be_u32(chunk));
        }
        codes
    }

    /// The text a single code draws, or the replacement character when the
    /// CMap has no entry for it.
    fn code_text(&self, code: u32) -> &str {
        self.to_unicode
            .get(&code)
            .map_or("\u{fffd}", String::as_str)
    }

    /// A code as its `code_width` big-endian bytes.
    fn code_bytes(&self, code: u32) -> Vec<u8> {
        let width = self.code_width.clamp(1, 4);
        let all = code.to_be_bytes();
        all.get(all.len().saturating_sub(width)..)
            .unwrap_or(&all)
            .to_vec()
    }
}

/// Read up to the last four bytes of `bytes` as a big-endian integer.
fn be_u32(bytes: &[u8]) -> u32 {
    let tail = bytes.get(bytes.len().saturating_sub(4)..).unwrap_or(bytes);
    let mut buf = [0u8; 4];
    let start = 4usize.saturating_sub(tail.len());
    if let Some(dst) = buf.get_mut(start..) {
        dst.copy_from_slice(tail);
    }
    u32::from_be_bytes(buf)
}

/// Decode UTF-16BE bytes (the `/ToUnicode` destination form) to a `String`.
fn utf16be(bytes: &[u8]) -> String {
    let units = bytes.chunks_exact(2).map(|p| match p {
        [a, b] => u16::from_be_bytes([*a, *b]),
        _ => 0,
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{fffd}'))
        .collect()
}

/// Parse a `/ToUnicode` CMap stream into a [`FontCodec`]. Every branch
/// advances the cursor, so the scan terminates on the bounded stream; the
/// resulting map is capped at [`MAX_CMAP_ENTRIES`].
fn parse_tounicode(bytes: &[u8]) -> FontCodec {
    let mut cur = Cursor::new(bytes);
    let mut to_unicode: BTreeMap<u32, String> = BTreeMap::new();
    let mut code_width = 2usize; // Identity-H default if no codespacerange
    loop {
        cur.skip_ws_and_comments();
        let Some(b) = cur.peek() else { break };
        match b {
            b'<' if cur.peek_at(1) == Some(b'<') => cur.skip_dict(),
            b'<' => {
                let _ = cur.read_hex_string();
            }
            b'(' => {
                let _ = cur.read_literal_string();
            }
            b'[' | b']' | b'{' | b'}' => {
                cur.bump();
            }
            b'/' => cur.skip_name(),
            b'%' => cur.skip_comment(),
            b if is_regular_start(b) => {
                let word = cur.read_word();
                match word.as_slice() {
                    b"begincodespacerange" => {
                        if let Some(w) = read_codespacerange(&mut cur) {
                            code_width = w;
                        }
                    }
                    b"beginbfchar" => read_bfchar(&mut cur, &mut to_unicode),
                    b"beginbfrange" => read_bfrange(&mut cur, &mut to_unicode),
                    _ => {}
                }
            }
            _ => {
                cur.bump();
            }
        }
    }
    let mut from_unicode: BTreeMap<char, u32> = BTreeMap::new();
    for (code, text) in &to_unicode {
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            // Single-character values are invertible; the first code wins so a
            // later duplicate can't shadow it.
            from_unicode.entry(c).or_insert(*code);
        }
    }
    FontCodec {
        code_width: code_width.clamp(1, 4),
        to_unicode,
        from_unicode,
    }
}

/// `<lo> <hi>` pairs until `endcodespacerange`; the code width is the byte
/// length of the first low bound (Identity CMaps use two).
fn read_codespacerange(cur: &mut Cursor<'_>) -> Option<usize> {
    let mut width = None;
    loop {
        cur.skip_ws_and_comments();
        match cur.peek() {
            Some(b'<') => {
                let lo = cur.read_hex_string();
                if width.is_none() && !lo.is_empty() {
                    width = Some(lo.len());
                }
                cur.skip_ws_and_comments();
                if cur.peek() == Some(b'<') {
                    let _ = cur.read_hex_string();
                }
            }
            Some(b) if is_regular_start(b) => {
                if cur.read_word() == b"endcodespacerange" {
                    break;
                }
            }
            Some(_) => cur.bump().map_or((), |_| ()),
            None => break,
        }
    }
    width
}

/// `<src> <dst>` pairs until `endbfchar`.
fn read_bfchar(cur: &mut Cursor<'_>, map: &mut BTreeMap<u32, String>) {
    loop {
        cur.skip_ws_and_comments();
        match cur.peek() {
            Some(b'<') => {
                let src = cur.read_hex_string();
                cur.skip_ws_and_comments();
                if cur.peek() != Some(b'<') {
                    continue;
                }
                let dst = cur.read_hex_string();
                if map.len() < MAX_CMAP_ENTRIES {
                    map.insert(be_u32(&src), utf16be(&dst));
                }
            }
            Some(b) if is_regular_start(b) => {
                if cur.read_word() == b"endbfchar" {
                    break;
                }
            }
            Some(_) => cur.bump().map_or((), |_| ()),
            None => break,
        }
    }
}

/// `<lo> <hi> <dst>` (scalar) or `<lo> <hi> [<d0> <d1> …]` (per-code) triples
/// until `endbfrange`.
fn read_bfrange(cur: &mut Cursor<'_>, map: &mut BTreeMap<u32, String>) {
    loop {
        cur.skip_ws_and_comments();
        match cur.peek() {
            Some(b'<') => {
                let lo = be_u32(&cur.read_hex_string());
                cur.skip_ws_and_comments();
                if cur.peek() != Some(b'<') {
                    continue;
                }
                let hi = be_u32(&cur.read_hex_string());
                cur.skip_ws_and_comments();
                match cur.peek() {
                    Some(b'<') => {
                        let base = cur.read_hex_string();
                        insert_bfrange_scalar(map, lo, hi, &base);
                    }
                    Some(b'[') => {
                        cur.bump();
                        let mut code = lo;
                        loop {
                            cur.skip_ws_and_comments();
                            match cur.peek() {
                                Some(b'<') => {
                                    let d = cur.read_hex_string();
                                    if code <= hi && map.len() < MAX_CMAP_ENTRIES {
                                        map.insert(code, utf16be(&d));
                                    }
                                    code = code.saturating_add(1);
                                }
                                Some(b']') => {
                                    cur.bump();
                                    break;
                                }
                                Some(_) => cur.bump().map_or((), |_| ()),
                                None => break,
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(b) if is_regular_start(b) => {
                if cur.read_word() == b"endbfrange" {
                    break;
                }
            }
            Some(_) => cur.bump().map_or((), |_| ()),
            None => break,
        }
    }
}

/// Assign `lo..=hi` the single BMP value in `base`, incrementing per step
/// (ISO 32000-1 §9.10.3). A multi-character base (a ligature) is assigned only
/// to `lo`, since there is no well-defined increment.
fn insert_bfrange_scalar(map: &mut BTreeMap<u32, String>, lo: u32, hi: u32, base: &[u8]) {
    let base_text = utf16be(base);
    let mut chars = base_text.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(c), None) => Some(u32::from(c)),
        _ => None,
    };
    match single {
        Some(base_u) => {
            let span = hi.saturating_sub(lo);
            for k in 0..=span {
                if map.len() >= MAX_CMAP_ENTRIES {
                    break;
                }
                let code = lo.saturating_add(k);
                if let Some(ch) = char::from_u32(base_u.saturating_add(k)) {
                    map.insert(code, ch.to_string());
                }
            }
        }
        None => {
            if !base_text.is_empty() && map.len() < MAX_CMAP_ENTRIES {
                map.insert(lo, base_text);
            }
        }
    }
}

/// Build a `/ToUnicode` codec for each font in the page's `/Resources /Font`.
/// A missing, unreadable, or codec-less font is simply absent — the scanner
/// then reads its operands as raw single-byte text (the prior behaviour).
fn resolve_page_fonts(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_dict: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> BTreeMap<Vec<u8>, FontCodec> {
    let mut out = BTreeMap::new();
    let Some(resources) = resolve_dict_entry(src, doc, page_dict, b"Resources", budget, g) else {
        return out;
    };
    let Some(fonts) = resolve_dict_entry(src, doc, &resources, b"Font", budget, g) else {
        return out;
    };
    for (name, value) in &fonts {
        let Some(font_dict) = as_dict(src, doc, value, budget, g) else {
            continue;
        };
        let Some(tu) = dict_get(&font_dict, b"ToUnicode") else {
            continue;
        };
        let Some((stream_dict, data)) = as_stream(src, doc, tu, budget, g) else {
            continue;
        };
        let filter = stream_filter(&stream_dict).ok().flatten();
        let Ok(decoded) = decode_content(filter, data.as_slice(), budget, g) else {
            continue;
        };
        let codec = parse_tounicode(&decoded);
        if !codec.to_unicode.is_empty() {
            out.insert(name.to_vec(), codec);
        }
    }
    out
}

/// Resolve a dictionary-valued entry (direct or a reference) to its pairs.
fn resolve_dict_entry(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    dict: &[(Bytes, Obj)],
    key: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<Vec<(Bytes, Obj)>> {
    let value = dict_get(dict, key)?;
    as_dict(src, doc, value, budget, g)
}

/// Resolve an object (direct dict or a reference to one) to its pairs.
fn as_dict(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    value: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<Vec<(Bytes, Obj)>> {
    let obj = match value {
        Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g).ok()?,
        other => other.clone(),
    };
    match obj {
        Obj::Dict(d) => Some(d),
        _ => None,
    }
}

/// Resolve an object (direct stream or a reference to one) to its dict + data.
fn as_stream(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    value: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<(Vec<(Bytes, Obj)>, Bytes)> {
    let obj = match value {
        Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g).ok()?,
        other => other.clone(),
    };
    match obj {
        Obj::Stream { dict, data } => Some((dict, data)),
        _ => None,
    }
}

/// Rewrite one show-text operand if it carries `find`. Returns the fully
/// re-encoded operand (delimiters included), `None` when `find` is absent, or
/// an error when the match is real but cannot be re-encoded through the
/// embedded font (a replacement character it has no glyph for).
fn rewrite_operand(
    raw: &[u8],
    delim: Option<u8>,
    codec: Option<&FontCodec>,
    find: &str,
    replace: &str,
) -> Result<Option<Vec<u8>>, EditError> {
    match codec {
        Some(c) if c.code_width >= 2 => rewrite_cid(raw, c, find, replace),
        _ => Ok(rewrite_simple(raw, delim, find, replace)),
    }
}

/// The single-byte path: match literal ASCII bytes and splice, preserving the
/// operand's delimiter. A wider character cannot be matched or encoded here
/// without the font's byte encoding, so it is left for the CID path.
fn rewrite_simple(raw: &[u8], delim: Option<u8>, find: &str, replace: &str) -> Option<Vec<u8>> {
    if !find.is_ascii() || !replace.is_ascii() || !contains(raw, find.as_bytes()) {
        return None;
    }
    let new_text = replace_first(raw, find.as_bytes(), replace.as_bytes());
    Some(match delim {
        Some(b'<') => encode_pdf_hex(&new_text),
        _ => encode_pdf_literal(&new_text),
    })
}

/// The CID path: decode the operand through `/ToUnicode`, match `find` as
/// text, and re-encode the replacement into the font's own codes.
fn rewrite_cid(
    raw: &[u8],
    codec: &FontCodec,
    find: &str,
    replace: &str,
) -> Result<Option<Vec<u8>>, EditError> {
    let codes = codec.split_codes(raw);
    // Operand text, plus the code index each character came from, so a text
    // match maps back to a whole-code range.
    let mut text = String::new();
    let mut char_code: Vec<usize> = Vec::new();
    for (i, &code) in codes.iter().enumerate() {
        for ch in codec.code_text(code).chars() {
            text.push(ch);
            char_code.push(i);
        }
    }
    let Some(char_start) = find_char_index(&text, find) else {
        return Ok(None);
    };
    let char_end = char_start.saturating_add(find.chars().count());

    // The match must cover whole codes (it can't split a multi-character
    // glyph). With one character per code — the usual subset — this holds
    // trivially.
    let code_start = char_code.get(char_start).copied().unwrap_or(codes.len());
    let last_matched_code = char_code
        .get(char_end.saturating_sub(1))
        .copied()
        .unwrap_or(code_start);
    let code_end = last_matched_code.saturating_add(1);
    let starts_clean =
        char_start == 0 || char_code.get(char_start.saturating_sub(1)).copied() != Some(code_start);
    let ends_clean =
        char_end >= char_code.len() || char_code.get(char_end).copied() != Some(last_matched_code);
    if !starts_clean || !ends_clean {
        return Err(EditError::Unsupported(
            "the match falls inside a multi-character glyph this edit can't split".to_string(),
        ));
    }

    // Re-encode the replacement through the SAME font; refuse a character the
    // embedded subset has no glyph for rather than draw a wrong one.
    let mut new_codes: Vec<u32> = codes.get(..code_start).unwrap_or_default().to_vec();
    for ch in replace.chars() {
        match codec.from_unicode.get(&ch) {
            Some(&code) => new_codes.push(code),
            None => {
                return Err(EditError::Unsupported(format!(
                    "the document's font has no glyph for {ch:?}; an in-place edit reuses the embedded font and cannot add one"
                )));
            }
        }
    }
    new_codes.extend_from_slice(codes.get(code_end..).unwrap_or_default());

    let mut bytes = Vec::with_capacity(new_codes.len().saturating_mul(codec.code_width.max(1)));
    for code in new_codes {
        bytes.extend_from_slice(&codec.code_bytes(code));
    }
    Ok(Some(encode_pdf_hex(&bytes)))
}

/// The character index (not byte offset) at which `find` begins in `text`.
fn find_char_index(text: &str, find: &str) -> Option<usize> {
    let byte = text.find(find)?;
    Some(text.get(..byte)?.chars().count())
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
        let _ = self.read_name_token();
    }

    /// Read a `/Name`, returning the bytes AFTER the `/` (no escape handling —
    /// a font resource name is a plain `/F12`), leaving the cursor after it.
    fn read_name_token(&mut self) -> Vec<u8> {
        self.bump(); // the '/'
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

    /// A one-page document with a Type0/Identity-H font whose `/ToUnicode`
    /// maps a small code set, and whose content shows 2-byte codes — the shape
    /// a Chrome/Office export produces, and the one the old reader could not
    /// edit (its operand bytes are codes, not characters).
    fn cid_page(body: &[u8]) -> Vec<u8> {
        // Codes → Unicode: 0001..=000F ↦ M o t h e r b a d G p i c s SPACE.
        let cmap: &[u8] = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n15 beginbfchar\n<0001> <004D>\n<0002> <006F>\n<0003> <0074>\n<0004> <0068>\n<0005> <0065>\n<0006> <0072>\n<0007> <0062>\n<0008> <0061>\n<0009> <0064>\n<000A> <0047>\n<000B> <0070>\n<000C> <0069>\n<000D> <0063>\n<000E> <0073>\n<000F> <0020>\nendbfchar\nendcmap\nend\nend\n";
        let tounicode = {
            let mut s = format!("<< /Length {} >>\nstream\n", cmap.len()).into_bytes();
            s.extend_from_slice(cmap);
            s.extend_from_slice(b"\nendstream");
            s
        };
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
                b"<< /Type /Font /Subtype /Type0 /BaseFont /AAAAAA+Arial /Encoding /Identity-H /DescendantFonts [7 0 R] /ToUnicode 6 0 R >>".to_vec(),
            ),
            (6, tounicode),
            (
                7,
                b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /AAAAAA+Arial /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>".to_vec(),
            ),
        ])
    }

    // "Motherboard" in the font's codes: M o t h e r b o a r d.
    const MOTHERBOARD: &[u8] =
        b"BT /F1 12 Tf 10 100 Td <00010002000300040005000600070002000800060009> Tj ET\n";

    #[test]
    fn edits_cid_text_through_the_embedded_font() {
        let src = cid_page(MOTHERBOARD);
        let (out, report) = run(&src, "Motherboard", "Graphics").expect("edit");
        assert_eq!(report.replaced, 1);

        // An incremental update, as for the simple-font path.
        assert_eq!(out.get(..src.len()), Some(src.as_slice()), "original kept");

        let content = page_content_text(&out);
        let text = String::from_utf8_lossy(&content);
        // "Graphics" re-encoded into the SAME font's codes: G r a p h i c s =
        // 000a 0006 0008 000b 0004 000c 000d 000e.
        assert!(
            text.contains("<000a00060008000b0004000c000d000e>"),
            "re-encoded through the embedded font: {text}"
        );
        // The original "Motherboard" code run is gone.
        assert!(
            !text.contains("0001000200030004"),
            "old codes replaced: {text}"
        );
    }

    #[test]
    fn edits_a_cid_substring_and_keeps_the_rest() {
        // Replace just "board" (b o a r d = 0007 0002 0008 0006 0009) with
        // "pics" (p i c s = 000b 000c 000d 000e), leaving "Mother" intact.
        let src = cid_page(MOTHERBOARD);
        let (out, _) = run(&src, "board", "pics").expect("edit");
        let content = page_content_text(&out);
        let text = String::from_utf8_lossy(&content);
        // Mother (0001 0002 0003 0004 0005 0006) + pics (000b 000c 000d 000e).
        assert!(
            text.contains("<0001000200030004000500060008000b000c000d000e>")
                || text.contains("<000100020003000400050006000b000c000d000e>"),
            "prefix kept, suffix re-encoded: {text}"
        );
    }

    #[test]
    fn refuses_a_replacement_glyph_the_font_lacks() {
        // 'z' (U+007A) is not in the font's /ToUnicode — the subset has no
        // glyph for it, so the edit refuses rather than drawing a wrong glyph.
        let src = cid_page(MOTHERBOARD);
        let err = run(&src, "Motherboard", "Motherboardz").expect_err("no glyph");
        assert!(
            matches!(err, EditError::Unsupported(ref m) if m.contains("no glyph")),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_tounicode_reads_bfchar_bfrange_and_inverts() {
        let cmap: &[u8] = b"1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n2 beginbfchar\n<0001> <0041>\n<0002> <0042>\nendbfchar\n1 beginbfrange\n<0010> <0012> <0061>\nendbfrange\n";
        let codec = parse_tounicode(cmap);
        assert_eq!(codec.code_width, 2);
        assert_eq!(codec.code_text(0x0001), "A");
        assert_eq!(codec.code_text(0x0002), "B");
        // The bfrange assigns 0x10→a, 0x11→b, 0x12→c by incrementing.
        assert_eq!(codec.code_text(0x0010), "a");
        assert_eq!(codec.code_text(0x0011), "b");
        assert_eq!(codec.code_text(0x0012), "c");
        // Inverse: character → the font's own code.
        assert_eq!(codec.from_unicode.get(&'A'), Some(&0x0001));
        assert_eq!(codec.from_unicode.get(&'c'), Some(&0x0012));
        // Code splitting and re-encoding round-trip at 2-byte width.
        assert_eq!(codec.split_codes(&[0x00, 0x01, 0x00, 0x02]), vec![1, 2]);
        assert_eq!(codec.code_bytes(0x0001), vec![0x00, 0x01]);
    }
}
