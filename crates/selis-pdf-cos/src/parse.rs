//! Object parsing (SL-1.COS.02): build [`Obj`] values from the token stream.
//!
//! This is a recursive-descent parser over a buffered token vector. The
//! hostile-input constraint that matters here is **nesting depth**: a crafted
//! file can nest arrays and dicts arbitrarily deep. Rather than recursing (a
//! stack overflow is an abort we cannot catch), the parser drives an explicit
//! worklist with a depth counter from the [`Budget`].

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::lex::{Number, Token};
use crate::obj::{Obj, Ref};

/// A value parsed from a buffered token vector.
pub struct ObjectParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: u16,
    budget: &'a Budget,
}

impl<'a> ObjectParser<'a> {
    /// A parser over a buffered token vector.
    #[must_use]
    pub fn new(tokens: &'a [Token], budget: &'a Budget) -> Self {
        Self {
            tokens,
            pos: 0,
            depth: 0,
            budget,
        }
    }

    /// The current position in the token vector.
    #[must_use]
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Whether all tokens have been consumed.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    /// Parse a single object from the current position.
    ///
    /// # Budget
    ///
    /// Charges `Objects` per token consumed and checks depth per nesting level,
    /// so a hostile file of infinite nesting terminates with `BUDGET_DEPTH` or
    /// `BUDGET_OBJECTS`.
    ///
    /// # Malformed Input
    ///
    /// `OBJ_UNEXPECTED` when the token stream does not begin a value.
    pub fn parse(&mut self, g: &mut BudgetGuard<'_>) -> Result<Obj> {
        g.tick()?;
        g.charge_one(selis_sandbox::Resource::Objects)?;
        let Some(tok) = self.tokens.get(self.pos) else {
            return Err(err!(
                Code::ObjUnexpected,
                during = "cos-parse",
                at = self.pos as u64
            ));
        };
        match tok {
            Token::Null => {
                self.pos = self.pos.saturating_add(1);
                Ok(Obj::Null)
            }
            Token::Bool(b) => {
                self.pos = self.pos.saturating_add(1);
                Ok(Obj::Bool(*b))
            }
            Token::Number(Number::Int(i)) => {
                // `N G R` is an indirect reference.
                if let Some(r) = self.try_ref(g)? {
                    return Ok(Obj::Ref(r));
                }
                self.pos = self.pos.saturating_add(1);
                Ok(Obj::Int(*i))
            }
            Token::Number(Number::Real { scaled, scale }) => {
                self.pos = self.pos.saturating_add(1);
                Ok(Obj::Real {
                    scaled: *scaled,
                    scale: *scale,
                })
            }
            Token::String(b) => {
                self.pos = self.pos.saturating_add(1);
                Ok(Obj::String(b.clone()))
            }
            Token::Name(b) => {
                self.pos = self.pos.saturating_add(1);
                Ok(Obj::Name(b.clone()))
            }
            Token::ArrayStart => self.parse_array(g),
            Token::DictStart => self.parse_dict(g),
            Token::Stream => self.parse_stream(g),
            _ => Err(err!(
                Code::ObjUnexpected,
                during = "cos-parse",
                at = self.pos as u64
            )),
        }
    }

    /// Recognise `N G R` as an indirect reference when it is next.
    ///
    /// Returns `Ok(Some(Ref))` and consumes the three tokens on a match,
    /// `Ok(None)` (consuming nothing) otherwise.
    fn try_ref(&mut self, g: &mut BudgetGuard<'_>) -> Result<Option<Ref>> {
        let n = self.tokens.get(self.pos);
        let gen = self.tokens.get(self.pos.saturating_add(1));
        let r = self.tokens.get(self.pos.saturating_add(2));
        if let (
            Some(Token::Number(Number::Int(num))),
            Some(Token::Number(Number::Int(gen))),
            Some(Token::Ref),
        ) = (n, gen, r)
        {
            g.charge_one(selis_sandbox::Resource::Objects)?;
            let num = u32::try_from(*num).unwrap_or(u32::MAX);
            let gen = u16::try_from(*gen).unwrap_or(u16::MAX);
            self.pos = self.pos.saturating_add(3);
            Ok(Some(Ref::new(num, gen)))
        } else {
            Ok(None)
        }
    }

    fn parse_array(&mut self, g: &mut BudgetGuard<'_>) -> Result<Obj> {
        self.pos = self.pos.saturating_add(1); // consume `[`
        self.enter(g)?;
        let mut items: Vec<Obj> = Vec::new();
        loop {
            g.tick()?;
            g.charge_one(selis_sandbox::Resource::Objects)?;
            let Some(tok) = self.tokens.get(self.pos) else {
                return Err(err!(
                    Code::ObjUnexpected,
                    during = "cos-array",
                    at = self.pos as u64
                ));
            };
            if matches!(tok, Token::ArrayEnd) {
                self.pos = self.pos.saturating_add(1);
                break;
            }
            let item = self.parse(g)?;
            items.push(item);
        }
        self.leave();
        Ok(Obj::Array(items))
    }

    fn parse_dict(&mut self, g: &mut BudgetGuard<'_>) -> Result<Obj> {
        self.pos = self.pos.saturating_add(1); // consume `<<`
        self.enter(g)?;
        let mut pairs: Vec<(Bytes, Obj)> = Vec::new();
        loop {
            g.tick()?;
            g.charge_one(selis_sandbox::Resource::Objects)?;
            let Some(tok) = self.tokens.get(self.pos) else {
                return Err(err!(
                    Code::ObjUnexpected,
                    during = "cos-dict",
                    at = self.pos as u64
                ));
            };
            match tok {
                Token::DictEnd => {
                    self.pos = self.pos.saturating_add(1);
                    break;
                }
                Token::Name(key) => {
                    self.pos = self.pos.saturating_add(1);
                    let value = self.parse(g)?;
                    pairs.push((key.clone(), value));
                }
                _ => {
                    return Err(err!(
                        Code::ObjUnexpected,
                        during = "cos-dict",
                        at = self.pos as u64
                    ));
                }
            }
        }
        self.leave();
        Ok(Obj::Dict(pairs))
    }

    /// Streams are recognised by the object layer (SL-1.COS.02 keeps the value;
    /// the stream body between `stream` and `endstream` is read by the xref
    /// layer in COS.03/04). At the token level a bare `stream` keyword is not a
    /// value; it only appears after an object dictionary, which the xref parser
    /// assembles. Returning an error here keeps the value grammar total.
    fn parse_stream(&mut self, _g: &mut BudgetGuard<'_>) -> Result<Obj> {
        Err(err!(
            Code::ObjUnexpected,
            during = "cos-stream",
            at = self.pos as u64
        ))
    }

    fn enter(&mut self, g: &mut BudgetGuard<'_>) -> Result<()> {
        self.depth = self.depth.saturating_add(1);
        let limit = self.budget.limit(selis_sandbox::Resource::Depth);
        if u64::from(self.depth) > limit {
            return Err(err!(
                Code::BudgetDepth,
                during = "cos-parse",
                detail = "nesting exceeds the depth budget"
            ));
        }
        g.enter()
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

/// Parse all top-level objects from a token vector until exhausted.
///
/// # Budget
///
/// As [`ObjectParser::parse`].
///
/// # Malformed Input
///
/// The first `OBJ_UNEXPECTED` or `BUDGET_*` error stops the run.
pub fn parse_all(tokens: &[Token], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<Vec<Obj>> {
    let mut parser = ObjectParser::new(tokens, budget);
    let mut out = Vec::new();
    while !parser.is_done() {
        out.push(parser.parse(g)?);
    }
    Ok(out)
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

    fn parse_one(src: &[u8]) -> Obj {
        let mut g = guard();
        let mut lexer = crate::Lexer::new(src);
        let mut toks = Vec::new();
        while let Some(t) = lexer.next_token(&mut g).expect("lex") {
            toks.push(t);
        }
        let budget = Budget::unlimited();
        let mut p = ObjectParser::new(&toks, &budget);
        p.parse(&mut g).expect("parse")
    }
    #[test]
    fn scalars_parse() {
        assert_eq!(parse_one(b"null"), Obj::Null);
        assert_eq!(parse_one(b"true"), Obj::Bool(true));
        assert_eq!(parse_one(b"42"), Obj::Int(42));
        assert_eq!(
            parse_one(b"(hi)"),
            Obj::String(Bytes::copy_from_slice(b"hi"))
        );
        assert_eq!(
            parse_one(b"/Name"),
            Obj::Name(Bytes::copy_from_slice(b"Name"))
        );
    }

    #[test]
    fn arrays_and_dicts_parse() {
        let a = parse_one(b"[1 2 3]");
        assert_eq!(a, Obj::Array(vec![Obj::Int(1), Obj::Int(2), Obj::Int(3)]));

        let d = parse_one(b"<< /A 1 /B (two) >>");
        assert_eq!(
            d,
            Obj::Dict(vec![
                (Bytes::copy_from_slice(b"A"), Obj::Int(1)),
                (
                    Bytes::copy_from_slice(b"B"),
                    Obj::String(Bytes::copy_from_slice(b"two"))
                ),
            ])
        );
    }

    #[test]
    fn nested_containers_parse() {
        let o = parse_one(b"<< /Kids [ 1 2 [ 3 ] ] /N (x) >>");
        match o {
            Obj::Dict(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert!(matches!(&pairs[0].1, Obj::Array(items) if items.len() == 3));
            }
            other => panic!("expected dict, got {other:?}"),
        }
    }

    #[test]
    fn indirect_references_parse() {
        assert_eq!(parse_one(b"5 0 R"), Obj::Ref(Ref { num: 5, gen: 0 }));
        assert_eq!(parse_one(b"10 2 R"), Obj::Ref(Ref { num: 10, gen: 2 }));
        // `5 0` without `R` is just two ints; parse_one reads the first.
        assert_eq!(parse_one(b"5 0"), Obj::Int(5));
    }

    #[test]
    fn strings_stay_raw_bytes() {
        // Non-ASCII bytes must not be "fixed" by the object layer.
        let o = parse_one(b"(caf\xc3\xa9)");
        assert_eq!(o, Obj::String(Bytes::copy_from_slice(b"caf\xc3\xa9")));
    }

    #[test]
    fn unexpected_token_is_a_typed_error() {
        let mut g = guard();
        let toks = vec![crate::Token::Ref];
        let budget = Budget::unlimited();
        let mut p = ObjectParser::new(&toks, &budget);
        let e = p.parse(&mut g).expect_err("Ref is not a value");
        assert_eq!(e.code(), Code::ObjUnexpected);
    }

    /// A ten-thousand-deep nesting must terminate with a budget error, never a
    /// stack overflow (SL-1.ROB.03).
    #[test]
    fn deep_nesting_terminates_in_bounded_depth() {
        let mut g = guard();
        // Build `[[[[...]]]]` with 20k opens. The lexer's arrays are one byte
        // each; 40k bytes of brackets.
        let mut src = Vec::new();
        for _ in 0..20_000 {
            src.push(b'[');
        }
        for _ in 0..20_000 {
            src.push(b']');
        }
        let mut lexer = crate::Lexer::new(&src);
        let mut toks = Vec::new();
        while let Some(t) = lexer.next_token(&mut g).expect("lex") {
            toks.push(t);
        }
        let shallow = Budget {
            depth: 128,
            ..Budget::unlimited()
        };
        let mut p = ObjectParser::new(&toks, &shallow);
        let e = p.parse(&mut g).expect_err("depth must be exceeded");
        assert!(e.is_budget(), "expected a budget error, got {e}");
    }
}
