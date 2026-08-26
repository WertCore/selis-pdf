//! Content-stream tokeniser (SL-2.CONT.01).
//!
//! The content grammar differs from COS in three ways:
//! * **no indirect references** — `R` is just a name;
//! * **inline images** are embedded in the token stream (`BI ... ID ... EI`);
//! * operators are bare names at the end of an operand group.
//!
//! The tokeniser produces a flat token stream; the dispatcher groups operands
//! with the operator that follows them.

use selis_error::Result;
use selis_sandbox::BudgetGuard;

/// A content-stream token.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// A number operand.
    Num(f64),
    /// A name operand (slash-prefixed, e.g. `/DeviceRGB`).
    OperandName(selis_bytes::Bytes),
    /// An operator (a bare word, e.g. `cm`).
    Name(selis_bytes::Bytes),
    /// A string operand.
    Str(selis_bytes::Bytes),
    /// An array `[` `]`.
    ArrStart,
    /// End of an array.
    ArrEnd,
    /// A dictionary `<<` `>>` (used by inline-image dictionaries).
    DictStart,
    /// End of a dictionary.
    DictEnd,
    /// `BI` — begin inline image.
    BI,
    /// `ID` — inline image data follows.
    ID,
    /// `EI` — end inline image.
    EI,
}

/// The content lexer over a byte buffer.
#[derive(Debug)]
pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20)
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

impl<'a> Lexer<'a> {
    /// A lexer over a content stream.
    #[must_use]
    pub fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0 }
    }

    /// The current byte offset.
    #[must_use]
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Jump to a byte offset (used by the inline-image extractor to skip past
    /// the `ID` … `EI` block).
    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
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

    /// The next token, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// `LEX_UNTERMINATED_STRING` when a string runs to end of stream.
    pub fn next_token(&mut self, g: &mut BudgetGuard<'_>) -> Result<Option<Tok>> {
        loop {
            self.skip_ws_and_comments();
            let Some(b) = self.peek() else {
                return Ok(None);
            };
            g.tick()?;
            g.charge_one(selis_sandbox::Resource::Objects)?;
            match b {
                b'[' => {
                    self.bump();
                    return Ok(Some(Tok::ArrStart));
                }
                b']' => {
                    self.bump();
                    return Ok(Some(Tok::ArrEnd));
                }
                b'<' => {
                    if self.src.get(self.pos.saturating_add(1)) == Some(&b'<') {
                        self.bump();
                        self.bump();
                        return Ok(Some(Tok::DictStart));
                    }
                    return self.lex_hex_string();
                }
                b'>' => {
                    if self.src.get(self.pos.saturating_add(1)) == Some(&b'>') {
                        self.bump();
                        self.bump();
                        return Ok(Some(Tok::DictEnd));
                    }
                    self.bump();
                    continue;
                }
                b'(' => return self.lex_string(),
                b'/' => return self.lex_name(),
                _ if b.is_ascii_digit() || b == b'+' || b == b'-' || b == b'.' => {
                    return self.lex_number();
                }
                _ => return self.lex_word(),
            }
        }
    }

    fn lex_number(&mut self) -> Result<Option<Tok>> {
        let mut text = Vec::new();
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'+' || b == b'-' || b == b'.' || b == b'e' || b == b'E' {
                text.push(b);
                self.bump();
            } else if is_ws(b) || is_delimiter(b) {
                break;
            } else {
                // Not a number after all; treat as a word (e.g. an operator
                // starting with a digit is invalid, but tolerate).
                text.push(b);
                self.bump();
                return self.finish_word(text);
            }
        }
        let s = String::from_utf8_lossy(&text);
        let v = s.parse::<f64>().unwrap_or(0.0);
        Ok(Some(Tok::Num(v)))
    }

    fn lex_name(&mut self) -> Result<Option<Tok>> {
        self.bump(); // consume `/`
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            if is_ws(b) || is_delimiter(b) {
                break;
            }
            out.push(b);
            self.bump();
        }
        Ok(Some(Tok::OperandName(selis_bytes::Bytes::copy_from_slice(
            &out,
        ))))
    }

    fn lex_string(&mut self) -> Result<Option<Tok>> {
        self.bump(); // consume `(`
        let mut out = Vec::new();
        let mut depth = 0u32;
        loop {
            let Some(b) = self.peek() else {
                return Err(selis_error::err!(
                    selis_error::Code::LexUnterminatedString,
                    during = "content-lex",
                    at = self.pos as u64
                ));
            };
            self.bump();
            match b {
                b'(' => {
                    depth = depth.saturating_add(1);
                    out.push(b'(');
                }
                b')' => {
                    if depth == 0 {
                        break;
                    }
                    depth = depth.saturating_sub(1);
                    out.push(b')');
                }
                b'\\' => {
                    let Some(e) = self.peek() else {
                        break;
                    };
                    self.bump();
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'(' => out.push(b'('),
                        b')' => out.push(b')'),
                        b'\\' => out.push(b'\\'),
                        _ => out.push(e),
                    }
                }
                other => out.push(other),
            }
        }
        Ok(Some(Tok::Str(selis_bytes::Bytes::copy_from_slice(&out))))
    }

    fn lex_hex_string(&mut self) -> Result<Option<Tok>> {
        self.bump(); // consume `<`
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
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
                        if let Some(h) = hi.take() {
                            out.push((h << 4) | v);
                        } else {
                            hi = Some(v);
                        }
                    }
                    None => {}
                },
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        Ok(Some(Tok::Str(selis_bytes::Bytes::copy_from_slice(&out))))
    }

    fn lex_word(&mut self) -> Result<Option<Tok>> {
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            if is_ws(b) || is_delimiter(b) {
                break;
            }
            out.push(b);
            self.bump();
        }
        self.finish_word(out)
    }

    fn finish_word(&mut self, word: Vec<u8>) -> Result<Option<Tok>> {
        let b = &word[..];
        if b == b"BI" {
            Ok(Some(Tok::BI))
        } else if b == b"ID" {
            Ok(Some(Tok::ID))
        } else if b == b"EI" {
            Ok(Some(Tok::EI))
        } else {
            Ok(Some(Tok::Name(selis_bytes::Bytes::copy_from_slice(b))))
        }
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(b.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(b.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// Tokenise an entire content stream.
///
/// # Budget
///
/// Charged per token.
///
/// # Malformed Input
///
/// The first lexer error, or `BUDGET_*` on exhaustion.
pub fn tokenise(src: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<Tok>> {
    let mut lexer = Lexer::new(src);
    let mut out = Vec::new();
    while let Some(t) = lexer.next_token(g)? {
        out.push(t);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    #[test]
    fn tokens_operators_and_numbers() {
        let mut g = guard();
        let toks = tokenise(b"1 0 0 1 0 0 cm 10 w", &mut g).expect("lex");
        assert_eq!(toks.len(), 9);
        assert_eq!(toks[0], Tok::Num(1.0));
        assert_eq!(
            toks[8],
            Tok::Name(selis_bytes::Bytes::copy_from_slice(b"w"))
        );
    }

    #[test]
    fn inline_image_keywords() {
        let mut g = guard();
        let toks = tokenise(b"BI /W 2 /H 2 ID data EI", &mut g).expect("lex");
        assert!(toks.contains(&Tok::BI));
        assert!(toks.contains(&Tok::ID));
        assert!(toks.contains(&Tok::EI));
    }

    #[test]
    fn names_and_arrays() {
        let mut g = guard();
        let toks = tokenise(b"[1 2 3] /Name (str)", &mut g).expect("lex");
        assert_eq!(toks[0], Tok::ArrStart);
        assert_eq!(toks[4], Tok::ArrEnd);
        assert_eq!(
            toks[5],
            Tok::OperandName(selis_bytes::Bytes::copy_from_slice(b"Name"))
        );
        assert_eq!(
            toks[6],
            Tok::Str(selis_bytes::Bytes::copy_from_slice(b"str"))
        );
    }
}
