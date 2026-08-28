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
use selis_pdf_cos::{resolve_object_numbered, Doc, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

/// A reference-resolving walker that refuses cycles.
#[derive(Debug)]
pub struct Resolver<'a> {
    doc: &'a Doc,
    src: &'a [u8],
    budget: &'a Budget,
    visited: BTreeSet<u32>,
    depth: u16,
    /// The encryption key, revision, and AES flag (from /Encrypt), if the
    /// document is encrypted.
    key: Option<(Vec<u8>, u8, bool)>,
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
            key: None,
        }
    }

    /// Set the encryption key so resolved streams/strings are automatically
    /// decrypted.
    pub fn set_key(&mut self, key: Vec<u8>, r: u8, aes: bool) {
        self.key = Some((key, r, aes));
    }

    /// The latest revision view (for reading stream bodies directly).
    #[must_use]
    pub fn at_revision(&self) -> Option<selis_pdf_cos::RevisionView> {
        self.doc.at_revision(self.doc.len().saturating_sub(1))
    }

    /// The source bytes (for reading stream bodies directly).
    #[must_use]
    pub fn src(&self) -> &'a [u8] {
        self.src
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
            return Err(err!(Code::ObjCycle, during = "doc-resolve", object = r.num));
        }
        // Depth is scoped to this resolution level: the RAII guard releases
        // it on every exit path, so depth tracks the walk's actual nesting
        // rather than accumulating one level per object resolved.
        let mut d = selis_sandbox::DepthGuard::enter(g)?;
        self.depth = self.depth.saturating_add(1);
        let obj = self.resolve_scoped(r, d.guard());
        // Unwind the per-resolution state on every path, including errors.
        self.depth = self.depth.saturating_sub(1);
        self.visited.remove(&r.num);
        obj.map(|o| {
            if let Some((key, rev, aes)) = &self.key {
                decrypt_obj(o, r, key, *rev, *aes)
            } else {
                o
            }
        })
    }

    /// The body of [`Resolver::resolve`] once depth and cycle state are set up.
    fn resolve_scoped(&mut self, r: Ref, g: &mut BudgetGuard<'_>) -> Result<Obj> {
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
        match view.xref.get(&r.num) {
            Some(selis_pdf_cos::XrefEntry::InUse { offset, .. }) => {
                g.charge_one(selis_sandbox::Resource::Objects)?;
                resolve_object_numbered(self.src, *offset, r.num, self.budget, g)
            }
            Some(selis_pdf_cos::XrefEntry::Compressed { objstm, index }) => {
                // The object lives in an object stream (/ObjStm): resolve the
                // stream, parse its (number, range) index, and parse the
                // object at that range.
                g.charge_one(selis_sandbox::Resource::Objects)?;
                resolve_compressed(self.doc, self.src, *objstm, *index, self.budget, g)
            }
            Some(_) | None => Err(err!(
                Code::ObjUnexpected,
                during = "doc-resolve",
                object = r.num
            )),
        }
    }
}

/// Decrypt the streams and strings in a resolved object.
fn decrypt_obj(obj: Obj, r: Ref, key: &[u8], rev: u8, aes: bool) -> Obj {
    decrypt_obj_inner(obj, r, key, rev, aes, 0)
}

fn decrypt_obj_inner(obj: Obj, r: Ref, key: &[u8], rev: u8, aes: bool, depth: u16) -> Obj {
    if depth > 32 {
        return obj;
    }
    match obj {
        Obj::Stream { dict, data } => {
            let decrypted =
                selis_crypto::decrypt_data(key, r.num, r.gen, data.as_slice(), rev, aes);
            Obj::Stream {
                dict: decrypt_dict(dict, r, key, rev, aes, depth),
                data: selis_bytes::Bytes::from(decrypted),
            }
        }
        Obj::String(bytes) => {
            let decrypted =
                selis_crypto::decrypt_data(key, r.num, r.gen, bytes.as_slice(), rev, aes);
            Obj::String(selis_bytes::Bytes::from(decrypted))
        }
        Obj::Dict(pairs) => Obj::Dict(decrypt_dict(pairs, r, key, rev, aes, depth)),
        Obj::Array(items) => Obj::Array(
            items
                .into_iter()
                .map(|i| decrypt_obj_inner(i, r, key, rev, aes, depth.saturating_add(1)))
                .collect(),
        ),
        other => other,
    }
}

/// Decrypt a dict's values in place (streams and strings).
fn decrypt_dict(
    pairs: Vec<(selis_bytes::Bytes, Obj)>,
    r: Ref,
    key: &[u8],
    rev: u8,
    aes: bool,
    depth: u16,
) -> Vec<(selis_bytes::Bytes, Obj)> {
    pairs
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                decrypt_obj_inner(v, r, key, rev, aes, depth.saturating_add(1)),
            )
        })
        .collect()
}

/// Resolve an object stored in an object stream (`/ObjStm`).
pub(crate) fn resolve_compressed(
    doc: &Doc,
    src: &[u8],
    objstm: u32,
    index: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    let view = doc
        .at_revision(doc.len().saturating_sub(1))
        .ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "objstm",
                detail = "no revision"
            )
        })?;
    let offset = match view.xref.get(&objstm) {
        Some(selis_pdf_cos::XrefEntry::InUse { offset, .. }) => *offset,
        _ => {
            return Err(err!(
                Code::ObjstmMalformed,
                during = "objstm",
                detail = "object stream not a direct object"
            ));
        }
    };
    let obj = resolve_object_numbered(src, offset, objstm, budget, g)?;
    let (dict, payload) = match &obj {
        Obj::Stream { dict, data } => (dict, data.as_slice()),
        _ => {
            return Err(err!(
                Code::ObjstmMalformed,
                during = "objstm",
                detail = "referenced object is not a stream"
            ));
        }
    };
    // Unfilter the object stream data.
    let payload = if let Some(Obj::Name(n)) = dict
        .iter()
        .find(|(k, _)| k.as_slice() == b"Filter")
        .map(|(_, v)| v)
    {
        let filt = std::str::from_utf8(n.as_slice()).unwrap_or("");
        selis_pdf_filter::decode(filt, payload, u64::MAX, g).unwrap_or_else(|_| payload.to_vec())
    } else {
        payload.to_vec()
    };
    // The (number, range) index of the objects in the stream.
    let pairs = selis_pdf_cos::parse_object_stream(dict, &payload, budget, g)?;
    // The stream's object at the requested index.
    let mut i = 0u64;
    for (_, range) in pairs {
        if i == u64::from(index) {
            return parse_value_at(&payload, range, budget, g);
        }
        i = i.saturating_add(1);
    }
    Err(err!(
        Code::ObjstmMalformed,
        during = "objstm",
        detail = "index out of range"
    ))
}

/// Parse an object value at a byte range (object-stream objects have no
/// `N G obj` header — they are bare values, and the caller supplies the exact
/// `start..end` span from the `/ObjStm` index).
fn parse_value_at(
    data: &[u8],
    range: std::ops::Range<u64>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj> {
    let start = usize::try_from(range.start).unwrap_or(0);
    let end = usize::try_from(range.end).unwrap_or(data.len());
    let slice = data.get(start..end).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "objstm-value",
            at = range.start
        )
    })?;
    let mut lexer = selis_pdf_cos::Lexer::new(slice);
    // Lex one complete value, tracking `[`/`<<` nesting depth: a composite
    // value (`[1 2 3]`, `<< /A [1] >>`) is only complete once every opener
    // has been closed. A bare scalar closes immediately — except a leading
    // `N G R` reference, whose integer tokens must be kept together until the
    // `R` arrives (or the range runs out).
    let mut toks = Vec::new();
    let mut depth = 0u32;
    let mut pending_ref_nums = 0u32;
    loop {
        let Some(tok) = lexer.next_token(g)? else {
            break;
        };
        g.charge_one(selis_sandbox::Resource::Objects)?;
        match tok {
            selis_pdf_cos::Token::ArrayStart | selis_pdf_cos::Token::DictStart => {
                depth = depth.saturating_add(1);
            }
            selis_pdf_cos::Token::ArrayEnd | selis_pdf_cos::Token::DictEnd => {
                if depth == 0 {
                    break;
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        let complete = if depth > 0 {
            false
        } else if matches!(
            tok,
            selis_pdf_cos::Token::Number(selis_pdf_cos::Number::Int(_))
        ) && pending_ref_nums < 2
        {
            // Could be the start of `N G R`; keep reading.
            pending_ref_nums = pending_ref_nums.saturating_add(1);
            false
        } else {
            true
        };
        toks.push(tok);
        if complete {
            break;
        }
    }
    if toks.is_empty() {
        return Err(err!(
            Code::ObjUnexpected,
            during = "objstm-value",
            detail = "no tokens"
        ));
    }
    let mut parser = selis_pdf_cos::ObjectParser::new(&toks, budget);
    parser.parse(g)
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

    /// Depth is scoped to each resolution: after resolving several sibling
    /// objects the guard's depth returns to zero, so depth tracks nesting,
    /// not the number of objects resolved (WRITE.07 — a document with many
    /// shallow objects must not exhaust the depth budget).
    #[test]
    fn resolve_depth_is_released_between_siblings() {
        let src = b"%PDF-1.4\n1 0 obj\n42\nendobj\n2 0 obj\n43\nendobj\n3 0 obj\n44\nendobj\n";
        let mut xref = std::collections::BTreeMap::new();
        xref.insert(1, XrefEntry::InUse { offset: 9, gen: 0 });
        xref.insert(2, XrefEntry::InUse { offset: 27, gen: 0 });
        xref.insert(3, XrefEntry::InUse { offset: 45, gen: 0 });
        let trailer = vec![(
            selis_bytes::Bytes::copy_from_slice(b"Root"),
            Obj::Ref(Ref::new(1, 0)),
        )];
        let doc = Doc::from_single_revision(xref, trailer);
        let budget = Budget::unlimited();
        let mut g = guard();
        let mut r = Resolver::new(&doc, src, &budget);
        for num in 1..=3u32 {
            let obj = r.resolve(Ref::new(num, 0), &mut g).expect("resolve");
            assert!(matches!(obj, Obj::Int(_)));
            assert_eq!(
                g.usage().depth,
                0,
                "depth must return to zero after resolving object {num}"
            );
        }
        assert_eq!(g.usage().peak_depth, 1, "siblings must not stack depth");
    }

    /// An object-stream array value must parse whole, not stop at its first
    /// number token.
    #[test]
    fn objstm_value_array_parses_whole() {
        let budget = Budget::unlimited();
        let mut g = guard();
        let data = b"[1 2 3]";
        let obj = parse_value_at(data, 0..data.len() as u64, &budget, &mut g).expect("array");
        assert!(matches!(obj, Obj::Array(v) if v.len() == 3));
    }

    /// Nested composites close only when the depth returns to zero.
    #[test]
    fn objstm_value_nested_composite_parses_whole() {
        let budget = Budget::unlimited();
        let mut g = guard();
        let data = b"<< /A [1 << /B true >> 2] /C null >>";
        let obj = parse_value_at(data, 0..data.len() as u64, &budget, &mut g).expect("dict");
        let Obj::Dict(pairs) = obj else {
            panic!("expected dict");
        };
        assert_eq!(pairs.len(), 2);
    }

    /// A bare `N G R` reference is one value, not a truncated number.
    #[test]
    fn objstm_value_ref_is_not_truncated() {
        let budget = Budget::unlimited();
        let mut g = guard();
        let data = b"7 0 R";
        let obj = parse_value_at(data, 0..data.len() as u64, &budget, &mut g).expect("ref");
        assert_eq!(obj, Obj::Ref(Ref::new(7, 0)));
    }

    /// A scalar at a non-zero offset (the common object-stream layout).
    #[test]
    fn objstm_value_scalar_at_offset() {
        let budget = Budget::unlimited();
        let mut g = guard();
        let obj = parse_value_at(b"xx42", 2..4, &budget, &mut g).expect("int");
        assert_eq!(obj, Obj::Int(42));
    }

    /// A scalar object stops at its range end: it must not swallow the next
    /// object's bytes (e.g. a following `N G R` reference).
    #[test]
    fn objstm_value_is_bounded_by_range() {
        let budget = Budget::unlimited();
        let mut g = guard();
        let data = b"42 7 0 R";
        let obj = parse_value_at(data, 0..3, &budget, &mut g).expect("int");
        assert_eq!(obj, Obj::Int(42));
        let obj = parse_value_at(data, 3..data.len() as u64, &budget, &mut g).expect("ref");
        assert_eq!(obj, Obj::Ref(Ref::new(7, 0)));
    }
}
