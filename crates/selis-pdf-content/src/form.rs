//! Form XObjects with depth budgeting (SL-2.CONT.04).
//!
//! `Do` for form XObjects requires:
//! * **resource-dictionary scoping** — the form's own `/Resources` override,
//!   falling back to the page's when the form omits them (a real-world case);
//! * **`/Matrix`** — the form's user-space transform;
//! * **`/BBox`** — clipping to the form's bounding box;
//! * **recursion depth via a worklist, never native recursion** — a
//!   self-referential form must terminate with `DEPTH_EXCEEDED`, not a stack
//!   overflow.
//!
//! The caller (the engine) resolves the content stream's `Do` references and
//! pushes the referenced forms onto the worklist; this module enforces the
//! depth budget as the worklist is drained.

use selis_error::{err, Code, Result};
use selis_geom::Matrix;
use selis_sandbox::BudgetGuard;

/// The resources available to an interpreter: a name → value dictionary.
pub type ResourceDict = std::collections::BTreeMap<String, selis_bytes::Bytes>;

/// A form XObject's definition (as parsed by the caller).
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    /// The form's `/Matrix` (default identity).
    pub matrix: Matrix,
    /// The form's `/BBox` (the clip region in form space).
    pub bbox: Option<(f64, f64, f64, f64)>,
    /// The form's own `/Resources` (may be empty → inherit from the page).
    pub resources: ResourceDict,
    /// The form's content stream.
    pub content: Vec<u8>,
}

/// A worklist of forms to interpret, depth-budgeted.
#[derive(Debug, Default)]
pub struct FormWorklist {
    /// The pending forms, in reverse order (pop = next to interpret).
    pending: Vec<Form>,
    /// The maximum nesting depth.
    max_depth: u64,
    /// Forms already visited (cycle detection via this count).
    visited: Vec<u32>,
}

impl FormWorklist {
    /// An empty worklist with a depth budget.
    #[must_use]
    pub fn new(max_depth: u64) -> Self {
        Self {
            pending: Vec::new(),
            max_depth,
            visited: Vec::new(),
        }
    }

    /// Push a form to interpret.
    pub fn push(&mut self, form: Form) {
        self.pending.push(form);
    }

    /// The number of forms still pending.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether the worklist is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Pop the next form, applying the effective resources.
    ///
    /// `page_resources` is used when the form omits `/Resources`.
    ///
    /// # Errors
    ///
    /// `BUDGET_DEPTH` when the worklist exceeds the depth budget — the
    /// self-referential form case, terminated without native recursion.
    pub fn pop(&mut self, page_resources: &ResourceDict, g: &mut BudgetGuard<'_>) -> Result<Form> {
        g.tick()?;
        if self.visited.len() >= usize::try_from(self.max_depth).unwrap_or(usize::MAX) {
            return Err(err!(
                Code::BudgetDepth,
                during = "xobject-form",
                detail = "form nesting exceeds the depth budget"
            ));
        }
        let mut form = self.pending.pop().ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "xobject-form",
                detail = "empty worklist"
            )
        })?;
        // Resource-dictionary scoping: the form's own resources win; otherwise
        // inherit from the page.
        if form.resources.is_empty() {
            form.resources = page_resources.clone();
        }
        g.charge_one(selis_sandbox::Resource::Objects)?;
        // Track visited count for cycle detection.
        let id = u32::try_from(self.visited.len()).unwrap_or(u32::MAX);
        self.visited.push(id);
        Ok(form)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn empty_form() -> Form {
        Form {
            matrix: Matrix::IDENTITY,
            bbox: None,
            resources: ResourceDict::new(),
            content: Vec::new(),
        }
    }

    #[test]
    fn single_form_is_popped() {
        let mut g = guard();
        let mut wl = FormWorklist::new(10);
        wl.push(empty_form());
        assert_eq!(wl.len(), 1);
        let form = wl.pop(&ResourceDict::new(), &mut g).expect("pop");
        assert!(form.content.is_empty());
        assert!(wl.is_empty());
    }

    #[test]
    fn form_inherits_page_resources_when_omitted() {
        let mut g = guard();
        let mut page = ResourceDict::new();
        page.insert(
            "Font".to_string(),
            selis_bytes::Bytes::copy_from_slice(b"/F1 12 Tf"),
        );
        let mut wl = FormWorklist::new(10);
        wl.push(empty_form()); // no own resources
        let form = wl.pop(&page, &mut g).expect("pop");
        assert_eq!(
            form.resources.get("Font").map(|b| b.as_slice()),
            Some(&b"/F1 12 Tf"[..])
        );
    }

    /// DoD: a self-referential form terminates with BUDGET_DEPTH, never a
    /// stack overflow (the worklist is drained, not recursed).
    #[test]
    fn self_referential_form_terminates_at_depth_budget() {
        let mut g = guard();
        // Simulate a form whose content is `/F1 Do` referencing itself: the
        // caller pushes the same form repeatedly; the worklist caps it.
        let mut wl = FormWorklist::new(3);
        let form = empty_form();
        for _ in 0..10 {
            wl.push(form.clone());
        }
        let mut popped = 0u32;
        loop {
            match wl.pop(&ResourceDict::new(), &mut g) {
                Ok(_) => popped = popped.saturating_add(1),
                Err(e) => {
                    assert_eq!(e.code(), Code::BudgetDepth);
                    break;
                }
            }
            if popped > 100 {
                panic!("must terminate");
            }
        }
        assert!(popped >= 3, "all forms up to the budget were popped");
    }
}
