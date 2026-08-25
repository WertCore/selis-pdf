//! Clipping (SL-2.RAST.03).
//!
//! The clip path is the intersection of every `W`/`W*` path seen so far. This
//! module maintains the clip stack with a depth budget (the same budget the
//! `q`/`Q` stack uses — every `q`/`W` pair nests), and drives the backend's
//! clip operation. Text render modes 4–7 (which add glyph outlines to the
//! clip) land with the text model; the path-clip machinery is here.

use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

use crate::{Backend, FillRule, Path};

/// The clip stack: intersecting clip paths, depth-budgeted.
#[derive(Debug, Default)]
pub struct ClipStack {
    /// The clip paths, innermost last (the effective clip is their
    /// intersection).
    paths: Vec<(Path, FillRule)>,
    /// The maximum nesting depth.
    max_depth: u64,
}

impl ClipStack {
    /// An empty clip stack with a depth budget.
    #[must_use]
    pub fn new(max_depth: u64) -> Self {
        Self {
            paths: Vec::new(),
            max_depth,
        }
    }

    /// The number of clip paths currently applied.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the clip stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Push a clip path (`W`/`W*`), intersecting it with the current clip.
    ///
    /// # Errors
    ///
    /// `BUDGET_DEPTH` when the stack exceeds the depth budget (a hostile
    /// nesting of clip paths).
    pub fn push(&mut self, path: Path, rule: FillRule, g: &mut BudgetGuard<'_>) -> Result<()> {
        if self.paths.len() >= usize::try_from(self.max_depth).unwrap_or(usize::MAX) {
            return Err(err!(
                Code::BudgetDepth,
                during = "clip-stack",
                detail = "clip nesting exceeds the depth budget"
            ));
        }
        self.paths.push((path, rule));
        g.enter()?;
        Ok(())
    }

    /// Pop the most recent clip path (paired with a `Q`).
    ///
    /// # Errors
    ///
    /// `BUDGET_DEPTH` on underflow (a `Q` with no matching `W`).
    pub fn pop(&mut self, g: &mut BudgetGuard<'_>) -> Result<()> {
        self.paths.pop().ok_or_else(|| {
            err!(
                Code::BudgetDepth,
                during = "clip-stack",
                detail = "Q with no matching clip"
            )
        })?;
        g.leave();
        Ok(())
    }

    /// Replay the whole clip stack onto a backend (after a `Q` or at paint
    /// time), so the backend's clip is the intersection of all of them.
    pub fn apply<B: Backend>(&self, backend: &mut B) {
        for (path, rule) in &self.paths {
            backend.clip(path, *rule);
        }
    }

    /// The innermost clip path, if any.
    #[must_use]
    pub fn innermost(&self) -> Option<&(Path, FillRule)> {
        self.paths.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn rect() -> Path {
        Path {
            commands: vec![crate::PathCmd::Move(selis_geom::Point::new(0.0, 0.0))],
        }
    }

    #[test]
    fn push_pop_roundtrip() {
        let mut g = guard();
        let mut stack = ClipStack::new(10);
        stack.push(rect(), FillRule::NonZero, &mut g).expect("push");
        assert_eq!(stack.len(), 1);
        stack.pop(&mut g).expect("pop");
        assert!(stack.is_empty());
    }

    #[test]
    fn underflow_is_a_typed_error() {
        let mut g = guard();
        let mut stack = ClipStack::new(10);
        let e = stack.pop(&mut g).expect_err("underflow");
        assert_eq!(e.code(), Code::BudgetDepth);
    }

    #[test]
    fn depth_budget_is_enforced() {
        let mut g = guard();
        let mut stack = ClipStack::new(2);
        stack.push(rect(), FillRule::NonZero, &mut g).expect("1");
        stack.push(rect(), FillRule::NonZero, &mut g).expect("2");
        let e = stack
            .push(rect(), FillRule::NonZero, &mut g)
            .expect_err("depth");
        assert_eq!(e.code(), Code::BudgetDepth);
    }

    #[test]
    fn apply_replays_the_stack() {
        let mut g = guard();
        let mut stack = ClipStack::new(10);
        stack.push(rect(), FillRule::EvenOdd, &mut g).expect("push");
        let mut backend = crate::RecordingBackend::default();
        stack.apply(&mut backend);
        assert!(matches!(
            backend.calls.first(),
            Some(crate::Call::Clip {
                rule: FillRule::EvenOdd
            })
        ));
    }
}
