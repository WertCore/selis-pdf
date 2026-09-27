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

/// Maximum bytes in a decoded PDF string (the ISO 32000-1 §7.3.4.2
/// implementation limit; PDF 2.0 raises it to 65 535).
///
/// Enforced in the lexer rather than in the text assembler, because the
/// explosion SL-3.TEXT.16 tracks starts here: a string past the limit must
/// never become an operand that a show-text can turn into characters.
pub const MAX_STRING_BYTES: usize = 32_767;

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

    /// The next token, or `None` at end of stream. Every call advances the
    /// scan position, so the interpreter always makes progress.
    ///
    /// # Errors
    ///
    /// A `BUDGET_*` code on resource exhaustion.
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
                    // An over-limit hex string is consumed and emits nothing;
                    // keep lexing rather than reporting end-of-stream.
                    if let Some(tok) = self.lex_hex_string()? {
                        return Ok(Some(tok));
                    }
                    continue;
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
                // Delimiter bytes the content grammar does not use (`{`, `}`,
                // and a stray `)`). They are delimiters, so `lex_word` would
                // return an empty token without advancing — stalling the
                // interpreter in an unbounded loop. Consume and continue so a
                // hostile stream always makes progress (SL-2.CONT.01).
                b')' | b'{' | b'}' => {
                    self.bump();
                    continue;
                }
                b'(' => {
                    // An over-limit string is consumed and emits nothing; keep
                    // lexing rather than reporting end-of-stream, which would
                    // silently truncate the rest of the page.
                    if let Some(tok) = self.lex_string()? {
                        return Ok(Some(tok));
                    }
                    continue;
                }
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
                break; // truncated at EOF: emit the partial string
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
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'(' => out.push(b'('),
                        b')' => out.push(b')'),
                        b'\\' => out.push(b'\\'),
                        // Line continuation: the backslash + EOL vanish
                        // (§7.3.4.2, the COS lexer's semantics).
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.bump();
                            }
                        }
                        b'\n' => {}
                        // Octal escapes `\ddd` — one to three octal digits,
                        // high-order overflow ignored (§7.3.4.2).
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
                            out.push(u8::try_from(v & 0xff).unwrap_or(0));
                        }
                        _ => out.push(e),
                    }
                }
                other => out.push(other),
            }
            if out.len() > MAX_STRING_BYTES {
                self.skip_over_limit_string();
                return Ok(None);
            }
        }
        Ok(Some(Tok::Str(selis_bytes::Bytes::copy_from_slice(&out))))
    }

    /// An out-of-envelope string: consume the rest of the literal and emit
    /// **no** string token.
    ///
    /// A truncated string would be worse than none. ISO 32000-1 §7.3.4.2 caps a
    /// string at [`MAX_STRING_BYTES`]; past that the object is a defect, and the
    /// corpus files that probe it (`6.1.12-t03-fail-c` carries 65 538 bytes,
    /// `6.1.13-t03-fail-a` 32 770) are named `fail` for exactly that reason.
    /// Handing a 32 767-byte prefix downstream still fabricates tens of
    /// thousands of characters the page never shows, which is the char-count
    /// explosion SL-3.TEXT.16 exists to stop. Dropping the operand leaves the
    /// token stream well-formed — the enclosing show-text simply has no string
    /// to draw — and costs the reader nothing but the defect.
    fn skip_over_limit_string(&mut self) {
        let mut depth = 0u32;
        while let Some(b) = self.peek() {
            self.bump();
            match b {
                b'\\' => {
                    // Skip the escaped byte too, so an escaped `)` cannot end
                    // the scan early.
                    if self.peek().is_some() {
                        self.bump();
                    }
                }
                b'(' => depth = depth.saturating_add(1),
                b')' => {
                    if depth == 0 {
                        break;
                    }
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
        }
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
        // Same envelope as a literal string: a hex string over the limit is a
        // defect, and emitting a truncated prefix would fabricate characters
        // the page never shows (SL-3.TEXT.16).
        if out.len() > MAX_STRING_BYTES {
            return Ok(None);
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

    /// Hostile delimiters `)`, `{`, `}` must not stall the lexer: the
    /// interpreter advances past them in bounded time (SL-2.CONT.01).
    #[test]
    fn stray_delimiter_bytes_do_not_stall() {
        let mut g = guard();
        let toks = tokenise(b"} ) { abc", &mut g).expect("stray delimiters advance");
        // The delimiters are consumed; the token after them is `abc`.
        let last = toks.last().expect("at least one token");
        assert_eq!(
            *last,
            Tok::Name(selis_bytes::Bytes::copy_from_slice(b"abc"))
        );
    }

    /// An unterminated string at EOF emits a partial token and does not
    /// return an error, so a truncated content stream never aborts the page.
    #[test]
    fn unterminated_string_at_eof_is_tolerated() {
        let mut g = guard();
        let toks = tokenise(b"(hello world", &mut g).expect("truncated string");
        if let Some(Tok::Str(s)) = toks.first() {
            assert_eq!(s.as_slice(), b"hello world");
        } else {
            panic!("expected a truncated string token");
        }
    }

    // ── SL-3.TEXT.16: the /Length-impl-limit string explosion ─────────────
    //
    // The three veraPDF "Implementation limits" files carry a show-text string
    // of 65 538 / 32 770 bytes. Before the envelope was enforced, each H was
    // emitted as a character, so extraction reported 65 538 characters where
    // MuPDF reads 63. The counts below are the regression pin.

    /// The largest string that is still a legal operand, byte for byte.
    #[test]
    fn a_string_at_the_limit_is_kept_whole() {
        let mut body = vec![b'H'; MAX_STRING_BYTES];
        let mut src = vec![b'('];
        src.append(&mut body);
        src.extend_from_slice(b") Tj ET");
        let mut g = guard();
        let toks = tokenise(&src, &mut g).expect("limit-sized string");
        let strs: Vec<_> = toks.iter().filter(|t| matches!(t, Tok::Str(_))).collect();
        assert_eq!(strs.len(), 1, "the limit-sized string must survive");
        match strs[0] {
            Tok::Str(s) => assert_eq!(s.as_slice().len(), MAX_STRING_BYTES),
            _ => unreachable!("filtered to Tok::Str"),
        }
    }

    /// One byte over the limit: the operand is dropped, not truncated.
    ///
    /// This is the pin for the char-count explosion. A truncated 32 767-byte
    /// prefix would still have been 32 767 characters, so the assertion is on
    /// the *absence* of a string token, not on a length.
    #[test]
    fn a_string_past_the_limit_yields_no_operand() {
        let before = MAX_STRING_BYTES + 1;
        let mut body = vec![b'H'; before];
        let mut src = vec![b'('];
        src.append(&mut body);
        src.extend_from_slice(b") Tj ET");
        let mut g = guard();
        let toks = tokenise(&src, &mut g).expect("over-limit string");
        let emitted: usize = toks
            .iter()
            .filter_map(|t| match t {
                Tok::Str(s) => Some(s.as_slice().len()),
                _ => None,
            })
            .sum();
        assert_eq!(emitted, 0, "no string token may escape the limit");
    }

    /// The over-limit literal must be consumed, not merely abandoned: the
    /// operator and any following operands still have to lex, or the rest of
    /// the page is lost.
    #[test]
    fn lexing_continues_after_an_over_limit_string() {
        let mut body = vec![b'H'; MAX_STRING_BYTES + 10];
        let mut src = vec![b'('];
        src.append(&mut body);
        src.extend_from_slice(b") Tj (next) Tj ET");
        let mut g = guard();
        let toks = tokenise(&src, &mut g).expect("over-limit then a good string");
        let strs: Vec<_> = toks
            .iter()
            .filter_map(|t| match t {
                Tok::Str(s) => Some(s.as_slice().to_vec()),
                _ => None,
            })
            .collect();
        assert_eq!(
            strs,
            vec![b"next".to_vec()],
            "the following string still lexes"
        );
        assert!(
            toks.iter()
                .any(|t| t == &Tok::Name(selis_bytes::Bytes::copy_from_slice(b"Tj"))),
            "the show-text operators survive"
        );
    }

    /// The hex-string path has the same envelope as the literal path.
    #[test]
    fn a_hex_string_past_the_limit_yields_no_operand() {
        // Two hex digits decode to one byte, so the encoded form needs
        // 2 x (limit + 1) digits to exceed it.
        let mut src = vec![b'<'];
        src.extend(std::iter::repeat_n(b'4', (MAX_STRING_BYTES + 1) * 2).collect::<Vec<u8>>());
        src.extend_from_slice(b"> Tj ET");
        let mut g = guard();
        let toks = tokenise(&src, &mut g).expect("over-limit hex string");
        let emitted: usize = toks
            .iter()
            .filter_map(|t| match t {
                Tok::Str(s) => Some(s.as_slice().len()),
                _ => None,
            })
            .sum();
        assert_eq!(emitted, 0, "a hex string over the limit is a defect too");
    }

    /// Escape decoding still runs while under the limit — the limit must not
    /// have broken the normal string semantics it sits next to.
    #[test]
    fn escapes_still_decode_under_the_limit() {
        let mut g = guard();
        let toks = tokenise(br"(a\(b\)c\101\n) Tj ET", &mut g).expect("escapes");
        match toks.first() {
            Some(Tok::Str(s)) => assert_eq!(s.as_slice(), b"a(b)cA\n"),
            other => panic!("expected a decoded string, got {other:?}"),
        }
    }
}
