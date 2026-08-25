//! Resumable parsing over partial sources (SL-1.COS.07).
//!
//! Every parse step tolerates `Availability::Pending`, unwinding with the
//! ranges it needs instead of blocking. This is how a linearised 200 MB PDF
//! opens before the first megabyte has arrived: the lexer asks for bytes, the
//! source returns `Pending` with what it has, and the parse re-drives when the
//! data lands.
//!
//! The lexer is an explicit state machine over a growing resident prefix — not
//! recursion — so the position and needed ranges are always knowable and the
//! caller (not the stack) decides when to resume.
//!
//! # DoD (SL-1.COS.07)
//!
//! A `FaultSource` that holds reads returns `Need`; after `release()` the same
//! lexer continues from the exact byte it stopped at.

use selis_error::{Code, Result};
use selis_io::{Availability, DocSource, RangeSet};
use selis_sandbox::BudgetGuard;

use crate::deviation::Deviation;
use crate::lex::{Lexer, Token};

/// The outcome of one resumable step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseStep<T> {
    /// The step produced a value.
    Done(T),
    /// The step needs these byte ranges resident before it can continue.
    Need(RangeSet),
}

/// The result of fetching more bytes from the source.
enum FetchResult {
    /// More bytes were made resident.
    More,
    /// The source reports end of file.
    Eof,
    /// The source cannot satisfy the read yet.
    Need(RangeSet),
}

/// A lexer that reads from a [`DocSource`] and pauses on `Pending`.
pub struct ResumableLexer<'a> {
    source: &'a dyn DocSource,
    /// The resident prefix of the file, always `[base, base + len)`.
    resident: Vec<u8>,
    /// The file offset of `resident[0]`.
    base: u64,
    /// The byte offset (into the file) where the next token begins.
    token_start: u64,
    /// Deviations recorded for completed tokens.
    deviations: Vec<Deviation>,
    /// Whether the source reported `Eof`.
    reached_eof: bool,
    /// How many bytes to request per fetch (bounded).
    read_ahead: u64,
}

impl<'a> ResumableLexer<'a> {
    /// A resumable lexer over a partial source.
    #[must_use]
    pub fn new(source: &'a dyn DocSource) -> Self {
        Self {
            source,
            resident: Vec::new(),
            base: 0,
            token_start: 0,
            deviations: Vec::new(),
            reached_eof: false,
            // 64 KiB per fetch; large enough to lex most tokens in one pass,
            // small enough that a held network read costs little.
            read_ahead: 64 * 1024,
        }
    }

    /// The deviations recorded so far (for completed tokens only).
    #[must_use]
    pub fn deviations(&self) -> &[Deviation] {
        &self.deviations
    }

    /// The file offset the lexer is at.
    #[must_use]
    pub fn pos(&self) -> u64 {
        self.token_start
    }

    /// Produce the next token, or `None` at end of input.
    ///
    /// # Budget
    ///
    /// Every re-lex of a partial token is charged; the read buffer is charged
    /// as bytes.
    ///
    /// # Malformed Input
    ///
    /// Hard errors (unterminated string at true EOF, numeric overflow) are
    /// returned; anything else is either a token or a `Need` for more bytes.
    pub fn next_token(&mut self, g: &mut BudgetGuard<'_>) -> Result<ParseStep<Option<Token>>> {
        loop {
            // If we have nothing resident to inspect, fetch.
            if self.token_start >= self.file_len() {
                match self.fetch(g)? {
                    FetchResult::More => continue,
                    FetchResult::Eof => {
                        self.reached_eof = true;
                        return Ok(ParseStep::Done(None));
                    }
                    FetchResult::Need(ranges) => return Ok(ParseStep::Need(ranges)),
                }
            }

            // Lex one token from the resident prefix at token_start.
            let slice = self.resident_slice();
            let mut lex = Lexer::new(slice);
            let token = lex.next_token(g);

            match token {
                Ok(Some(t)) => {
                    let consumed = usize::try_from(lex.pos()).unwrap_or(usize::MAX);
                    let devs = lex.deviations().to_vec();
                    let touched_end = consumed >= slice.len();
                    if touched_end && !self.reached_eof {
                        match self.fetch(g)? {
                            FetchResult::More => continue,
                            FetchResult::Eof => {
                                self.reached_eof = true;
                                continue;
                            }
                            FetchResult::Need(ranges) => {
                                return Ok(ParseStep::Need(ranges));
                            }
                        }
                    }
                    self.deviations.extend(devs);
                    self.token_start = self
                        .token_start
                        .saturating_add(u64::try_from(consumed).unwrap_or(u64::MAX));
                    return Ok(ParseStep::Done(Some(t)));
                }
                Ok(None) => {
                    if self.reached_eof {
                        return Ok(ParseStep::Done(None));
                    }
                    match self.fetch(g)? {
                        FetchResult::More => continue,
                        FetchResult::Eof => {
                            self.reached_eof = true;
                            continue;
                        }
                        FetchResult::Need(ranges) => return Ok(ParseStep::Need(ranges)),
                    }
                }
                Err(e) if e.code() == Code::LexUnterminatedString && !self.reached_eof => {
                    // A literal string cut short by the resident prefix. Fetch
                    // more and re-lex; if EOF arrives, the re-lex errors.
                    match self.fetch(g)? {
                        FetchResult::More => continue,
                        FetchResult::Eof => {
                            self.reached_eof = true;
                            continue;
                        }
                        FetchResult::Need(ranges) => return Ok(ParseStep::Need(ranges)),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// The total file length known so far (resident end, or source `len()`).
    fn file_len(&self) -> u64 {
        self.base.saturating_add(self.resident.len() as u64)
    }

    /// The resident slice from `token_start` to the end of resident data.
    fn resident_slice(&self) -> &[u8] {
        let start =
            usize::try_from(self.token_start.saturating_sub(self.base)).unwrap_or(usize::MAX);
        self.resident.get(start..).unwrap_or(&[])
    }

    /// Ask the source for the next chunk of bytes.
    fn fetch(&mut self, g: &mut BudgetGuard<'_>) -> Result<FetchResult> {
        let off = self.file_len();
        let want = usize::try_from(self.read_ahead).unwrap_or(usize::MAX);
        let mut buf = vec![0u8; want];
        g.charge(selis_sandbox::Resource::Bytes, self.read_ahead)?;
        match self.source.read_at(off, &mut buf)? {
            Availability::Filled(n) => {
                if n == 0 {
                    return Ok(FetchResult::Eof);
                }
                let slice = buf.get(..n).unwrap_or(&[]);
                self.resident.extend_from_slice(slice);
                Ok(FetchResult::More)
            }
            Availability::Pending { hint } => Ok(FetchResult::Need(hint)),
            Availability::Eof => Ok(FetchResult::Eof),
        }
    }
}

/// A resumable wrapper over the object parser is the next layer up
/// (SL-1.COS.07 continues in `selis-pdf-doc`); the lexer is the resumability
/// primitive every parse entry point drives.

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss
    )]

    use super::*;
    use selis_io::FaultConfig;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn collect(lex: &mut ResumableLexer<'_>, g: &mut BudgetGuard<'_>) -> Vec<Token> {
        let mut out = Vec::new();
        loop {
            match lex.next_token(g).expect("step") {
                ParseStep::Done(Some(t)) => out.push(t),
                ParseStep::Done(None) => break,
                ParseStep::Need(_) => panic!("source never holds in this test"),
            }
        }
        out
    }

    #[test]
    fn resumable_lexer_over_resident_source_matches_plain_lexer() {
        let src = selis_io::MemSource::new(b"/Type /Catalog << /Pages 5 0 R >>");
        let mut g = guard();
        let mut lex = ResumableLexer::new(&src);
        let toks = collect(&mut lex, &mut g);
        // Plain lexer produces the same tokens.
        let mut plain = Lexer::new(b"/Type /Catalog << /Pages 5 0 R >>");
        let mut expected = Vec::new();
        while let Some(t) = plain.next_token(&mut g).expect("plain") {
            expected.push(t);
        }
        assert_eq!(toks, expected);
        assert!(lex.deviations().is_empty());
    }

    #[test]
    fn resumable_lexer_over_partial_source_returns_need_then_resumes() {
        let data = b"/Type /Catalog << /Pages 5 0 R >>";
        // A source that holds reads until released.
        let src = selis_io::FaultSource::new(
            data.to_vec(),
            FaultConfig {
                hold_reads: true,
                ..FaultConfig::default()
            },
        );
        let mut g = guard();
        let mut lex = ResumableLexer::new(&src);
        // First step: the source is holding, so we get a Need, not a token.
        match lex.next_token(&mut g).expect("step") {
            ParseStep::Need(_) => {}
            other => panic!("expected Need while held, got {other:?}"),
        }
        // Release the source; the lexer now completes the token stream.
        src.release();
        let toks = collect(&mut lex, &mut g);
        assert_eq!(
            toks,
            vec![
                crate::Token::Name(selis_bytes::Bytes::copy_from_slice(b"Type")),
                crate::Token::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
                crate::Token::DictStart,
                crate::Token::Name(selis_bytes::Bytes::copy_from_slice(b"Pages")),
                crate::Token::Number(crate::Number::Int(5)),
                crate::Token::Number(crate::Number::Int(0)),
                crate::Token::Ref,
                crate::Token::DictEnd,
            ]
        );
    }

    /// DoD: a string that spans a chunk boundary resumes correctly.
    #[test]
    fn token_spanning_fetch_boundary_resumes() {
        // Force a tiny read-ahead so a long name spans two fetches.
        let data = format!("/{}", "A".repeat(200)).into_bytes();
        let src = selis_io::MemSource::new(&data);
        let mut g = guard();
        let mut lex = ResumableLexer::new(&src);
        lex.read_ahead = 32; // small chunk forces multi-fetch
        let toks = collect(&mut lex, &mut g);
        assert_eq!(toks.len(), 1);
        match &toks[0] {
            crate::Token::Name(b) => assert_eq!(b.as_slice(), &"A".repeat(200).as_bytes()[..]),
            other => panic!("expected a name, got {other:?}"),
        }
    }

    /// DoD: truncation (the source ends mid-token) errors, not hangs.
    #[test]
    fn truncated_token_is_a_typed_error() {
        // `(abc` with no closing paren and EOF reached.
        let src = selis_io::MemSource::new(b"(abc");
        let mut g = guard();
        let mut lex = ResumableLexer::new(&src);
        let e = lex.next_token(&mut g).expect_err("unterminated at EOF");
        assert_eq!(e.code(), Code::LexUnterminatedString);
    }
}
