//! `selis-pdf-cos` — the COS object layer (SL-1.COS.*).
//!
//! Owns the lexer, object model, xref tables and streams, trailer, incremental
//! updates, linearisation parsing, and damaged-file reconstruction
//! (01-ARCHITECTURE.md §3). Open-sourced under Apache-2.0 (ADR-P0030); the
//! single most fuzzed crate in the engine.
//!
//! **Phase 1 writes no pixels.** It builds the object layer and, more
//! importantly, establishes that the engine can survive the real world's PDFs.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod deviation;
pub mod lex;
pub mod obj;
pub mod parse;
pub mod reconstruct;
pub mod resume;
pub mod revision;
pub mod xref;
pub mod xref_stream;

pub use deviation::Deviation;
pub use lex::{tokenise, Lexer, Number, Token};
pub use obj::{Obj, Ref};
pub use parse::{parse_all, ObjectParser};
pub use reconstruct::{reconstruct, reconstruct_index, recover_root, Reconstruction};
pub use resume::{ParseStep, ResumableLexer};
pub use revision::{parse_revisions, Doc, Revision, RevisionView};
pub use xref::{parse_classic_xref, XrefEntry, XrefIndex};
pub use xref_stream::{parse_object_stream, parse_xref_stream, XrefStream};

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard, FixedClock, Surface};

/// A parsed document, placeholder for the full document model (SL-1.DOC.*).
///
/// This is what `parse` produces in Phase 1: the tokens, the parsed object
/// tree, and the deviations the lexer tolerated.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Document {
    /// Tokens produced by lexing the whole input.
    pub tokens: Vec<lex::Token>,
    /// Deviations the lexer tolerated.
    pub deviations: Vec<Deviation>,
    /// The top-level objects parsed from the token stream (COS.02).
    pub objects: Vec<obj::Obj>,
}

/// Parse the COS object layer of a byte buffer.
///
/// This is the **entry point the fuzz target drives**: it must never panic,
/// never allocate unboundedly, and terminate within the budget
/// (ADR-P0006, SL-0.SEC.02).
///
/// # Budget
///
/// The caller supplies a [`Budget`]; every token and object is charged, and the
/// wall-clock is checked so a hostile file of infinite tokens terminates.
///
/// # Malformed Input
///
/// Anything that is not strict COS is either recorded as a [`Deviation`] or
/// returned as a typed error — never a panic.
pub fn parse(src: &[u8], budget: &Budget) -> Result<Document> {
    let mut g = BudgetGuard::new(*budget, &NO_CLOCK, Default::default());
    let mut lexer = Lexer::new(src);
    let mut tokens = Vec::new();
    while let Some(tok) = lexer.next_token(&mut g)? {
        tokens.push(tok);
    }
    let deviations = lexer.deviations().to_vec();
    let objects = parse_all(&tokens, budget, &mut g)?;
    Ok(Document {
        tokens,
        deviations,
        objects,
    })
}

static NO_CLOCK: FixedClock = FixedClock(0);

/// Open a document under the viewer profile, for tools and tests.
///
/// # Budget
///
/// Uses the [`Surface::Viewer`] profile.
///
/// # Malformed Input
///
/// As [`parse`].
pub fn open(src: &[u8]) -> Result<Document> {
    parse(src, &Budget::profile(Surface::Viewer))
}
