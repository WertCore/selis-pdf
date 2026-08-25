//! `selis-pdf-content` — the content-stream interpreter (SL-2.CONT.*).
//!
//! Tokenises and dispatches PDF content streams: the graphics and text
//! operators that paint a page. Phase 2.1 delivers the tokeniser + operator
//! dispatch (SL-2.CONT.01); the graphics state machine, paths, text, and
//! imaging land in SL-2.CONT.02–06.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod dispatch;
pub mod form;
pub mod gstate;
pub mod lex;
pub mod path;

pub use dispatch::{Dispatch, Interpreter, Operand, Operator};
pub use form::{Form, FormWorklist, ResourceDict};
pub use gstate::{GState, GStateStack};
pub use lex::{tokenise, Lexer, Tok};
pub use path::{PaintOp, PaintedPath, Path, Segment};

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
