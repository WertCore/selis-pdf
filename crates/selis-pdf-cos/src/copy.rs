//! Object-graph copying with renumbering (WRITE.03/04).
//!
//! Copies a set of objects (reachable from a root) from one document into
//! another, renumbering references to avoid collisions. This is the foundation
//! for Merge, Split, Page operations, Metadata editor, and Redaction.

use std::collections::HashMap;

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::{Obj, Ref};

/// Collect all objects reachable from `roots` in the source document, renumber
/// them, and return the (renumbered) objects plus the remap (source object
/// number → new object number).
///
/// # Budget
///
/// Bounded by the number of objects resolved and the budget.
///
/// # Malformed Input
///
/// `OBJ_UNEXPECTED` when a reachable object cannot be resolved.
pub fn collect_objects(
    src: &[u8],
    roots: &[Ref],
    next_num: &mut u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<(u32, Obj)>, HashMap<u32, u32>)> {
    let startxref = crate::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = crate::parse_revisions(src, startxref, budget, g)?;
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut pending: Vec<Ref> = roots.to_vec();
    let mut resolved: Vec<(u32, Obj)> = Vec::new();
    let mut visited: std::collections::BTreeSet<(u32, u16)> = std::collections::BTreeSet::new();

    // Pass 1: walk the subgraph, resolve each object, and assign it a new
    // object number.
    while let Some(r) = pending.pop() {
        if !visited.insert((r.num, r.gen)) {
            continue;
        }
        let obj = resolve_ref(src, &doc, r, budget, g)?;
        let new_num = *remap.entry(r.num).or_insert_with(|| {
            let n = *next_num;
            *next_num = next_num.saturating_add(1);
            n
        });
        collect_refs(&obj, &mut pending);
        resolved.push((new_num, obj));
    }

    // Pass 2: renumber with the *complete* remap. Renumbering mid-walk (while
    // children are still unvisited) would leave stale source numbers behind.
    let objects = resolved
        .into_iter()
        .map(|(new_num, obj)| (new_num, renumber_refs(obj, &remap)))
        .collect();

    Ok((objects, remap))
}

/// Find the byte offset of a reference in the source file via its xref index.
fn offset_of(src: &[u8], r: Ref, budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<u64> {
    let startxref = crate::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = crate::parse_revisions(src, startxref, budget, g)
        .map_err(|_| err!(Code::ObjUnexpected, during = "copy", object = r.num))?;
    for view in doc.revisions() {
        if let Some(crate::XrefEntry::InUse { offset, .. }) = view.entries.get(&r.num) {
            return Ok(*offset);
        }
    }
    Err(err!(Code::ObjUnexpected, during = "copy", object = r.num))
}

/// Resolve any object by reference: a direct in-use object, or a compressed
/// object inside an object stream.
pub fn resolve_ref(
    src: &[u8],
    doc: &crate::revision::Doc,
    r: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    for view in doc.revisions() {
        match view.entries.get(&r.num) {
            Some(crate::XrefEntry::InUse { offset, .. }) => {
                return crate::resolve::resolve_object_numbered(src, *offset, r.num, budget, g);
            }
            Some(crate::XrefEntry::Compressed { objstm, index }) => {
                return resolve_compressed(src, doc, *objstm, *index, budget, g);
            }
            Some(_) | None => {}
        }
    }
    Err(err!(Code::ObjUnexpected, during = "copy", object = r.num))
}

/// Resolve an object from an object stream.
fn resolve_compressed(
    src: &[u8],
    doc: &crate::revision::Doc,
    objstm: u32,
    index: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    let view = doc.revisions().last().ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "objstm",
            detail = "no revision"
        )
    })?;
    let offset = match view.entries.get(&objstm) {
        Some(crate::XrefEntry::InUse { offset, .. }) => *offset,
        _ => {
            return Err(err!(
                Code::ObjstmMalformed,
                during = "objstm",
                detail = "object stream not a direct object"
            ));
        }
    };
    let stream = crate::resolve::resolve_object_numbered(src, offset, objstm, budget, g)?;
    let (dict, data) = match &stream {
        Obj::Stream { dict, data } => (dict, data.as_slice()),
        _ => {
            return Err(err!(
                Code::ObjstmMalformed,
                during = "objstm",
                detail = "not a stream"
            ));
        }
    };
    // The object stream's payload is usually filtered; decode it (predictors
    // included — a `/DecodeParms` PNG predictor must be undone before the
    // `N G` offset header is readable).
    let payload = crate::xref::decode_stream_payload(dict, data, budget, g);
    let pairs = crate::xref_stream::parse_object_stream(dict, &payload, budget, g)?;
    let (_, range) = pairs
        .get(usize::try_from(index).unwrap_or(usize::MAX))
        .ok_or_else(|| err!(Code::ObjUnexpected, during = "objstm", object = index))?;
    let start = usize::try_from(range.start).unwrap_or(0);
    let end = usize::try_from(range.end).unwrap_or(payload.len());
    let slice = payload
        .get(start..end)
        .ok_or_else(|| err!(Code::ObjUnexpected, during = "objstm", at = range.start))?;
    let mut lexer = crate::Lexer::new(slice);
    let mut toks = Vec::new();
    while let Some(tok) = lexer.next_token(g)? {
        toks.push(tok);
    }
    let mut parser = crate::parse::ObjectParser::new(&toks, budget);
    parser.parse(g)
}

/// Collect all refs from an object for later resolution.
fn collect_refs(obj: &Obj, pending: &mut Vec<Ref>) {
    match obj {
        Obj::Ref(r) => pending.push(*r),
        Obj::Array(items) => {
            for item in items {
                collect_refs(item, pending);
            }
        }
        Obj::Dict(pairs) => {
            for (_, v) in pairs {
                collect_refs(v, pending);
            }
        }
        Obj::Stream { dict, data: _ } => {
            for (_, v) in dict {
                collect_refs(v, pending);
            }
        }
        _ => {}
    }
}

/// Renumber all refs in an object according to the remap table. Numbers not
/// present in the map are kept as-is.
#[must_use]
pub fn renumber(obj: Obj, remap: &HashMap<u32, u32>) -> Obj {
    renumber_refs(obj, remap)
}

/// Renumber all refs in an object according to the remap table.
fn renumber_refs(obj: Obj, remap: &HashMap<u32, u32>) -> Obj {
    match obj {
        Obj::Ref(r) => {
            let new_num = remap.get(&r.num).copied().unwrap_or(r.num);
            Obj::Ref(Ref::new(new_num, r.gen))
        }
        Obj::Array(items) => {
            Obj::Array(items.into_iter().map(|i| renumber_refs(i, remap)).collect())
        }
        Obj::Dict(pairs) => Obj::Dict(
            pairs
                .into_iter()
                .map(|(k, v)| (k, renumber_refs(v, remap)))
                .collect(),
        ),
        Obj::Stream { dict, data } => Obj::Stream {
            dict: dict
                .into_iter()
                .map(|(k, v)| (k, renumber_refs(v, remap)))
                .collect(),
            data,
        },
        other => other,
    }
}

/// Collapse byte-identical objects to a single representative (WRITE.03:
/// "identical fonts across inputs deduplicate"). Objects serialise
/// identically only when they are the same value — same primitives, same
/// stream bytes, same refs — so merging them never changes meaning; every
/// reference to a removed object is redirected at its survivor.
///
/// Page-tree nodes (`/Type /Page`, `/Type /Pages`) and annotations
/// (`/Type /Annot`) are never merged: their position or page affiliation is
/// structural, and collapsing two of them onto one object would look like a
/// cycle to a tree walker or attach one annotation to two pages.
///
/// Deduplication runs to a fixpoint: once two identical leaves (e.g. font
/// files) merge, their parents (font descriptors, resource dicts) often
/// become identical too and merge on a later pass. Each pass strictly
/// reduces the object count, so this terminates.
///
/// Returns the surviving objects (original numbers preserved), the composite
/// remap (removed number → surviving number, fully resolved across passes),
/// and the number of objects removed.
///
/// # Budget
///
/// Each pass serialises every object once; serialisation charges the budget.
/// An object that fails to serialise is left untouched (never merged).
///
/// # Malformed Input
///
/// None: dedup only transforms caller-owned, already-parsed objects.
pub fn dedup_objects(
    mut objects: Vec<(u32, Obj)>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> (Vec<(u32, Obj)>, HashMap<u32, u32>, usize) {
    let mut total: HashMap<u32, u32> = HashMap::new();
    let mut removed_total = 0usize;
    loop {
        let mut canonical: HashMap<Vec<u8>, u32> = HashMap::new();
        let mut remap: HashMap<u32, u32> = HashMap::new();
        for (num, obj) in &objects {
            // Page-tree nodes and annotations carry structural identity — a
            // page's position in `/Kids` is what makes it, and an annotation
            // belongs to exactly one page — so merging two of them onto one
            // object referenced twice would look like a cycle to a tree
            // walker or attach one annotation to two pages. Value objects
            // (streams, fonts, descriptors) merge.
            if has_structural_identity(obj) {
                continue;
            }
            let mut w = crate::Writer::new(budget);
            let Ok(bytes) = w.to_bytes(obj, g) else {
                // Cannot serialise: keep the object, never merge it.
                continue;
            };
            match canonical.get(&bytes) {
                Some(&first) if first != *num => {
                    remap.insert(*num, first);
                }
                Some(_) => {}
                None => {
                    canonical.insert(bytes, *num);
                }
            }
        }
        if remap.is_empty() {
            break;
        }
        // Fold this pass into the composite remap. A value recorded by an
        // earlier pass may itself have been merged now; redirect it. (A
        // remap value is a canonical of this pass and therefore never a key
        // of this pass, so one lookup resolves it.)
        for v in total.values_mut() {
            if let Some(next) = remap.get(v) {
                *v = *next;
            }
        }
        for (&dup, &canon) in &remap {
            total.insert(dup, canon);
        }
        removed_total = removed_total.saturating_add(remap.len());
        objects = objects
            .into_iter()
            .filter(|(num, _)| !remap.contains_key(num))
            .map(|(num, obj)| (num, renumber_refs(obj, &remap)))
            .collect();
    }
    (objects, total, removed_total)
}

/// True when `obj` carries structural identity and must not be merged by
/// dedup: page-tree nodes (`/Type /Page`, `/Type /Pages`) and annotations
/// (`/Type /Annot`), which each belong to exactly one page.
fn has_structural_identity(obj: &Obj) -> bool {
    let pairs: &[(selis_bytes::Bytes, Obj)] = match obj {
        Obj::Dict(pairs) | Obj::Stream { dict: pairs, .. } => pairs,
        _ => return false,
    };
    pairs.iter().any(|(k, v)| {
        k.as_slice() == b"Type"
            && matches!(v, Obj::Name(n) if matches!(
                n.as_slice(),
                b"Page" | b"Pages" | b"Annot"
            ))
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used
    )]

    use super::*;

    /// Build a minimal two-page PDF: each page has its own content stream and
    /// its own `/Resources` pointing at its own image XObject.
    fn two_page_source() -> Vec<u8> {
        let numbered: &[(u32, &[u8])] = &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R /Resources 5 0 R >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Length 8 >>\nstream\n/ImA Do\nendstream\nendobj\n",
            ),
            (5, b"5 0 obj\n<< /XObject << /ImA 7 0 R >> >>\nendobj\n"),
            (
                6,
                b"6 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 8 0 R /Resources 9 0 R >>\nendobj\n",
            ),
            (
                7,
                b"7 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\x00\nendstream\nendobj\n",
            ),
            (
                8,
                b"8 0 obj\n<< /Length 8 >>\nstream\n/ImB Do\nendstream\nendobj\n",
            ),
            (9, b"9 0 obj\n<< /XObject << /ImB 10 0 R >> >>\nendobj\n"),
            (
                10,
                b"10 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\xFF\nendstream\nendobj\n",
            ),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets: Vec<(u32, u64)> = Vec::new();
        for (num, body) in numbered {
            offsets.push((*num, u64::try_from(out.len()).unwrap_or(0)));
            out.extend_from_slice(body);
        }
        offsets.sort_by_key(|(num, _)| *num);
        let xref_at = u64::try_from(out.len()).unwrap_or(0);
        out.extend_from_slice(b"xref\n0 11\n0000000000 65535 f \n");
        for (_, off) in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n<< /Size 11 /Root 1 0 R >>\nstartxref\n");
        out.extend_from_slice(format!("{xref_at}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");
        out
    }

    /// Copying page 2's subgraph must remap every reference: the resources
    /// dict's `/ImB` target must equal the image's new number, not a stale
    /// source number (regression: renumbering mid-walk left stale refs).
    #[test]
    fn copied_subgraph_has_no_stale_references() {
        let src = two_page_source();
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut next_num = 3u32;
        // Page 2's roots: its content stream (8) and resources (9).
        let roots = [Ref::new(8, 0), Ref::new(9, 0)];
        let (objects, remap) =
            collect_objects(&src, &roots, &mut next_num, &budget, &mut g).expect("collect");
        // Every reachable source object got a fresh number.
        assert_eq!(remap.len(), 3, "content + resources + image: {remap:?}");
        // Find the resources dict among the output objects and check /ImB.
        let resources = objects
            .iter()
            .find(|(num, _)| *num == remap[&9])
            .expect("resources object");
        let Obj::Dict(pairs) = &resources.1 else {
            panic!("resources is not a dict");
        };
        let xobjects = pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"XObject")
            .map(|(_, v)| v)
            .expect("XObject key");
        let Obj::Dict(xpairs) = xobjects else {
            panic!("XObject is not a dict");
        };
        let im_b = xpairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"ImB")
            .map(|(_, v)| v)
            .expect("ImB key");
        let Obj::Ref(target) = im_b else {
            panic!("ImB is not a reference");
        };
        assert_eq!(
            target.num, remap[&10],
            "/ImB must point at the remapped image, not a stale source number"
        );
        // And the remapped image object is part of the output.
        assert!(
            objects.iter().any(|(num, _)| *num == remap[&10]),
            "remapped image object missing from output"
        );
    }

    fn name_obj(n: &[u8]) -> Obj {
        Obj::Name(selis_bytes::Bytes::copy_from_slice(n))
    }

    /// Dedup runs to a fixpoint: merging identical font files makes their
    /// font descriptors identical, which makes the resource dicts identical —
    /// three levels collapse across passes, and the composite remap resolves
    /// every removed number to the final survivor.
    #[test]
    fn dedup_cascades_to_a_fixpoint() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let stream = |num: u32| {
            (
                num,
                Obj::Stream {
                    dict: vec![],
                    data: selis_bytes::Bytes::copy_from_slice(b"identical font file bytes"),
                },
            )
        };
        let descriptor = |num: u32, file: u32| {
            (
                num,
                Obj::Dict(vec![
                    (
                        selis_bytes::Bytes::copy_from_slice(b"FontFile"),
                        Obj::Ref(Ref::new(file, 0)),
                    ),
                    (
                        selis_bytes::Bytes::copy_from_slice(b"FontName"),
                        name_obj(b"F1"),
                    ),
                ]),
            )
        };
        let resources = |num: u32, font: u32| {
            (
                num,
                Obj::Dict(vec![(
                    selis_bytes::Bytes::copy_from_slice(b"F1"),
                    Obj::Ref(Ref::new(font, 0)),
                )]),
            )
        };
        let objects = vec![
            stream(10),
            stream(20),
            descriptor(11, 10),
            descriptor(21, 20),
            resources(12, 11),
            resources(22, 21),
        ];
        let (survivors, remap, removed) = dedup_objects(objects, &budget, &mut g);
        assert_eq!(removed, 3, "one object removed per level: {remap:?}");
        assert_eq!(survivors.len(), 3);
        // Every removed number resolves to its final survivor.
        assert_eq!(remap[&20], 10, "font file merged at level 1");
        assert_eq!(remap[&21], 11, "descriptor merged once files matched");
        assert_eq!(remap[&22], 12, "resources merged once descriptors matched");
        // The surviving resources dict points at the surviving descriptor.
        let surviving = survivors
            .iter()
            .find(|(num, _)| *num == 12)
            .map(|(_, o)| o)
            .expect("surviving resources");
        let Obj::Dict(pairs) = surviving else {
            panic!("dict");
        };
        assert_eq!(
            pairs.first().map(|(_, v)| v),
            Some(&Obj::Ref(Ref::new(11, 0))),
            "refs redirect at the final survivor"
        );
    }

    /// Objects that are not byte-identical (different payloads) never merge.
    #[test]
    fn dedup_keeps_distinct_objects() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let objects = vec![
            (
                1u32,
                Obj::Stream {
                    dict: vec![],
                    data: selis_bytes::Bytes::copy_from_slice(b"aaa"),
                },
            ),
            (
                2u32,
                Obj::Stream {
                    dict: vec![],
                    data: selis_bytes::Bytes::copy_from_slice(b"bbb"),
                },
            ),
        ];
        let (survivors, remap, removed) = dedup_objects(objects, &budget, &mut g);
        assert_eq!(removed, 0);
        assert!(remap.is_empty());
        assert_eq!(survivors.len(), 2);
    }
}
