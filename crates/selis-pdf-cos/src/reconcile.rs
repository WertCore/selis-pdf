//! Cross-document reconciliation for merge (SL-1A.WRITE.04).
//!
//! When N documents become one, their document-level structures must be
//! reconciled, not concatenated blindly: outlines, named destinations, page
//! labels, form fields, structure trees. This module provides the primitives:
//!
//! * [`copy_value_into`] — deep-copies an object from a source document into
//!   the builder, remapping page references at the merged pages (so a
//!   bookmark's destination points at the merged page, not a fresh copy of
//!   the source page) and terminating on cycles.
//! * [`number_tree_pairs`] / [`name_tree_pairs`] — flatten PDF's number and
//!   name trees into (key, value) pairs so a merged tree can be rebuilt.
//! * [`merge_number_trees`] — rebuild one number tree from several, with each
//!   input's integer keys shifted by its page offset (page labels).

use std::collections::HashMap;

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::copy::resolve_ref;
use crate::doc_writer::DocumentBuilder;
use crate::revision::Doc;
use crate::{Obj, Ref};

/// Deep-copy `obj` from the source document into `builder`, returning the
/// object that stands in its place in the merged document.
///
/// * References whose target is a page in `page_map` are redirected at the
///   merged page instead of being copied — destinations in outlines and
///   name trees must land on the merged page, not a duplicate of it.
/// * Every other reference is resolved, copied recursively, and renumbered;
///   `cache` records source object → merged number so shared objects are
///   copied once and cycles terminate.
/// * Stream payloads are carried verbatim; dictionaries and arrays are
///   walked.
///
/// Copied objects are appended to `builder` as they are created.
///
/// # Budget
///
/// Each resolved object charges the budget; bounded by the reachable graph.
///
/// # Malformed Input
///
/// `OBJ_UNEXPECTED` when a referenced object cannot be resolved.
pub fn copy_value_into(
    builder: &mut DocumentBuilder,
    src: &[u8],
    doc: &Doc,
    page_map: &HashMap<u32, Ref>,
    cache: &mut HashMap<(u32, u16), u32>,
    obj: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    match obj {
        Obj::Ref(r) => {
            if let Some(merged_page) = page_map.get(&r.num) {
                return Ok(Obj::Ref(*merged_page));
            }
            if let Some(&num) = cache.get(&(r.num, r.gen)) {
                return Ok(Obj::Ref(Ref::new(num, 0)));
            }
            let resolved = resolve_ref(src, doc, *r, budget, g)?;
            // Register the number BEFORE recursing so self-references and
            // cycles resolve to it.
            let num = builder.allocate();
            cache.insert((r.num, r.gen), num);
            let copied = copy_value_into(builder, src, doc, page_map, cache, &resolved, budget, g)?;
            builder.add_object(num, copied);
            Ok(Obj::Ref(Ref::new(num, 0)))
        }
        Obj::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(copy_value_into(
                    builder, src, doc, page_map, cache, item, budget, g,
                )?);
            }
            Ok(Obj::Array(out))
        }
        Obj::Dict(pairs) => {
            let mut out = Vec::with_capacity(pairs.len());
            for (k, v) in pairs {
                out.push((
                    k.clone(),
                    copy_value_into(builder, src, doc, page_map, cache, v, budget, g)?,
                ));
            }
            Ok(Obj::Dict(out))
        }
        Obj::Stream { dict, data } => {
            let mut out = Vec::with_capacity(dict.len());
            for (k, v) in dict {
                out.push((
                    k.clone(),
                    copy_value_into(builder, src, doc, page_map, cache, v, budget, g)?,
                ));
            }
            Ok(Obj::Stream {
                dict: out,
                data: data.clone(),
            })
        }
        other => Ok(other.clone()),
    }
}

/// Flatten a number tree (`/Nums` leaves, `/Kids` intermediates) into
/// (key, value) pairs. Values that are references are resolved against the
/// source document.
///
/// # Errors
///
/// `OBJ_UNEXPECTED` when a referenced value cannot be resolved;
/// `OBJSTM_MALFORMED` shapes are not expected in well-formed trees.
pub fn number_tree_pairs(
    root: &Obj,
    src: &[u8],
    doc: &Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<(i64, Obj)>> {
    let mut out = Vec::new();
    number_tree_walk(root, src, doc, budget, g, &mut out)?;
    Ok(out)
}

fn number_tree_walk(
    node: &Obj,
    src: &[u8],
    doc: &Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    out: &mut Vec<(i64, Obj)>,
) -> Result<()> {
    // Descend through indirect nodes.
    let node: Obj = match node {
        Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g)?,
        other => other.clone(),
    };
    let Obj::Dict(pairs) = &node else {
        return Ok(());
    };
    for (k, v) in pairs {
        if k.as_slice() == b"Nums" {
            if let Obj::Array(items) = v {
                let mut it = items.iter();
                while let Some(key) = it.next() {
                    let Some(val) = it.next() else { break };
                    if let Obj::Int(n) = key {
                        let val = match val {
                            Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g)?,
                            other => other.clone(),
                        };
                        out.push((*n, val));
                    }
                }
            }
        } else if k.as_slice() == b"Kids" {
            if let Obj::Array(items) = v {
                for kid in items {
                    number_tree_walk(kid, src, doc, budget, g, out)?;
                }
            }
        }
    }
    Ok(())
}

/// Flatten a name tree (`/Names` leaves, `/Kids` intermediates) into
/// (name, value) pairs. Values that are references are resolved against the
/// source document.
///
/// # Errors
///
/// `OBJ_UNEXPECTED` when a referenced value cannot be resolved.
pub fn name_tree_pairs(
    root: &Obj,
    src: &[u8],
    doc: &Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<(Bytes, Obj)>> {
    let mut out = Vec::new();
    name_tree_walk(root, src, doc, budget, g, &mut out)?;
    Ok(out)
}

fn name_tree_walk(
    node: &Obj,
    src: &[u8],
    doc: &Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    out: &mut Vec<(Bytes, Obj)>,
) -> Result<()> {
    let node: Obj = match node {
        Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g)?,
        other => other.clone(),
    };
    let Obj::Dict(pairs) = &node else {
        return Ok(());
    };
    for (k, v) in pairs {
        if k.as_slice() == b"Names" {
            if let Obj::Array(items) = v {
                let mut it = items.iter();
                while let Some(key) = it.next() {
                    let Some(val) = it.next() else { break };
                    if let Obj::String(name) = key {
                        let val = match val {
                            Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g)?,
                            other => other.clone(),
                        };
                        out.push((name.clone(), val));
                    }
                }
            }
        } else if k.as_slice() == b"Kids" {
            if let Obj::Array(items) = v {
                for kid in items {
                    name_tree_walk(kid, src, doc, budget, g, out)?;
                }
            }
        }
    }
    Ok(())
}

/// Flatten a number tree into (key, value) pairs without resolving values:
/// references stay references, so copying them through
/// [`copy_value_into`] with a cache shared with an already-copied subgraph
/// redirects them at the survivors. The structure tree's `/ParentTree`
/// values must land on the same merged elements its `/K` subtree produced.
///
/// # Budget
///
/// Each tree node descended charges the budget; bounded by the tree size.
///
/// # Malformed Input
///
/// `OBJ_UNEXPECTED` when a node reference cannot be resolved; odd-length
/// `/Nums` arrays are truncated at the dangling key.
pub fn number_tree_pairs_unresolved(
    root: &Obj,
    src: &[u8],
    doc: &Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<(i64, Obj)>> {
    let mut out = Vec::new();
    number_tree_walk_unresolved(root, src, doc, budget, g, &mut out)?;
    Ok(out)
}

fn number_tree_walk_unresolved(
    node: &Obj,
    src: &[u8],
    doc: &Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    out: &mut Vec<(i64, Obj)>,
) -> Result<()> {
    let node: Obj = match node {
        Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g)?,
        other => other.clone(),
    };
    let Obj::Dict(pairs) = &node else {
        return Ok(());
    };
    for (k, v) in pairs {
        if k.as_slice() == b"Nums" {
            if let Obj::Array(items) = v {
                let mut it = items.iter();
                while let Some(key) = it.next() {
                    let Some(val) = it.next() else { break };
                    if let Obj::Int(n) = key {
                        out.push((*n, val.clone()));
                    }
                }
            }
        } else if k.as_slice() == b"Kids" {
            if let Obj::Array(items) = v {
                for kid in items {
                    number_tree_walk_unresolved(kid, src, doc, budget, g, out)?;
                }
            }
        }
    }
    Ok(())
}

/// Merge several number trees into one flat tree, shifting each input's keys
/// by its `offset`. Used for `/PageLabels`: input *i*'s page indices shift by
/// the number of pages merged before it. Inputs are (tree root, page offset).
///
/// The result is a single leaf node `<< /Nums [...] >>` with sorted keys.
#[must_use]
pub fn merge_number_trees(inputs: &[(Vec<(i64, Obj)>, i64)]) -> Obj {
    let mut all: Vec<(i64, Obj)> = Vec::new();
    for (pairs, offset) in inputs {
        for (k, v) in pairs {
            all.push((k.saturating_add(*offset), v.clone()));
        }
    }
    all.sort_by(|a, b| a.0.cmp(&b.0));
    let mut nums: Vec<Obj> = Vec::with_capacity(all.len().saturating_mul(2));
    for (k, v) in all {
        nums.push(Obj::Int(k));
        nums.push(v);
    }
    Obj::Dict(vec![(Bytes::copy_from_slice(b"Nums"), Obj::Array(nums))])
}

/// Build a flat single-leaf name tree from (name, value) pairs:
/// `<< /Names [...] >>` sorted lexicographically by name.
#[must_use]
pub fn build_name_tree(mut pairs: Vec<(Bytes, Obj)>) -> Obj {
    pairs.sort_by(|a, b| a.0.as_slice().cmp(b.0.as_slice()));
    let mut names: Vec<Obj> = Vec::with_capacity(pairs.len().saturating_mul(2));
    for (k, v) in pairs {
        names.push(Obj::String(k));
        names.push(v);
    }
    Obj::Dict(vec![(Bytes::copy_from_slice(b"Names"), Obj::Array(names))])
}

/// The catalog entry (`key`) of the document's catalog object, when present.
///
/// # Errors
///
/// `OBJ_UNEXPECTED` when the catalog cannot be resolved.
pub fn catalog_entry(
    src: &[u8],
    doc: &Doc,
    key: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Obj>> {
    let root = doc
        .revisions()
        .iter()
        .rev()
        .find_map(|rev| rev.root)
        .ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "reconcile",
                detail = "no root"
            )
        })?;
    let catalog = resolve_ref(src, doc, root, budget, g)?;
    let Obj::Dict(pairs) = catalog else {
        return Ok(None);
    };
    Ok(pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v.clone()))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    /// A one-page PDF with a catalog carrying /Outlines and /PageLabels:
    /// catalog(1) → pages(2) → page(3), content(4), outlines root(5) with one
    /// item(6) destinating the page, labels tree(7).
    fn outlined_source() -> Vec<u8> {
        let numbered: &[(u32, &[u8])] = &[
            (
                1,
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /PageLabels 7 0 R >>\nendobj\n",
            ),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n"),
            (
                5,
                b"5 0 obj\n<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>\nendobj\n",
            ),
            (
                6,
                b"6 0 obj\n<< /Title (Chap 1) /Parent 5 0 R /Dest [3 0 R /Fit] >>\nendobj\n",
            ),
            (
                7,
                b"7 0 obj\n<< /Nums [0 << /S /r >>] >>\nendobj\n",
            ),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets: Vec<(u32, u64)> = Vec::new();
        for (num, body) in numbered {
            offsets.push((*num, out.len() as u64));
            out.extend_from_slice(body);
        }
        offsets.sort_by_key(|(num, _)| *num);
        let xref_at = out.len() as u64;
        out.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        for (_, off) in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n");
        out.extend_from_slice(format!("{xref_at}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");
        out
    }

    fn open(src: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Doc {
        let startxref = crate::xref::find_startxref(src, 4096).unwrap_or(0);
        crate::parse_revisions(src, startxref, budget, g).expect("open")
    }

    /// copy_value_into redirects page refs at the mapped merged page and
    /// copies everything else exactly once.
    #[test]
    fn copy_remaps_page_refs() {
        let src = outlined_source();
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let doc = open(&src, &budget, &mut g);
        let mut builder = DocumentBuilder::new();
        let mut page_map = HashMap::new();
        page_map.insert(3u32, Ref::new(99, 0));
        let mut cache = HashMap::new();
        // Copy the outline item by reference: its /Dest page ref must land
        // on the mapped merged page (99).
        let copied = copy_value_into(
            &mut builder,
            &src,
            &doc,
            &page_map,
            &mut cache,
            &Obj::Ref(Ref::new(6, 0)),
            &budget,
            &mut g,
        )
        .expect("copy");
        let merged_ref = match copied {
            Obj::Ref(r) => r,
            _ => panic!("outline item copies as a ref"),
        };
        let _ = merged_ref;
        let bytes = builder.write(&budget, &mut g).expect("write");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("99 0 R"),
            "destination remapped to merged page: {text}"
        );
        assert!(text.contains("(Chap 1)"), "title carried");
    }

    /// Number-tree pairs flatten /Nums leaves and merge with offsets.
    #[test]
    fn number_trees_merge_with_offsets() {
        let src = outlined_source();
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let doc = open(&src, &budget, &mut g);
        let labels = catalog_entry(&src, &doc, b"PageLabels", &budget, &mut g)
            .expect("entry")
            .expect("labels present");
        let pairs = number_tree_pairs(&labels, &src, &doc, &budget, &mut g).expect("pairs");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, 0);
        // Merging two inputs offsets the second by one page.
        let merged = merge_number_trees(&[(pairs.clone(), 0), (pairs, 1)]);
        let Obj::Dict(pairs) = &merged else {
            panic!("dict")
        };
        let Some((_, Obj::Array(nums))) = pairs.iter().find(|(k, _)| k.as_slice() == b"Nums")
        else {
            panic!("nums");
        };
        let keys: Vec<i64> = nums
            .iter()
            .step_by(2)
            .filter_map(|o| match o {
                Obj::Int(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec![0, 1], "keys shifted by page offset");
    }
}
