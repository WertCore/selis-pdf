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

        // Subsection entries: `N COUNT` then COUNT 20-byte lines.
        loop {
            // At the `trailer` keyword the table is done.
            p = skip_ws(src, p);
            if src.get(p..p.saturating_add(7)) == Some(b"trailer") {
                break;
            }
            // Read the subsection header `N COUNT`.
            let (start_num, count, next_p) = read_subsection_header(src, p)?;
            p = next_p;
            let start_num_u64 = u64::from(start_num);
            for i in 0..count {
                g.charge_one(selis_sandbox::Resource::Objects)?;
                let (offset, gen, in_use, next_p) = read_entry_line(src, p)?;
                p = next_p;
                let num =
                    u32::try_from(start_num_u64.saturating_add(u64::from(i))).unwrap_or(u32::MAX);
                let entry = if in_use {
                    XrefEntry::InUse { offset, gen }
                } else {
                    XrefEntry::Free {
                        next_free: u32::try_from(offset).unwrap_or(u32::MAX),
                        gen,
                    }
                };
                // Newest revision wins.
                index.entries.insert(num, entry);
            }
        }

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
    } else if src.get(p).is_some_and(|&b| b.is_ascii_digit()) {
        parse_xref_stream_revision(src, cursor, p, budget, g)
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

    // Subsection entries: `N COUNT` then COUNT 20-byte lines.
    loop {
        // At the `trailer` keyword the table is done.
        p = skip_ws(src, p);
        if src.get(p..p.saturating_add(7)) == Some(b"trailer") {
            break;
        }
        // Read the subsection header `N COUNT`.
        let (start_num, count, next_p) = read_subsection_header(src, p)?;
        p = next_p;
        let start_num_u64 = u64::from(start_num);
        for i in 0..count {
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
    // Decode the payload (xref streams are typically /FlateDecode).
    let payload = if let Some(Obj::Name(n)) = dict
        .iter()
        .find(|(k, _)| k.as_slice() == b"Filter")
        .map(|(_, v)| v)
    {
        let filt = std::str::from_utf8(n.as_slice()).unwrap_or("");
        selis_pdf_filter::decode(filt, data, budget.bytes, g).unwrap_or_else(|_| data.to_vec())
    } else {
        data.to_vec()
    };
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

/// Read one 20-byte xref entry line (offset, generation, `n`/`f`).
fn read_entry_line(src: &[u8], p: usize) -> Result<(u64, u16, bool, usize)> {
    let line = src
        .get(p..p.saturating_add(20))
        .ok_or_else(|| err!(Code::XrefMalformed, during = "xref-entry", at = p as u64))?;
    let offset_str = line.get(0..10).unwrap_or(&[]);
    let gen_str = line.get(11..16).unwrap_or(&[]);
    let flag = line.get(17).copied().unwrap_or(b' ');

    let parse_field = |field: &[u8]| -> i64 {
        let mut v: i64 = 0;
        for &b in field {
            if b.is_ascii_digit() {
                let d = i64::from(b.wrapping_sub(b'0'));
                v = v.wrapping_mul(10).wrapping_add(d);
            }
        }
        v
    };

    let offset = parse_field(offset_str);
    let gen_i = parse_field(gen_str);
    let offset = u64::try_from(offset).unwrap_or(u64::MAX);
    let gen = u16::try_from(gen_i).unwrap_or(u16::MAX);
    let in_use = flag == b'n';
    Ok((offset, gen, in_use, p.saturating_add(20)))
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
