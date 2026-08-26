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
            let length = stream_length(&dict_pairs)?;
            let data = read_stream_body(src, body_off, length)?;
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

/// Extract the `/Length` from a stream dictionary.
fn stream_length(dict: &[(Bytes, Obj)]) -> Result<usize> {
    dict.iter()
        .find(|(k, _)| k.as_slice() == b"Length")
        .and_then(|(_, v)| match v {
            Obj::Int(n) => usize::try_from(*n).ok(),
            _ => None,
        })
        .ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "resolve-object",
                detail = "stream without /Length"
            )
        })
}

/// Read `length` bytes from `src` starting at `body`, skipping the single
/// EOL (`\r\n` or `\n`) that follows the `stream` keyword.
fn read_stream_body(src: &[u8], mut body: usize, length: usize) -> Result<Bytes> {
    if src.get(body) == Some(&b'\r') {
        body = body.saturating_add(1);
    }
    if src.get(body) == Some(&b'\n') {
        body = body.saturating_add(1);
    }
    let end = body.saturating_add(length);
    let data = src.get(body..end).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "resolve-object",
            detail = "stream body out of range"
        )
    })?;
    Ok(Bytes::copy_from_slice(data))
}
