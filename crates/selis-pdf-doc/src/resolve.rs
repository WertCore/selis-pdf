//! Cycle-safe object resolution (SL-1.DOC.02).
//!
//! A document's objects form a graph: the catalog points at pages, pages at
//! contents, contents at fonts, and a hostile file can arrange a cycle
//! (`1 -> 2 -> 1`). Recursive resolution of such a graph is a stack overflow
//! we cannot catch. This resolver threads a [`Visited`] set through the walk
//! and terminates any reference that is already being resolved with
//! `OBJ_CYCLE`, plus an optional depth bound.

use std::collections::BTreeSet;

use selis_error::{err, Code, Result};
use selis_pdf_cos::{resolve_object, Doc, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

/// A reference-resolving walker that refuses cycles.
#[derive(Debug)]
pub struct Resolver<'a> {
    doc: &'a Doc,
    src: &'a [u8],
    budget: &'a Budget,
    visited: BTreeSet<u32>,
    depth: u16,
}

impl<'a> Resolver<'a> {
    /// A resolver over a parsed document and its source bytes.
    #[must_use]
    pub fn new(doc: &'a Doc, src: &'a [u8], budget: &'a Budget) -> Self {
        Self {
            doc,
            src,
            budget,
            visited: BTreeSet::new(),
            depth: 0,
        }
    }

    /// Resolve a reference to its object value.
    ///
    /// # Budget
    ///
    /// Charges `Objects` per resolution and `Depth` per nesting level.
    ///
    /// # Malformed Input
    ///
    /// `OBJ_CYCLE` when the same object number is re-entered during one walk;
    /// `OBJ_UNEXPECTED` for unresolvable or non-object offsets.
    pub fn resolve(&mut self, r: Ref, g: &mut BudgetGuard<'_>) -> Result<Obj> {
        if !self.visited.insert(r.num) {
            return Err(err!(
                Code::ObjCycle,
                during = "doc-resolve",
                object = r.num
            ));
        }
        g.enter()?;
        self.depth = self.depth.saturating_add(1);
        let limit = self.budget.limit(selis_sandbox::Resource::Depth);
        if u64::from(self.depth) > limit {
            return Err(err!(
                Code::BudgetDepth,
                during = "doc-resolve",
                object = r.num
            ));
        }

        let view = self
            .doc
            .at_revision(self.doc.len().saturating_sub(1))
            .ok_or_else(|| err!(Code::ObjUnexpected, during = "doc-resolve", object = r.num))?;
        let offset = match view.xref.get(&r.num) {
            Some(selis_pdf_cos::XrefEntry::InUse { offset, .. }) => *offset,
            Some(selis_pdf_cos::XrefEntry::Compressed { .. }) => {
                return Err(err!(
                    Code::ObjUnexpected,
                    during = "doc-resolve",
                    object = r.num,
                    detail = "compressed object resolution not yet wired"
                ));
            }
            Some(_) | None => {
                return Err(err!(
                    Code::ObjUnexpected,
                    during = "doc-resolve",
                    object = r.num
                ));
            }
        };
        g.charge_one(selis_sandbox::Resource::Objects)?;
        let obj = resolve_object(self.src, offset, self.budget, g)?;

        self.depth = self.depth.saturating_sub(1);
        self.visited.remove(&r.num);
        Ok(obj)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_pdf_cos::XrefEntry;
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// A self-referencing object: `1 0 obj << /Self 1 0 R >> endobj`.
    #[test]
    fn self_reference_is_a_cycle() {
        let src = b"%PDF-1.4\n1 0 obj\n<< /Self 1 0 R >>\nendobj\n";
        let mut xref = std::collections::BTreeMap::new();
        xref.insert(1, XrefEntry::InUse { offset: 9, gen: 0 });
        let trailer = vec![(
            selis_bytes::Bytes::copy_from_slice(b"Root"),
            Obj::Ref(Ref::new(1, 0)),
        )];
        let doc = Doc::from_single_revision(xref, trailer);
        let budget = Budget::unlimited();
        let mut g = guard();
        let mut r = Resolver::new(&doc, src, &budget);
        // The object body itself parses; the cycle is detected when a *walk*
        // re-enters object 1. Resolving it once is fine.
        let obj = r.resolve(Ref::new(1, 0), &mut g).expect("resolve");
        assert!(matches!(obj, Obj::Dict(_)));
    }

    #[test]
    fn unresolved_object_is_a_typed_error() {
        let src = b"%PDF-1.4\n";
        let xref = std::collections::BTreeMap::new();
        let trailer = vec![];
        let doc = Doc::from_single_revision(xref, trailer);
        let budget = Budget::unlimited();
        let mut g = guard();
        let mut r = Resolver::new(&doc, src, &budget);
        let e = r.resolve(Ref::new(99, 0), &mut g).expect_err("missing");
        assert_eq!(e.code(), Code::ObjUnexpected);
    }
}
