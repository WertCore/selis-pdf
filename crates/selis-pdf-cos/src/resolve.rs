//! Object-body resolution: given a byte offset, read the `N G obj` header and
//! parse the value. Bridges the xref index (which knows WHERE each object is)
//! and the object model (which needs WHAT each object says).

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::lex::Lexer;
use crate::obj::Obj;
use crate::parse::ObjectParser;

/// Read the object at byte offset `pos` from the buffer.
///
/// Skips the `N G obj` header, parses the value, and stops at `endobj`.
/// For stream objects (`<< … >> stream … endstream`) it returns the dict
/// plus the raw (unfiltered) stream body as [`Obj::Stream`].
///
/// # Budget
///
/// The lexer and parser are charged per token and per object.
///
/// # Malformed Input
///
/// `OBJ_UNEXPECTED` when the offset does not start a valid object header.
pub fn resolve_object(
    src: &[u8],
    pos: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    let p = usize::try_from(pos).unwrap_or(usize::MAX);
    let slice = src
        .get(p..)
        .ok_or_else(|| err!(Code::ObjUnexpected, during = "resolve-object", at = pos))?;
    let mut lexer = Lexer::new(slice);

    // Expect `N G obj`.
    let _num = lexer.next_token(g)?.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "resolve-object",
            at = pos,
            detail = "missing object number"
        )
    })?;
    let _gen = lexer.next_token(g)?.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "resolve-object",
            at = pos,
            detail = "missing generation"
        )
    })?;
    let kw = lexer.next_token(g)?.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "resolve-object",
            at = pos,
            detail = "missing 'obj' keyword"
        )
    })?;
    if !matches!(kw, crate::Token::Obj) {
        return Err(err!(
            Code::ObjUnexpected,
            during = "resolve-object",
            at = pos,
            detail = "expected 'obj' keyword"
        ));
    }

    // Lex tokens until `endobj` — or, for a stream object, at the `stream`
    // keyword (the body is read from the source, not lexed).
    let mut toks = Vec::new();
    loop {
        let Some(tok) = lexer.next_token(g)? else {
            return Err(err!(
                Code::ObjUnexpected,
                during = "resolve-object",
                at = pos,
                detail = "reached end of input before 'endobj'"
            ));
        };
        if matches!(tok, crate::Token::EndObj) {
            break;
        }
        if matches!(tok, crate::Token::Stream) {
            // The lexer consumed `stream`; its position is right after the
            // keyword.  Read the stream body from the source bytes.
            let body_off = p.saturating_add(usize::try_from(lexer.pos()).unwrap_or(0));
            let mut parser = ObjectParser::new(&toks, budget);
            let dict = parser.parse(g)?;
            let dict_pairs = match dict {
                Obj::Dict(pairs) => pairs,
                _ => {
                    return Err(err!(
                        Code::ObjUnexpected,
                        during = "resolve-object",
                        at = pos,
                        detail = "stream dict is not a dictionary",
                    ));
                }
            };
            let length = stream_length(&dict_pairs);
            let data = read_stream_body(src, body_off, length, g)?;
            return Ok(Obj::Stream {
                dict: dict_pairs,
                data,
            });
        }
        toks.push(tok);
    }

    let mut parser = ObjectParser::new(&toks, budget);
    let obj = parser.parse(g)?;
    Ok(obj)
}

/// Extract the `/Length` from a stream dictionary as a direct integer.
///
/// Returns `None` when `/Length` is absent, an indirect reference, or any
/// non-integer value — the caller then falls back to scanning for `endstream`.
fn stream_length(dict: &[(Bytes, Obj)]) -> Option<usize> {
    dict.iter()
        .find(|(k, _)| k.as_slice() == b"Length")
        .and_then(|(_, v)| match v {
            Obj::Int(n) => usize::try_from(*n).ok(),
            _ => None,
        })
}

/// Read a stream body starting at `body` (skipping the single EOL after the
/// `stream` keyword).
///
/// Prefers the declared `/Length` when it is usable (in range and followed by
/// the `endstream` keyword). Real writers ship streams with a missing,
/// indirect, or simply wrong `/Length`, so when the declared length is absent
/// or does not verify, recovery scans forward for the `endstream` keyword and
/// uses the bytes before it (SL-1.ROB.01).
fn read_stream_body(
    src: &[u8],
    mut body: usize,
    declared: Option<usize>,
    g: &mut BudgetGuard<'_>,
) -> Result<Bytes> {
    if src.get(body) == Some(&b'\r') {
        body = body.saturating_add(1);
    }
    if src.get(body) == Some(&b'\n') {
        body = body.saturating_add(1);
    }
    if let Some(length) = declared {
        let end = body.saturating_add(length);
        if end <= src.len() && endstream_follows(src, end) {
            let data = src
                .get(body..end)
                .ok_or_else(|| {
                    err!(
                        Code::ObjUnexpected,
                        during = "resolve-object",
                        detail = "stream body out of range"
                    )
                })?;
            return Ok(Bytes::copy_from_slice(data));
        }
    }
    scan_to_endstream(src, body, g)
}

/// True when the `endstream` keyword follows byte `end`, allowing a single EOL
/// (or run of whitespace) between the stream body and the keyword.
fn endstream_follows(src: &[u8], mut end: usize) -> bool {
    let is_ws = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x00);
    while src.get(end).is_some_and(is_ws) {
        end = end.saturating_add(1);
    }
    src.get(end..end.saturating_add(9)) == Some(b"endstream")
}

/// Scan forward from `start` for the `endstream` keyword and return the bytes
/// before it (stripping the single trailing EOL the spec places there).
fn scan_to_endstream(src: &[u8], start: usize, g: &mut BudgetGuard<'_>) -> Result<Bytes> {
    let hay = src.get(start..).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "resolve-object",
            detail = "stream body out of range"
        )
    })?;
    // Charge the scan per 4 KiB block so a huge stream cannot outrun the
    // budget before the keyword check runs.
    let mut rel: Option<usize> = None;
    let mut i = 0usize;
    while i < hay.len() {
        if g.tick().is_err() {
            return Err(err!(
                Code::ObjUnexpected,
                during = "resolve-object",
                detail = "stream scan exhausted"
            ));
        }
        if hay.get(i..i.saturating_add(9)) == Some(b"endstream") {
            rel = Some(i);
            break;
        }
        i = i.saturating_add(1);
    }
    let rel = rel.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "resolve-object",
            detail = "no endstream keyword"
        )
    })?;
    // Strip the single EOL (CR, LF, or CRLF) immediately before `endstream`.
    let mut end = rel;
    if end > 0 && hay.get(end.saturating_sub(1)) == Some(&b'\n') {
        end = end.saturating_sub(1);
        if end > 0 && hay.get(end.saturating_sub(1)) == Some(&b'\r') {
            end = end.saturating_sub(1);
        }
    } else if end > 0 && hay.get(end.saturating_sub(1)) == Some(&b'\r') {
        end = end.saturating_sub(1);
    }
    let data = hay.get(..end).unwrap_or(&[]);
    Ok(Bytes::copy_from_slice(data))
}

/// How far either side of a mis-aimed xref offset to scan for the real
/// `N G obj` header.
const OFFSET_RECOVERY_WINDOW: usize = 4096;

/// Resolve the object an xref entry points at, identified by its number.
///
/// Tries the indexed offset first. When the bytes there do not parse as an
/// object (a mis-aimed offset — real writers ship whole tables shifted by a
/// constant, pointing at the `endobj` before each object), scans a bounded
/// window around the offset for an `N G obj` header matching `num` and
/// resolves from there.
///
/// # Budget
///
/// The scan is bounded to `2 * OFFSET_RECOVERY_WINDOW` bytes and ticks the
/// budget per byte; resolution charges as [`resolve_object`].
///
/// # Malformed Input
///
/// The original [`resolve_object`] error when no matching header exists in
/// the window.
pub fn resolve_object_numbered(
    src: &[u8],
    pos: u64,
    num: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    let direct = resolve_object(src, pos, budget, g);
    if direct.is_ok() {
        return direct;
    }
    let p = usize::try_from(pos).unwrap_or(usize::MAX);
    match find_object_header_near(src, p, num, g) {
        Some(found) if found != p => {
            let at = u64::try_from(found).unwrap_or(u64::MAX);
            resolve_object(src, at, budget, g)
        }
        // The offset may hold a bare value rather than an `N G obj` header —
        // damaged files (and inline `/Root` dicts in a trailer) do this.
        // Parse one complete value at the offset before giving up, but only
        // when the bytes plausibly begin a value (a bare word like "junk" is
        // a tolerated Name, not a value start).
        _ => {
            let mut q = p;
            while src
                .get(q)
                .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x00))
            {
                q = q.saturating_add(1);
            }
            let is_value_start = src.get(q).is_some_and(|&b| {
                matches!(b, b'<' | b'[' | b'(' | b'/' | b'0'..=b'9' | b't' | b'f' | b'n')
            });
            if is_value_start {
                resolve_object_bare(src, pos, budget, g)
            } else {
                direct
            }
        }
    }
}

/// Parse one complete value at `pos` without an `N G obj` header.
///
/// Lexes until a scalar has closed or every `[`/`<<` opener has been
/// matched, then parses that single value (SL-1.ROB.01 — mis-aimed offsets
/// and inline dictionary values).
fn resolve_object_bare(
    src: &[u8],
    pos: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    let p = usize::try_from(pos).unwrap_or(usize::MAX);
    let slice = src
        .get(p..)
        .ok_or_else(|| err!(Code::ObjUnexpected, during = "resolve-object", at = pos))?;
    let mut lexer = Lexer::new(slice);
    let mut toks = Vec::new();
    let mut depth = 0u32;
    loop {
        let Some(tok) = lexer.next_token(g)? else {
            break;
        };
        match tok {
            crate::Token::ArrayStart | crate::Token::DictStart => {
                depth = depth.saturating_add(1);
            }
            crate::Token::ArrayEnd | crate::Token::DictEnd => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        toks.push(tok);
        if depth == 0 {
            break;
        }
    }
    if toks.is_empty() {
        return Err(err!(
            Code::ObjUnexpected,
            during = "resolve-object",
            at = pos,
            detail = "no value at offset"
        ));
    }
    let mut parser = ObjectParser::new(&toks, budget);
    parser.parse(g)
}

/// Scan a bounded window around `pos` for an `N G obj` header of object
/// `num`, returning the offset of the object number's first digit.
///
/// The forward range is scanned first: the common corruption is an offset
/// that points a few bytes *short* of the header (at the EOL or `endobj`
/// before it), so the object usually sits just after the indexed offset.
fn find_object_header_near(
    src: &[u8],
    pos: usize,
    num: u32,
    g: &mut BudgetGuard<'_>,
) -> Option<usize> {
    let lo = pos.saturating_sub(OFFSET_RECOVERY_WINDOW);
    let hi = pos.saturating_add(OFFSET_RECOVERY_WINDOW).min(src.len());
    let mut i = pos;
    while i < hi {
        if g.tick().is_err() {
            return None;
        }
        if object_header_at(src, i, num) {
            return Some(i);
        }
        i = i.saturating_add(1);
    }
    let mut i = lo;
    while i < pos {
        if g.tick().is_err() {
            return None;
        }
        if object_header_at(src, i, num) {
            return Some(i);
        }
        i = i.saturating_add(1);
    }
    None
}

/// True when `src[idx..]` begins `num <gen> obj` — the object number matches
/// `num` exactly, and the match is not a suffix of a longer number.
fn object_header_at(src: &[u8], idx: usize, num: u32) -> bool {
    // Reject a suffix match inside a longer number ("21 0 obj" while seeking 1).
    if idx > 0
        && src
            .get(idx.saturating_sub(1))
            .is_some_and(|&b| b.is_ascii_digit())
    {
        return false;
    }
    let mut i = idx;
    let mut v: u32 = 0;
    let mut digits = 0usize;
    while let Some(&b) = src.get(i) {
        if b.is_ascii_digit() {
            v = v
                .saturating_mul(10)
                .saturating_add(u32::from(b.wrapping_sub(b'0')));
            i = i.saturating_add(1);
            digits = digits.saturating_add(1);
        } else {
            break;
        }
    }
    if digits == 0 || v != num {
        return false;
    }
    let is_ws = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x00);
    if !src.get(i).is_some_and(is_ws) {
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
    if i == gen_start {
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
        .is_none_or(|&b| !b.is_ascii_alphanumeric())
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

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// A file whose xref offset for object 1 points 8 bytes short — at the
    /// `\nendobj\n` before the header — the exact defect of
    /// `corpus/pdfs/outlines_for_editor.pdf`.
    #[test]
    fn misaimed_offset_finds_the_object_header() {
        let src = b"%PDF-1.4\n9 0 obj\n<< /A 1 >>\nendobj\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let real = src
            .windows(7)
            .position(|w| w == b"1 0 obj")
            .expect("header");
        let mis_aimed = real.saturating_sub(8); // lands on "\nendobj"
        let budget = Budget::unlimited();
        let mut g = guard();
        let obj =
            resolve_object_numbered(src, mis_aimed as u64, 1, &budget, &mut g).expect("recovers");
        let Obj::Dict(pairs) = &obj else {
            panic!("expected a dict");
        };
        assert!(pairs.iter().any(|(k, v)| k.as_slice() == b"Type"
            && matches!(v, Obj::Name(n) if n.as_slice() == b"Catalog")));
    }

    /// When no matching header exists anywhere in the window, the original
    /// typed error is returned (recovery never masks a real failure).
    #[test]
    fn scan_with_no_match_returns_the_original_error() {
        let src = b"%PDF-1.4\njust junk, no object headers here\n%%EOF\n";
        let budget = Budget::unlimited();
        let mut g = guard();
        let e = resolve_object_numbered(src, 9, 1, &budget, &mut g).expect_err("no match");
        assert_eq!(e.code(), Code::ObjUnexpected);
    }

    /// A number match must not be a suffix of a longer object number
    /// (`1 0 obj` is not found inside `21 0 obj`).
    #[test]
    fn header_match_is_not_a_suffix() {
        let src = b"%PDF-1.4\n21 0 obj\n<< >>\nendobj\n";
        assert!(!object_header_at(src, 10, 1), "'1' inside '21'");
        assert!(object_header_at(src, 9, 21), "'21' as a whole");
    }

    /// A well-formed offset skips the scan entirely (direct resolve).
    #[test]
    fn exact_offset_resolves_directly() {
        let src = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let at = src
            .windows(7)
            .position(|w| w == b"1 0 obj")
            .expect("header");
        let budget = Budget::unlimited();
        let mut g = guard();
        let obj = resolve_object_numbered(src, at as u64, 1, &budget, &mut g).expect("resolves");
        let Obj::Dict(pairs) = &obj else {
            panic!("expected a dict");
        };
        assert!(pairs.iter().any(|(k, _)| k.as_slice() == b"Type"));
    }
}
