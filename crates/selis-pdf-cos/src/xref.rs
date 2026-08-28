//! Classic cross-reference table and trailer (SL-1.COS.03).
//!
//! Parses `startxref`, the classic xref table, the trailer dictionary, and
//! `/Prev` chains. Tolerates the real-world deviations the task lists:
//! wrong subsection counts, off-by-N object offsets, missing `endobj`, and a
//! `/Prev` cycle (depth- and visited-set-guarded).
//!
//! Xref *streams* and object streams are SL-1.COS.04; this module is the
//! classic-table half.

use std::collections::BTreeMap;

use selis_error::{err, Code, Result};

use crate::obj::{Obj, Ref};
use crate::parse::ObjectParser;

/// An entry in a cross-reference table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    /// A free entry: generation of the next free object, and the next free
    /// object number (the classic linked free list).
    Free {
        /// The next free object number.
        next_free: u32,
        /// The generation of the next free object.
        gen: u16,
    },
    /// An in-use entry: byte offset of the object and its generation.
    InUse {
        /// Byte offset of the object.
        offset: u64,
        /// Generation number.
        gen: u16,
    },
    /// A compressed entry: the object lives in an object stream.
    Compressed {
        /// The object stream number.
        objstm: u32,
        /// The index of the object within that stream.
        index: u32,
    },
}

impl XrefEntry {
    /// The generation number of the entry.
    #[must_use]
    pub const fn gen(self) -> u16 {
        match self {
            XrefEntry::Free { gen, .. } | XrefEntry::InUse { gen, .. } => gen,
            XrefEntry::Compressed { .. } => 0,
        }
    }
}

/// A parsed cross-reference index: object number → entry.
///
/// `BTreeMap` gives deterministic iteration for `inspect --json` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XrefIndex {
    /// Object number → entry.
    pub entries: BTreeMap<u32, XrefEntry>,
    /// The trailer dictionary of the newest revision.
    pub trailer: Vec<(selis_bytes::Bytes, Obj)>,
    /// The byte offset of the `startxref` keyword.
    pub startxref_offset: u64,
    /// The trailer's `/Size` when present.
    pub size: Option<u64>,
    /// The trailer's `/Root` reference when present.
    pub root: Option<Ref>,
    /// The trailer's `/Prev` chain offset (0 = none), for cycle detection.
    pub prev: Option<u64>,
}

impl XrefIndex {
    /// The entry for an object number, if any.
    #[must_use]
    pub fn get(&self, num: u32) -> Option<XrefEntry> {
        self.entries.get(&num).copied()
    }
}

/// Parse the classic xref section starting at the `startxref` keyword
/// position. The function reads the integer value after `startxref` to find
/// the actual xref table offset, then walks the table and `/Prev` chain.
///
/// # Budget
///
/// Charges the depth budget per `/Prev` hop (cycle-guarded), and the object
/// budget per xref entry and per trailer object.
///
/// # Malformed Input
///
/// Returns `XREF_MALFORMED` for an unreadable table, `XREF_PREV_CYCLE` for a
/// cyclic `/Prev` chain, and `TRAILER_MISSING_ROOT` when no revision supplies
/// a `/Root`.
pub fn parse_classic_xref(
    src: &[u8],
    startxref_keyword_pos: u64,
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<XrefIndex> {
    let mut index = XrefIndex {
        startxref_offset: startxref_keyword_pos,
        ..XrefIndex::default()
    };
    // Read the integer after `startxref`.
    let p = usize::try_from(startxref_keyword_pos).unwrap_or(usize::MAX);
    let after_kw = p.saturating_add("startxref".len());
    let (xref_offset, _) = read_int(src, after_kw)?;
    let mut cursor = u64::try_from(xref_offset).unwrap_or(u64::MAX);
    let mut visited: Vec<u64> = Vec::new();

    // Walk `/Prev` chains, newest first, newest winning on conflict.
    loop {
        if visited.contains(&cursor) {
            return Err(err!(
                Code::XrefPrevCycle,
                during = "xref-prev-chain",
                at = cursor
            ));
        }
        if visited.len() >= 512 {
            return Err(err!(
                Code::XrefPrevCycle,
                during = "xref-prev-chain",
                at = cursor
            ));
        }
        visited.push(cursor);
        g.enter()?; // depth budget per hop

        let pos = usize::try_from(cursor).unwrap_or(usize::MAX);
        let slice = src
            .get(pos..)
            .ok_or_else(|| err!(Code::XrefMalformed, during = "xref-table", at = cursor))?;

        // Skip whitespace then expect `xref`.
        let xref_kw = find_keyword(slice, b"xref")
            .ok_or_else(|| err!(Code::XrefMalformed, during = "xref-table", at = cursor))?;
        let mut p = pos.saturating_add(xref_kw).saturating_add(4);

        // Subsection entries: `N COUNT` then COUNT entry lines.
        let (entries, trailer_pos) = read_subsection_entries(src, p, budget, g)?;
        p = trailer_pos;
        index.entries.extend(entries);

        // Parse the trailer dictionary.
        let (trailer, _next_p) = parse_trailer(src, p, budget, g)?;
        if index.trailer.is_empty() {
            index.trailer = trailer.clone();
        }
        for (k, v) in &trailer {
            if k.as_slice() == b"Size" {
                if let Obj::Int(s) = v {
                    if index.size.is_none() {
                        index.size = Some(u64::try_from(*s).unwrap_or(u64::MAX));
                    }
                }
            }
            if k.as_slice() == b"Root" {
                if let Obj::Ref(r) = v {
                    if index.root.is_none() {
                        index.root = Some(*r);
                    }
                }
            }
            if k.as_slice() == b"Prev" {
                if let Obj::Int(p) = v {
                    if index.prev.is_none() {
                        index.prev = Some(u64::try_from(*p).unwrap_or(u64::MAX));
                    }
                }
            }
        }

        match index.prev {
            Some(prev) if prev != 0 => {
                cursor = prev;
                index.prev = None; // clear so we don't loop on the same value
            }
            _ => break,
        }
    }

    if index.root.is_none() {
        return Err(err!(
            Code::TrailerMissingRoot,
            during = "xref-trailer",
            at = startxref_keyword_pos
        ));
    }
    Ok(index)
}

/// Read the integer that follows the `startxref` keyword (at `after_kw`).
///
/// Returns `(offset, next_position)`.
///
/// # Budget
///
/// No heap allocation; reads a single integer.
///
/// # Malformed Input
///
/// `XREF_MALFORMED` when the value is not a valid integer.
pub(crate) fn read_startxref_value(src: &[u8], after_kw: usize) -> Result<(u64, usize)> {
    let (off, next) = read_int(src, after_kw)?;
    Ok((u64::try_from(off).unwrap_or(u64::MAX), next))
}

/// Parse exactly one revision (classic xref table or xref stream) at `cursor`.
///
/// Returns `(entries, trailer, /Prev)` — the per-revision data SL-1.COS.05
/// keeps addressable.  Dispatches automatically between classic tables and
/// xref streams based on the first non-whitespace bytes at the cursor.
pub(crate) fn parse_one_revision(
    src: &[u8],
    cursor: u64,
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<(
    BTreeMap<u32, XrefEntry>,
    Vec<(selis_bytes::Bytes, Obj)>,
    Option<u64>,
)> {
    let pos = usize::try_from(cursor).unwrap_or(usize::MAX);
    let _ = src
        .get(pos..)
        .ok_or_else(|| err!(Code::XrefMalformed, during = "xref-table", at = cursor))?;

    // Skip whitespace and detect the revision type.
    let p = skip_ws(src, pos);

    if src.get(p..p.saturating_add(4)) == Some(b"xref") {
        parse_classic_revision(src, p.saturating_add(4), budget, g)
    } else if src.get(p).is_some_and(|&b| b.is_ascii_digit())
        && looks_like_object_header(src, p)
    {
        parse_xref_stream_revision(src, cursor, p, budget, g)
    } else if let Some(kw) = find_xref_keyword_near(src, p) {
        // The `startxref` offset points before the real table (a stray EOL or
        // `endobj` in between), or into unrelated bytes that merely start with
        // a digit (a mis-aimed offset landing in a content stream): resume
        // from the `xref` keyword itself.
        parse_classic_revision(src, kw.saturating_add(4), budget, g)
    } else if let Some(obj_pos) = find_obj_header_near(src, p) {
        // Same mis-aim, but the revision is an xref stream object.
        parse_xref_stream_revision(src, cursor, obj_pos, budget, g)
    } else {
        Err(err!(
            Code::XrefMalformed,
            during = "xref-table",
            at = cursor
        ))
    }
}

/// Parse a classic xref table that starts at byte `p` (after the `xref`
/// keyword, which has already been consumed from the cursor).
fn parse_classic_revision(
    src: &[u8],
    mut p: usize,
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<(
    BTreeMap<u32, XrefEntry>,
    Vec<(selis_bytes::Bytes, Obj)>,
    Option<u64>,
)> {
    let mut entries: BTreeMap<u32, XrefEntry> = BTreeMap::new();

    // Subsection entries: `N COUNT` then COUNT entry lines.
    let (parsed, trailer_pos) = read_subsection_entries(src, p, budget, g)?;
    entries.extend(parsed);
    p = trailer_pos;

    // Parse the trailer dictionary.
    let (trailer, _next_p) = parse_trailer(src, p, budget, g)?;
    let prev = trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"Prev")
        .and_then(|(_, v)| match v {
            Obj::Int(p) => Some(u64::try_from(*p).unwrap_or(u64::MAX)),
            _ => None,
        });
    Ok((entries, trailer, prev))
}

/// Parse an xref stream (`/Type /XRef`) that starts at byte `p` (the object
/// number of the stream object).
fn parse_xref_stream_revision(
    src: &[u8],
    cursor: u64,
    p: usize,
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<(
    BTreeMap<u32, XrefEntry>,
    Vec<(selis_bytes::Bytes, Obj)>,
    Option<u64>,
)> {
    // Resolve the stream object: dict plus raw (unfiltered) body.
    let offset = u64::try_from(p).unwrap_or(cursor);
    let obj = crate::resolve::resolve_object(src, offset, budget, g)?;
    let (dict, data) = match &obj {
        Obj::Stream { dict, data } => (dict, data.as_slice()),
        _ => {
            return Err(err!(
                Code::XrefMalformed,
                during = "xref-stream",
                at = offset,
                detail = "not an xref stream object"
            ));
        }
    };
    // Decode the payload through the `/Filter` chain with `/DecodeParms`:
    // xref streams commonly pair `/FlateDecode` with a PNG predictor, and the
    // entry fields are unusable unless the predictor is undone.
    let payload = decode_stream_payload(dict, data, budget, g);
    let xs = crate::xref_stream::parse_xref_stream(dict, &payload, budget, g)?;
    let mut entries = BTreeMap::new();
    xs.apply(&mut entries);
    let trailer = xs.trailer;
    let prev = trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"Prev")
        .and_then(|(_, v)| match v {
            Obj::Int(prev) => Some(u64::try_from(*prev).unwrap_or(u64::MAX)),
            _ => None,
        });
    Ok((entries, trailer, prev))
}

/// Find `startxref` in the last `window` bytes of the buffer.
///
/// Returns the byte offset of the **last** `startxref` keyword (the newest
/// revision's), or `None` if absent. Scanning from the end is what makes
/// incremental files resolve to the newest revision rather than the original.
///
/// # Budget
///
/// Scans at most `window` bytes; no heap allocation.
///
/// # Malformed Input
///
/// `None` means the tail does not contain a `startxref` keyword — a file the
/// classic-trailer path cannot open (the damaged-file reconstruction in
/// SL-1.COS.06 is the fallback).
#[must_use]
pub fn find_startxref(src: &[u8], window: usize) -> Option<u64> {
    let tail_start = src.len().saturating_sub(window);
    let tail = src.get(tail_start..)?;
    // Find the LAST `startxref` in the window.
    let mut idx = None;
    let mut from = 0usize;
    while let Some(rel) = find_keyword(tail.get(from..)?, b"startxref") {
        idx = Some(from.saturating_add(rel));
        from = idx.unwrap_or(0).saturating_add(1);
    }
    idx.map(|i| tail_start.saturating_add(i) as u64)
}

/// Read the decimal integer at `p`, skipping surrounding whitespace.
/// Returns `(value, next_position)`.
fn read_int(src: &[u8], mut p: usize) -> Result<(i64, usize)> {
    p = skip_ws(src, p);
    let mut value: i64 = 0;
    let mut seen = false;
    while let Some(&b) = src.get(p) {
        if b.is_ascii_digit() {
            seen = true;
            let d = i64::from(b.wrapping_sub(b'0'));
            value = match value.checked_mul(10).and_then(|v| v.checked_add(d)) {
                Some(v) => v,
                None => {
                    return Err(err!(
                        Code::LexNumberOverflow,
                        during = "xref-int",
                        at = p as u64
                    ))
                }
            };
            p = p.saturating_add(1);
        } else {
            break;
        }
    }
    if !seen {
        return Err(err!(
            Code::XrefMalformed,
            during = "xref-int",
            at = p as u64
        ));
    }
    Ok((value, p))
}

/// Read the two numbers of a subsection header, consuming trailing whitespace
/// so `p` lands on the first entry line.
fn read_subsection_header(src: &[u8], p: usize) -> Result<(u32, u32, usize)> {
    let (start, p) = read_int(src, p)?;
    let (count, p) = read_int(src, p)?;
    let p = skip_ws(src, p);
    Ok((
        u32::try_from(start).unwrap_or(u32::MAX),
        u32::try_from(count).unwrap_or(u32::MAX),
        p,
    ))
}

/// Read subsection entries (`N COUNT` headers followed by entry lines) until
/// the `trailer` keyword. Returns the entries and the `trailer` position.
///
/// A damaged writer can declare more entries than it ships; the loop stops
/// early when it reaches `trailer` rather than reading past the table into
/// the trailer dictionary (SL-1.ROB.01).
fn read_subsection_entries(
    src: &[u8],
    mut p: usize,
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<(BTreeMap<u32, XrefEntry>, usize)> {
    let _ = budget;
    let mut entries: BTreeMap<u32, XrefEntry> = BTreeMap::new();
    loop {
        p = skip_ws(src, p);
        if src.get(p..p.saturating_add(7)) == Some(b"trailer") {
            return Ok((entries, p));
        }
        let (start_num, count, next_p) = read_subsection_header(src, p)?;
        p = next_p;
        let start_num_u64 = u64::from(start_num);
        for i in 0..count {
            // A misdeclared count can overshoot into the trailer.
            let q = skip_ws(src, p);
            if src.get(q..q.saturating_add(7)) == Some(b"trailer") {
                return Ok((entries, q));
            }
            g.charge_one(selis_sandbox::Resource::Objects)?;
            let (offset, gen, in_use, next_p) = read_entry_line(src, p)?;
            p = next_p;
            let num = u32::try_from(start_num_u64.saturating_add(u64::from(i))).unwrap_or(u32::MAX);
            let entry = if in_use {
                XrefEntry::InUse { offset, gen }
            } else {
                XrefEntry::Free {
                    next_free: u32::try_from(offset).unwrap_or(u32::MAX),
                    gen,
                }
            };
            entries.insert(num, entry);
        }
    }
}

/// Read a bounded run of decimal digits: up to `max_digits`. Returns the digit
/// count, the value, and the position after the last digit.
fn read_decimal_field(src: &[u8], mut p: usize, max_digits: usize) -> (usize, u64, usize) {
    let mut v: u64 = 0;
    let mut n = 0usize;
    while n < max_digits {
        let Some(&b) = src.get(p) else {
            break;
        };
        if !b.is_ascii_digit() {
            break;
        }
        v = v
            .saturating_mul(10)
            .saturating_add(u64::from(b.wrapping_sub(b'0')));
        n = n.saturating_add(1);
        p = p.saturating_add(1);
    }
    (n, v, p)
}

/// Read one xref entry line starting at `p`, returning `(offset, gen,
/// in_use, next_position)`.
///
/// The spec fixes entries at 20 bytes (10-digit offset, space, 5-digit
/// generation, space, `n`/`f` flag, 2-byte EOL), but real writers deviate:
/// a trailing space before CRLF makes 21-byte lines, a bare LF makes 19.
/// The fields are therefore parsed individually and the remainder of the
/// line consumed afterwards, instead of slicing a fixed 20-byte record
/// (SL-1.ROB.01).
fn read_entry_line(src: &[u8], p: usize) -> Result<(u64, u16, bool, usize)> {
    let is_inline_ws = |b: &u8| matches!(b, b' ' | b'\t' | 0x0c | 0x00);
    let mut i = p;
    while src.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i = i.saturating_add(1);
    }
    // The offset field (at least one digit, at most ten).
    let (offset_digits, offset, next) = read_decimal_field(src, i, 10);
    if offset_digits == 0 {
        return Err(err!(Code::XrefMalformed, during = "xref-entry", at = p as u64));
    }
    i = next;
    while src.get(i).is_some_and(is_inline_ws) {
        i = i.saturating_add(1);
    }
    // The generation field.
    let (_gen_digits, gen, next) = read_decimal_field(src, i, 5);
    i = next;
    while src.get(i).is_some_and(is_inline_ws) {
        i = i.saturating_add(1);
    }
    // The usage flag.
    let flag = src.get(i).copied().unwrap_or(b' ');
    let in_use = flag == b'n';
    if src.get(i).is_some() {
        i = i.saturating_add(1);
    }
    // Trailing whitespace and the end-of-line (CRLF, LF, or CR).
    while src.get(i).is_some_and(is_inline_ws) {
        i = i.saturating_add(1);
    }
    if src.get(i) == Some(&b'\r') {
        i = i.saturating_add(1);
    }
    if src.get(i) == Some(&b'\n') {
        i = i.saturating_add(1);
    }
    Ok((offset, u16::try_from(gen).unwrap_or(u16::MAX), in_use, i))
}

/// Parse the `trailer << ... >>` dictionary.
fn parse_trailer(
    src: &[u8],
    p: usize,
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Result<(Vec<(selis_bytes::Bytes, Obj)>, usize)> {
    // Find the `<<` after `trailer` and lex from just before it so the lexer
    // produces the `DictStart` token itself.
    let after_trailer = p.saturating_add("trailer".len());
    let q = skip_ws(src, after_trailer);
    if src.get(q) != Some(&b'<') || src.get(q.saturating_add(1)) != Some(&b'<') {
        return Err(err!(
            Code::XrefMalformed,
            during = "xref-trailer",
            at = q as u64
        ));
    }

    let trailer_slice = src.get(after_trailer..).unwrap_or(&[]);
    let mut lexer = crate::Lexer::new(trailer_slice);
    let mut tokens = Vec::new();
    loop {
        match lexer.next_token(g)? {
            Some(tok) => {
                let is_dict_end = matches!(tok, crate::Token::DictEnd);
                tokens.push(tok);
                if is_dict_end {
                    break;
                }
            }
            None => break,
        }
    }
    let mut parser = ObjectParser::new(&tokens, budget);
    let obj = parser.parse(g)?;
    let dict = match obj {
        Obj::Dict(pairs) => pairs,
        other => {
            return Err(err!(
                Code::XrefMalformed,
                during = "xref-trailer",
                at = q as u64,
                detail = format!("trailer is a {}", other.type_name())
            ));
        }
    };
    let consumed = after_trailer.saturating_add(usize::try_from(lexer.pos()).unwrap_or(usize::MAX));
    Ok((dict, consumed))
}

fn skip_ws(src: &[u8], mut p: usize) -> usize {
    while let Some(&b) = src.get(p) {
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x00) {
            p = p.saturating_add(1);
        } else {
            break;
        }
    }
    p
}

/// Find a keyword in `src`, returning its index or `None`.
fn find_keyword(src: &[u8], kw: &[u8]) -> Option<usize> {
    src.windows(kw.len()).position(|w| w == kw)
}

/// Decode a stream object's payload through its `/Filter` chain with
/// `/DecodeParms` (predictors included). Falls back to the raw data when no
/// filter is declared or the decode fails, so a damaged stream never aborts
/// the revision walk.
///
/// # Budget
///
/// `decode_chain` bounds every stage at `budget.bytes`.
///
/// # Malformed Input
///
/// Never errors: a decode failure returns the raw (still filtered) data and
/// the caller's structural checks decide what happens next.
pub(crate) fn decode_stream_payload(
    dict: &[(selis_bytes::Bytes, Obj)],
    data: &[u8],
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
) -> Vec<u8> {
    let filters: Vec<String> = match dict
        .iter()
        .find(|(k, _)| k.as_slice() == b"Filter")
        .map(|(_, v)| v)
    {
        Some(Obj::Name(n)) => vec![String::from_utf8_lossy(n.as_slice()).to_string()],
        Some(Obj::Array(items)) => items
            .iter()
            .filter_map(|o| match o {
                Obj::Name(n) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
                _ => None,
            })
            .collect(),
        _ => return data.to_vec(),
    };
    if filters.is_empty() {
        return data.to_vec();
    }
    let parms = decode_parms_from_dict(dict);
    selis_pdf_filter::decode_chain(&filters, &parms, data, budget.bytes, g)
        .unwrap_or_else(|_| data.to_vec())
}

/// `/DecodeParms` from a stream dict: one dict, or an array aligned with the
/// filter chain. Unknown or absent parameters keep their spec defaults.
fn decode_parms_from_dict(
    dict: &[(selis_bytes::Bytes, Obj)],
) -> Vec<selis_pdf_filter::DecodeParms> {
    let from_pairs = |pairs: &[(selis_bytes::Bytes, Obj)]| -> selis_pdf_filter::DecodeParms {
        let int = |key: &[u8]| -> i64 {
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == key)
                .and_then(|(_, v)| match v {
                    Obj::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0)
        };
        selis_pdf_filter::DecodeParms {
            predictor: u16::try_from(int(b"Predictor")).unwrap_or(1),
            columns: u32::try_from(int(b"Columns")).unwrap_or(1),
            colors: u32::try_from(int(b"Colors")).unwrap_or(1),
            bits_per_component: u32::try_from(int(b"BitsPerComponent")).unwrap_or(8),
            early_change: u8::try_from(int(b"EarlyChange")).unwrap_or(0),
        }
    };
    match dict
        .iter()
        .find(|(k, _)| k.as_slice() == b"DecodeParms")
        .map(|(_, v)| v)
    {
        Some(Obj::Dict(pairs)) => vec![from_pairs(pairs)],
        Some(Obj::Array(items)) => items
            .iter()
            .map(|o| match o {
                Obj::Dict(pairs) => from_pairs(pairs),
                _ => selis_pdf_filter::DecodeParms::default(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// How far past a bad `startxref` offset to scan for the real xref section.
const XREF_RECOVERY_WINDOW: usize = 4096;

/// True when `src[idx]` begins a whole `xref` keyword: the byte before is not
/// alphanumeric (rejecting the `xref` inside `startxref`) and neither is the
/// byte after.
fn xref_keyword_at(src: &[u8], idx: usize) -> bool {
    if src.get(idx..idx.saturating_add(4)) != Some(b"xref") {
        return false;
    }
    let before_ok = idx == 0
        || src
            .get(idx.saturating_sub(1))
            .is_some_and(|&b| !b.is_ascii_alphanumeric());
    let after_ok = src
        .get(idx.saturating_add(4))
        .is_none_or(|&b| !b.is_ascii_alphanumeric());
    before_ok && after_ok
}

/// True when `src[idx..]` starts with the shape of an indirect-object header:
/// `N G obj` — digits, whitespace, digits, whitespace, then the `obj` keyword
/// terminated by a non-word byte. Used to tell a real xref-stream object apart
/// from unrelated digits a mis-aimed `startxref` can land on (SL-1.ROB.01).
fn looks_like_object_header(src: &[u8], idx: usize) -> bool {
    let is_ws = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x00);
    let mut i = idx;
    // The object number (at least one digit).
    let num_start = i;
    while src.get(i).is_some_and(|b| b.is_ascii_digit()) {
        i = i.saturating_add(1);
    }
    if i == num_start || !src.get(i).is_some_and(is_ws) {
        return false;
    }
    while src.get(i).is_some_and(is_ws) {
        i = i.saturating_add(1);
    }
    // The generation number.
    let gen_start = i;
    while src.get(i).is_some_and(|b| b.is_ascii_digit()) {
        i = i.saturating_add(1);
    }
    if i == gen_start || !src.get(i).is_some_and(is_ws) {
        return false;
    }
    while src.get(i).is_some_and(is_ws) {
        i = i.saturating_add(1);
    }
    if src.get(i..i.saturating_add(3)) != Some(b"obj") {
        return false;
    }
    // The keyword must end there: `objx` is a word, not the keyword.
    src.get(i.saturating_add(3))
        .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_')
}


/// Scan a bounded window after `pos` for the `xref` keyword of a classic
/// table.
///
/// A damaged writer can store a `startxref` offset a few bytes short of the
/// real table (pointing at a stray EOL or the `endobj` before it). The strict
/// cursor check rejects such files even though the table itself is intact, so
/// recovery scans forward within [`XREF_RECOVERY_WINDOW`] and resumes from the
/// first whole `xref` keyword.
///
/// # Budget
///
/// No heap allocation; scans at most `XREF_RECOVERY_WINDOW` bytes.
#[must_use]
fn find_xref_keyword_near(src: &[u8], pos: usize) -> Option<usize> {
    let end = pos.saturating_add(XREF_RECOVERY_WINDOW).min(src.len());
    let mut i = pos;
    while i < end {
        if xref_keyword_at(src, i) {
            return Some(i);
        }
        i = i.saturating_add(1);
    }
    None
}

/// Scan a bounded window after `pos` for the byte offset of an `N G obj`
/// header (the xref stream object a mis-aimed `startxref` points just before).
///
/// # Budget
///
/// No heap allocation; scans at most `XREF_RECOVERY_WINDOW` bytes.
#[must_use]
fn find_obj_header_near(src: &[u8], pos: usize) -> Option<usize> {
    let end = pos.saturating_add(XREF_RECOVERY_WINDOW).min(src.len());
    let slice = src.get(pos..end)?;
    let mut from = 0usize;
    while from < slice.len() {
        let rel = slice.get(from..)?.windows(4).position(|w| w == b" obj")?;
        let rel = from.saturating_add(rel);
        // Walk back over the generation and object numbers.
        let mut j = rel;
        while j > 0
            && slice
                .get(j.saturating_sub(1))
                .is_some_and(|b| b.is_ascii_digit())
        {
            j = j.saturating_sub(1);
        }
        let gen_digits = rel.saturating_sub(j);
        if gen_digits > 0 && j > 0 && slice.get(j.saturating_sub(1)) == Some(&b' ') {
            let mut k = j.saturating_sub(1);
            while k > 0
                && slice
                    .get(k.saturating_sub(1))
                    .is_some_and(|b| b.is_ascii_digit())
            {
                k = k.saturating_sub(1);
            }
            let num_digits = j.saturating_sub(1).saturating_sub(k);
            let boundary_ok = k == 0
                || slice
                    .get(k.saturating_sub(1))
                    .is_some_and(|b| !b.is_ascii_alphanumeric());
            if num_digits > 0
                && boundary_ok
                && slice
                    .get(rel.saturating_add(4))
                    .is_none_or(|&b| b.is_ascii_whitespace())
            {
                return Some(pos.saturating_add(k));
            }
        }
        from = rel.saturating_add(1);
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss
    )]

    use super::*;
    use selis_sandbox::{CancelToken, FixedClock};

    /// A minimal classic PDF with one object and an xref table.
    fn classic_pdf() -> Vec<u8> {
        let body = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        // Object 1 begins after the `%PDF-1.4\n` header.
        let obj1_offset = "%PDF-1.4\n".len();
        let xref_offset = body.len();
        let mut out = body.to_vec();
        out.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                obj1_offset, xref_offset
            )
            .as_bytes(),
        );
        out
    }

    fn guard() -> selis_sandbox::BudgetGuard<'static> {
        selis_sandbox::Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    #[test]
    fn classic_xref_parses() {
        let pdf = classic_pdf();
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = guard();
        let startxref = find_startxref(&pdf, 2048).expect("startxref found");
        let index = parse_classic_xref(&pdf, startxref, &budget, &mut g).expect("xref parses");
        assert_eq!(index.root, Some(Ref::new(1, 0)));
        assert_eq!(index.size, Some(2));
        // Object 1 is in-use at the body offset.
        match index.get(1) {
            Some(XrefEntry::InUse { offset, gen }) => {
                // Object 1 begins right after the header line `%PDF-1.4\n`.
                assert_eq!(offset, "%PDF-1.4\n".len() as u64);
                assert_eq!(gen, 0);
            }
            other => panic!("expected in-use entry, got {other:?}"),
        }
    }

    /// `startxref` points at stray bytes (an `endobj`) before the real `xref`
    /// keyword; the revision walk scans forward and recovers the table.
    #[test]
    fn misaimed_startxref_recovers_the_classic_table() {
        let body = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let obj1_offset = "%PDF-1.4\n".len();
        let mut out = body.to_vec();
        let mis_aim = out.len(); // startxref lands here, 8 bytes short
        out.extend_from_slice(b"endobj\n\n");
        out.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                obj1_offset, mis_aim
            )
            .as_bytes(),
        );
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = guard();
        let startxref = find_startxref(&out, 2048).expect("startxref");
        let doc = crate::parse_revisions(&out, startxref, &budget, &mut g).expect("recovers");
        let rev = doc.revisions().last().expect("one revision");
        assert_eq!(rev.root, Some(Ref::new(1, 0)));
        match rev.entries.get(&1) {
            Some(XrefEntry::InUse { offset, gen }) => {
                assert_eq!(*offset, obj1_offset as u64);
                assert_eq!(*gen, 0);
            }
            other => panic!("expected in-use entry, got {other:?}"),
        }
    }

    /// Same mis-aim, but the revision is an xref stream object: the recovery
    /// scan finds the `N G obj` header and parses the stream.
    #[test]
    fn misaimed_startxref_recovers_an_xref_stream() {
        let body = b"%PDF-1.5\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let obj1_offset = "%PDF-1.5\n".len() as u64;
        let mut out = body.to_vec();
        let mis_aim = out.len();
        out.extend_from_slice(b"endobj\n\n");
        // One 4-byte row per entry, /W [1 2 1]:
        //   entry 0: type 0 (free), entry 1: type 1 at obj1_offset.
        let payload: [u8; 8] = [
            0,
            0,
            0,
            0,
            1,
            u8::try_from(obj1_offset >> 8).unwrap_or(0),
            u8::try_from(obj1_offset & 0xff).unwrap_or(0),
            0,
        ];
        out.extend_from_slice(
            format!(
                "7 0 obj\n<< /Type /XRef /Size 2 /Root 1 0 R /W [1 2 1] /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&payload);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out.extend_from_slice(format!("startxref\n{}\n%%EOF\n", mis_aim).as_bytes());
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = guard();
        let startxref = find_startxref(&out, 2048).expect("startxref");
        let doc = crate::parse_revisions(&out, startxref, &budget, &mut g).expect("recovers");
        let rev = doc.revisions().last().expect("one revision");
        assert_eq!(rev.root, Some(Ref::new(1, 0)));
        match rev.entries.get(&1) {
            Some(XrefEntry::InUse { offset, gen }) => {
                assert_eq!(*offset, obj1_offset);
                assert_eq!(*gen, 0);
            }
            other => panic!("expected in-use entry, got {other:?}"),
        }
    }

    #[test]
    fn cyclic_prev_chain_terminates() {
        // A single table whose trailer `/Prev` points back at itself.
        let body = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let xref_offset = body.len();
        let trailer = format!(
            "trailer\n<< /Size 2 /Root 1 0 R /Prev {} >>\nstartxref\n{}\n%%EOF\n",
            xref_offset, xref_offset
        );
        let mut out = body;
        out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        out.extend_from_slice(trailer.as_bytes());
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = guard();
        let startxref = find_startxref(&out, 2048).expect("startxref");
        let e = parse_classic_xref(&out, startxref, &budget, &mut g).expect_err("cycle detected");
        assert_eq!(e.code(), Code::XrefPrevCycle);
    }
}
