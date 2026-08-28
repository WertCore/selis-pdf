//! The COS lexer (SL-1.COS.01).
//!
//! Tokenises the COS syntax against a byte buffer, budget-charged per token.
//! The guiding principle (SL-1.COS.01 Risk) is:
//!
//! > **Permissive in what you accept, explicit about what you accepted.**
//!
//! Every deviation from strict syntax is tolerated *and* recorded in a
//! [`crate::deviation::Deviation`] list, so `selis inspect --json` can tell the
//! user "this file is malformed in these ways" and the corpus can assert on it.
//!
//! # The known malformations
//!
//! Real-world PDFs routinely contain:
//! * numbers like `--5` and `6.` (double signs, trailing dots),
//! * names with malformed `#xx` escapes,
//! * literal strings with unbalanced parens,
//! * hex strings of odd length,
//! * stray `>` and reserved `{`/`}` delimiters.
//!
//! Each is accepted with a deviation rather than rejected, because rejecting
//! them rejects real files that Acrobat opens.

use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

use crate::deviation::Deviation;

/// A COS number. PDF forbids exponent notation; a real is decimal digits with
/// an optional fraction. Integers are kept as `i64`, reals as a scaled
/// integer to stay deterministic and exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Number {
    /// An integer.
    Int(i64),
    /// A decimal fraction, exact: `scaled` with `scale` fractional digits.
    Real {
        /// The digits, as a signed integer.
        scaled: i64,
        /// Number of fractional digits.
        scale: u8,
    },
}

/// A COS lexical token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// A number.
    Number(Number),
    /// A literal or hex string, raw bytes (text decoding is a later step).
    String(selis_bytes::Bytes),
    /// A name, `#xx` escapes decoded.
    Name(selis_bytes::Bytes),
    /// `[`
    ArrayStart,
    /// `]`
    ArrayEnd,
    /// `<<`
    DictStart,
    /// `>>`
    DictEnd,
    /// The bare word `stream`.
    Stream,
    /// The bare word `endstream`.
    EndStream,
    /// The bare word `obj`.
    Obj,
    /// The bare word `endobj`.
    EndObj,
    /// The bare word `R` (an indirect reference marker).
    Ref,
}

/// PDF whitespace: NUL, tab, LF, FF, CR, space (ISO 32000-2 §7.2.2).
fn is_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20)
}

/// PDF delimiters (ISO 32000-2 §7.2.2).
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(b.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(b.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// The COS lexer: a cursor over a byte buffer.
///
/// Not resumable in this form (resumability over partial sources is
/// SL-1.COS.07); this is the buffer-complete lexer that the resumable wrapper
/// drives later.
#[derive(Debug)]
pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    deviations: Vec<Deviation>,
}

impl<'a> Lexer<'a> {
    /// A lexer over the whole buffer.
    #[must_use]
    pub fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            deviations: Vec::new(),
        }
    }

    /// The deviations recorded so far.
    #[must_use]
    pub fn deviations(&self) -> &[Deviation] {
        &self.deviations
    }

    /// The current byte offset.
    #[must_use]
    pub fn pos(&self) -> u64 {
        self.pos as u64
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.src.get(self.pos.saturating_add(n)).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.src.get(self.pos).copied();
        if b.is_some() {
            self.pos = self.pos.saturating_add(1);
        }
        b
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b) if is_ws(b) => {
                    self.bump();
                }
                Some(b'%') => {
                    // Comment runs to EOL (LF or CR).
                    while let Some(c) = self.peek() {
                        if c == b'\n' || c == b'\r' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    /// The next token, or `None` at end of input.
    ///
    /// # Errors
    ///
    /// * `LEX_NUMBER_OVERFLOW` when a number exceeds `i64`.
    /// * `LEX_UNTERMINATED_STRING` when a string runs to end of input.
    /// * `BUDGET_*` when a budget dimension is exhausted (charged per token).
    pub fn next_token(&mut self, g: &mut BudgetGuard<'_>) -> Result<Option<Token>> {
        loop {
            self.skip_ws_and_comments();
            let Some(b) = self.peek() else {
                return Ok(None);
            };
            // Budget: every token costs objects, and the wall-clock is checked
            // so a hostile file of infinite tokens still terminates.
            g.tick()?;
            g.charge_one(selis_sandbox::Resource::Objects)?;

            match b {
                b'[' => {
                    self.bump();
                    return Ok(Some(Token::ArrayStart));
                }
                b']' => {
                    self.bump();
                    return Ok(Some(Token::ArrayEnd));
                }
                b')' => {
                    // An unbalanced closing paren with no opener.
                    let off = self.pos as u64;
                    self.bump();
                    self.deviations
                        .push(Deviation::UnexpectedClosingParen { offset: off });
                    continue; // tolerated, recorded, skipped
                }
                b'{' | b'}' => {
                    let off = self.pos as u64;
                    self.bump();
                    self.deviations
                        .push(Deviation::ReservedDelimiter { offset: off });
                    continue; // tolerated, recorded, skipped
                }
                b'/' => return self.lex_name(g),
                b'(' => return self.lex_literal_string(g),
                b'<' => {
                    // `<<` is a dict start; a lone `<` opens a hex string.
                    if self.peek_at(1) == Some(b'<') {
                        self.bump();
                        self.bump();
                        return Ok(Some(Token::DictStart));
                    }
                    return self.lex_hex_string(g);
                }
                b'>' => {
                    if self.peek_at(1) == Some(b'>') {
                        self.bump();
                        self.bump();
                        return Ok(Some(Token::DictEnd));
                    }
                    let off = self.pos as u64;
                    self.bump();
                    self.deviations
                        .push(Deviation::UnexpectedGt { offset: off });
                    continue;
                }
                _ if is_digit(b) || b == b'+' || b == b'-' || b == b'.' => {
                    return self.lex_number(g);
                }
                _ => return self.lex_word_or_keyword(g),
            }
        }
    }

    /// Push one byte into a token buffer, charging the byte budget so a
    /// 100 MB name/string from a hostile file exhausts `BUDGET_BYTES` before
    /// the buffer is built, not after (ADR-P0006).
    fn push_charged(&mut self, g: &mut BudgetGuard<'_>, out: &mut Vec<u8>, b: u8) -> Result<()> {
        g.charge(selis_sandbox::Resource::Bytes, 1)?;
        out.push(b);
        Ok(())
    }

    fn lex_number(&mut self, _g: &mut BudgetGuard<'_>) -> Result<Option<Token>> {
        let start = self.pos;
        let mut sign = 1i64;
        let mut saw_sign = false;
        let mut saw_dot = false;
        let mut digits: i64 = 0;
        let mut scale: u8 = 0;
        let mut overflow = false;

        // Signs. A second sign is the `--5` malformation.
        while let Some(b) = self.peek() {
            if b == b'+' || b == b'-' {
                if saw_sign {
                    self.deviations.push(Deviation::DoubleSign {
                        offset: self.pos as u64,
                    });
                    if b == b'-' {
                        sign = sign.wrapping_neg();
                    }
                } else {
                    saw_sign = true;
                    if b == b'-' {
                        sign = -1;
                    }
                }
                self.bump();
            } else {
                break;
            }
        }

        // Integer part.
        while let Some(b) = self.peek() {
            if !is_digit(b) {
                break;
            }
            self.bump();
            let d = i64::from(b.wrapping_sub(b'0'));
            match digits.checked_mul(10).and_then(|x| x.checked_add(d)) {
                Some(x) => digits = x,
                None => {
                    overflow = true;
                }
            }
        }

        // Fraction.
        if self.peek() == Some(b'.') {
            self.bump();
            saw_dot = true;
            let mut frac_digits = 0u8;
            while let Some(b) = self.peek() {
                if !is_digit(b) {
                    break;
                }
                self.bump();
                let d = i64::from(b.wrapping_sub(b'0'));
                match digits.checked_mul(10).and_then(|x| x.checked_add(d)) {
                    Some(x) => digits = x,
                    None => {
                        overflow = true;
                    }
                }
                frac_digits = frac_digits.saturating_add(1);
            }
            if frac_digits == 0 {
                // `6.` with no fraction digits is a deviation.
                self.deviations.push(Deviation::TrailingDot {
                    offset: self.pos.saturating_sub(1) as u64,
                });
            }
            scale = frac_digits;
        }

        if overflow {
            digits = i64::MAX;
            self.deviations.push(Deviation::NumberOverflow {
                offset: start as u64,
            });
        }

        let value = if sign < 0 {
            digits.wrapping_neg()
        } else {
            digits
        };
        if saw_dot && scale > 0 {
            Ok(Some(Token::Number(Number::Real {
                scaled: value,
                scale,
            })))
        } else {
            Ok(Some(Token::Number(Number::Int(value))))
        }
    }

    fn lex_name(&mut self, g: &mut BudgetGuard<'_>) -> Result<Option<Token>> {
        let start = self.pos as u64;
        self.bump(); // consume `/`
        let mut out = Vec::new();
        loop {
            let Some(b) = self.peek() else {
                break;
            };
            if is_ws(b) || is_delimiter(b) {
                break;
            }
            self.bump();
            if b == b'#' {
                // `#xx` hex escape.
                match (self.peek(), self.peek_at(1)) {
                    (Some(a), Some(bb)) => {
                        if let (Some(x), Some(y)) = (hex_val(a), hex_val(bb)) {
                            let _ = self.bump();
                            let _ = self.bump();
                            self.push_charged(g, &mut out, (x << 4) | y)?;
                        } else {
                            self.deviations.push(Deviation::BadNameEscape {
                                offset: self.pos.saturating_sub(1) as u64,
                            });
                            self.push_charged(g, &mut out, b'#')?;
                        }
                    }
                    _ => {
                        self.deviations.push(Deviation::BadNameEscape {
                            offset: self.pos.saturating_sub(1) as u64,
                        });
                        self.push_charged(g, &mut out, b'#')?;
                    }
                }
            } else {
                self.push_charged(g, &mut out, b)?;
            }
        }
        if out.is_empty() {
            self.deviations.push(Deviation::EmptyName { offset: start });
        }
        Ok(Some(Token::Name(selis_bytes::Bytes::copy_from_slice(&out))))
    }

    fn lex_literal_string(&mut self, g: &mut BudgetGuard<'_>) -> Result<Option<Token>> {
        let start = self.pos as u64;
        self.bump(); // consume `(`
        let mut out = Vec::new();
        let mut depth = 0u32;
        loop {
            let Some(b) = self.peek() else {
                self.deviations
                    .push(Deviation::UnterminatedString { offset: start });
                return Err(err!(
                    Code::LexUnterminatedString,
                    during = "lex-string",
                    at = start
                ));
            };
            self.bump();
            match b {
                b'(' => {
                    depth = depth.saturating_add(1);
                    self.push_charged(g, &mut out, b'(')?;
                }
                b')' => {
                    if depth == 0 {
                        break; // the string is closed
                    }
                    depth = depth.saturating_sub(1);
                    self.push_charged(g, &mut out, b')')?;
                }
                b'\\' => {
                    let Some(e) = self.peek() else {
                        self.deviations
                            .push(Deviation::UnterminatedString { offset: start });
                        return Err(err!(
                            Code::LexUnterminatedString,
                            during = "lex-string",
                            at = start
                        ));
                    };
                    self.bump();
                    match e {
                        b'n' => self.push_charged(g, &mut out, b'\n')?,
                        b'r' => self.push_charged(g, &mut out, b'\r')?,
                        b't' => self.push_charged(g, &mut out, b'\t')?,
                        b'b' => self.push_charged(g, &mut out, 0x08)?,
                        b'f' => self.push_charged(g, &mut out, 0x0c)?,
                        b'(' => self.push_charged(g, &mut out, b'(')?,
                        b')' => self.push_charged(g, &mut out, b')')?,
                        b'\\' => self.push_charged(g, &mut out, b'\\')?,
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.bump();
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = i32::from(e.wrapping_sub(b'0'));
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(o @ b'0'..=b'7') => {
                                        self.bump();
                                        v = v
                                            .wrapping_mul(8)
                                            .wrapping_add(i32::from(o.wrapping_sub(b'0')));
                                    }
                                    _ => break,
                                }
                            }
                            self.push_charged(g, &mut out, u8::try_from(v & 0xff).unwrap_or(0))?;
                        }
                        other => self.push_charged(g, &mut out, other)?,
                    }
                }
                other => self.push_charged(g, &mut out, other)?,
            }
        }
        Ok(Some(Token::String(selis_bytes::Bytes::copy_from_slice(
            &out,
        ))))
    }

    fn lex_hex_string(&mut self, g: &mut BudgetGuard<'_>) -> Result<Option<Token>> {
        let start = self.pos as u64;
        self.bump(); // consume `<`
        let mut out = Vec::new();
        let mut nibble_hi: Option<u8> = None;
        loop {
            let Some(b) = self.peek() else {
                break;
            };
            self.bump();
            match b {
                b'>' => break,
                b if is_ws(b) => continue,
                b => match hex_val(b) {
                    Some(v) => {
                        if let Some(hi) = nibble_hi.take() {
                            self.push_charged(g, &mut out, (hi << 4) | v)?;
                        } else {
                            nibble_hi = Some(v);
                        }
                    }
                    None => {
                        self.deviations.push(Deviation::InvalidHexDigit {
                            offset: self.pos.saturating_sub(1) as u64,
                        });
                    }
                },
            }
        }
        if let Some(hi) = nibble_hi {
            // Odd number of digits: pad the final nibble with 0.
            self.deviations
                .push(Deviation::OddLengthHex { offset: start });
            self.push_charged(g, &mut out, hi << 4)?;
        }
        Ok(Some(Token::String(selis_bytes::Bytes::copy_from_slice(
            &out,
        ))))
    }

    fn lex_word_or_keyword(&mut self, _g: &mut BudgetGuard<'_>) -> Result<Option<Token>> {
        let start = self.pos as u64;
        let mut word = Vec::new();
        while let Some(b) = self.peek() {
            if is_ws(b) || is_delimiter(b) {
                break;
            }
            word.push(b);
            self.bump();
        }
        // A delimiter that reached this branch (every one is handled in
        // `next_token`, so this is defensive) must still consume a byte, or
        // the lexer loops forever.
        if word.is_empty() {
            self.bump();
            self.deviations
                .push(Deviation::UnknownWord { offset: start });
            return Ok(Some(Token::Name(selis_bytes::Bytes::new())));
        }
        match &word[..] {
            b"true" => Ok(Some(Token::Bool(true))),
            b"false" => Ok(Some(Token::Bool(false))),
            b"null" => Ok(Some(Token::Null)),
            b"stream" => Ok(Some(Token::Stream)),
            b"endstream" => Ok(Some(Token::EndStream)),
            b"obj" => Ok(Some(Token::Obj)),
            b"endobj" => Ok(Some(Token::EndObj)),
            b"R" => Ok(Some(Token::Ref)),
            _ => {
                // An unknown bare word is tolerated and recorded. The parser
                // decides whether to ignore it.
                self.deviations
                    .push(Deviation::UnknownWord { offset: start });
                Ok(Some(Token::Name(selis_bytes::Bytes::copy_from_slice(
                    &word,
                ))))
            }
        }
    }
}

/// Tokenise an entire buffer into a vector, for tests and non-resumable
/// callers.
///
/// # Budget
///
/// Every token is charged against `g`; the wall-clock is checked so a hostile
/// file of infinite tokens terminates.
///
/// # Malformed Input
///
/// Tolerated malformations are recorded as deviations on the [`Lexer`]; hard
/// failures (unterminated strings, numeric overflow) are typed errors.
///
/// # Errors
///
/// The first lexer error, or `BUDGET_*` on exhaustion.
pub fn tokenise(src: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<Token>> {
    let mut lexer = Lexer::new(src);
    let mut out = Vec::new();
    while let Some(tok) = lexer.next_token(g)? {
        out.push(tok);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    // Test vectors are fixed and known-length; indexing them is deliberate.
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss
    )]

    use super::*;
    use selis_bytes::Bytes;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn lex(src: &[u8]) -> (Vec<Token>, Vec<Deviation>) {
        let mut g = guard();
        let mut l = Lexer::new(src);
        let mut toks = Vec::new();
        // A typed error (unterminated string, overflow) is an allowed outcome:
        // the property under test is "no panic", not "always succeeds".
        while let Some(t) = l.next_token(&mut g).unwrap_or(None) {
            toks.push(t);
        }
        let devs = l.deviations().to_vec();
        (toks, devs)
    }

    #[test]
    fn basic_tokens() {
        let src = b"/Type /Catalog << /Pages 5 0 R >>";
        let (toks, devs) = lex(src);
        assert!(devs.is_empty());
        assert_eq!(
            toks,
            vec![
                Token::Name(Bytes::copy_from_slice(b"Type")),
                Token::Name(Bytes::copy_from_slice(b"Catalog")),
                Token::DictStart,
                Token::Name(Bytes::copy_from_slice(b"Pages")),
                Token::Number(Number::Int(5)),
                Token::Number(Number::Int(0)),
                Token::Ref,
                Token::DictEnd,
            ]
        );
    }

    #[test]
    fn numbers_including_malformations() {
        let (toks, devs) = lex(b"1 2.5 -3 --5 6. .5");
        assert_eq!(toks[0], Token::Number(Number::Int(1)));
        assert_eq!(
            toks[1],
            Token::Number(Number::Real {
                scaled: 25,
                scale: 1
            })
        );
        assert_eq!(toks[2], Token::Number(Number::Int(-3)));
        // `--5` tolerates to 5 and records a DoubleSign deviation.
        assert_eq!(toks[3], Token::Number(Number::Int(5)));
        // `6.` tolerates to 6 and records a TrailingDot deviation.
        assert_eq!(toks[4], Token::Number(Number::Int(6)));
        // `.5` is a valid real (spec grammar allows leading dot).
        assert_eq!(
            toks[5],
            Token::Number(Number::Real {
                scaled: 5,
                scale: 1
            })
        );
        assert!(
            devs.iter()
                .any(|d| matches!(d, Deviation::DoubleSign { .. })),
            "double sign must be recorded"
        );
        assert!(
            devs.iter()
                .any(|d| matches!(d, Deviation::TrailingDot { .. })),
            "trailing dot must be recorded"
        );
    }

    #[test]
    fn names_decode_hash_escapes() {
        let (toks, devs) = lex(b"/A#20B /C#GG");
        assert_eq!(toks[0], Token::Name(Bytes::copy_from_slice(b"A B")));
        assert_eq!(toks[1], Token::Name(Bytes::copy_from_slice(b"C#GG")));
        assert!(devs
            .iter()
            .any(|d| matches!(d, Deviation::BadNameEscape { .. })));
    }

    #[test]
    fn literal_strings_handle_escapes_and_nesting() {
        let (toks, _) = lex(b"(hello\\(world\\)\\n\\051)");
        // `\n` = LF, `\051` octal = 0x29 = `)`.
        assert_eq!(
            toks[0],
            Token::String(Bytes::copy_from_slice(b"hello(world)\n)"))
        );
    }

    #[test]
    fn hex_strings_and_odd_length() {
        let (toks, devs) = lex(b"<48656c6c6f> <abc>");
        assert_eq!(toks[0], Token::String(Bytes::copy_from_slice(b"Hello")));
        // `abc` is odd: padded to `ab c0`.
        assert_eq!(
            toks[1],
            Token::String(Bytes::copy_from_slice(&[0xab, 0xc0]))
        );
        assert!(devs
            .iter()
            .any(|d| matches!(d, Deviation::OddLengthHex { .. })));
    }

    #[test]
    fn comments_and_whitespace_are_skipped() {
        let (toks, _) = lex(b"% a comment\n 1 % trailing\n2");
        assert_eq!(
            toks,
            vec![Token::Number(Number::Int(1)), Token::Number(Number::Int(2)),]
        );
    }

    #[test]
    fn unterminated_string_is_a_typed_error() {
        let mut g = guard();
        let mut l = Lexer::new(b"(abc");
        let err = l.next_token(&mut g).expect_err("unterminated");
        assert_eq!(err.code(), Code::LexUnterminatedString);
        assert!(l
            .deviations()
            .iter()
            .any(|d| matches!(d, Deviation::UnterminatedString { .. })));
    }

    #[test]
    fn number_overflow_clamps_and_is_recorded() {
        let mut g = guard();
        let mut l = Lexer::new(b"99999999999999999999999999");
        let tok = l.next_token(&mut g).expect("clamped, not failed");
        assert!(matches!(tok, Some(Token::Number(_))));
        assert!(l
            .deviations()
            .iter()
            .any(|d| matches!(d, Deviation::NumberOverflow { .. })));
    }

    #[test]
    fn unknown_words_are_tolerated_and_recorded() {
        let (toks, devs) = lex(b"hello");
        assert_eq!(toks[0], Token::Name(Bytes::copy_from_slice(b"hello")));
        assert!(devs
            .iter()
            .any(|d| matches!(d, Deviation::UnknownWord { .. })));
    }

    /// SL-1.COS.01 DoD: table-driven test of the malformations from
    /// `docs/specs/MALFORMATIONS.md`.
    #[test]
    fn table_driven_malformations_match_the_catalogue() {
        // (input, expected deviation kind, expected token)
        let cases: &[(&[u8], fn(&Deviation) -> bool, Token)] = &[
            (
                b"--5",
                |d| matches!(d, Deviation::DoubleSign { .. }),
                Token::Number(Number::Int(5)),
            ),
            (
                b"6.",
                |d| matches!(d, Deviation::TrailingDot { .. }),
                Token::Number(Number::Int(6)),
            ),
            (
                b"/Name#Zz",
                |d| matches!(d, Deviation::BadNameEscape { .. }),
                Token::Name(Bytes::copy_from_slice(b"Name#Zz")),
            ),
            (
                b"<abc>",
                |d| matches!(d, Deviation::OddLengthHex { .. }),
                Token::String(Bytes::copy_from_slice(&[0xab, 0xc0])),
            ),
            (
                b"<ab!cd>",
                |d| matches!(d, Deviation::InvalidHexDigit { .. }),
                Token::String(Bytes::copy_from_slice(&[0xab, 0xcd])),
            ),
            (
                b">",
                |d| matches!(d, Deviation::UnexpectedGt { .. }),
                Token::Number(Number::Int(0)),
            ),
            (
                b"{1}",
                |d| matches!(d, Deviation::ReservedDelimiter { .. }),
                Token::Number(Number::Int(1)),
            ),
            (
                b"foo",
                |d| matches!(d, Deviation::UnknownWord { .. }),
                Token::Name(Bytes::copy_from_slice(b"foo")),
            ),
        ];
        for (src, pred, expected) in cases {
            let (toks, devs) = lex(src);
            // For the `>` case there is no token; use a sentinel to assert the
            // deviation and, when present, the token.
            assert!(
                devs.iter().any(|d| pred(d)),
                "expected a deviation for input {:?}",
                String::from_utf8_lossy(src)
            );
            if !matches!(expected, Token::Number(Number::Int(0))) {
                assert_eq!(
                    &toks[0],
                    expected,
                    "token for {:?}",
                    String::from_utf8_lossy(src)
                );
            }
        }
    }

    /// SL-1.COS.01 DoD: any byte sequence either tokenises or errors, never
    /// panics.
    #[cfg(test)]
    mod proptests {
        use super::*;

        proptest::proptest! {
            #[test]
            fn any_bytes_never_panic(
                data in proptest::collection::vec(proptest::arbitrary::any::<u8>(), 0..128)
            ) {
                let mut g = guard();
                let mut l = Lexer::new(&data);
                loop {
                    match l.next_token(&mut g) {
                        Ok(Some(_)) => continue,
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        }
    }
}
