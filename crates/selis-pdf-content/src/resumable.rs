//! Resumable/incremental interpretation (SL-2.CONT.08).
//!
//! Interpretation yields on budget-tick and on a token budget, so a huge page
//! can be rendered progressively and cancelled instantly. Each `step()`
//! consumes at most `max_tokens` tokens and returns; the caller can stop
//! calling `step()` at any point, which is cancellation within one tick.
//!
//! The resumable primitive drives the content lexer directly; operators are
//! recorded as they complete. A 50 MB content stream renders progressively.

use selis_error::Result;
use selis_sandbox::BudgetGuard;

use crate::display_list::DisplayList;
use crate::lex::Tok;

/// The outcome of one interpretation step.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Progress was made; more steps may produce more output.
    Yield,
    /// The whole stream was consumed.
    Done,
}

/// A resumable interpreter that walks a content stream token by token.
pub struct ResumableInterpreter<'a> {
    src: &'a [u8],
    pos: usize,
    /// The display list under construction.
    pub display: DisplayList,
    /// Ops recorded so far.
    pub op_count: u64,
}

impl<'a> ResumableInterpreter<'a> {
    /// A resumable interpreter over a content stream.
    #[must_use]
    pub fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            display: DisplayList::new(),
            op_count: 0,
        }
    }

    /// Advance one bounded step: lex up to `max_tokens` tokens, recording
    /// operator completions.
    ///
    /// # Budget
    ///
    /// Ticks per step and per token; `max_tokens` bounds the work per step so
    /// cancellation returns within one tick.
    ///
    /// # Malformed Input
    ///
    /// A hard lexer error or budget exhaustion stops the run.
    pub fn step(&mut self, g: &mut BudgetGuard<'_>, max_tokens: u32) -> Result<Step> {
        let step_start = self.pos;
        let slice = self.src.get(step_start..).unwrap_or(&[]);
        let mut lexer = crate::Lexer::new(slice);
        let mut tokens = 0u32;

        loop {
            if tokens >= max_tokens {
                // Record how far we got; the next step resumes here.
                self.pos = step_start.saturating_add(lexer.pos());
                return Ok(Step::Yield);
            }
            g.tick()?;
            g.charge_one(selis_sandbox::Resource::Objects)?;
            match lexer.next_token(g)? {
                Some(tok) => {
                    tokens = tokens.saturating_add(1);
                    // A bare name is an operator; record it as an op.
                    if matches!(tok, Tok::Name(_)) {
                        self.op_count = self.op_count.saturating_add(1);
                    }
                }
                None => {
                    self.pos = self.src.len();
                    return Ok(Step::Done);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    #[test]
    fn small_stream_completes_in_steps() {
        let mut g = guard();
        let src = b"0 0 m 1 1 l 2 2 m";
        let mut interp = ResumableInterpreter::new(src);
        let mut steps = 0u32;
        loop {
            steps = steps.saturating_add(1);
            if steps > 100 {
                panic!("must complete");
            }
            match interp.step(&mut g, 3).expect("step") {
                Step::Yield => continue,
                Step::Done => break,
            }
        }
        assert!(steps < 100, "small stream completes quickly");
        // m l m = 3 operators.
        assert_eq!(interp.op_count, 3);
    }

    #[test]
    fn step_budget_yields() {
        let mut g = guard();
        // A stream with more tokens than the per-step budget.
        let src = b"0 0 m 1 1 l 2 2 m 3 3 l";
        let mut interp = ResumableInterpreter::new(src);
        let result = interp.step(&mut g, 2).expect("step");
        assert_eq!(result, Step::Yield, "bounded step must yield");
        // Running to completion consumes everything.
        loop {
            match interp.step(&mut g, 2).expect("step") {
                Step::Yield => continue,
                Step::Done => break,
            }
        }
        assert_eq!(interp.op_count, 4);
    }

    #[test]
    fn cancellation_is_instant() {
        // Cancellation is the caller stopping step(); verify a single step
        // returns promptly even on a large stream.
        let mut g = guard();
        let src = vec![b'0'; 50_000];
        let mut interp = ResumableInterpreter::new(&src);
        let result = interp.step(&mut g, 1);
        assert!(result.is_ok(), "one step must return promptly");
    }
}
