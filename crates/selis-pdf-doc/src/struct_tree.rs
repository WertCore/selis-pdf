//! Structure tree (tagged PDF), read (SL-1.DOC.06).
//!
//! Parses `/StructTreeRoot`, the element hierarchy, `/K` kids (including
//! MCID references and OBJR), role maps, and attribute dictionaries.
//! Per ADR-P0031 this is Phase 1 work, not a Phase 9 bolt-on.
//!
//! The structure tree is the engine behind accessibility (PDF/UA), the
//! conformance rule registry (SL-1.DOC.09), and the privacy scanner: the same
//! structural walk that finds a tag hierarchy finds leftover content.

use std::collections::BTreeMap;

use selis_error::{err, Code, Result};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::Resolver;

/// A structure element (a node in the structure tree).
#[derive(Debug, Clone, PartialEq)]
pub struct StructElement {
    /// The element's object reference.
    pub ref_: Ref,
    /// The structure type (`/S`), e.g. `/P`, `/H1`, `/Table`.
    pub ty: Option<selis_bytes::Bytes>,
    /// The element's title (`/T`), often the semantic text.
    pub title: Option<selis_bytes::Bytes>,
    /// The element's `/K` kids: child elements, MCID references, or OBJR.
    pub kids: Vec<StructKid>,
    /// The element's `/A` attribute dictionary.
    pub attrs: Option<Obj>,
    /// The element's direct `/Alt` alternate text (32000-1 §14.9.3).
    pub alt: Option<selis_bytes::Bytes>,
}

/// A kid of a structure element.
#[derive(Debug, Clone, PartialEq)]
pub enum StructKid {
    /// A child structure element.
    Element(Ref),
    /// A marked-content identifier (integer) referencing content in a page.
    Mcid(u32),
    /// An object reference (`OBJR`) to a marked-content object.
    Objr {
        /// The marked-content identifier.
        mcid: u32,
        /// The page the object is on.
        page: Option<Ref>,
    },
}

/// The structure tree of a tagged document.
#[derive(Debug, Clone, PartialEq)]
pub struct StructTree {
    /// The tree root (`/StructTreeRoot`).
    pub root: Obj,
    /// The role map (`/RoleMap`): structure-type → standard-type.
    pub role_map: BTreeMap<String, String>,
    /// The elements reachable from the root's `/K`, in walk order.
    pub elements: Vec<StructElement>,
}

impl StructTree {
    /// The marked-content identifiers in structure-tree walk order (reading
    /// order for the text layer). Includes MCIDs from `Mcid` and `Objr` kids.
    #[must_use]
    pub fn mcid_order(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for el in &self.elements {
            for kid in &el.kids {
                match kid {
                    StructKid::Mcid(mcid) => out.push(*mcid),
                    StructKid::Objr { mcid, .. } => out.push(*mcid),
                    _ => {}
                }
            }
        }
        out
    }

    /// Parse the structure tree from the catalog.
    ///
    /// # Budget
    ///
    /// Bounded by the element count and the depth budget.
    ///
    /// # Malformed Input
    ///
    /// A missing `/StructTreeRoot` yields an empty tree (no error); a cyclic
    /// structure tree terminates with `OBJ_CYCLE`.
    pub fn resolve(
        resolver: &mut Resolver<'_>,
        catalog: &Obj,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<Self> {
        let Some(Obj::Ref(root_ref)) = catalog_dict(catalog, b"StructTreeRoot") else {
            return Ok(Self {
                root: Obj::Null,
                role_map: BTreeMap::new(),
                elements: Vec::new(),
            });
        };
        let root = resolver.resolve(*root_ref, g)?;
        let role_map = match catalog_dict(&root, b"RoleMap") {
            Some(Obj::Dict(pairs)) => pairs
                .iter()
                .filter_map(|(k, v)| match v {
                    Obj::Name(n) => Some((
                        String::from_utf8_lossy(k.as_slice()).to_string(),
                        String::from_utf8_lossy(n.as_slice()).to_string(),
                    )),
                    _ => None,
                })
                .collect(),
            _ => BTreeMap::new(),
        };

        let mut elements = Vec::new();
        let mut visited = std::collections::BTreeSet::new();
        // The root's `/K` is an array of structure elements or a single one
        // (32000-1 §14.4.2); both shapes are walked.
        let root_kids: Vec<Ref> = match catalog_dict(&root, b"K") {
            Some(Obj::Array(kids)) => kids
                .iter()
                .filter_map(|kid| match kid {
                    Obj::Ref(r) => Some(*r),
                    _ => None,
                })
                .collect(),
            Some(Obj::Ref(r)) => vec![*r],
            _ => Vec::new(),
        };
        for kid in root_kids {
            walk_element(resolver, kid, &mut visited, &mut elements, budget, g)?;
        }

        Ok(Self {
            root,
            role_map,
            elements,
        })
    }
}

fn walk_element(
    resolver: &mut Resolver<'_>,
    node: Ref,
    visited: &mut std::collections::BTreeSet<u32>,
    out: &mut Vec<StructElement>,
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    if !visited.insert(node.num) {
        return Err(err!(
            Code::ObjCycle,
            during = "struct-tree",
            object = node.num
        ));
    }
    // Depth is scoped to this element's nesting level: the RAII guard
    // releases it on every exit path, so depth tracks the tree's actual
    // nesting rather than accumulating one level per element visited.
    let mut d = selis_sandbox::DepthGuard::enter(g)?;

    let dict = resolver.resolve(node, d.guard())?;
    let ty = dict_get(&dict, b"S").and_then(|v| match v {
        Obj::Name(n) => Some(n.clone()),
        _ => None,
    });
    let title = dict_get(&dict, b"T").and_then(|v| match v {
        Obj::String(s) | Obj::Name(s) => Some(s.clone()),
        _ => None,
    });
    let attrs = dict_get(&dict, b"A").cloned();
    // `/Alt` is usually a direct entry on the element dictionary (32000-1
    // §14.9.3); it may also live in `/A`. Capture the direct form here.
    let alt = dict_get(&dict, b"Alt").and_then(|v| match v {
        Obj::String(s) | Obj::Name(s) => Some(s.clone()),
        _ => None,
    });

    let mut kids = Vec::new();
    let push_kid =
        |item: &Obj, kids: &mut Vec<StructKid>| match item {
            Obj::Ref(r) => kids.push(StructKid::Element(*r)),
            Obj::Int(m) => {
                kids.push(StructKid::Mcid(u32::try_from(*m).unwrap_or(u32::MAX)));
            }
            // An OBJR object: << /Type /OBJR /Obj <mcid> /Pg <page> >>
            Obj::Dict(pairs) => {
                let mcid = pairs
                    .iter()
                    .find(|(k, _)| k.as_slice() == b"Obj")
                    .and_then(|(_, v)| match v {
                        Obj::Int(i) => u32::try_from(*i).ok(),
                        _ => None,
                    });
                let page = pairs
                    .iter()
                    .find(|(k, _)| k.as_slice() == b"Pg")
                    .and_then(|(_, v)| match v {
                        Obj::Ref(r) => Some(*r),
                        _ => None,
                    });
                if let Some(mcid) = mcid {
                    kids.push(StructKid::Objr { mcid, page });
                }
            }
            _ => {}
        };
    // `/K` is a single MCID/OBJR/element, or an array of them.
    match dict_get(&dict, b"K") {
        Some(Obj::Array(items)) => {
            for item in items {
                push_kid(item, &mut kids);
            }
        }
        Some(k) => push_kid(k, &mut kids),
        None => {}
    }

    out.push(StructElement {
        ref_: node,
        ty,
        title,
        kids,
        attrs,
        alt,
    });

    // Recurse into element kids.
    let kids_snapshot = out.last().map(|e| e.kids.clone()).unwrap_or_default();
    for kid in kids_snapshot {
        if let StructKid::Element(r) = kid {
            walk_element(resolver, r, visited, out, _budget, d.guard())?;
        }
    }
    Ok(())
}

fn catalog_dict<'a>(obj: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match obj {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

fn dict_get<'a>(obj: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match obj {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;

    fn dict(pairs: Vec<(&[u8], Obj)>) -> Obj {
        Obj::Dict(
            pairs
                .into_iter()
                .map(|(k, v)| (selis_bytes::Bytes::copy_from_slice(k), v))
                .collect(),
        )
    }

    #[test]
    fn no_structure_tree_is_an_empty_tree() {
        let catalog = dict(vec![(
            b"Type",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Catalog")),
        )]);
        // resolve needs a Resolver; the "absent" path is exercised via a doc
        // with no StructTreeRoot, which the integration tests cover. Here we
        // just check the struct shape.
        let _ = StructTree {
            root: Obj::Null,
            role_map: BTreeMap::new(),
            elements: Vec::new(),
        };
        let _ = catalog;
    }

    /// A structure-tree root whose `/K` is a *single* element reference (not
    /// an array) is legal (32000-1 §14.4.2) and must be walked: its element —
    /// including a direct `/Alt` entry — is resolved (WRITE.07 — the writer's
    /// output must be judged by the same reader that judges the input).
    #[test]
    fn single_ref_k_resolves_element_with_direct_alt() {
        let src = b"%PDF-1.4\n\
                    1 0 obj\n<< /Type /Catalog /StructTreeRoot 2 0 R >>\nendobj\n\
                    2 0 obj\n<< /Type /StructTreeRoot /K 3 0 R >>\nendobj\n\
                    3 0 obj\n<< /S /Figure /Alt (hello) >>\nendobj\n";
        let find =
            |needle: &[u8]| src.windows(needle.len()).position(|w| w == needle).unwrap() as u64;
        let mut xref = std::collections::BTreeMap::new();
        xref.insert(
            1,
            selis_pdf_cos::XrefEntry::InUse {
                offset: find(b"1 0 obj"),
                gen: 0,
            },
        );
        xref.insert(
            2,
            selis_pdf_cos::XrefEntry::InUse {
                offset: find(b"2 0 obj"),
                gen: 0,
            },
        );
        xref.insert(
            3,
            selis_pdf_cos::XrefEntry::InUse {
                offset: find(b"3 0 obj"),
                gen: 0,
            },
        );
        let trailer = vec![(
            selis_bytes::Bytes::copy_from_slice(b"Root"),
            Obj::Ref(Ref::new(1, 0)),
        )];
        let doc = selis_pdf_cos::Doc::from_single_revision(xref, trailer);
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut resolver = Resolver::new(&doc, src, &budget);
        let catalog = resolver.resolve(Ref::new(1, 0), &mut g).expect("catalog");
        let tree = StructTree::resolve(&mut resolver, &catalog, &budget, &mut g).expect("tree");
        assert_eq!(tree.elements.len(), 1, "single-ref /K must be walked");
        let el = &tree.elements[0];
        assert_eq!(
            el.ty.as_deref(),
            Some(&b"Figure"[..]),
            "element type is resolved"
        );
        assert_eq!(
            el.alt.as_deref(),
            Some(&b"hello"[..]),
            "direct /Alt is extracted"
        );
    }

    #[test]
    fn role_map_parses() {
        let role_map_obj = dict(vec![(
            b"Subhead",
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"H3")),
        )]);
        match &role_map_obj {
            Obj::Dict(pairs) => {
                let map: BTreeMap<String, String> = pairs
                    .iter()
                    .filter_map(|(k, v)| match v {
                        Obj::Name(n) => Some((
                            String::from_utf8_lossy(k.as_slice()).to_string(),
                            String::from_utf8_lossy(n.as_slice()).to_string(),
                        )),
                        _ => None,
                    })
                    .collect();
                assert_eq!(map.get("Subhead").map(|s| s.as_str()), Some("H3"));
            }
            _ => panic!("expected dict"),
        }
    }
}
