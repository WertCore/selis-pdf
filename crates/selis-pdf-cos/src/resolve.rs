//! Object-body resolution: given a byte offset, read the `N G obj` header and
//! parse the value. Bridges the xref index (which knows WHERE each object is)
//! and the object model (which needs WHAT each object says).

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::lex::Lexer;
use crate::obj::Obj;
use crate::parse::ObjectParser;

/// Read the object at byte offset `pos` from the buffer.
///
/// Skips the `N G obj` header, parses the value, and stops at `endobj`.
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

    // Parse the value.
    let toks = lex_tokens_until(&mut lexer, g, &[crate::Token::EndObj])?;
    let mut parser = ObjectParser::new(&toks, budget);
    let obj = parser.parse(g)?;

    Ok(obj)
}

/// Lex tokens until one of the stop tokens is encountered (it is consumed).
fn lex_tokens_until(
    lexer: &mut Lexer<'_>,
    g: &mut BudgetGuard<'_>,
    stop: &[crate::Token],
) -> Result<Vec<crate::Token>> {
    let mut out = Vec::new();
    loop {
        let Some(tok) = lexer.next_token(g)? else {
            return Err(err!(
                Code::ObjUnexpected,
                during = "lex-tokens",
                detail = "reached end of input before stop token"
            ));
        };
        if stop.contains(&tok) {
            break;
        }
        out.push(tok);
    }
    Ok(out)
}
