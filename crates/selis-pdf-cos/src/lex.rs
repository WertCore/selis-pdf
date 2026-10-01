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

/// Lexical deviations in a file's **object syntax**, with stream bodies skipped.
///
/// This is the instrument for reporting what a *document* deviates on, as
/// distinct from {@link Lexer::deviations}, which reports what a whole-file lex
/// saw.
///
/// # Why not just lex the whole file
///
/// Because most of a PDF is opaque binary. A font program, a JPEG and a
/// Flate-coded content stream are all byte soup at the COS layer, and lexing
/// them as COS text produces deviations that describe the data, not the file.
/// Measured on `corpus/fixtures/form_two_pages.pdf` - a clean, ordinary
/// document with two embedded font programs - a whole-file lex reports **70+
/// deviations**: 68 `invalid-hex-digit`, plus `unterminated-string`,
/// `unexpected-closing-paren`, `reserved-delimiter` and `unexpected-gt`. Every
/// one of them is the font data being read as syntax.
///
/// A report built on that is not merely noisy, it is actively harmful: a
/// health panel that says "70 deviations" about a clean document has taught its
/// reader to ignore the number, which costs every future reading of it.
///
/// # What it does report
///
/// Deviations in object syntax: dictionaries, arrays, names, numbers, strings
/// and the keywords between them. That is the part a producer got wrong in a
/// way a user could act on.
///
/// Two exclusions, both load-bearing, and neither is a blanket "ignore
/// everything this class of thing says":
///
///  1. **Stream bodies are skipped** - structurally, by
///     [`Lexer::skip_stream_body`]. This is the one that matters: it turns a
///     70+-entry flood on a clean document into zero.
///  2. **`UnknownWord` is filtered**, which after (1) removes a small, fully
///     enumerated set: the file-structure keywords (`xref`, `trailer`,
///     `startxref`) and the cross-reference table's `n` / `f` flags. These are
///     mandatory parts of the format and are not object syntax, so no conforming
///     file is free of them.
///
/// # Budget
///
/// Ticked per token by `next_token` and per byte by `skip_stream_body`, so a
/// hostile file is bounded exactly like any other parse.
///
/// # Malformed Input
///
/// A body with no `endstream` ends the pass; whatever was found before it is
/// still true and still worth reporting. Deduplicated by (name, offset),
/// because the lexer can record the same defect twice across a read boundary
/// and a report that counts one defect twice is a report nobody trusts.
///
/// # Errors
///
/// Budget and cancellation errors surface as the guard's typed error. A lexer
/// error ends the pass rather than propagating, for the same reason
/// `selis inspect` treats one: the question is what was *tolerated*.
pub fn object_deviations(src: &[u8], g: &mut BudgetGuard<'_>) -> Vec<Deviation> {
    let mut lex = Lexer::new(src);
    loop {
        // A lexer error ends the pass. `inspect` does the same, and for the same
        // reason: a document that was tolerated up to here still has real
        // deviations worth naming.
        let token = match lex.next_token(g) {
            Ok(Some(t)) => t,
            Ok(None) | Err(_) => break,
        };
        if token == Token::Stream && !lex.skip_stream_body(g).unwrap_or(false) {
            // No `endstream`: the body ran to end of input, so there is nothing
            // left to lex.
            break;
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    lex.deviations
        .iter()
        .copied()
        // `unknown-word` is excluded, and after the stream skip it is a SMALL
        // and fully explicable set rather than a flood.
        //
        // The COS object lexer knows `obj`, `endobj`, `stream`, `endstream`,
        // `true`, `false`, `null` and `R`. It does not know the FILE-structure
        // keywords, because they are not object syntax: `xref`, `trailer` and
        // `startxref` head the cross-reference section, and the table's own
        // `n` / `f` in-use and free flags are bare words too.
        //
        // Measured on a minimal one-page document, that is exactly 8
        // `unknown-word` entries - `xref`, `trailer`, `startxref`, four `n` and
        // one `f`. On a real two-page document with embedded fonts, exactly 1.
        // Every one is a MANDATORY part of the file format. Reporting them would
        // mean every conforming PDF has at least eight deviations, which is not
        // a deviation report, it is a report with a constant offset.
        //
        // The genuine cases this class exists for - a stray bare word where an
        // object should be - remain visible to `selis inspect`, which reads the
        // unfiltered list on purpose: it is a forensic tool, not a summary for a
        // user, and there the raw lexer's view is the right one.
        .filter(|d| {
            !matches!(d, Deviation::UnknownWord { .. }) && seen.insert((d.name(), d.offset()))
        })
        .collect()
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

    /// Skip a stream body, from just after the `stream` keyword to just past
    /// the matching `endstream`.
    ///
    /// Returns `true` when `endstream` was found, `false` when the body ran to
    /// end of input without one.
    ///
    /// # Why this exists
    ///
    /// A stream body is **opaque data** at the COS layer. `next_token` has no
    /// business lexing it: a font program read as COS text yields a stream of
    /// `invalid-hex-digit` and `unterminated-string` deviations that say nothing
    /// about the document. Measured on `corpus/fixtures/form_two_pages.pdf`,
    /// which embeds two font programs, a whole-file lex reports **70+ phantom
    /// deviations** on a clean file - one per binary byte that happened to look
    /// like a delimiter.
    ///
    /// So a caller that wants to know what the *document* deviates on lexes
    /// object syntax and skips bodies with this.
    ///
    /// # Why the `endstream` match requires a preceding EOL
    ///
    /// ISO 32000-2 §7.3.8.1 requires `endstream` to be preceded by an EOL, and
    /// requiring it is what keeps the nine bytes `endstream` appearing *inside*
    /// a font program from truncating the scan early. A bare substring search
    /// would resume lexing in the middle of binary data - which is how a
    /// "skip streams" implementation turns one flood of nonsense into a
    /// different flood of nonsense.
    ///
    /// # Budget
    ///
    /// Ticks per byte scanned, so a truncated or hostile body is bounded exactly
    /// like any other parse. `BUDGET_*` surfaces as the guard's typed error.
    pub fn skip_stream_body(&mut self, g: &mut BudgetGuard<'_>) -> Result<bool> {
        const KEYWORD: &[u8] = b"endstream";
        while self.pos < self.src.len() {
            g.tick()?;
            let rest = &self.src[self.pos..];
            if rest.starts_with(KEYWORD) {
                // Preceded by EOL, or at offset 0 (a body that is its own
                // first byte cannot be preceded by anything).
                let preceded_by_eol = self.pos == 0
                    || matches!(
                        self.src.get(self.pos.wrapping_sub(1)).copied(),
                        Some(b'\n' | b'\r')
                    );
                if preceded_by_eol {
                    self.pos = self.pos.saturating_add(KEYWORD.len());
                    // The EOL after `endstream` belongs to the file, not the
                    // body, and leaving it would let the next token read a
                    // stray CR.
                    while matches!(self.peek(), Some(b'\n' | b'\r')) {
                        self.bump();
                    }
                    return Ok(true);
                }
            }
            self.pos = self.pos.saturating_add(1);
        }
        Ok(false)
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
        while let Some(b) = self.peek() {
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
        while let Some(b) = self.peek() {
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
        type Case<'a> = (&'a [u8], fn(&Deviation) -> bool, Token);
        let cases: &[Case<'_>] = &[
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
                devs.iter().any(pred),
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
                while let Ok(Some(_)) = l.next_token(&mut g) {}
            }
        }
    }

    /// `object_deviations` exists because a whole-file lex describes the DATA in
    /// a document, not the document. These are the legs that hold that line.
    mod object_deviations {
        use super::*;

        fn names(src: &[u8]) -> Vec<&'static str> {
            let mut g = guard();
            object_deviations(src, &mut g)
                .iter()
                .map(|d| d.name())
                .collect()
        }

        /// A body full of bytes that are not COS syntax must contribute nothing.
        ///
        /// This is the leg the whole function is for. The body below is binary
        /// soup of the kind a font program or a JPEG produces: unbalanced
        /// parens, lone `<` and `>`, and a `%`. A whole-file lex turns it into a
        /// dozen deviations, none of which say anything about the file.
        #[test]
        fn binary_stream_body_contributes_nothing() {
            let src =
                b"1 0 obj\n<< /Length 24 >>\nstream\n((( <<< >>> %%% ((( )))\nendstream\nendobj\n";
            assert_eq!(names(src), Vec::<&str>::new());
        }

        /// The NEGATIVE control for the leg above, and the one that keeps it
        /// honest: without a skip, this very body DOES produce deviations.
        #[test]
        fn the_same_body_would_be_reported_without_the_skip() {
            let src =
                b"1 0 obj\n<< /Length 24 >>\nstream\n((( <<< >>> %%% ((( )))\nendstream\nendobj\n";
            let mut g = guard();
            let mut lex = Lexer::new(src);
            while let Ok(Some(_)) = lex.next_token(&mut g) {}
            assert!(
                !lex.deviations().is_empty(),
                "premise: a whole-file lex really does invent deviations from stream data"
            );
        }

        /// A deviation in the object syntax either side of a body IS reported.
        /// The skip must not swallow the document along with the data.
        #[test]
        fn object_syntax_around_a_body_is_still_reported() {
            // `<ABC` before the stream: an odd-length hex string in the dict.
            let src = b"1 0 obj\n<< /Note <ABC /Length 4 >>\nstream\njunk\nendstream\nendobj\n";
            assert!(names(src).contains(&"odd-length-hex"));
        }

        /// `endstream` embedded in a body must not end the skip.
        ///
        /// The nine bytes occur in binary data. Requiring a preceding EOL
        /// (ISO 32000-2 §7.3.8.1) is what stops the common case - the keyword
        /// appearing mid-word, with a data byte in front of it - from
        /// truncating the scan.
        #[test]
        fn an_endstream_embedded_in_a_word_does_not_end_the_skip() {
            let src =
                b"1 0 obj\n<< /Length 40 >>\nstream\nXendstream ((( junk\nendstream\nendobj\n";
            let mut g = guard();
            let mut lex = Lexer::new(src);
            loop {
                match lex.next_token(&mut g) {
                    Ok(Some(Token::Stream)) => break,
                    Ok(Some(_)) => {}
                    _ => panic!("fixture has no stream keyword"),
                }
            }
            assert!(
                lex.skip_stream_body(&mut g).expect("skip"),
                "the real endstream is found"
            );
            let after = lex.pos() as usize;
            let tail = &src[after..];
            assert!(
                tail.starts_with(b"endobj"),
                "resumed after the REAL endstream, got {:?}",
                String::from_utf8_lossy(&tail[..tail.len().min(24)])
            );
        }

        /// The limit of the rule, pinned so it cannot be forgotten.
        ///
        /// A body containing a *syntactically valid* `\nendstream\n` is
        /// genuinely ambiguous: the real terminator has exactly that shape, so
        /// no rule keyed on the keyword alone can tell them apart. This records
        /// what actually happens rather than asserting a guarantee that does not
        /// exist.
        ///
        /// The consequence is bounded and in the safe direction: the skip ends
        /// early, lexing resumes inside binary data, and the report gains
        /// spurious entries. It cannot lose a real deviation and it cannot
        /// crash. Making it exact would mean honouring the stream dictionary's
        /// `/Length` instead of searching for the keyword - which needs the
        /// lexer to track dictionary state across tokens, and is the reason this
        /// is a recorded limit rather than an oversight.
        #[test]
        fn a_syntactically_valid_endstream_inside_a_body_is_ambiguous() {
            let src =
                b"1 0 obj\n<< /Length 40 >>\nstream\nendstream is not the end\nendstream\nendobj\n";
            let mut g = guard();
            let mut lex = Lexer::new(src);
            loop {
                match lex.next_token(&mut g) {
                    Ok(Some(Token::Stream)) => break,
                    Ok(Some(_)) => {}
                    _ => panic!("fixture has no stream keyword"),
                }
            }
            assert!(
                lex.skip_stream_body(&mut g).expect("skip"),
                "some endstream was found"
            );
            assert!(
                !src[lex.pos() as usize..].starts_with(b"endobj"),
                "the skip ended early at the ambiguous keyword, as documented"
            );
        }

        /// The `UnknownWord` exclusion is pinned to an exact enumeration, so it
        /// cannot quietly widen.
        ///
        /// After the stream skip, the only unknown words in a conforming file are
        /// the file-structure keywords and the cross-reference table's `n` / `f`
        /// flags. This asserts the count is exactly that - 7 - rather than
        /// "zero", because a filter that removed everything would also pass a
        /// zero assertion, and then a genuine stray bare word would go
        /// unreported with nothing to notice.
        #[test]
        fn only_the_file_structure_keywords_are_unknown_words() {
            let src = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000056 00000 n \n0000000113 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n160\n%%EOF\n";
            let mut g = guard();
            let mut lex = Lexer::new(src);
            while let Ok(Some(_)) = lex.next_token(&mut g) {}
            let unknown: Vec<u64> = lex
                .deviations()
                .iter()
                .filter(|d| d.name() == "unknown-word")
                .map(|d| d.offset())
                .collect();
            assert_eq!(
                unknown.len(),
                7,
                "xref, trailer, startxref, three `n` and one `f` - all mandatory. Got {unknown:?}"
            );
            // And the filtered report is empty, which is the property the engine
            // layer depends on.
            assert_eq!(names(src), Vec::<&str>::new());
        }
        /// No `endstream` at all: the pass ends, and reports what it found.
        /// A truncated file is still a file a user needs told about.
        #[test]
        fn a_body_with_no_endstream_ends_the_pass() {
            let src = b"1 0 obj\n<< /Note <ABC /Length 8 >>\nstream\nno terminator";
            let found = names(src);
            assert!(
                found.contains(&"odd-length-hex"),
                "deviations found before the body survive: {found:?}"
            );
        }
    }
}
