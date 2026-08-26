//! `selis-pdf-content` — the content-stream interpreter (SL-2.CONT.*).
//!
//! Tokenises and dispatches PDF content streams: the graphics and text
//! operators that paint a page. Phase 2.1 delivers the tokeniser + operator
//! dispatch (SL-2.CONT.01); the graphics state machine, paths, text, and
//! imaging land in SL-2.CONT.02–06.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod dispatch;
pub mod display_list;
pub mod exec;
pub mod form;
pub mod gstate;
pub mod inline_image;
pub mod lex;
pub mod marked_content;
pub mod path;
pub mod resumable;
pub mod text;

pub use dispatch::{Dispatch, Interpreter, Operand, Operator};
pub use display_list::{diff, DisplayList, GlyphRun, Op, ResolvedState};
pub use form::{Form, FormWorklist, ResourceDict};
pub use gstate::{GState, GStateStack};
pub use inline_image::{extract_inline_image, InlineImage};
pub use lex::{tokenise, Lexer, Tok};
pub use marked_content::{MarkedContent, McStack, McidMap};
pub use path::{PaintOp, PaintedPath, Path, Segment};
pub use resumable::{ResumableInterpreter, Step};

use selis_error::Result;
use selis_sandbox::BudgetGuard;

/// Run a content stream to completion, returning the dispatched operators.
///
/// # Budget
///
/// Charged per token and operator; a hostile stream of infinite operators
/// terminates.
///
/// # Malformed Input
///
/// Unknown operators are deviations, never fatal; a hard lexer error or
/// budget exhaustion stops the run.
pub fn interpret(src: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<Dispatch>> {
    let mut interp = Interpreter::new(src);
    interp.run(g)?;
    Ok(interp.ops)
}
