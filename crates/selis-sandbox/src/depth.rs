//! Depth guards and the no-native-recursion rule (SL-0.SBX.04).
//!
//! # Why native recursion is banned in three crates
//!
//! `selis-pdf-cos`, `selis-pdf-content` and `selis-pdf-doc` walk structures whose
//! nesting depth is chosen by the document: `/Prev` chains, nested arrays, form
//! XObjects referencing form XObjects, page trees. Native recursion on those is a
//! stack overflow, and a stack overflow is a **process abort we cannot catch** —
//! it defeats the panic trampoline of `SL-0.ERR.03` and takes the user's unsaved
//! work with it.
//!
//! So those three crates use explicit worklists with a depth counter. This module
//! provides the counter as an RAII guard that cannot forget to decrement, plus the
//! worked example the rule refers to.
//!
//! # The worklist pattern
//!
//! ```
//! use selis_sandbox::{Budget, Resource};
//! # #[derive(Clone)] enum Node { Leaf(u32), Branch(Vec<Node>) }
//!
//! /// Sum a tree of document-chosen depth without recursing.
//! fn sum(root: &Node, g: &mut selis_sandbox::BudgetGuard<'_>) -> selis_error::Result<u32> {
//!     // (node, depth) pairs. The depth travels with the item, so the counter
//!     // cannot drift out of step with the traversal.
//!     let mut work = vec![(root.clone(), 0u16)];
//!     let mut total = 0u32;
//!     while let Some((node, depth)) = work.pop() {
//!         g.tick()?;                       // deadline + cancellation, every iteration
//!         if u64::from(depth) > g.budget().limit(Resource::Depth) {
//!             return Err(selis_error::Error::new(selis_error::Code::BudgetDepth));
//!         }
//!         match node {
//!             Node::Leaf(v) => total = total.saturating_add(v),
//!             Node::Branch(kids) => {
//!                 for k in kids {
//!                     g.charge_one(Resource::Objects)?;
//!                     work.push((k, depth.saturating_add(1)));
//!                 }
//!             }
//!         }
//!     }
//!     Ok(total)
//! }
//!
//! let tree = Node::Branch(vec![Node::Leaf(1), Node::Branch(vec![Node::Leaf(2)])]);
//! let budget = Budget::unlimited();
//! let mut g = budget.guard();
//! assert_eq!(sum(&tree, &mut g).unwrap(), 3);
//! ```
//!
//! Note the two properties that matter: `tick()` runs on **every** iteration, and
//! the depth bound is checked against the item's own depth rather than against a
//! counter maintained by hand.

use selis_error::Result;

use crate::budget::BudgetGuard;

/// An RAII nesting level.
///
/// Entering charges depth; dropping releases it. Use this wherever a scope
/// genuinely maps to a nesting level — a `q`/`Q` pair, an object-stream
/// expansion, a `/Prev` hop — so that an early `return` cannot leak depth.
#[derive(Debug)]
pub struct DepthGuard<'g, 'c> {
    guard: &'g mut BudgetGuard<'c>,
}

impl<'g, 'c> DepthGuard<'g, 'c> {
    /// Enter a nesting level.
    ///
    /// # Errors
    ///
    /// `BUDGET_DEPTH` when the depth limit is reached, at which point the budget
    /// guard is poisoned and no further work is possible.
    pub fn enter(guard: &'g mut BudgetGuard<'c>) -> Result<Self> {
        guard.enter()?;
        Ok(Self { guard })
    }

    /// The budget guard, for charging other resources inside this level.
    #[must_use]
    pub fn guard(&mut self) -> &mut BudgetGuard<'c> {
        self.guard
    }

    /// The current depth.
    #[must_use]
    pub fn depth(&self) -> u16 {
        self.guard.usage().depth
    }
}

impl Drop for DepthGuard<'_, '_> {
    fn drop(&mut self) {
        self.guard.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Budget, Surface};
    use selis_error::Code;

    fn shallow() -> Budget {
        Budget {
            depth: 3,
            ..Budget::unlimited()
        }
    }

    #[test]
    fn depth_is_released_on_drop_including_early_return() {
        let budget = shallow();
        let mut g = budget.guard();

        fn nest(g: &mut BudgetGuard<'_>, remaining: u32) -> Result<u16> {
            let mut d = DepthGuard::enter(g)?;
            let here = d.depth();
            if remaining == 0 {
                // Early return: the guard still releases.
                return Ok(here);
            }
            nest(d.guard(), remaining.saturating_sub(1))
        }

        assert_eq!(nest(&mut g, 2).expect("within depth"), 3);
        assert_eq!(g.usage().depth, 0, "every level was released");
        assert_eq!(g.usage().peak_depth, 3);
    }

    #[test]
    fn exceeding_depth_is_a_typed_error() {
        let budget = shallow();
        let mut g = budget.guard();
        // The DepthGuard owns the first level; its lifetime outlives the
        // direct charges below, so they stack on top of it.
        let mut level = DepthGuard::enter(&mut g).expect("1");
        level.guard().enter().expect("2");
        level.guard().enter().expect("3");
        assert_eq!(
            level.guard().enter().expect_err("4 exceeds").code(),
            Code::BudgetDepth
        );
    }

    /// A ten-thousand-deep `/Prev` chain must terminate, not overflow the stack.
    /// This is the SL-1.ROB.03 case, asserted here at the kernel level.
    #[test]
    fn a_ten_thousand_deep_chain_terminates_in_bounded_depth() {
        let budget = Budget::profile(Surface::Viewer);
        let mut g = budget.guard();
        let mut entered = 0u32;
        // Iterative, exactly as the rule requires: no stack growth at all.
        while g.enter().is_ok() {
            entered = entered.saturating_add(1);
            if entered > 100_000 {
                panic!("depth was unbounded");
            }
        }
        assert_eq!(u64::from(entered), u64::from(budget.depth));
        assert_eq!(g.poisoned_by(), Some(crate::Resource::Depth));
    }
}
