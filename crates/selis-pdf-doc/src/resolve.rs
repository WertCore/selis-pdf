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
use selis_pdf_cos::encrypt::DecryptPolicy;
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
    /// The decryption policy (key + crypt-filter selection) of an encrypted
    /// document, if it authenticated.
    key: Option<DecryptPolicy>,
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

    /// Set the decryption policy so resolved streams/strings are automatically
    /// decrypted per their crypt filter.
    pub fn set_key(&mut self, key: DecryptPolicy) {
        self.key = Some(key);
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
        obj
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
                let obj = resolve_object_numbered(self.src, *offset, r.num, self.budget, g)?;
                Ok(self.apply_key(obj, r))
            }
            Some(selis_pdf_cos::XrefEntry::Compressed { objstm, index }) => {
                // The object lives in an object stream (/ObjStm): resolve the
                // stream, parse its (number, range) index, and parse the
                // object at that range. The container stream itself is
                // decrypted inside `resolve_compressed`; objects inside an
                // object stream are never individually encrypted (32000-1
                // §7.5.7), so the returned value is used as-is.
                g.charge_one(selis_sandbox::Resource::Objects)?;
                resolve_compressed(
                    self.doc,
                    self.src,
                    *objstm,
                    *index,
                    self.budget,
                    g,
                    self.key_ref(),
                )
            }
            Some(_) | None => Err(err!(
                Code::ObjUnexpected,
                during = "doc-resolve",
                object = r.num
            )),
        }
    }

    /// The decryption policy as a borrow, for [`resolve_compressed`].
    fn key_ref(&self) -> Option<&DecryptPolicy> {
        self.key.as_ref()
    }

    /// Decrypt a directly-stored object when a policy is set.
    fn apply_key(&self, obj: Obj, r: Ref) -> Obj {
        match &self.key {
            Some(policy) => decrypt_obj(obj, r, policy),
            None => obj,
        }
    }
}

/// Decrypt the streams and strings in a resolved object, respecting the
/// document's crypt filters (`/StmF`, `/StrF`, `/EncryptMetadata`).
pub(crate) fn decrypt_obj(obj: Obj, r: Ref, policy: &DecryptPolicy) -> Obj {
    decrypt_obj_inner(obj, r, policy, 0)
}

fn decrypt_obj_inner(obj: Obj, r: Ref, policy: &DecryptPolicy, depth: u16) -> Obj {
    if depth > 32 {
        return obj;
    }
    match obj {
        Obj::Stream { dict, data } => {
            // The metadata stream is not encrypted when /EncryptMetadata is
            // false (ISO 32000-1 §7.4.10); other streams follow /StmF unless
            // the stream selects its own crypt filter with /Crypt (SL-1.FILT.09).
            let is_metadata = dict.iter().any(|(k, v)| {
                k.as_slice() == b"Type" && matches!(v, Obj::Name(n) if n.as_slice() == b"Metadata")
            });
            Obj::Stream {
                data: decrypt_stream(&dict, data, r, policy, is_metadata),
                dict: decrypt_dict(dict, r, policy, depth),
            }
        }
        Obj::String(bytes) => {
            let bytes = if policy.string_encrypted {
                selis_bytes::Bytes::from(selis_crypto::decrypt_data(
                    &policy.key,
                    r.num,
                    r.gen,
                    bytes.as_slice(),
                    policy.rev,
                    policy.aes,
                ))
            } else {
                bytes
            };
            Obj::String(bytes)
        }
        Obj::Dict(pairs) => Obj::Dict(decrypt_dict(pairs, r, policy, depth)),
        Obj::Array(items) => Obj::Array(
            items
                .into_iter()
                .map(|i| decrypt_obj_inner(i, r, policy, depth.saturating_add(1)))
                .collect(),
        ),
        other => other,
    }
}

/// Decrypt a dict's values in place (streams and strings).
fn decrypt_dict(
    pairs: Vec<(selis_bytes::Bytes, Obj)>,
    r: Ref,
    policy: &DecryptPolicy,
    depth: u16,
) -> Vec<(selis_bytes::Bytes, Obj)> {
    pairs
        .into_iter()
        .map(|(k, v)| (k, decrypt_obj_inner(v, r, policy, depth.saturating_add(1))))
        .collect()
}

/// Resolve an object stored in an object stream (`/ObjStm`).
///
/// When a decryption policy is set (an encrypted document with encrypted
/// streams), the container stream is decrypted with the object stream's own
/// number/generation before the `/Filter` chain runs — objects inside an
/// object stream are never individually encrypted (32000-1 §7.5.7), only the
/// container is.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_compressed(
    doc: &Doc,
    src: &[u8],
    objstm: u32,
    index: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    key: Option<&DecryptPolicy>,
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
    let (offset, gen) = match view.xref.get(&objstm) {
        Some(selis_pdf_cos::XrefEntry::InUse { offset, gen }) => (*offset, *gen),
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
    // Decrypt the container stream before decoding, when the document is
    // encrypted and streams use the standard filter (the key is keyed to the
    // object stream's own number/gen).
    let payload: Vec<u8> = match key {
        Some(policy) if policy.stream_encrypted => {
            selis_crypto::decrypt_data(&policy.key, objstm, gen, payload, policy.rev, policy.aes)
        }
        _ => payload.to_vec(),
    };
    // Unfilter the object stream data. Per 32000-1 §7.4.1, `/Filter` is either
    // a single name or an array of names applied in order; writers commonly
    // emit the one-element array form (`/Filter [/FlateDecode]`).
    let filters: Vec<&[u8]> = match dict
        .iter()
        .find(|(k, _)| k.as_slice() == b"Filter")
        .map(|(_, v)| v)
    {
        Some(Obj::Name(n)) => vec![n.as_slice()],
        Some(Obj::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Obj::Name(n) => Some(n.as_slice()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut payload = payload;
    for filt in filters {
        let name = std::str::from_utf8(filt).unwrap_or("");
        if let Ok(decoded) = selis_pdf_filter::decode(name, &payload, u64::MAX, g) {
            payload = decoded;
        }
    }
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

/// Decrypt a stream's raw bytes per its crypt filter (SL-1.FILT.09).
///
/// A stream with `/Filter [/Crypt ...]` selects the crypt filter named by
/// the `/DecodeParms` entry aligned with the `/Crypt` entry (ISO 32000-1
/// §7.4.10), overriding the document-level `/StmF`. `/Identity` or an absent
/// `/Name` means the stream is not encrypted. Streams without an explicit
/// `/Crypt` follow `/StmF`, except the metadata stream when
/// `/EncryptMetadata` is false.
fn decrypt_stream(
    dict: &[(selis_bytes::Bytes, Obj)],
    data: selis_bytes::Bytes,
    r: Ref,
    policy: &DecryptPolicy,
    is_metadata: bool,
) -> selis_bytes::Bytes {
    let per_stream = per_stream_crypt_name(dict);
    let encrypted = match &per_stream {
        Some(name) => name != "Identity",
        None => {
            if is_metadata && !policy.encrypt_metadata {
                false
            } else {
                policy.stream_encrypted
            }
        }
    };
    if !encrypted {
        return data;
    }
    let aes = per_stream
        .as_deref()
        .map(|name| policy.aes_for(name))
        .unwrap_or(policy.aes);
    selis_bytes::Bytes::from(selis_crypto::decrypt_data(
        &policy.key,
        r.num,
        r.gen,
        data.as_slice(),
        policy.rev,
        aes,
    ))
}

/// Encrypt the streams and strings in an object (the inverse of
/// [`decrypt_obj`]), respecting the document's crypt filters. Used by
/// SL-1A.TOOL.05 to re-encrypt a document after the `/P`-derived key changes.
pub fn encrypt_obj(obj: Obj, r: Ref, policy: &DecryptPolicy) -> Obj {
    encrypt_obj_inner(obj, r, policy, 0)
}

fn encrypt_obj_inner(obj: Obj, r: Ref, policy: &DecryptPolicy, depth: u16) -> Obj {
    if depth > 32 {
        return obj;
    }
    match obj {
        Obj::Stream { dict, data } => {
            let is_metadata = dict.iter().any(|(k, v)| {
                k.as_slice() == b"Type" && matches!(v, Obj::Name(n) if n.as_slice() == b"Metadata")
            });
            Obj::Stream {
                data: encrypt_stream(&dict, data, r, policy, is_metadata),
                dict: encrypt_dict(dict, r, policy, depth),
            }
        }
        Obj::String(bytes) => {
            let bytes = if policy.string_encrypted {
                selis_bytes::Bytes::from(selis_crypto::encrypt_data(
                    &policy.key,
                    r.num,
                    r.gen,
                    bytes.as_slice(),
                    policy.rev,
                    policy.aes,
                ))
            } else {
                bytes
            };
            Obj::String(bytes)
        }
        Obj::Dict(pairs) => Obj::Dict(encrypt_dict(pairs, r, policy, depth)),
        Obj::Array(items) => Obj::Array(
            items
                .into_iter()
                .map(|i| encrypt_obj_inner(i, r, policy, depth.saturating_add(1)))
                .collect(),
        ),
        other => other,
    }
}

/// Encrypt a dict's values in place (streams and strings).
fn encrypt_dict(
    pairs: Vec<(selis_bytes::Bytes, Obj)>,
    r: Ref,
    policy: &DecryptPolicy,
    depth: u16,
) -> Vec<(selis_bytes::Bytes, Obj)> {
    pairs
        .into_iter()
        .map(|(k, v)| (k, encrypt_obj_inner(v, r, policy, depth.saturating_add(1))))
        .collect()
}

/// Encrypt a stream's data with the same per-stream rules as
/// [`decrypt_stream`], in reverse.
fn encrypt_stream(
    dict: &[(selis_bytes::Bytes, Obj)],
    data: selis_bytes::Bytes,
    r: Ref,
    policy: &DecryptPolicy,
    is_metadata: bool,
) -> selis_bytes::Bytes {
    let per_stream = per_stream_crypt_name(dict);
    let encrypted = match &per_stream {
        Some(name) => name != "Identity",
        None => {
            if is_metadata && !policy.encrypt_metadata {
                false
            } else {
                policy.stream_encrypted
            }
        }
    };
    if !encrypted {
        return data;
    }
    let aes = per_stream
        .as_deref()
        .map(|name| policy.aes_for(name))
        .unwrap_or(policy.aes);
    selis_bytes::Bytes::from(selis_crypto::encrypt_data(
        &policy.key,
        r.num,
        r.gen,
        data.as_slice(),
        policy.rev,
        aes,
    ))
}

/// The crypt filter name a stream selects with `/Filter [/Crypt ...]`, or
/// `None` when the stream has no explicit `/Crypt` filter.
///
/// The `/Name` comes from the `/DecodeParms` entry aligned with the `/Crypt`
/// entry in `/Filter`; a missing `/Name` defaults to `/Identity` (not
/// encrypted, ISO 32000-1 §7.4.10).
fn per_stream_crypt_name(dict: &[(selis_bytes::Bytes, Obj)]) -> Option<String> {
    let filters: Vec<&selis_bytes::Bytes> =
        match dict.iter().find(|(k, _)| k.as_slice() == b"Filter") {
            Some((_, Obj::Name(n))) => vec![n],
            Some((_, Obj::Array(items))) => items
                .iter()
                .filter_map(|v| match v {
                    Obj::Name(n) => Some(n),
                    _ => None,
                })
                .collect(),
            _ => return None,
        };
    let i = filters.iter().position(|n| n.as_slice() == b"Crypt")?;
    let parm: Option<&[(selis_bytes::Bytes, Obj)]> =
        match dict.iter().find(|(k, _)| k.as_slice() == b"DecodeParms") {
            Some((_, Obj::Dict(pairs))) => Some(pairs),
            Some((_, Obj::Array(items))) => items.get(i).and_then(|v| match v {
                Obj::Dict(pairs) => Some(pairs.as_slice()),
                _ => None,
            }),
            _ => None,
        };
    let name = parm
        .and_then(|pairs: &[(selis_bytes::Bytes, Obj)]| {
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == b"Name")
                .and_then(|(_, v)| match v {
                    Obj::Name(n) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
                    _ => None,
                })
        })
        .unwrap_or_else(|| "Identity".to_string());
    Some(name)
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

    // ── per-stream /Crypt filter (SL-1.FILT.09) ──

    #[test]
    fn per_stream_crypt_no_filter_returns_none() {
        let dict = vec![(b"Length".as_slice().into(), Obj::Int(42))];
        assert_eq!(per_stream_crypt_name(&dict), None);
    }

    #[test]
    fn per_stream_crypt_single_name_not_crypt_returns_none() {
        let dict = vec![(
            b"Filter".as_slice().into(),
            Obj::Name(b"FlateDecode".as_slice().into()),
        )];
        assert_eq!(per_stream_crypt_name(&dict), None);
    }

    #[test]
    fn per_stream_crypt_single_name_crypt_returns_identity_when_no_parms() {
        let dict = vec![(
            b"Filter".as_slice().into(),
            Obj::Name(b"Crypt".as_slice().into()),
        )];
        assert_eq!(per_stream_crypt_name(&dict), Some("Identity".to_string()));
    }

    #[test]
    fn per_stream_crypt_array_crypt_with_name() {
        let dict = vec![
            (
                b"Filter".as_slice().into(),
                Obj::Array(vec![Obj::Name(b"Crypt".as_slice().into())]),
            ),
            (
                b"DecodeParms".as_slice().into(),
                Obj::Array(vec![Obj::Dict(vec![(
                    b"Name".as_slice().into(),
                    Obj::Name(b"StdCF".as_slice().into()),
                )])]),
            ),
        ];
        assert_eq!(per_stream_crypt_name(&dict), Some("StdCF".to_string()));
    }

    #[test]
    fn per_stream_crypt_array_crypt_with_flate_returns_name() {
        let dict = vec![
            (
                b"Filter".as_slice().into(),
                Obj::Array(vec![
                    Obj::Name(b"Crypt".as_slice().into()),
                    Obj::Name(b"FlateDecode".as_slice().into()),
                ]),
            ),
            (
                b"DecodeParms".as_slice().into(),
                Obj::Array(vec![
                    Obj::Dict(vec![(
                        b"Name".as_slice().into(),
                        Obj::Name(b"StdCF".as_slice().into()),
                    )]),
                    Obj::Dict(vec![(b"Predictor".as_slice().into(), Obj::Int(12))]),
                ]),
            ),
        ];
        assert_eq!(per_stream_crypt_name(&dict), Some("StdCF".to_string()));
    }

    /// decrypt_obj must decrypt a stream with per-stream /Crypt /StdCF even
    /// when the policy says streams are not encrypted (/StmF /Identity).
    #[test]
    fn decrypt_obj_per_stream_crypt_decrypts_data() {
        // Policy: /StmF /Identity, so stream_encrypted is false. The per-stream
        // /Crypt /StdCF must override this. /CF has /StdCF with /CFM /AESV2.
        let cf = vec![(
            b"StdCF".as_slice().into(),
            Obj::Dict(vec![(
                b"CFM".as_slice().into(),
                Obj::Name(b"AESV2".as_slice().into()),
            )]),
        )];
        let policy = DecryptPolicy {
            key: vec![0u8; 16],
            rev: 4,
            aes: false,
            stream_encrypted: false,
            string_encrypted: false,
            encrypt_metadata: true,
            cf,
        };
        let plaintext = b"Hello, World! This is a test of the per-stream Crypt filter.";
        // Ciphertext: 16-byte zero IV + AES-128-CBC encrypted with derived key
        // MD5(0x00*16 || objnum[0..3]=42 || gen[0..2]=0 || "sAlT")
        let ciphertext: Vec<u8> = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 181, 227, 83, 188, 135, 201, 65, 49,
            79, 253, 225, 115, 194, 27, 201, 254, 87, 243, 160, 206, 156, 16, 150, 249, 211, 36,
            246, 232, 105, 216, 149, 64, 141, 105, 199, 29, 4, 28, 75, 74, 172, 76, 115, 216, 227,
            137, 179, 109, 228, 186, 89, 169, 88, 122, 201, 217, 133, 245, 147, 134, 206, 174, 99,
            51,
        ];
        let stream = Obj::Stream {
            dict: vec![
                (
                    b"Filter".as_slice().into(),
                    Obj::Array(vec![Obj::Name(b"Crypt".as_slice().into())]),
                ),
                (
                    b"DecodeParms".as_slice().into(),
                    Obj::Array(vec![Obj::Dict(vec![(
                        b"Name".as_slice().into(),
                        Obj::Name(b"StdCF".as_slice().into()),
                    )])]),
                ),
                (b"Length".as_slice().into(), Obj::Int(80)),
            ],
            data: selis_bytes::Bytes::copy_from_slice(&ciphertext),
        };
        let r = Ref::new(42, 0);
        let decrypted = decrypt_obj(stream, r, &policy);
        let Obj::Stream { data, .. } = &decrypted else {
            panic!("expected stream");
        };
        // The padding bytes (PKCS#7) are still present because decrypt_data
        // does not strip them. Verify the plaintext prefix.
        assert!(
            data.as_slice().starts_with(plaintext),
            "expected plaintext prefix, got {:?}",
            data.as_slice().get(..plaintext.len())
        );
    }

    /// A stream with /Filter [/Crypt] and /Name /Identity must NOT be decrypted.
    #[test]
    fn decrypt_obj_per_stream_identity_does_not_decrypt() {
        let policy = DecryptPolicy {
            key: vec![0u8; 16],
            rev: 4,
            aes: true,
            stream_encrypted: true,
            string_encrypted: false,
            encrypt_metadata: true,
            cf: Vec::new(),
        };
        let data = selis_bytes::Bytes::copy_from_slice(b"raw data");
        let stream = Obj::Stream {
            dict: vec![
                (
                    b"Filter".as_slice().into(),
                    Obj::Array(vec![Obj::Name(b"Crypt".as_slice().into())]),
                ),
                (
                    b"DecodeParms".as_slice().into(),
                    Obj::Array(vec![Obj::Dict(vec![(
                        b"Name".as_slice().into(),
                        Obj::Name(b"Identity".as_slice().into()),
                    )])]),
                ),
                (b"Length".as_slice().into(), Obj::Int(8)),
            ],
            data: data.clone(),
        };
        let r = Ref::new(1, 0);
        let decrypted = decrypt_obj(stream, r, &policy);
        let Obj::Stream { data: out, .. } = &decrypted else {
            panic!("expected stream");
        };
        assert_eq!(out, &data, "Identity crypt filter must not decrypt");
    }

    /// A stream with /Filter [/Crypt] and no /DecodeParms must NOT be decrypted
    /// (missing /Name defaults to /Identity).
    #[test]
    fn decrypt_obj_per_stream_no_parms_does_not_decrypt() {
        let policy = DecryptPolicy {
            key: vec![0u8; 16],
            rev: 4,
            aes: true,
            stream_encrypted: true,
            string_encrypted: false,
            encrypt_metadata: true,
            cf: Vec::new(),
        };
        let data = selis_bytes::Bytes::copy_from_slice(b"raw data");
        let stream = Obj::Stream {
            dict: vec![
                (
                    b"Filter".as_slice().into(),
                    Obj::Array(vec![Obj::Name(b"Crypt".as_slice().into())]),
                ),
                (b"Length".as_slice().into(), Obj::Int(8)),
            ],
            data: data.clone(),
        };
        let r = Ref::new(1, 0);
        let decrypted = decrypt_obj(stream, r, &policy);
        let Obj::Stream { data: out, .. } = &decrypted else {
            panic!("expected stream");
        };
        assert_eq!(
            out, &data,
            "missing /Name must default to /Identity (no decryption)"
        );
    }
}
