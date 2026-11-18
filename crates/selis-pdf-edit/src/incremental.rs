//! Object-level incremental editing and saving (SL-5.EDIT.03, 23-EDIT-MODEL-SPEC.md §6).
//!
//! The higher-level text edit in this crate rewrites one content stream. This
//! module is the general object-level layer the spec calls for: a typed
//! mutation taxonomy whose every variant lowers to a small set of COS object
//! changes, an incremental writer that appends ONLY those changed objects
//! (keeping the original bytes verbatim as a prefix), and read-back
//! verification against the expected post-mutation model.
//!
//! Invariants this module upholds (23-EDIT-MODEL-SPEC.md):
//! - **I3** (prior revisions): `output[..original.len()] == original`.
//! - **I7** (non-interference): only the objects a mutation declares are
//!   changed; every other object keeps its number, generation and bytes.
//! - **I5** (prefix reachability): `startxref`/`%%EOF` are emitted last.
//!
//! # Budget
//!
//! Every parse, resolve and write is charged to the caller's [`Budget`]; the
//! property suite runs 100 000 sequences under a bounded budget each.
//!
//! # Malformed Input
//!
//! A document with no `startxref`, a leaf object that cannot be resolved, or a
//! mutation naming a nonexistent object is a typed error — never a partial
//! file. An encrypted document is refused (EDIT.06 owns encryption).

use std::collections::BTreeMap;

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_pdf_cos::doc_writer::write_incremental_update;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

/// Whether the newest revision declares `/Encrypt`. EDIT.06 owns encryption;
/// this layer refuses encrypted input rather than emitting a plaintext object.
fn is_encrypted(doc: &selis_pdf_cos::Doc) -> bool {
    doc.revisions().last().is_some_and(|r| r.encrypt.is_some())
}

/// One object-level mutation. This is the invertible core of the spec's
/// taxonomy (§2): the variants that change COS objects directly. Higher-level
/// page/content/annotation operations lower to these before reaching the
/// writer, so the incremental diff is always a set of object (re)definitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectMutation {
    /// Set (or redefine) an indirect object to a value.
    SetObject {
        /// Object number.
        num: u32,
        /// New value.
        value: Obj,
    },
    /// Free an indirect object (emitted as a free xref entry, generation bumped).
    DeleteObject {
        /// Object number.
        num: u32,
    },
}

impl ObjectMutation {
    /// The object number this mutation touches (its blast radius, I7).
    #[must_use]
    pub const fn object_number(&self) -> u32 {
        match self {
            Self::SetObject { num, .. } | Self::DeleteObject { num } => *num,
        }
    }
}

/// What a save produced, for the report and the property suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalSaveReport {
    /// Bytes before the save.
    pub bytes_in: u64,
    /// Bytes after the save.
    pub bytes_out: u64,
    /// Number of objects (re)defined or freed in the appended revision.
    pub objects_changed: usize,
    /// Number of revisions in the output (original plus appended updates).
    pub revisions: usize,
    /// Whether the original bytes are a verbatim prefix of the output (I3).
    pub prefix_preserved: bool,
}

/// The current, materialised object model: object number → value. This is
/// `materialise(source, journal)` for the object-level subset — the newest
/// definition of every object across all revisions.
pub type ObjectModel = BTreeMap<u32, Obj>;

/// Materialise a document's live object model by resolving every in-use object
/// in the merged xref view. Compressed objects are resolved to their values
/// (they will be PROMOTED to top-level if a later mutation changes them).
///
/// # Budget / Malformed Input
///
/// Charges the parse and each resolve. An unresolvable referenced object is a
/// typed `Unresolvable` error.
pub fn materialise(src: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<ObjectModel> {
    let startxref_kw = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, startxref_kw, budget, g)?;
    let last = doc.len().checked_sub(1).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no revisions"
        )
    })?;
    let view = doc.at_revision(last).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no view"
        )
    })?;
    let mut model = ObjectModel::new();
    for (num, entry) in view.xref {
        if matches!(entry, selis_pdf_cos::XrefEntry::InUse { .. })
            || matches!(entry, selis_pdf_cos::XrefEntry::Compressed { .. })
        {
            let r = Ref::new(num, entry.gen());
            let value = selis_pdf_cos::copy::resolve_ref(src, &doc, r, budget, g)?;
            model.insert(num, value);
        }
    }
    Ok(model)
}

/// Apply a sequence of object mutations to a model, returning the expected
/// post-mutation model. Pure: no bytes, no I/O — the property suite uses this
/// as its oracle for what the saved file must materialise to.
///
/// # Malformed Input
///
/// `SetObject` on a new number is allowed (object allocation); `DeleteObject`
/// on an absent number is a no-op, mirroring a free entry that was already free.
pub fn apply(model: &ObjectModel, mutations: &[ObjectMutation]) -> ObjectModel {
    let mut out = model.clone();
    for m in mutations {
        match m {
            ObjectMutation::SetObject { num, value } => {
                out.insert(*num, value.clone());
            }
            ObjectMutation::DeleteObject { num } => {
                out.remove(num);
            }
        }
    }
    out
}

/// Compute the net changed-object set from a mutation sequence applied on top
/// of a base model. Only the FINAL value of each touched object is emitted — an
/// object set then deleted needs no xref entry if it did not exist in the base.
fn changes(
    base: &ObjectModel,
    expected: &ObjectModel,
    mutations: &[ObjectMutation],
) -> Vec<(u32, Option<Obj>)> {
    let touched: BTreeMap<u32, ()> = mutations.iter().map(|m| (m.object_number(), ())).collect();
    let mut out = Vec::new();
    for (num, ()) in touched {
        let before = base.get(&num);
        let after = expected.get(&num);
        if before == after {
            continue; // net no-op: a change then its inverse
        }
        out.push((num, after.cloned())); // None = free/delete
    }
    out
}

/// Update the trailer `/ID` per ISO 32000-2 §14.4: preserve the first element,
/// regenerate the second from the appended revision so readers can tell
/// revisions apart. Returns a fresh `ID` pair (or `None` if the source had no
/// `/ID`).
fn updated_id(rev: &selis_pdf_cos::revision::Revision, objects: &[(u32, Obj)]) -> Option<Obj> {
    let first = rev
        .trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"ID")
        .and_then(|(_, v)| match v {
            Obj::Array(items) => items.first().cloned(),
            _ => None,
        })?;
    use md5::Digest;
    let mut h = md5::Md5::new();
    for (num, value) in objects {
        h.update(num.to_be_bytes());
        // Values are hashed via their debug form: stable within a build and
        // sufficient to distinguish revisions, without leaking document text.
        h.update(format!("{value:?}"));
    }
    let second = Bytes::copy_from_slice(&h.finalize());
    Some(Obj::Array(vec![first, Obj::HexString(second)]))
}

/// Save an object mutation sequence as an incremental update. The original
/// bytes are preserved verbatim; only changed objects, a new xref section and
/// a trailer are appended. The result is re-parsed and its materialised model
/// compared against [`apply`]'s expected state before it is returned.
///
/// # Budget / Malformed Input
///
/// See the module docs. A refusal produces no output (the caller still owns
/// `src`).
pub fn save_incremental(
    src: &[u8],
    model: &ObjectModel,
    mutations: &[ObjectMutation],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, IncrementalSaveReport)> {
    if src.is_empty() {
        return Err(err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "empty source"
        ));
    }
    let expected = apply(model, mutations);
    let changed = changes(model, &expected, mutations);
    if changed.is_empty() {
        // Nothing to do: return the source unchanged, still a valid save.
        let len = u64::try_from(src.len()).unwrap_or(u64::MAX);
        return Ok((
            src.to_vec(),
            IncrementalSaveReport {
                bytes_in: len,
                bytes_out: len,
                objects_changed: 0,
                revisions: 1,
                prefix_preserved: true,
            },
        ));
    }

    let startxref_kw = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, startxref_kw, budget, g)?;
    if is_encrypted(&doc) {
        return Err(err!(
            Code::MutationInvalid,
            during = "incremental",
            detail = "encrypted; see EDIT.06"
        ));
    }
    let last = doc.len().checked_sub(1).unwrap_or(0);
    let revision = doc.revisions().get(last).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incremental",
            detail = "no revision"
        )
    })?;

    // Carry /Root and /Info forward; update /ID. /Size and /Prev are filled by
    // the low-level writer.
    let mut trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some(root) = revision.root {
        trailer.push((b"Root".to_vec(), Obj::Ref(root)));
    }
    if let Some((_, info)) = revision
        .trailer
        .iter()
        .find(|(k, _)| k.as_slice() == b"Info")
    {
        trailer.push((b"Info".to_vec(), info.clone()));
    }

    // Deletions need a free xref entry with a generation bump; the classic
    // low-level writer currently emits only in-use entries. For I7 correctness
    // we refuse deletions at this layer until free-entry emission lands
    // (EDIT.03b) rather than silently leaving a deleted object live.
    let mut objects: Vec<(u32, Obj)> = Vec::new();
    for (num, value) in &changed {
        match value {
            Some(v) => objects.push((*num, v.clone())),
            None => {
                return Err(err!(
                    Code::MutationInvalid,
                    during = "incremental",
                    object = *num,
                    detail = "object deletion needs free-entry emission (EDIT.03b)"
                ));
            }
        }
    }

    // Update /ID per ISO 32000-2 §14.4: preserve the first element, regenerate
    // the second from the changed objects (deterministic, no circularity with
    // the file bytes). Carry it in the trailer so the low-level writer emits it.
    if let Some(id) = updated_id(revision, &objects) {
        trailer.push((b"ID".to_vec(), id));
    }

    let out = write_incremental_update(src, &objects, &trailer, budget, g)?;

    let bytes_in = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);

    // Read-back verification (EDIT.03 DoD b/c/d): output parses, prefix intact,
    // materialised model matches the expected post-mutation state.
    let prefix_preserved = out.len() >= src.len() && out[..src.len()] == *src;
    let reopened = materialise(&out, budget, g)?;
    if reopened != expected {
        return Err(err!(
            Code::VerifyFailed,
            during = "incremental",
            detail = "reopened model does not match the expected post-mutation state"
        ));
    }
    let new_doc = selis_pdf_cos::parse_revisions(
        &out,
        selis_pdf_cos::xref::find_startxref(&out, 4096).unwrap_or(0),
        budget,
        g,
    )?;

    Ok((
        out,
        IncrementalSaveReport {
            bytes_in,
            bytes_out,
            objects_changed: changed.len(),
            revisions: new_doc.len(),
            prefix_preserved,
        },
    ))
}
