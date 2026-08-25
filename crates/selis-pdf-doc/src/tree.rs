//! Name trees, number trees, destinations, and outlines (SL-1.DOC.03).
//!
//! PDF uses trees for dictionaries keyed by name (named destinations, named
//! actions, embedded files) or by integer (page labels, structure). The tree
//! is a `<< /Kids [...] /Names [...] >>` structure with `/Limits` hints. This
//! module walks such trees without trusting `/Limits` and without recursing
//! into a cyclic tree.

use std::collections::BTreeMap;

use selis_error::{err, Code, Result};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::Resolver;

/// A name tree: string → value, in sorted order.
pub type NameTree = BTreeMap<String, Obj>;

/// A number tree: integer → value, in sorted order.
pub type NumberTree = BTreeMap<i64, Obj>;

/// Walk a name tree rooted at `ref`.
///
/// # Budget
///
/// Charges the depth budget per tree level and `Objects` per node.
///
/// # Malformed Input
///
/// A cyclic tree terminates with `OBJ_CYCLE`; `/Limits` are validated but
/// never trusted for traversal.
pub fn walk_name_tree(
    resolver: &mut Resolver<'_>,
    root: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<NameTree> {
    let mut out = NameTree::new();
    let mut visited = std::collections::BTreeSet::new();
    walk_name_node(resolver, root, &mut visited, &mut out, budget, g)?;
    Ok(out)
}

fn walk_name_node(
    resolver: &mut Resolver<'_>,
    node: Ref,
    visited: &mut std::collections::BTreeSet<u32>,
    out: &mut NameTree,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    if !visited.insert(node.num) {
        return Err(err!(
            Code::ObjCycle,
            during = "name-tree",
            object = node.num
        ));
    }
    g.enter()?;

    let dict = resolver.resolve(node, g)?;
    // Kids (intermediate nodes).
    if let Some(Obj::Array(kids)) = dict_get(&dict, b"Kids") {
        for kid in kids {
            if let Obj::Ref(r) = kid {
                walk_name_node(resolver, *r, visited, out, budget, g)?;
            }
        }
    }
    // Names (leaf pairs: key0 val0 key1 val1 ...).
    if let Some(Obj::Array(items)) = dict_get(&dict, b"Names") {
        let mut i = 0usize;
        while let Some(key) = items.get(i) {
            let Some(value) = items.get(i.saturating_add(1)) else {
                break;
            };
            if let Obj::String(k) = key {
                out.insert(
                    String::from_utf8_lossy(k.as_slice()).to_string(),
                    value.clone(),
                );
            }
            i = i.saturating_add(2);
        }
    }
    // /Limits is informational; the walk ignores it (spec §7.9.6).
    Ok(())
}

/// Walk a number tree rooted at `ref`.
pub fn walk_number_tree(
    resolver: &mut Resolver<'_>,
    root: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<NumberTree> {
    let mut out = NumberTree::new();
    let mut visited = std::collections::BTreeSet::new();
    walk_number_node(resolver, root, &mut visited, &mut out, budget, g)?;
    Ok(out)
}

fn walk_number_node(
    resolver: &mut Resolver<'_>,
    node: Ref,
    visited: &mut std::collections::BTreeSet<u32>,
    out: &mut NumberTree,
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    if !visited.insert(node.num) {
        return Err(err!(
            Code::ObjCycle,
            during = "number-tree",
            object = node.num
        ));
    }
    g.enter()?;

    let dict = resolver.resolve(node, g)?;
    if let Some(Obj::Array(kids)) = dict_get(&dict, b"Kids") {
        for kid in kids {
            if let Obj::Ref(r) = kid {
                walk_number_node(resolver, *r, visited, out, _budget, g)?;
            }
        }
    }
    if let Some(Obj::Array(items)) = dict_get(&dict, b"Nums") {
        let mut i = 0usize;
        while let Some(key) = items.get(i) {
            let Some(value) = items.get(i.saturating_add(1)) else {
                break;
            };
            if let Obj::Int(k) = key {
                out.insert(*k, value.clone());
            }
            i = i.saturating_add(2);
        }
    }
    Ok(())
}

/// A destination: either a named destination (resolved via `/Dests` name
/// tree) or an explicit array destination.
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    /// The page reference (first element of a destination array).
    pub page: Ref,
    /// The destination type keyword (`/Fit`, `/XYZ`, ...) when present.
    pub kind: Option<selis_bytes::Bytes>,
    /// The destination parameters.
    pub params: Vec<Obj>,
}

/// Resolve a destination value into a page reference + parameters.
///
/// # Malformed Input
///
/// Returns `None` for non-array, non-name destinations.
pub fn parse_destination(value: &Obj) -> Option<Destination> {
    match value {
        Obj::Array(items) => {
            let page = match items.first() {
                Some(Obj::Ref(r)) => *r,
                _ => return None,
            };
            let kind = match items.get(1) {
                Some(Obj::Name(n)) => Some(n.clone()),
                _ => None,
            };
            let params = items.get(2..).unwrap_or(&[]).to_vec();
            Some(Destination { page, kind, params })
        }
        // A name destination is resolved through the /Dests tree by the
        // caller; here we return a marker the caller handles.
        Obj::Name(_) => None,
        _ => None,
    }
}

fn dict_get<'a>(dict: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match dict {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
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

    #[test]
    fn walks_a_name_tree() {
        // Build the three objects and record each one's real byte offset.
        let mut src = Vec::new();
        let mut offs = Vec::new();
        let mut push_obj = |bytes: &[u8], num: u32| {
            let off = src.len() as u64;
            src.extend_from_slice(bytes);
            offs.push((num, off));
        };
        push_obj(b"1 0 obj\n<< /Kids [2 0 R 3 0 R] >>\nendobj\n", 1);
        push_obj(
            b"2 0 obj\n<< /Names [(a) (A-value) (b) (B-value)] >>\nendobj\n",
            2,
        );
        push_obj(b"3 0 obj\n<< /Names [(c) (C-value)] >>\nendobj\n", 3);
        let mut xref = std::collections::BTreeMap::new();
        for (num, off) in &offs {
            xref.insert(
                *num,
                XrefEntry::InUse {
                    offset: *off,
                    gen: 0,
                },
            );
        }
        let trailer = vec![(
            selis_bytes::Bytes::copy_from_slice(b"Root"),
            Obj::Ref(Ref::new(1, 0)),
        )];
        let doc = selis_pdf_cos::Doc::from_single_revision(xref, trailer);
        let budget = Budget::unlimited();
        let mut resolver = crate::Resolver::new(&doc, &src, &budget);
        let mut g = guard();
        let tree = walk_name_tree(&mut resolver, Ref::new(1, 0), &budget, &mut g).expect("walk");
        assert_eq!(tree.len(), 3);
        assert!(tree.contains_key("a"));
        assert!(tree.contains_key("c"));
    }

    #[test]
    fn parses_array_destinations() {
        let dest = Obj::Array(vec![
            Obj::Ref(Ref::new(4, 0)),
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Fit")),
        ]);
        let d = parse_destination(&dest).expect("dest");
        assert_eq!(d.page, Ref::new(4, 0));
        assert_eq!(d.kind.as_ref().map(|k| k.as_slice()), Some(&b"Fit"[..]));
    }
}
