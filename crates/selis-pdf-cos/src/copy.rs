//! Object-graph copying with renumbering (WRITE.03/04).
//!
//! Copies a set of objects (reachable from a root) from one document into
//! another, renumbering references to avoid collisions. This is the foundation
//! for Merge, Split, Page operations, Metadata editor, and Redaction.

use std::collections::HashMap;

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::{Obj, Ref};
use crate::resolve_object;

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
    let mut objects: Vec<(u32, Obj)> = Vec::new();
    let mut visited: std::collections::BTreeSet<(u32, u16)> = std::collections::BTreeSet::new();

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
        objects.push((new_num, renumber_refs(obj, &remap)));
    }

    Ok((objects, remap))
}

/// Find the byte offset of a reference in the source file via its xref index.
fn offset_of(
    src: &[u8],
    r: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<u64> {
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
                return crate::resolve_object(src, *offset, budget, g);
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
    let view = doc
        .revisions()
        .last()
        .ok_or_else(|| err!(Code::ObjUnexpected, during = "objstm", detail = "no revision"))?;
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
    let stream = crate::resolve_object(src, offset, budget, g)?;
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
    // The object stream's payload is usually filtered; decode it.
    let filters: Vec<String> = match dict_find(dict, b"Filter") {
        Some(Obj::Name(n)) => vec![String::from_utf8_lossy(n.as_slice()).to_string()],
        Some(Obj::Array(items)) => items
            .iter()
            .filter_map(|o| match o {
                Obj::Name(n) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let payload = if filters.is_empty() {
        data.to_vec()
    } else {
        selis_pdf_filter::decode_chain(&filters, &[], data, budget.bytes, g)
            .unwrap_or_else(|_| data.to_vec())
    };
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

/// Look up a key in a dict value's pairs.
fn dict_find<'a>(dict: &'a [(selis_bytes::Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    dict.iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

/// Collect all refs from an object for later resolution.
fn collect_refs(obj: &Obj, pending: &mut Vec<Ref>) {    match obj {
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
        Obj::Dict(pairs) => {
            Obj::Dict(pairs.into_iter().map(|(k, v)| (k, renumber_refs(v, remap))).collect())
        }
        Obj::Stream { dict, data } => {
            Obj::Stream {
                dict: dict.into_iter().map(|(k, v)| (k, renumber_refs(v, remap))).collect(),
                data,
            }
        }
        other => other,
    }
}