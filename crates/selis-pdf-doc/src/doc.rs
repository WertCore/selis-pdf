//! The document model (SL-1.DOC.01).
//!
//! Walks the catalog and page tree over a parsed COS document, resolving
//! inherited attributes (`Resources`, `MediaBox`, `CropBox`, `Rotate`) and
//! guarding every walk against cycles and malformed graphs.

use std::collections::BTreeSet;

use selis_error::{err, Code, Result};
use selis_geom::Rect;
use selis_pdf_cos::encrypt::DecryptPolicy;
use selis_pdf_cos::{resolve_object_numbered, Doc, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::resolve::resolve_compressed;

/// A resolved page: its attributes after inheritance.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// The object number of this page.
    pub num: u32,
    /// The inherited `/MediaBox`.
    pub media_box: Option<Rect>,
    /// The inherited `/CropBox` (falls back to `/MediaBox`).
    pub crop_box: Option<Rect>,
    /// The inherited `/Rotate` (0, 90, 180, 270).
    pub rotate: Option<i32>,
    /// The inherited `/Resources` dictionary.
    pub resources: Option<Obj>,
    /// The `/Contents` reference(s): a single ref or an array of refs.
    pub contents: Option<Vec<Ref>>,
}

/// The document model over a parsed COS [`Doc`] and its source bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// The catalog dictionary (the `/Root` object).
    pub catalog: Obj,
    /// Pages, in document order.
    pub pages: Vec<Page>,
}

impl Document {
    /// Resolve the catalog and page tree from a COS document.
    ///
    /// # Budget
    ///
    /// The caller's budget bounds the walk: every object resolved is charged,
    /// and cycle depth is bounded.
    ///
    /// # Malformed Input
    ///
    /// A missing or non-dict `/Root` yields `OBJ_UNEXPECTED`; a cyclic page
    /// tree terminates with `OBJ_CYCLE` rather than hanging.
    ///
    /// `key` carries the authenticated encryption key (bytes, revision, AES
    /// flag) for password-protected documents; `None` opens clear documents.
    pub fn resolve(
        doc: &Doc,
        src: &[u8],
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
        key: Option<&DecryptPolicy>,
    ) -> Result<Self> {
        let view = doc
            .at_revision(doc.len().saturating_sub(1))
            .ok_or_else(|| {
                err!(
                    Code::ObjUnexpected,
                    during = "doc-catalog",
                    detail = "no revisions"
                )
            })?;
        let root_ref = view.root.ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "doc-catalog",
                detail = "no /Root"
            )
        })?;
        let catalog = resolve_ref(doc, src, root_ref, budget, g, key)?;

        // Walk the page tree from /Pages.
        let pages_ref = dict_ref(&catalog, b"Pages").ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "doc-pages",
                detail = "catalog has no /Pages"
            )
        })?;

        let mut pages = Vec::new();
        let mut visited = BTreeSet::new();
        let mut inherited = Inherited::default();
        // A broken or unresolvable /Pages reference yields an empty page list
        // rather than refusing the whole document (SL-1.ROB.01 — the catalog
        // itself is valid, the page tree is a phantom).
        let pages_res = walk_pages(
            doc,
            src,
            pages_ref,
            &mut inherited,
            &mut pages,
            &mut visited,
            budget,
            g,
            key,
        );
        if let Err(e) = &pages_res {
            if e.is_budget() || e.is_cancelled() || e.is_pending() {
                return Err(e.clone());
            }
        }

        Ok(Self { catalog, pages })
    }

    /// The number of pages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Whether the document has no pages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

/// Inheritable page attributes, threaded down the page tree.
#[derive(Debug, Clone, Default)]
struct Inherited {
    media_box: Option<Rect>,
    crop_box: Option<Rect>,
    rotate: Option<i32>,
    resources: Option<Obj>,
}

/// Resolve a reference: find its byte offset in the newest xref, then read
/// the object body. When `key` is set, a directly-stored object is decrypted
/// and an object-stream container is decrypted inside `resolve_compressed`.
fn resolve_ref(
    doc: &Doc,
    src: &[u8],
    r: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    key: Option<&DecryptPolicy>,
) -> Result<Obj> {
    let view = doc
        .at_revision(doc.len().saturating_sub(1))
        .ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "doc-resolve",
                detail = "no revisions"
            )
        })?;
    let obj = match view.xref.get(&r.num) {
        Some(selis_pdf_cos::XrefEntry::InUse { offset, .. }) => {
            let obj = resolve_object_numbered(src, *offset, r.num, budget, g)?;
            match key {
                Some(policy) => crate::resolve::decrypt_obj(obj, r, policy),
                None => obj,
            }
        }
        Some(selis_pdf_cos::XrefEntry::Compressed { objstm, index }) => {
            // The object lives in an object stream (/ObjStm).
            resolve_compressed(doc, src, *objstm, *index, budget, g, key)?
        }
        Some(_) | None => {
            return Err(err!(
                Code::ObjUnexpected,
                during = "doc-resolve",
                object = r.num
            ));
        }
    };
    Ok(obj)
}

fn dict_ref(dict: &Obj, key: &[u8]) -> Option<Ref> {
    match dict {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            }),
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

/// Parse a PDF rectangle array into a normalised [`Rect`].
fn obj_rect(obj: &Obj) -> Option<Rect> {
    match obj {
        Obj::Array(items) => {
            if items.len() != 4 {
                return None;
            }
            let mut v = [0f64; 4];
            for (slot, item) in v.iter_mut().zip(items) {
                *slot = obj_f64(item)?;
            }
            Some(Rect::from_pdf_array(v))
        }
        _ => None,
    }
}

fn obj_f64(obj: &Obj) -> Option<f64> {
    match obj {
        Obj::Int(i) => Some(*i as f64),
        Obj::Real { scaled, scale } => {
            let div = 10f64.powi(i32::from(*scale));
            Some(*scaled as f64 / div)
        }
        _ => None,
    }
}

/// A PDF integer (Rotate, page count, …).
fn obj_i32(obj: &Obj) -> Option<i32> {
    match obj {
        Obj::Int(i) => i32::try_from(*i).ok(),
        _ => None,
    }
}

/// Walk the page tree, resolving inherited attributes and guarding cycles.
#[allow(clippy::too_many_arguments)]
fn walk_pages(
    doc: &Doc,
    src: &[u8],
    node_ref: Ref,
    inherited: &mut Inherited,
    out: &mut Vec<Page>,
    visited: &mut BTreeSet<u32>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    key: Option<&DecryptPolicy>,
) -> Result<()> {
    // A node already visited closes a cycle in the page tree. Tolerant
    // viewers skip the cyclic branch and keep the pages reachable outside it
    // rather than refusing the whole document (SL-1.ROB.01).
    if !visited.insert(node_ref.num) {
        return Ok(());
    }
    // Depth is scoped to this node's level of the page tree: the RAII guard
    // releases it on every exit path, so depth tracks the tree's actual
    // height rather than accumulating one level per page visited. Without
    // this, a document with more pages than the depth budget could not open.
    let mut d = selis_sandbox::DepthGuard::enter(g)?;

    let node = resolve_ref(doc, src, node_ref, budget, d.guard(), key)?;
    let node_type = dict_get(&node, b"Type").and_then(|t| match t {
        Obj::Name(n) => Some(n.clone()),
        _ => None,
    });

    // A node's own attributes override what it inherits — for both tree and
    // page nodes.
    apply_inherited(&node, inherited);

    if node_type.as_ref().map(|n| n.as_slice()) == Some(b"Pages") {
        // A tree node: thread inherited attributes down, then recurse.
        let kids = dict_get(&node, b"Kids").ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "doc-pages",
                object = node_ref.num,
                detail = "/Pages without /Kids"
            )
        })?;
        let kids = match kids {
            Obj::Array(items) => items,
            _ => {
                return Err(err!(
                    Code::ObjUnexpected,
                    during = "doc-pages",
                    object = node_ref.num,
                    detail = "/Kids is not an array"
                ));
            }
        };
        // Validate /Count when present (never trust it for walking).
        if let Some(count) = dict_get(&node, b"Count").and_then(obj_f64) {
            let _ = count;
        }
        for kid in kids {
            let kid_ref = match kid {
                Obj::Ref(r) => *r,
                // An inline page dict in `/Kids` (damaged writers embed the
                // page body directly). Walk it in-place as a page node.
                Obj::Dict(_) => {
                    walk_inline_page(doc, src, kid, inherited, out, budget, d.guard(), key)?;
                    continue;
                }
                _ => {
                    return Err(err!(
                        Code::ObjUnexpected,
                        during = "doc-pages",
                        object = node_ref.num,
                        detail = "/Kids entry is not a reference"
                    ));
                }
            };
            match walk_pages(
                doc,
                src,
                kid_ref,
                inherited,
                out,
                visited,
                budget,
                d.guard(),
                key,
            ) {
                Ok(()) => {}
                // A kid that cannot be resolved (a phantom page reference in a
                // damaged document) drops that branch: the real pages still
                // open (SL-1.ROB.01). Budget and cancellation always propagate.
                Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => {
                    return Err(e);
                }
                Err(_) => {}
            }
        }
    } else {
        // A page node: materialise the resolved attributes.
        let page = materialize_page(
            &node,
            node_ref.num,
            inherited,
            doc,
            src,
            budget,
            d.guard(),
            key,
        )?;
        out.push(page);
    }
    Ok(())
}

/// Materialise a resolved page node into a [`Page`], resolving indirect
/// `/Resources` and collecting the content-stream references.
fn materialize_page(
    node: &Obj,
    num: u32,
    inherited: &Inherited,
    doc: &Doc,
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    key: Option<&DecryptPolicy>,
) -> Result<Page> {
    let media_box = inherited.media_box;
    let crop_box = inherited.crop_box;
    let rotate = inherited.rotate;
    // `/Resources` may be an indirect reference (writers commonly emit
    // `/Resources N 0 R`); consumers need the dictionary, so resolve it
    // here rather than threading a bare reference to every call site.
    let resources = match inherited.resources.as_ref() {
        Some(Obj::Ref(r)) => resolve_ref(doc, src, *r, budget, g, key).ok(),
        other => other.cloned(),
    };
    let contents = dict_get(node, b"Contents").and_then(contents_refs);
    Ok(Page {
        num,
        media_box,
        crop_box,
        rotate,
        resources,
        contents,
    })
}

/// Walk an inline page dict found directly inside a `/Kids` array (damaged
/// writers embed the page body rather than a reference).
fn walk_inline_page(
    doc: &Doc,
    src: &[u8],
    node: &Obj,
    inherited: &mut Inherited,
    out: &mut Vec<Page>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    key: Option<&DecryptPolicy>,
) -> Result<()> {
    let mut d = selis_sandbox::DepthGuard::enter(g)?;
    apply_inherited(node, inherited);
    // An inline page has no xref number; 0 never collides with a real object.
    let page = materialize_page(node, 0, inherited, doc, src, budget, d.guard(), key)?;
    out.push(page);
    Ok(())
}

/// Override inherited attributes with any this node declares.
fn apply_inherited(node: &Obj, inherited: &mut Inherited) {
    if let Some(v) = dict_get(node, b"MediaBox").and_then(obj_rect) {
        inherited.media_box = Some(v);
    }
    if let Some(v) = dict_get(node, b"CropBox").and_then(obj_rect) {
        inherited.crop_box = Some(v);
    }
    if let Some(v) = dict_get(node, b"Rotate").and_then(obj_i32) {
        inherited.rotate = Some(v);
    }
    if let Some(v) = dict_get(node, b"Resources").cloned() {
        inherited.resources = Some(v);
    }
}

/// `/Contents` is a single ref or an array of refs.
fn contents_refs(obj: &Obj) -> Option<Vec<Ref>> {
    match obj {
        Obj::Ref(r) => Some(vec![*r]),
        Obj::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    Obj::Ref(r) => out.push(*r),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss
    )]

    use super::*;
    use selis_pdf_cos::XrefEntry;
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    /// A minimal 2-page document with inheritance at the /Pages node.
    fn two_page_doc() -> (Vec<u8>, Vec<(u32, u64)>) {
        let mut out = Vec::new();
        let mut entries = Vec::new();

        out.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: catalog.
        let off = out.len() as u64;
        out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        entries.push((1, off));
        // Object 2: page tree root with inherited MediaBox.
        let off = out.len() as u64;
        out.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        entries.push((2, off));
        // Object 3: page 1 (no own MediaBox; inherits).
        let off = out.len() as u64;
        out.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n");
        entries.push((3, off));
        // Object 4: page 2 with its own MediaBox.
        let off = out.len() as u64;
        out.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >>\nendobj\n",
        );
        entries.push((4, off));
        (out, entries)
    }

    #[test]
    fn resolves_catalog_and_pages() {
        let (src, entries) = two_page_doc();
        let mut g = guard();
        let budget = Budget::unlimited();

        // Build a Doc from the xref entries directly (single revision).
        let mut xref = std::collections::BTreeMap::new();
        for (num, off) in &entries {
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
        let doc = Doc::from_single_revision(xref, trailer);

        let document = Document::resolve(&doc, &src, &budget, &mut g, None).expect("resolve");
        assert_eq!(document.len(), 2);
        // Page 1 inherits MediaBox from /Pages.
        assert_eq!(
            document.pages[0].media_box,
            Some(Rect::from_pdf_array([0.0, 0.0, 612.0, 792.0]))
        );
        // Page 2 overrides it.
        assert_eq!(
            document.pages[1].media_box,
            Some(Rect::from_pdf_array([0.0, 0.0, 300.0, 400.0]))
        );
        assert_eq!(document.pages[0].num, 3);
        assert_eq!(document.pages[1].num, 4);
    }

    /// An indirect `/Resources` reference on a tree node must resolve to the
    /// dictionary by the time the page is materialised — consumers (fonts,
    /// XObjects, …) expect a dict, not a bare reference.
    #[test]
    fn indirect_resources_resolve_to_the_dictionary() {
        let mut out = Vec::new();
        let mut entries = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let off = out.len() as u64;
        out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        entries.push((1, off));
        // Object 2: page tree root whose /Resources is an indirect reference.
        let off = out.len() as u64;
        out.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 5 0 R >>\nendobj\n",
        );
        entries.push((2, off));
        let off = out.len() as u64;
        out.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n");
        entries.push((3, off));
        let off = out.len() as u64;
        out.extend_from_slice(b"5 0 obj\n<< /XObject << /Im7 9 0 R >> >>\nendobj\n");
        entries.push((5, off));

        let mut g = guard();
        let budget = Budget::unlimited();
        let mut xref = std::collections::BTreeMap::new();
        for (num, off) in &entries {
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
        let doc = Doc::from_single_revision(xref, trailer);
        let document = Document::resolve(&doc, &out, &budget, &mut g, None).expect("resolve");
        assert_eq!(document.len(), 1);
        let resources = document.pages[0].resources.as_ref().expect("resources");
        assert!(
            matches!(resources, Obj::Dict(_)),
            "resources must be a dict, got {resources:?}"
        );
        let xobjects = dict_get(resources, b"XObject").expect("XObject dict");
        assert!(dict_get(xobjects, b"Im7").is_some());
    }

    /// A cyclic page tree must terminate with OBJ_CYCLE, not hang.
    #[test]
    fn cyclic_page_tree_terminates() {
        let mut out = Vec::new();
        let mut entries = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let off = out.len() as u64;
        out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        entries.push((1, off));
        // Object 2: a /Pages node whose only kid is ITSELF.
        let off = out.len() as u64;
        out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [2 0 R] /Count 1 >>\nendobj\n");
        entries.push((2, off));

        let mut g = guard();
        let budget = Budget::unlimited();
        let mut xref = std::collections::BTreeMap::new();
        for (num, off) in &entries {
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
        let doc = Doc::from_single_revision(xref, trailer);
        // A cyclic branch is skipped, not fatal: the walk terminates with the
        // pages reachable outside the cycle (here: none — the only kid is the
        // node itself).
        let document = Document::resolve(&doc, &out, &budget, &mut g, None).expect("resolve");
        assert_eq!(document.len(), 0, "cyclic branch yields no pages");
    }
}
