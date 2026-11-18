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
//! * it reflows the LINE, not the page: the replacement is spliced into the
//!   existing string, and the text that follows it on the same line is pushed
//!   along by the width the edit added, measured in the page's own font. It
//!   does not re-break the paragraph — a line that grows past the margin
//!   still runs off it (that is the Phase-5 reconstruction half, built on
//!   this);
//! * it edits single-byte (WinAnsi/ASCII) text directly, AND — for a Type0/CID
//!   subset whose glyph codes are not its characters (the common Chrome/Office
//!   export) — it reads and rewrites through the font's own `/ToUnicode` map
//!   (SL-5.TEXTEDIT.02): the search is matched as text, and the replacement is
//!   re-encoded into the SAME codes, so the edit draws with the glyphs the
//!   embedded subset already carries. A font this slice cannot read a code map
//!   for is refused, never mangled;
//! * it rewrites a page whose `/Contents` is one Flate or raw stream, or
//!   several of them: a page whose content was split by a stamp is read as
//!   one run of operators and written back as one stream, so the text stays
//!   editable after a text box or an image has been put on the page. An
//!   unsupported stream filter is still refused;
//! * it does not extend or re-subset a font, so every replacement character
//!   must be one the page's font already carries a glyph for — a character it
//!   does not (a letter the original run never used) is refused rather than
//!   drawn with a wrong or missing glyph.
//!
//! The edit itself never touches the filesystem or the network: it takes the
//! bytes the caller hands it and returns the bytes the caller hands out. The
//! one explicit exception is [`save`] (SL-5.EDIT.04): it takes those returned
//! bytes and commits them to a path safely, verifying before replacing.

#![forbid(unsafe_code)]

mod font;
pub mod incremental;
pub mod journal;
pub mod rewrite;
pub mod save;

#[cfg(test)]
mod encrypted_tests;
#[cfg(test)]
mod incremental_tests;
#[cfg(test)]
mod journal_tests;
#[cfg(test)]
mod property_tests;
mod reflow;
#[cfg(test)]
mod rewrite_tests;
#[cfg(test)]
mod save_tests;

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
    let content_refs = content_refs(&page_dict)?;

    // The page's content, read as ONE run of operators: the streams are drawn
    // in order, so joining them with a newline is what the reader sees.
    let mut decoded: Vec<u8> = Vec::new();
    let mut filter: Option<Bytes> = None;
    let mut stream_dict: Vec<(Bytes, Obj)> = Vec::new();
    for (index, content_ref) in content_refs.iter().enumerate() {
        let content_obj = resolve_ref(src, &doc, *content_ref, budget, g)
            .map_err(|e| EditError::Unresolvable(format!("cannot read the page content: {e}")))?;
        let Obj::Stream {
            dict: part_dict,
            data,
        } = content_obj
        else {
            return Err(EditError::Malformed(
                "the page content is not a stream".to_string(),
            ));
        };
        let part_filter = stream_filter(&part_dict)?;
        let part = decode_content(part_filter.clone(), data.as_slice(), budget, g)?;
        if index == 0 {
            // The first stream's filter is the one the merged stream is
            // written back in: the page keeps the encoding it had.
            filter = part_filter;
            stream_dict = part_dict;
        }
        if !decoded.is_empty() {
            decoded.push(b'\n');
        }
        decoded.extend_from_slice(&part);
    }

    // A font whose glyph codes are not its characters (a Type0/CID subset, the
    // common case for Chrome/Office exports) carries a `/ToUnicode` CMap; load
    // one codec per page font so the search and the replacement can go through
    // the embedded font's own codes — no new face is shipped or embedded, the
    // glyphs the page already carries are reused (SL-5.TEXTEDIT.02).
    let fonts = resolve_page_fonts(src, &doc, &page_dict, budget, g);
    // The re-flow measures the replacement in the page's OWN font. A font
    // this cannot measure (one with no usable widths) is left out, and the
    // edit then leaves the following text where it was rather than guessing.
    let page_fonts = font::page_fonts(src, &doc, &page_dict, budget, g);

    // Find the first show-text operand that carries `find` and rewrite it,
    // decoding through the active font's codec when it has one.
    let Some(found) = find_show_text(&decoded, &fonts, find, replace, g)? else {
        return Err(EditError::Unsupported(format!(
            "the text {find:?} was not found in a single show-text operator on page {page_index}"
        )));
    };
    // The width the edit added, in unscaled text space units. A non-zero
    // delta pushes the text that follows on the same line along by it.
    let delta = reflow_delta(&page_fonts, &found);
    let rewritten = if delta.abs() > EPSILON {
        reflow::apply(&decoded, found.span, &found.operand, delta)
    } else {
        let mut plain = Vec::with_capacity(decoded.len());
        plain.extend_from_slice(decoded.get(..found.span.start).unwrap_or_default());
        plain.extend_from_slice(&found.operand);
        plain.extend_from_slice(decoded.get(found.span.end..).unwrap_or_default());
        plain
    };

    // Re-encode in the stream's original filter and rebuild the object.
    let (encoded, filter_name) = encode_content(filter, &rewritten);
    let new_stream = rebuild_stream(&stream_dict, encoded, filter_name);
    let trailer = carried_trailer(&doc)?;
    // The page's FIRST content stream is the one that is rewritten, and the
    // page is pointed at it. A page with no content stream cannot be
    // edited, and saying so is better than reaching past the end.
    let Some(first_content) = content_refs.first() else {
        return Err(EditError::Malformed(format!(
            "page {page_index} has no content stream to edit"
        )));
    };
    let mut objects: Vec<(u32, Obj)> = vec![(first_content.num, new_stream)];
    if content_refs.len() > 1 {
        // The page drew its content from several streams; it now draws it
        // from one. The page dictionary is rewritten to point at it, and every
        // other entry of the page is carried over untouched.
        let mut page: Vec<(Bytes, Obj)> = Vec::with_capacity(page_dict.len());
        for (key, value) in &page_dict {
            if key.as_slice() == b"Contents" {
                page.push((key.clone(), Obj::Ref(*first_content)));
            } else {
                page.push((key.clone(), value.clone()));
            }
        }
        objects.push((page_ref.num, Obj::Dict(page)));
    }
    let out = write_incremental_update(src, &objects, &trailer, budget, g)
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

/// The content-stream references of a page, in the order the page draws them.
///
/// A page may carry its content in one stream or in several. Several is the
/// NORMAL shape once anything has been stamped on the page: a text box, an
/// image or a redaction appends its own stream and leaves the page's
/// `/Contents` an array. The streams are drawn in order, exactly as if their
/// contents had been written one after another, so an edit reads them all and
/// writes them back as one.
fn content_refs(page_dict: &[(Bytes, Obj)]) -> Result<Vec<Ref>, EditError> {
    match dict_get(page_dict, b"Contents") {
        Some(Obj::Ref(r)) => Ok(vec![*r]),
        Some(Obj::Array(items)) => {
            let mut refs = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Obj::Ref(r) => refs.push(*r),
                    _ => {
                        return Err(EditError::Unsupported(
                            "this page's content is split across several streams; that is not \
                             editable yet"
                                .to_string(),
                        ));
                    }
                }
            }
            if refs.is_empty() {
                return Err(EditError::Unsupported(
                    "this page has no content stream to edit".to_string(),
                ));
            }
            Ok(refs)
        }
        _ => Err(EditError::Unsupported(
            "this page has no content stream to edit".to_string(),
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

/// A show-text operand found in the content, and the text state it was
/// drawn in.
struct ShowMatch {
    /// The operand's byte span (the `(…)` or `<…>` literal, delimiters
    /// included).
    span: Range<usize>,
    /// The re-encoded replacement operand, delimiters included.
    operand: Vec<u8>,
    /// The original operand's codes, in the page's font.
    codes: Vec<u8>,
    /// The replacement's codes, in the page's font.
    replacement: Vec<u8>,
    /// The font the operand was drawn in, by resource name.
    font: Option<Vec<u8>>,
    /// The font size (`Tf`).
    size: f64,
    /// The character spacing (`Tc`).
    char_spacing: f64,
    /// The word spacing (`Tw`).
    word_spacing: f64,
    /// The horizontal scaling (`Tz`, a percentage).
    h_scale: f64,
}

/// Find the first show-text (`Tj`/`TJ`/`'`/`"`) string operand whose text —
/// decoded through the active font's `/ToUnicode` codec when it has one —
/// carries `find`, and return the operand's span (delimiters included), its
/// fully re-encoded replacement, and the text state it was drawn in.
///
/// The scanner tracks the font the most recent `Tf` selected so that a
/// subset/CID operand (2-byte codes that are not characters) can be read and
/// rewritten through the embedded font instead of matched as raw bytes. It
/// tracks the size and spacing too, because the re-flow needs to know how
/// much WIDER the replacement is than what it replaced.
fn find_show_text(
    content: &[u8],
    fonts: &BTreeMap<Vec<u8>, FontCodec>,
    find: &str,
    replace: &str,
    _g: &mut BudgetGuard<'_>,
) -> Result<Option<ShowMatch>, EditError> {
    let mut cur = Cursor::new(content);
    // String operands collected since the last operator, with their spans.
    let mut pending: Vec<(Range<usize>, Vec<u8>)> = Vec::new();
    // The most recent `/Name` token, and the font the most recent `Tf` chose.
    let mut last_name: Option<Vec<u8>> = None;
    let mut current_font: Option<Vec<u8>> = None;
    // The text state an edit's width depends on.
    let mut size = 0.0f64;
    let mut char_spacing = 0.0f64;
    let mut word_spacing = 0.0f64;
    let mut h_scale = 100.0f64;
    // The numeric operands of the operator being read.
    let mut numbers: Vec<f64> = Vec::new();
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
            b if b.is_ascii_digit() || b == b'+' || b == b'-' || b == b'.' => {
                numbers.push(cur.read_number());
            }
            b if is_regular_start(b) => {
                let word = cur.read_word();
                match word.as_slice() {
                    b"Tf" => {
                        current_font.clone_from(&last_name);
                        size = numbers.first().copied().unwrap_or(0.0);
                    }
                    b"Tc" => char_spacing = numbers.first().copied().unwrap_or(0.0),
                    b"Tw" => word_spacing = numbers.first().copied().unwrap_or(0.0),
                    b"Tz" => h_scale = numbers.first().copied().unwrap_or(100.0),
                    _ => {}
                }
                if is_show_text_operator(&word) {
                    let codec = current_font.as_deref().and_then(|n| fonts.get(n));
                    let take_all = word == b"TJ";
                    for (span, raw) in select_operands(&pending, take_all) {
                        let delim = content.get(span.start).copied();
                        if let Some(rewrite) = rewrite_operand(&raw, delim, codec, find, replace)? {
                            return Ok(Some(ShowMatch {
                                span,
                                operand: rewrite.operand,
                                codes: raw,
                                replacement: rewrite.codes,
                                font: current_font.clone(),
                                size,
                                char_spacing,
                                word_spacing,
                                h_scale,
                            }));
                        }
                    }
                }
                pending.clear();
                numbers.clear();
            }
            _ => {
                cur.bump();
            }
        }
    }
    Ok(None)
}

/// How far apart two positions must be before the re-flow acts on them.
const EPSILON: f64 = 1e-6;

/// The width the edit added, in unscaled text space units.
///
/// The page's font carries the advances; the text state carries the size and
/// the spacing. Word spacing applies to a simple font's code 32 only, and the
/// horizontal scaling applies to the whole advance.
fn reflow_delta(page_fonts: &font::PageFonts, found: &ShowMatch) -> f64 {
    let Some(page_font) = found.font.as_ref().and_then(|name| page_fonts.get(name)) else {
        return 0.0;
    };
    let before = advance_of(
        page_font,
        &found.codes,
        found.size,
        found.char_spacing,
        found.word_spacing,
    );
    let after = advance_of(
        page_font,
        &found.replacement,
        found.size,
        found.char_spacing,
        found.word_spacing,
    );
    (after - before) * (found.h_scale / 100.0)
}

/// The advance of a run of codes, in unscaled text space units.
fn advance_of(
    font: &font::PageFont,
    codes: &[u8],
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
) -> f64 {
    let mut total = 0.0f64;
    let mut at = 0usize;
    while at < codes.len() {
        let width = font.code_width();
        let code = if width >= 2 {
            let hi = u32::from(codes.get(at).copied().unwrap_or(0));
            let lo = u32::from(codes.get(at.saturating_add(1)).copied().unwrap_or(0));
            at = at.saturating_add(2);
            (hi << 8) | lo
        } else {
            let code = u32::from(codes.get(at).copied().unwrap_or(0));
            at = at.saturating_add(1);
            code
        };
        total = total + font.advance(code) / 1000.0 * size + char_spacing;
        if !font.is_cid() && code == 32 {
            total += word_spacing;
        }
    }
    total
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

/// A pending operand, as far as the rewriter cares.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    /// A number, with its value.
    Num(f64),
    /// Anything else: a string, a name, an array.
    Other,
}

/// The text line matrix, as the text operators move it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TextMatrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    /// The text origin, x.
    e: f64,
    /// The text origin, y.
    f: f64,
}

impl TextMatrix {
    /// The identity matrix, which is where `BT` starts.
    const IDENTITY: TextMatrix = TextMatrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Move the origin by a translation.
    fn translate(&mut self, x: f64, y: f64) {
        self.e += x;
        self.f += y;
    }
}

/// A six-element matrix, in PDF's `[a b c d e f]` order.
type Matrix6 = [f64; 6];

/// The identity matrix.
const IDENTITY: Matrix6 = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Compose two matrices: `outer` applied after `inner`.
fn compose(outer: Matrix6, inner: Matrix6) -> Matrix6 {
    [
        outer[0] * inner[0] + outer[1] * inner[2],
        outer[0] * inner[1] + outer[1] * inner[3],
        outer[2] * inner[0] + outer[3] * inner[2],
        outer[2] * inner[1] + outer[3] * inner[3],
        outer[4] * inner[0] + outer[5] * inner[2] + inner[4],
        outer[4] * inner[1] + outer[5] * inner[3] + inner[5],
    ]
}

/// The inverse of a matrix, or `None` when it is singular.
fn invert(matrix: Matrix6) -> Option<Matrix6> {
    let det = matrix[0] * matrix[3] - matrix[1] * matrix[2];
    if !det.is_finite() || det == 0.0 {
        return None;
    }
    let inverse_det = 1.0 / det;
    Some([
        matrix[3] * inverse_det,
        -matrix[1] * inverse_det,
        -matrix[2] * inverse_det,
        matrix[0] * inverse_det,
        (matrix[2] * matrix[5] - matrix[3] * matrix[4]) * inverse_det,
        (matrix[1] * matrix[4] - matrix[0] * matrix[5]) * inverse_det,
    ])
}

/// One XObject `Do` that a strip kept, with the transform at the draw.
///
/// A form XObject executes its own content stream at the CTM in force at the
/// `Do` (further composed with the form's `/Matrix`). The caller needs that
/// transform to recurse into the form and strip the text under the same
/// page-space regions; an image draw needs no recursion and is reported only
/// through [`Redaction::removed_images`].
#[derive(Debug, Clone, PartialEq)]
pub struct XObjectDraw {
    /// The resource name the `Do` used, without the leading slash.
    pub name: String,
    /// The content transform at the `Do`, mapping that content's space into
    /// the page space the regions are expressed in.
    pub ctm: Matrix6,
}

/// What a redaction removed, and how to draw the box over it.
#[derive(Debug, Clone, PartialEq)]
pub struct Redaction {
    /// The content stream, with the text inside the regions removed.
    pub content: Vec<u8>,
    /// The inverse of the content's own transform at the end of the stream.
    ///
    /// The caller multiplies a **user-space** rectangle by it to draw the box
    /// in the content's own space. `None` when the content's transform is the
    /// identity, which is the usual case.
    pub inverse_transform: Option<Matrix6>,
    /// The resource names of the images that fell inside a region.
    ///
    /// The names are the page's own (`/Im1`, `/Im2`), so the caller can
    /// leave those images out of the resources it copies — and then their
    /// bytes never reach the output file. A redaction that covers an image
    /// but keeps the image is a black box over a secret.
    pub removed_images: Vec<String>,
    /// Every XObject `Do` that was KEPT, with the transform at the draw.
    ///
    /// Lets a caller recurse into form XObjects and strip their own streams
    /// under the same regions. A draw that was itself dropped (an image whose
    /// unit square fell inside a region) is not listed — nothing reaches the
    /// output that would need the XObject.
    pub xobject_draws: Vec<XObjectDraw>,
}

/// Remove what the given regions cover: the text runs inside them, and the
/// images drawn inside them.
///
/// The regions are in the page's own user space: points, y-up, at the
/// `/MediaBox`'s lower-left corner. A show operation (`Tj`, `TJ`, `'` or `"`)
/// is removed when the text matrix puts its origin inside a region — and the
/// operands it drew go with it, because a string left without its operator is
/// not a stream a reader can parse.
///
/// An image is drawn into the **unit square**, so the square's corners — put
/// through the content's transform — are where it lands on the page. A draw
/// whose box overlaps a region is removed, and the image's name is reported
/// in [`Redaction::removed_images`] so the caller can leave the image out of
/// the resources it copies: the bytes then never reach the output file.
/// A black box over a picture is not a redaction — the picture is still in
/// the file, and anyone with a file reader has it.
///
/// An **inline** image (`BI`/`ID`/`EI`) is not recognised: the rewriter does
/// not guess where the binary data ends, so such an image is covered by the
/// box rather than removed.
///
/// The text matrix is tracked across `BT`, `Td`, `TD`, `Tm`, `TL` and `T*`, and
/// the content's own transform across `q`, `Q` and `cm`, so the position a show
/// operation draws at is compared in the page's space — which is the space the
/// regions are in. A page whose content is scaled (a resize writes exactly
/// that) would otherwise be redacted in the wrong place.
///
/// Everything else is copied through byte for byte, so a stream with nothing
/// to remove comes back unchanged.
///
/// This is the page-level entry point: the content's own transform starts at
/// the identity. To strip a stream that executes inside a form XObject, use
/// [`strip_content_in_regions_with_ctm`] with the transform at the `Do`.
///
/// # Budget
///
/// The work is bounded by the length of the content stream: the stream is read
/// once, and the output is at most the input plus the text that was removed.
/// Nothing here grows with the number of regions.
///
/// # Malformed Input
///
/// A stream that is not a content stream is copied through unchanged: the
/// rewriter removes text, and it refuses to guess where the text is.
#[must_use]
pub fn strip_content_in_regions(content: &[u8], regions: &[(f64, f64, f64, f64)]) -> Redaction {
    strip_content_in_regions_with_ctm(content, regions, IDENTITY)
}

/// Strip text from a content stream that executes at `initial_ctm`.
///
/// Identical to [`strip_content_in_regions`] except that the content's own
/// transform starts at `initial_ctm` instead of the identity. A form XObject
/// executes its stream at the CTM in force at the `Do`, composed with the
/// form's own `/Matrix`, so a caller recursing into a form passes that composed
/// matrix here and the *same* page-space regions: a text origin is transformed
/// by the starting CTM before it is compared with a region.
///
/// The returned [`Redaction::xobject_draws`] carry CTMs already in the page
/// space, so nested forms can be recursed into with this same function.
///
/// # Budget
///
/// Linear in the content length, as [`strip_content_in_regions`]. The starting
/// transform is a fixed six-element matrix and adds no work.
///
/// # Malformed Input
///
/// A non-finite starting matrix makes every position test fail closed (the
/// transformed point cannot fall in a finite region), so nothing is removed
/// rather than an arbitrary region being redacted.
#[must_use]
pub fn strip_content_in_regions_with_ctm(
    content: &[u8],
    regions: &[(f64, f64, f64, f64)],
    initial_ctm: Matrix6,
) -> Redaction {
    let mut cur = Cursor::new(content);
    let mut out: Vec<u8> = Vec::new();
    // How much of the input has been copied out.
    let mut copied = 0usize;
    // Where the pending operand run starts in `out`, so a dropped show
    // operation can take its operands with it.
    let mut operand_start: Option<usize> = None;
    let mut operands: Vec<Pending> = Vec::new();
    // The array nesting: a number inside `[...]` is not an operand of the
    // operator that follows the array.
    let mut depth = 0usize;
    // The name of the last operand: `Do` takes one, and it is the name of
    // the image the page draws.
    let mut last_name: Option<String> = None;
    // How many times each XObject is drawn, and how many of those draws
    // fell inside a region.
    //
    // An image drawn twice — once under the redaction and once out of it —
    // must keep its resource, or the draw that was KEPT would name an
    // object the file no longer holds. So an image is removed only when
    // EVERY draw of it was dropped.
    let mut draws: Vec<(String, usize, usize)> = Vec::new();
    // Every kept `Do`, with the transform at the draw, for form recursion.
    let mut xobject_draws: Vec<XObjectDraw> = Vec::new();
    let mut tm = TextMatrix::IDENTITY;
    let mut leading = 0.0f64;
    let mut in_text = false;
    // The content's own transform, and the stack `q`/`Q` keep. A form's stream
    // starts at the composed CTM handed in by the caller, not at the identity.
    let mut ctm: Matrix6 = initial_ctm;
    let mut stack: Vec<Matrix6> = Vec::new();

    loop {
        cur.skip_ws_and_comments();
        let Some(byte) = cur.peek() else { break };
        match byte {
            b'(' => {
                let _ = cur.read_literal_string();
                push_operand(&mut operands, &mut operand_start, Pending::Other, out.len());
                copy_to(&mut out, &cur, &mut copied);
            }
            b'<' => {
                if cur.peek_at(1) == Some(b'<') {
                    cur.skip_dict();
                } else {
                    let _ = cur.read_hex_string();
                }
                push_operand(&mut operands, &mut operand_start, Pending::Other, out.len());
                copy_to(&mut out, &cur, &mut copied);
            }
            b'[' => {
                cur.bump();
                depth = depth.saturating_add(1);
                // The array is one operand, and it starts the run: dropping a
                // `TJ` has to take the whole array with it.
                push_operand(&mut operands, &mut operand_start, Pending::Other, out.len());
                copy_to(&mut out, &cur, &mut copied);
            }
            b']' => {
                cur.bump();
                depth = depth.saturating_sub(1);
                copy_to(&mut out, &cur, &mut copied);
            }
            b'/' => {
                let name = cur.read_name_token();
                last_name = Some(String::from_utf8_lossy(&name).into_owned());
                push_operand(&mut operands, &mut operand_start, Pending::Other, out.len());
                copy_to(&mut out, &cur, &mut copied);
            }
            b'%' => {
                cur.skip_comment();
                copy_to(&mut out, &cur, &mut copied);
            }
            b if b.is_ascii_digit() || b == b'+' || b == b'-' || b == b'.' => {
                let value = cur.read_number();
                if depth == 0 {
                    push_operand(
                        &mut operands,
                        &mut operand_start,
                        Pending::Num(value),
                        out.len(),
                    );
                }
                copy_to(&mut out, &cur, &mut copied);
            }
            b if is_regular_start(b) => {
                let word = cur.read_word();
                let op = String::from_utf8_lossy(&word).to_string();
                let mut drop_it = false;
                match op.as_str() {
                    "BT" => {
                        in_text = true;
                        tm = TextMatrix::IDENTITY;
                    }
                    "ET" => {
                        in_text = false;
                    }
                    "Td" | "TD" => {
                        if let (Some(x), Some(y)) =
                            (operand_number(&operands, 0), operand_number(&operands, 1))
                        {
                            tm.translate(x, y);
                            if op == "TD" {
                                leading = -y;
                            }
                        }
                    }
                    "TL" => {
                        if let Some(value) = operand_number(&operands, 0) {
                            leading = value;
                        }
                    }
                    "Tm" => {
                        let values = [
                            operand_number(&operands, 0),
                            operand_number(&operands, 1),
                            operand_number(&operands, 2),
                            operand_number(&operands, 3),
                            operand_number(&operands, 4),
                            operand_number(&operands, 5),
                        ];
                        if values.iter().all(|value| value.is_some()) {
                            let number =
                                |index: usize| values.get(index).copied().flatten().unwrap_or(0.0);
                            tm = TextMatrix {
                                a: number(0),
                                b: number(1),
                                c: number(2),
                                d: number(3),
                                e: number(4),
                                f: number(5),
                            };
                        }
                    }
                    "T*" => {
                        tm.f -= leading;
                    }
                    "q" => {
                        stack.push(ctm);
                    }
                    "Q" => {
                        if let Some(saved) = stack.pop() {
                            ctm = saved;
                        }
                    }
                    "cm" => {
                        let values = [
                            operand_number(&operands, 0),
                            operand_number(&operands, 1),
                            operand_number(&operands, 2),
                            operand_number(&operands, 3),
                            operand_number(&operands, 4),
                            operand_number(&operands, 5),
                        ];
                        if values.iter().all(|value| value.is_some()) {
                            let number =
                                |index: usize| values.get(index).copied().flatten().unwrap_or(0.0);
                            ctm = compose(
                                [
                                    number(0),
                                    number(1),
                                    number(2),
                                    number(3),
                                    number(4),
                                    number(5),
                                ],
                                ctm,
                            );
                        }
                    }
                    "'" | "\"" => {
                        // Both move to the start of the next line before they
                        // show, so the position to test is the moved one.
                        tm.f -= leading;
                        drop_it = in_text && inside_any(regions, tm.e, tm.f, ctm);
                    }
                    "Tj" | "TJ" => {
                        drop_it = in_text && inside_any(regions, tm.e, tm.f, ctm);
                    }
                    "Do" => {
                        // An image is drawn into the unit square, so the
                        // square's own corners — put through the content's
                        // transform — are where the image lands on the page.
                        //
                        // The draw is dropped when that box falls inside a
                        // region, and the name is remembered so the caller
                        // can leave the image out of the file altogether.
                        if let Some(name) = last_name.clone() {
                            let covered = image_inside_any(regions, ctm);
                            drop_it = covered;
                            match draws.iter_mut().find(|(drawn, _, _)| *drawn == name) {
                                Some((_, total, dropped)) => {
                                    *total = total.saturating_add(1);
                                    if covered {
                                        *dropped = dropped.saturating_add(1);
                                    }
                                }
                                None => draws.push((name.clone(), 1, usize::from(covered))),
                            }
                            // A draw that survives must be reproducible: for a
                            // form XObject the caller recurses at this CTM. A
                            // dropped draw needs no XObject in the output.
                            if !drop_it {
                                xobject_draws.push(XObjectDraw { name, ctm });
                            }
                        }
                    }
                    _ => {}
                }
                if drop_it {
                    // The show operation and the operands it drew both go.
                    drop_operands(&mut out, &mut operand_start);
                    operands.clear();
                    copied = cur.pos();
                    continue;
                }
                copy_to(&mut out, &cur, &mut copied);
                operands.clear();
                operand_start = None;
                last_name = None;
            }
            _ => {
                cur.bump();
                copy_to(&mut out, &cur, &mut copied);
            }
        }
    }
    // Whatever is left of the input (a stream with no trailing operator) is
    // copied through: losing it would change the stream in a way nobody asked
    // for.
    if copied < content.len() {
        out.extend_from_slice(content.get(copied..).unwrap_or(&[]));
    }
    let removed_images: Vec<String> = draws
        .into_iter()
        .filter(|(_, total, dropped)| *total > 0 && total == dropped)
        .map(|(name, _, _)| name)
        .collect();
    Redaction {
        content: out,
        inverse_transform: invert(ctm),
        removed_images,
        xobject_draws,
    }
}

/// Compose two PDF `[a b c d e f]` matrices: `outer` applied after `inner`.
///
/// Exposed so the tools layer can compose a form's `/Matrix` with the CTM at
/// the `Do` before recursing into the form's stream.
///
/// # Budget
///
/// Constant work: eight multiplies and six adds on a fixed six-element matrix.
///
/// # Malformed Input
///
/// No validation is performed; non-finite inputs propagate as non-finite
/// outputs. Callers that cannot tolerate that fail closed on the position
/// tests in [`strip_content_in_regions_with_ctm`].
#[must_use]
pub fn compose_matrix(outer: Matrix6, inner: Matrix6) -> Matrix6 {
    compose(outer, inner)
}

/// Copy the input up to the cursor's position.
pub(crate) fn copy_to(out: &mut Vec<u8>, cur: &Cursor<'_>, copied: &mut usize) {
    let end = cur.pos();
    if end > *copied {
        out.extend_from_slice(cur.b.get(*copied..end).unwrap_or(&[]));
        *copied = end;
    }
}

/// Record an operand, remembering where the run started.
fn push_operand(
    operands: &mut Vec<Pending>,
    operand_start: &mut Option<usize>,
    value: Pending,
    out_len: usize,
) {
    if operand_start.is_none() {
        *operand_start = Some(out_len);
    }
    operands.push(value);
}

/// Drop the pending operand run from the output.
fn drop_operands(out: &mut Vec<u8>, operand_start: &mut Option<usize>) {
    if let Some(start) = operand_start.take() {
        out.truncate(start);
    }
}

/// The numeric value of the operand at `index`, if it is a number.
fn operand_number(operands: &[Pending], index: usize) -> Option<f64> {
    match operands.get(index)? {
        Pending::Num(value) => Some(*value),
        Pending::Other => None,
    }
}

/// Whether a point is inside any of the regions.
///
/// The point is the text's origin in the **page's** space: the text matrix
/// places it in the content's own space, and the content's transform carries
/// it into the page's.
fn inside_any(regions: &[(f64, f64, f64, f64)], x: f64, y: f64, ctm: Matrix6) -> bool {
    let px = ctm[0] * x + ctm[2] * y + ctm[4];
    let py = ctm[1] * x + ctm[3] * y + ctm[5];
    regions
        .iter()
        .any(|(rx, ry, rw, rh)| px >= *rx && px <= rx + rw && py >= *ry && py <= ry + rh)
}

/// Whether an image drawn at the content's own transform falls in a region.
///
/// An image is drawn into the **unit square**, so the square's four corners —
/// put through the content's transform — are where the image lands on the
/// page. The image is removed when its box OVERLAPS a region: a redaction
/// that left the fragment of an image outside the box would have left the
/// secret in the file, and over-redacting a picture is a smaller harm than
/// leaving one.
fn image_inside_any(regions: &[(f64, f64, f64, f64)], ctm: Matrix6) -> bool {
    let mut left = f64::INFINITY;
    let mut right = f64::NEG_INFINITY;
    let mut bottom = f64::INFINITY;
    let mut top = f64::NEG_INFINITY;
    for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let px = ctm[0] * x + ctm[2] * y + ctm[4];
        let py = ctm[1] * x + ctm[3] * y + ctm[5];
        left = left.min(px);
        right = right.max(px);
        bottom = bottom.min(py);
        top = top.max(py);
    }
    regions.iter().any(|(rx, ry, rw, rh)| {
        let (rx1, ry1) = (rx + rw, ry + rh);
        left <= rx1 && right >= *rx && bottom <= ry1 && top >= *ry
    })
}

/// A content-stream byte that begins a regular (non-delimiter, non-ws) token.
pub(crate) fn is_regular_start(b: u8) -> bool {
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
pub(crate) fn resolve_dict_entry(
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
pub(crate) fn as_dict(
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

/// An operand after the edit: its encoded bytes, and the codes it draws.
struct Rewrite {
    /// The fully re-encoded operand, delimiters included.
    operand: Vec<u8>,
    /// The codes the operand draws, in the page's font.
    codes: Vec<u8>,
}

/// Rewrite one show-text operand if it carries `find`. Returns the fully
/// re-encoded operand and the codes it draws, `None` when `find` is absent, or
/// an error when the match is real but cannot be re-encoded through the
/// embedded font (a replacement character it has no glyph for).
fn rewrite_operand(
    raw: &[u8],
    delim: Option<u8>,
    codec: Option<&FontCodec>,
    find: &str,
    replace: &str,
) -> Result<Option<Rewrite>, EditError> {
    match codec {
        Some(c) if c.code_width >= 2 => rewrite_cid(raw, c, find, replace),
        _ => Ok(rewrite_simple(raw, delim, find, replace)),
    }
}

/// The single-byte path: match literal ASCII bytes and splice, preserving the
/// operand's delimiter. A wider character cannot be matched or encoded here
/// without the font's byte encoding, so it is left for the CID path.
fn rewrite_simple(raw: &[u8], delim: Option<u8>, find: &str, replace: &str) -> Option<Rewrite> {
    if !find.is_ascii() || !replace.is_ascii() || !contains(raw, find.as_bytes()) {
        return None;
    }
    let codes = replace_first(raw, find.as_bytes(), replace.as_bytes());
    Some(Rewrite {
        operand: match delim {
            Some(b'<') => encode_pdf_hex(&codes),
            _ => encode_pdf_literal(&codes),
        },
        codes,
    })
}

/// The CID path: decode the operand through `/ToUnicode`, match `find` as
/// text, and re-encode the replacement into the font's own codes.
fn rewrite_cid(
    raw: &[u8],
    codec: &FontCodec,
    find: &str,
    replace: &str,
) -> Result<Option<Rewrite>, EditError> {
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
    Ok(Some(Rewrite {
        operand: encode_pdf_hex(&bytes),
        codes: bytes,
    }))
}

/// The character index (not byte offset) at which `find` begins in `text`.
fn find_char_index(text: &str, find: &str) -> Option<usize> {
    let byte = text.find(find)?;
    Some(text.get(..byte)?.chars().count())
}

/// Look a value up by key in a dictionary.
///
/// # Budget
///
/// A linear scan of one dictionary; nothing is allocated.
///
/// # Malformed Input
///
/// A missing key is `None`, and the value is returned unresolved for the
/// caller to resolve in the document's own terms.
pub(crate) fn dict_get<'a>(pairs: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

/// A forward byte cursor over a content stream.
pub(crate) struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

    pub(crate) fn pos(&self) -> usize {
        self.i
    }

    /// Move the cursor to a position, clamped to the input's length.
    pub(crate) fn seek(&mut self, pos: usize) {
        self.i = pos.min(self.b.len());
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

    /// Read a number, leaving the cursor after it.
    ///
    /// A number that does not parse is zero: the rewriter that uses this is
    /// deciding where text sits, and a number it cannot read is one it cannot
    /// place anything by either.
    fn read_number(&mut self) -> f64 {
        let start = self.i;
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'+' {
                self.bump();
            } else {
                break;
            }
        }
        String::from_utf8_lossy(self.b.get(start..self.i).unwrap_or(&[]))
            .parse()
            .unwrap_or(0.0)
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

    /// A one-page document whose content is split across two streams.
    ///
    /// This is the shape a page takes once something has been stamped on it:
    /// the text box, the image or the redaction appends its own stream and
    /// leaves the page's `/Contents` an array. The text on such a page has
    /// to stay editable, or the editor can only be used once.
    fn page_with_split_content(first: &[u8], second: &[u8]) -> Vec<u8> {
        let stream = |body: &[u8]| {
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
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents [4 0 R 6 0 R] /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
            ),
            (4, stream(first)),
            (
                5,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            ),
            (6, stream(second)),
        ])
    }

    /// The page's `/Contents` in the newest revision, as it is written there.
    fn page_contents(out: &[u8]) -> Obj {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let startxref = selis_pdf_cos::xref::find_startxref(out, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(out, startxref, &budget, &mut g).expect("parse");
        let Obj::Dict(pairs) =
            resolve_ref(out, &doc, Ref::new(3, 0), &budget, &mut g).expect("page")
        else {
            panic!("the page is not a dictionary");
        };
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"Contents")
            .map(|(_, v)| v.clone())
            .expect("the page has no /Contents")
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

    /// An image drawn under the region is removed from the content stream,
    /// and its name is reported so the caller can leave it out of the file.
    #[test]
    fn an_image_under_the_region_is_removed_and_named() {
        let content = b"q 200 0 0 100 10 50 cm /Im1 Do Q";
        let out = strip_content_in_regions(content, &[(0.0, 40.0, 200.0, 120.0)]);
        let text = String::from_utf8_lossy(&out.content).to_string();
        assert!(!text.contains("Im1"), "the image draw is gone: {text}");
        assert!(!text.contains("Do"), "the operator is gone with it: {text}");
        assert_eq!(
            out.removed_images,
            vec!["Im1".to_string()],
            "the image is named so the caller can leave it out of the file"
        );
    }

    /// An image drawn OUTSIDE the region is kept, and it is not reported.
    #[test]
    fn an_image_outside_the_region_is_kept() {
        let content = b"q 10 0 0 10 10 180 cm /Im2 Do Q";
        let out = strip_content_in_regions(content, &[(0.0, 40.0, 200.0, 120.0)]);
        let text = String::from_utf8_lossy(&out.content).to_string();
        assert!(
            text.contains("/Im2 Do"),
            "the image draw is untouched: {text}"
        );
        assert!(
            out.removed_images.is_empty(),
            "nothing is reported as removed: {:?}",
            out.removed_images
        );
    }

    /// An image drawn TWICE — once under the region and once out of it —
    /// must keep its resource, or the draw that was kept would name an object
    /// the file no longer holds.
    #[test]
    fn an_image_drawn_outside_as_well_keeps_its_name() {
        let content = b"q 200 0 0 100 10 50 cm /Im1 Do Q q 10 0 0 10 10 180 cm /Im1 Do Q";
        let out = strip_content_in_regions(content, &[(0.0, 40.0, 200.0, 120.0)]);
        let text = String::from_utf8_lossy(&out.content).to_string();
        assert!(
            text.contains("/Im1 Do"),
            "the draw that was kept is still there: {text}"
        );
        assert!(
            out.removed_images.is_empty(),
            "the image is not reported as removed: {:?}",
            out.removed_images
        );
    }

    /// A `Do` that survives is reported with the transform at the draw, so a
    /// caller can recurse into a form XObject at the right CTM.
    #[test]
    fn a_kept_do_is_reported_with_its_ctm() {
        let content = b"q 2 0 0 3 5 7 cm /Fm1 Do Q";
        let out = strip_content_in_regions(content, &[(0.0, 0.0, 1.0, 1.0)]);
        assert_eq!(out.xobject_draws.len(), 1);
        assert_eq!(out.xobject_draws[0].name, "Fm1");
        assert_eq!(out.xobject_draws[0].ctm, [2.0, 0.0, 0.0, 3.0, 5.0, 7.0]);
    }

    /// Text in a form's own stream is stripped when the caller supplies the
    /// CTM at the `Do`; the *same* page-space regions apply.
    #[test]
    fn text_in_a_form_stream_is_stripped_at_the_do_ctm() {
        let form = b"BT /F1 12 Tf 20 160 Td (ADV-FORM-XOBJECT-SECRET) Tj ET";
        // The form executes at identity: text at (20,160) lands under the page
        // region, so it must go.
        let out = strip_content_in_regions_with_ctm(form, &[(10.0, 150.0, 100.0, 20.0)], IDENTITY);
        let text = String::from_utf8_lossy(&out.content);
        assert!(
            !text.contains("ADV-FORM-XOBJECT-SECRET"),
            "the form's text is gone: {text}"
        );
    }

    /// A form drawn with a translation in its CTM is stripped in the right
    /// place: the text origin is transformed into page space before the test.
    #[test]
    fn a_form_matrix_translation_is_honoured() {
        let form = b"BT /F1 12 Tf 15 5 Td (SECRET-AT-25-165) Tj ET";
        // Form content (15,5) translated by (10,160) lands at page (25,165),
        // inside the region.
        let shifted = compose_matrix([1.0, 0.0, 0.0, 1.0, 10.0, 160.0], IDENTITY);
        let out = strip_content_in_regions_with_ctm(form, &[(10.0, 150.0, 100.0, 20.0)], shifted);
        assert!(
            !String::from_utf8_lossy(&out.content).contains("SECRET-AT-25-165"),
            "text translated into the region is stripped"
        );
    }

    /// A longer replacement pushes the text that follows it on the SAME
    /// line along, so the edit does not run over it. The lines BELOW stay
    /// where they were.
    #[test]
    fn a_longer_replacement_pushes_the_following_text_along() {
        let src = page_with_content(
            b"BT /F1 12 Tf 20 160 Td (Total:) Tj 30 0 Td (USD) Tj 0 -20 Td (Below) Tj ET",
        );
        let (out, report) = run(&src, "Total:", "Total amount:").expect("the edit");
        assert_eq!(report.replaced, 1);
        // The incremental update keeps the original as a prefix, so the
        // check must read the NEW content stream - not the old one that is
        // still in the file.
        let produced = String::from_utf8_lossy(&page_content_text(&out)).into_owned();
        // The new text is in the stream.
        assert!(produced.contains("(Total amount:)"), "the edit: {produced}");
        // And the text that was drawn after it was pushed right by a
        // translation, before its own positioning operator.
        assert!(
            produced.contains("0 Td 30 0 Td (USD)"),
            "the following text is pushed along: {produced}"
        );
        let shift: f64 = produced
            .split("(Total amount:) Tj")
            .nth(1)
            .unwrap_or_default()
            .split(" 0 Td")
            .next()
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0.0);
        // The shift is a real width in the page's font, not a fudge: "a
        // mount:" in Helvetica at 12pt is tens of points, not zero.
        assert!(
            shift > 10.0 && shift < 100.0,
            "the push-along is a real width: {shift} in {produced}"
        );
        // The line below takes the shift back out: it starts where it did
        // before the edit.
        assert!(
            !produced.contains("0 -20 Td (Below)") && produced.contains(" -20 Td (Below)"),
            "the line below takes the shift back out: {produced}"
        );
    }

    /// A shorter replacement pulls the text that follows back, by the
    /// width it took away.
    #[test]
    fn a_shorter_replacement_pulls_the_following_text_back() {
        let src =
            page_with_content(b"BT /F1 12 Tf 20 160 Td (Total amount:) Tj 30 0 Td (USD) Tj ET");
        let (out, _) = run(&src, "Total amount:", "Total:").expect("the edit");
        let produced = String::from_utf8_lossy(&page_content_text(&out)).into_owned();
        let shift: f64 = produced
            .split("(Total:) Tj")
            .nth(1)
            .unwrap_or_default()
            .split(" 0 Td")
            .next()
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0.0);
        assert!(
            shift < -10.0 && shift > -100.0,
            "the following text is pulled back: {shift} in {produced}"
        );
    }

    /// A replacement of the SAME width leaves the following text alone:
    /// there is nothing to push along.
    #[test]
    fn a_same_width_replacement_leaves_the_following_text_alone() {
        // Colon and semicolon have the same advance in Helvetica.
        let src = page_with_content(b"BT /F1 12 Tf 20 160 Td (Total:) Tj 30 0 Td (USD) Tj ET");
        let (out, _) = run(&src, "Total:", "Total;").expect("the edit");
        let produced = String::from_utf8_lossy(&page_content_text(&out)).into_owned();
        assert!(
            produced.contains("(Total;) Tj 30 0 Td (USD)"),
            "no shift was needed: {produced}"
        );
    }

    /// A line positioned with an ABSOLUTE `Tm` carries the shift in its own
    /// coordinates, because a `Tm` discards the line matrix the translation
    /// was written into.
    #[test]
    fn an_absolute_line_position_carries_the_shift() {
        let src = page_with_content(
            b"BT /F1 12 Tf 1 0 0 1 20 160 Tm (Total:) Tj 1 0 0 1 90 160 Tm (USD) Tj ET",
        );
        let (out, _) = run(&src, "Total:", "Total amount:").expect("the edit");
        let produced = String::from_utf8_lossy(&page_content_text(&out)).into_owned();
        // The USD line moved right from x=90: the absolute position carries
        // the width the edit added.
        let moved_to: f64 = produced
            .split("(USD)")
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .rev()
            .nth(2)
            .unwrap_or_default()
            .parse()
            .unwrap_or(0.0);
        assert!(
            moved_to > 100.0 && moved_to < 200.0,
            "the absolute position carries the shift: {moved_to} in {produced}"
        );
    }

    #[test]
    fn edits_a_page_whose_content_was_split_by_a_stamp() {
        // A page whose text lives in one stream, and a stamp in another.
        let src = page_with_split_content(
            b"BT /F1 12 Tf 10 100 Td (Hello World) Tj ET\n",
            b"BT /F1 12 Tf 10 50 Td (Added by the editor) Tj ET\n",
        );
        let (out, report) = run(&src, "World", "Rust").expect("edit");
        assert_eq!(report.replaced, 1);

        // The new stream carries BOTH parts: the page still draws the stamp.
        let content = page_content_text(&out);
        let text = String::from_utf8_lossy(&content);
        assert!(
            text.contains("(Hello Rust)"),
            "the text was not changed: {text}"
        );
        assert!(
            text.contains("(Added by the editor)"),
            "the stamp was lost: {text}"
        );

        // And the page draws it from ONE stream now.
        assert!(
            matches!(page_contents(&out), Obj::Ref(_)),
            "the page should point at a single content stream"
        );

        // The original bytes are still a byte-identical prefix.
        assert_eq!(
            out.get(..src.len()),
            Some(src.as_slice()),
            "original preserved"
        );
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
