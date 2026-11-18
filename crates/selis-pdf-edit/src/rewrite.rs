//! Full-rewrite writer (`save_rewritten`, SL-5.EDIT.05).
//!
//! [`crate::incremental::save_incremental`] keeps the source bytes as a prefix.
//! This module is the **explicit exception** (23-EDIT-MODEL-SPEC.md §7, I2):
//! it builds a fresh single-revision document from the objects reachable from
//! the trailer roots, renumbers them from 1, and drops everything unreachable.
//! It is the path redaction and "clean save" need, and it is **never** chosen
//! implicitly.
//!
//! The rewrite is verification-gated. Nothing is returned to the caller until
//! the rewritten bytes have been reparsed and compared against the pre-save
//! document with [`selis_pdf_cos::verify::verify_structural`], plus a structural
//! fingerprint that counts structure-tree nodes and hashes every reachable
//! stream payload (so changed page content / text cannot slip through). If any
//! count differs, a typed [`RewriteDiscrepancy`] is returned and no bytes are
//! produced. [`save_rewritten_atomic`] then commits those verified bytes with
//! [`selis_io::commit_atomic`] — temp in the destination directory, fsync,
//! atomic rename, directory fsync — so a refused rewrite leaves the destination
//! byte-identical.
//!
//! # Pre-G2 verification scope
//!
//! Render-and-compare is not available until the renderer exists. Per
//! `23-EDIT-MODEL-SPEC.md §7` the comparison here is the accepted *structural*
//! standard: reparse, reference closure, page-tree consistency, count
//! expectations, structure nodes and a stream-payload digest. It must not be
//! reused for an operation that deliberately rewrites page content (that is
//! SL-1A.WRITE.08). Streams are copied byte-for-byte by this rewrite, so the
//! digest is an equality check, not a render.
//!
//! # Budget
//!
//! Every parse, resolve, serialisation and survey is charged to the caller's
//! [`Budget`] / [`BudgetGuard`]; the commit checks `tick()` before any I/O.
//!
//! # Malformed Input
//!
//! An unparseable source, an encrypted source (EDIT.06 owns encryption), an
//! object whose new number cannot be resolved, or any verification discrepancy
//! is a typed error. No bytes and no file are produced on any refusal.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use selis_bytes::Bytes;
use selis_error::{err, Code, Error, Result};
use selis_pdf_cos::copy::{collect_objects_with_doc, resolve_ref};
use selis_pdf_cos::verify::{verify_structural, Expectations, Fault, Observed};
use selis_pdf_cos::{Obj, Ref, XrefEntry};
use selis_sandbox::{Budget, BudgetGuard};

pub use selis_io::commit::{CommitFault as RewriteCommitFault, CommitReceipt as RewriteReceipt};

/// Maximum nesting followed while fingerprinting an object graph. Mirrors the
/// verifier's own walk cap; deeper graphs are refused rather than recursed into
/// without bound.
const MAX_FINGERPRINT_DEPTH: usize = 64;

/// A deliberately corrupted rewrite, for the DoD negative tests only.
///
/// Production callers use [`RewriteOptions::default`], which sets no fault.
/// Each variant produces a genuinely different object graph that the
/// verification gate must refuse; the fault is applied *after* renumbering and
/// *before* verification and commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteFault {
    /// Remove the first leaf from the page tree and decrement its `/Count`,
    /// simulating a rewrite that lost a page.
    DropFirstPage,
    /// Remove every `/Annots` entry from every page, simulating a rewrite that
    /// lost its annotations.
    DropAnnotations,
}

/// Options for [`save_rewritten_with`] / [`save_rewritten_atomic_with`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RewriteOptions {
    /// Corrupt the built object graph before verification (test hook).
    pub fault: Option<RewriteFault>,
    /// Deterministic commit fault, matching EDIT.04's kill-test hook.
    pub commit_fault: Option<RewriteCommitFault>,
}

/// The structural counts compared between the pre-save and rewritten document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewriteVerification {
    /// Walked leaf page count.
    pub pages: u64,
    /// Total `/Annots` items across all pages.
    pub annotations: u64,
    /// Recursive form-field count under `/AcroForm`.
    pub fields: u64,
    /// Optional-content group count.
    pub ocgs: u64,
    /// Outline entry count.
    pub outlines: u64,
    /// Embedded-file entry count.
    pub embedded_files: u64,
    /// Structure-tree element nodes reachable from `/Root`.
    pub structure_nodes: u64,
    /// Order-independent FNV digest over every reachable stream payload
    /// (page content is a stream, so this is the pre-G2 text/proxy check).
    pub content_digest: u64,
}

impl From<&Fingerprint> for RewriteVerification {
    fn from(fp: &Fingerprint) -> Self {
        Self {
            pages: 0,
            annotations: 0,
            fields: 0,
            ocgs: 0,
            outlines: 0,
            embedded_files: 0,
            structure_nodes: fp.structure_nodes,
            content_digest: fp.content_digest,
        }
    }
}

impl RewriteVerification {
    /// Overlay the counts observed by `verify_structural` onto the fingerprint.
    fn with_observed(mut self, observed: &Observed) -> Self {
        self.pages = observed.pages;
        self.annotations = observed.annotations;
        self.fields = observed.fields;
        self.ocgs = observed.ocgs;
        self.outlines = observed.outlines;
        self.embedded_files = observed.embedded_files;
        self
    }
}

/// A named discrepancy between the pre-save and rewritten document.
///
/// Carried in the typed [`Code::VerifyFailed`] error's detail so both callers
/// and tests can name exactly what refused the rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteDiscrepancy {
    /// The page count changed.
    Pages {
        /// Pages before the rewrite.
        before: u64,
        /// Pages after the rewrite.
        after: u64,
    },
    /// The annotation count changed.
    Annotations {
        /// Annotations before the rewrite.
        before: u64,
        /// Annotations after the rewrite.
        after: u64,
    },
    /// The form-field count changed.
    Fields {
        /// Fields before the rewrite.
        before: u64,
        /// Fields after the rewrite.
        after: u64,
    },
    /// The optional-content group count changed.
    Ocgs {
        /// OCGs before the rewrite.
        before: u64,
        /// OCGs after the rewrite.
        after: u64,
    },
    /// The outline entry count changed.
    Outlines {
        /// Outline entries before the rewrite.
        before: u64,
        /// Outline entries after the rewrite.
        after: u64,
    },
    /// The embedded-file count changed.
    EmbeddedFiles {
        /// Embedded files before the rewrite.
        before: u64,
        /// Embedded files after the rewrite.
        after: u64,
    },
    /// The structure-tree element count changed.
    StructureNodes {
        /// Structure nodes before the rewrite.
        before: u64,
        /// Structure nodes after the rewrite.
        after: u64,
    },
    /// A reachable stream payload changed (page content / text proxy).
    ContentDigest {
        /// FNV digest before the rewrite.
        before: u64,
        /// FNV digest after the rewrite.
        after: u64,
    },
    /// One of the two documents failed structural verification for a reason
    /// other than a count mismatch (e.g. an unresolvable reference).
    Fault(String),
}

impl fmt::Display for RewriteDiscrepancy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pages { before, after } => {
                write!(f, "page count changed: {before} -> {after}")
            }
            Self::Annotations { before, after } => {
                write!(f, "annotation count changed: {before} -> {after}")
            }
            Self::Fields { before, after } => {
                write!(f, "form-field count changed: {before} -> {after}")
            }
            Self::Ocgs { before, after } => {
                write!(
                    f,
                    "optional-content group count changed: {before} -> {after}"
                )
            }
            Self::Outlines { before, after } => {
                write!(f, "outline entry count changed: {before} -> {after}")
            }
            Self::EmbeddedFiles { before, after } => {
                write!(f, "embedded-file count changed: {before} -> {after}")
            }
            Self::StructureNodes { before, after } => {
                write!(f, "structure-tree node count changed: {before} -> {after}")
            }
            Self::ContentDigest { before, after } => {
                write!(
                    f,
                    "stream payload changed (content digest {before:x} -> {after:x})"
                )
            }
            Self::Fault(detail) => write!(f, "structural verification failed: {detail}"),
        }
    }
}

/// Counts computed while building the rewritten object graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuildInfo {
    objects_in: usize,
    objects_out: usize,
    objects_dropped: usize,
    objects_renumbered: usize,
}

/// What a rewrite produced, in addition to the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteReport {
    /// Input size in bytes.
    pub bytes_in: u64,
    /// Output size in bytes.
    pub bytes_out: u64,
    /// In-use objects in the source's merged xref view.
    pub objects_in: usize,
    /// Objects in the rewritten document (only reachable ones survive).
    pub objects_out: usize,
    /// How many source objects were unreachable and therefore dropped.
    pub objects_dropped: usize,
    /// How many objects received a new number.
    pub objects_renumbered: usize,
    /// The counts that were compared before the rewrite was accepted.
    pub verification: RewriteVerification,
}

/// What an atomic rewrite commit produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewriteCommitReport {
    /// Bytes placed in the destination.
    pub bytes: u64,
    /// Whether the containing directory was fsynced (POSIX yes; Windows no).
    pub directory_fsynced: bool,
    /// The counts that were compared before the commit was allowed.
    pub verification: RewriteVerification,
}

/// Turn a structural fault list into a single, printable detail string.
fn faults_detail(faults: &[Fault]) -> String {
    faults
        .iter()
        .map(|f| f.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Build `Expectations` from counts captured on the pre-save document.
fn expectations_from(observed: &Observed) -> Expectations {
    Expectations {
        pages: Some(observed.pages),
        annotations: Some(observed.annotations),
        fields: Some(observed.fields),
        ocgs: Some(observed.ocgs),
        outlines: Some(observed.outlines),
        embedded_files: Some(observed.embedded_files),
    }
}

fn discrepancy_error(discrepancy: RewriteDiscrepancy) -> Error {
    err!(
        Code::VerifyFailed,
        during = "rewrite",
        detail = format!("verification discrepancy: {discrepancy}")
    )
}

/// An FNV-1a 64-bit hash, used only to compare stream payloads. It is not a
/// security primitive: a collision cannot be engineered by a document this
/// code creates, and any difference is a safe (conservative) refusal.
fn fnv_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The extra structural state compared beyond `verify_structural`'s counts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    structure_nodes: u64,
    content_digest: u64,
}

/// Fold one object into the fingerprint: push its references, count structure
/// elements, and hash stream payloads. Bounded in nesting depth.
fn measure_obj(
    obj: &Obj,
    stack: &mut Vec<Ref>,
    structure_nodes: &mut u64,
    stream_hashes: &mut Vec<u64>,
    depth: usize,
) {
    if depth > MAX_FINGERPRINT_DEPTH {
        return;
    }
    match obj {
        Obj::Ref(r) => stack.push(*r),
        Obj::Array(items) => {
            for item in items {
                measure_obj(
                    item,
                    stack,
                    structure_nodes,
                    stream_hashes,
                    depth.saturating_add(1),
                );
            }
        }
        Obj::Dict(pairs) => {
            let is_struct_elem = pairs.iter().any(|(key, value)| {
                key.as_slice() == b"S"
                    || (key.as_slice() == b"Type"
                        && matches!(value, Obj::Name(name) if name.as_slice() == b"StructElem"))
            });
            if is_struct_elem {
                *structure_nodes = structure_nodes.saturating_add(1);
            }
            for (_, value) in pairs {
                measure_obj(
                    value,
                    stack,
                    structure_nodes,
                    stream_hashes,
                    depth.saturating_add(1),
                );
            }
        }
        Obj::Stream { dict, data } => {
            stream_hashes.push(fnv_bytes(0xcbf2_9ce4_8422_2325, data.as_slice()));
            for (_, value) in dict {
                measure_obj(
                    value,
                    stack,
                    structure_nodes,
                    stream_hashes,
                    depth.saturating_add(1),
                );
            }
        }
        _ => {}
    }
}

/// Compute the structure-node count and an order-independent digest of every
/// stream reachable from the newest trailer `/Root`.
fn fingerprint(doc_bytes: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<Fingerprint> {
    let start = selis_pdf_cos::xref::find_startxref(doc_bytes, 4096).ok_or_else(|| {
        err!(
            Code::VerifyFailed,
            during = "rewrite",
            detail = "fingerprint: no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(doc_bytes, start, budget, g)?;
    let root = doc
        .revisions()
        .last()
        .and_then(|rev| rev.root)
        .ok_or_else(|| {
            err!(
                Code::VerifyFailed,
                during = "rewrite",
                detail = "fingerprint: no /Root"
            )
        })?;

    let mut seen: BTreeSet<(u32, u16)> = BTreeSet::new();
    let mut stack: Vec<Ref> = vec![root];
    let mut structure_nodes = 0u64;
    let mut stream_hashes: Vec<u64> = Vec::new();
    while let Some(r) = stack.pop() {
        if !seen.insert((r.num, r.gen)) {
            continue;
        }
        let obj = resolve_ref(doc_bytes, &doc, r, budget, g)?;
        measure_obj(
            &obj,
            &mut stack,
            &mut structure_nodes,
            &mut stream_hashes,
            0,
        );
    }
    // Sort so object numbering (which the rewrite deliberately changes) cannot
    // affect the digest: only the multiset of stream payloads matters.
    stream_hashes.sort_unstable();
    let mut content_digest = 0xcbf2_9ce4_8422_2325u64;
    for hash in stream_hashes {
        content_digest = fnv_bytes(content_digest, &hash.to_be_bytes());
    }
    Ok(Fingerprint {
        structure_nodes,
        content_digest,
    })
}

/// Compare the pre-save and rewritten documents structurally, returning the
/// counts that matched. This is the verification gate: on any discrepancy a
/// typed [`Code::VerifyFailed`] error naming it is returned and the caller
/// produces no file.
///
/// # Errors
///
/// [`Code::VerifyFailed`] with a detail naming the first discrepancy, or a
/// propagated budget / cancellation / parse error. This function is read-only.
///
/// # Budget
///
/// Charges two `verify_structural` passes and two object-graph fingerprints.
///
/// # Malformed Input
///
/// Malformed *input* is a fault (and therefore a refusal), not a repair: a
/// document that does not verify before the rewrite is never rewritten.
pub fn verify_rewrite(
    before_bytes: &[u8],
    after_bytes: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<RewriteVerification> {
    let before = verify_structural(before_bytes, &Expectations::none(), budget, g)?;
    if !before.ok {
        return Err(discrepancy_error(RewriteDiscrepancy::Fault(format!(
            "pre-save document: {}",
            faults_detail(&before.faults)
        ))));
    }

    let expectations = expectations_from(&before.observed);
    let after = verify_structural(after_bytes, &expectations, budget, g)?;

    let discrepancy = if before.observed.pages != after.observed.pages {
        Some(RewriteDiscrepancy::Pages {
            before: before.observed.pages,
            after: after.observed.pages,
        })
    } else if before.observed.annotations != after.observed.annotations {
        Some(RewriteDiscrepancy::Annotations {
            before: before.observed.annotations,
            after: after.observed.annotations,
        })
    } else if before.observed.fields != after.observed.fields {
        Some(RewriteDiscrepancy::Fields {
            before: before.observed.fields,
            after: after.observed.fields,
        })
    } else if before.observed.ocgs != after.observed.ocgs {
        Some(RewriteDiscrepancy::Ocgs {
            before: before.observed.ocgs,
            after: after.observed.ocgs,
        })
    } else if before.observed.outlines != after.observed.outlines {
        Some(RewriteDiscrepancy::Outlines {
            before: before.observed.outlines,
            after: after.observed.outlines,
        })
    } else if before.observed.embedded_files != after.observed.embedded_files {
        Some(RewriteDiscrepancy::EmbeddedFiles {
            before: before.observed.embedded_files,
            after: after.observed.embedded_files,
        })
    } else {
        None
    };
    if let Some(discrepancy) = discrepancy {
        return Err(discrepancy_error(discrepancy));
    }

    let before_fp = fingerprint(before_bytes, budget, g)?;
    let after_fp = fingerprint(after_bytes, budget, g)?;
    if before_fp.structure_nodes != after_fp.structure_nodes {
        return Err(discrepancy_error(RewriteDiscrepancy::StructureNodes {
            before: before_fp.structure_nodes,
            after: after_fp.structure_nodes,
        }));
    }
    if before_fp.content_digest != after_fp.content_digest {
        return Err(discrepancy_error(RewriteDiscrepancy::ContentDigest {
            before: before_fp.content_digest,
            after: after_fp.content_digest,
        }));
    }

    // Counts and fingerprint match; any remaining fault is a structural fault
    // in the rewritten bytes (an unresolved reference, a bad page tree, …).
    if !after.ok {
        return Err(discrepancy_error(RewriteDiscrepancy::Fault(format!(
            "rewritten document: {}",
            faults_detail(&after.faults)
        ))));
    }

    Ok(RewriteVerification::from(&after_fp).with_observed(&after.observed))
}

/// Force a stream's `/Length` to its actual payload length. The rewrite copies
/// stream bytes verbatim; only this derived value is normalised.
fn normalise_stream_length(obj: &mut Obj) {
    if let Obj::Stream { dict, data } = obj {
        let length = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let value = Obj::Int(i64::try_from(length).unwrap_or(i64::MAX));
        if let Some((_, slot)) = dict.iter_mut().find(|(key, _)| key.as_slice() == b"Length") {
            *slot = value;
        } else {
            dict.push((Bytes::copy_from_slice(b"Length"), value));
        }
    }
}

/// Apply a deliberate corruption to the built object graph (test hook only).
fn apply_fault(objects: &mut [(u32, Obj)], root: Ref, fault: RewriteFault) {
    match fault {
        RewriteFault::DropFirstPage => {
            let pages_ref = objects
                .iter()
                .find(|(num, _)| *num == root.num)
                .and_then(|(_, obj)| match obj {
                    Obj::Dict(pairs) => pairs.iter().find(|(key, _)| key.as_slice() == b"Pages"),
                    _ => None,
                })
                .and_then(|(_, value)| match value {
                    Obj::Ref(r) => Some(*r),
                    _ => None,
                });
            let Some(pages_ref) = pages_ref else {
                return;
            };
            for (num, obj) in objects.iter_mut() {
                if *num != pages_ref.num {
                    continue;
                }
                if let Obj::Dict(pairs) = obj {
                    for (key, value) in pairs.iter_mut() {
                        match key.as_slice() {
                            b"Kids" => {
                                if let Obj::Array(kids) = value {
                                    if !kids.is_empty() {
                                        kids.remove(0);
                                    }
                                }
                            }
                            b"Count" => {
                                if let Obj::Int(count) = value {
                                    *count = count.saturating_sub(1);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        RewriteFault::DropAnnotations => {
            for (_, obj) in objects.iter_mut() {
                if let Obj::Dict(pairs) = obj {
                    let is_page = pairs.iter().any(|(key, value)| {
                        key.as_slice() == b"Type"
                            && matches!(value, Obj::Name(name) if name.as_slice() == b"Page")
                    });
                    if is_page {
                        pairs.retain(|(key, _)| key.as_slice() != b"Annots");
                    }
                }
            }
        }
    }
}

/// Collect, renumber and serialise the reachable object graph, WITHOUT
/// verification. `pub(crate)` so the negative tests can build a deliberately
/// corrupted candidate and feed it to [`verify_rewrite`] directly; production
/// callers always go through [`save_rewritten_with`], which verifies.
pub(crate) fn build_rewritten(
    src: &[u8],
    options: &RewriteOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, BuildInfo)> {
    if src.is_empty() {
        return Err(err!(
            Code::ObjUnexpected,
            during = "rewrite",
            detail = "empty source"
        ));
    }
    let start = selis_pdf_cos::xref::find_startxref(src, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "rewrite",
            detail = "no startxref"
        )
    })?;
    let doc = selis_pdf_cos::parse_revisions(src, start, budget, g)?;
    if doc
        .revisions()
        .last()
        .is_some_and(|rev| rev.encrypt.is_some())
    {
        return Err(err!(
            Code::MutationInvalid,
            during = "rewrite",
            detail = "encrypted source; see EDIT.06"
        ));
    }
    let last = doc.revisions().last().ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "rewrite",
            detail = "no revisions"
        )
    })?;
    let root = last.root.ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "rewrite",
            detail = "no /Root in the trailer"
        )
    })?;
    let info = last
        .trailer
        .iter()
        .find(|(key, _)| key.as_slice() == b"Info")
        .and_then(|(_, value)| match value {
            Obj::Ref(r) => Some(*r),
            _ => None,
        });

    let mut roots = vec![root];
    if let Some(info_ref) = info {
        roots.push(info_ref);
    }
    let mut next_num = 1u32;
    let (mut objects, remap) =
        collect_objects_with_doc(src, &doc, &roots, &mut next_num, budget, g)?;
    let new_root_num = remap.get(&root.num).copied().ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "rewrite",
            detail = "root was not renumbered"
        )
    })?;
    let new_root = Ref::new(new_root_num, 0);

    if let Some(fault) = options.fault {
        apply_fault(&mut objects, new_root, fault);
    }
    for (_, obj) in &mut objects {
        normalise_stream_length(obj);
    }

    let mut extra_trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some(info_ref) = info {
        let new_info = remap.get(&info_ref.num).copied().unwrap_or(info_ref.num);
        extra_trailer.push((b"Info".to_vec(), Obj::Ref(Ref::new(new_info, 0))));
    }
    if let Some((_, id)) = last.trailer.iter().find(|(key, _)| key.as_slice() == b"ID") {
        extra_trailer.push((b"ID".to_vec(), id.clone()));
    }

    let out = selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer(
        &objects,
        new_root,
        &extra_trailer,
        budget,
        g,
    )?;

    let objects_in = doc
        .at_revision(doc.len().saturating_sub(1))
        .map(|view| {
            view.xref
                .values()
                .filter(|entry| {
                    matches!(entry, XrefEntry::InUse { .. })
                        || matches!(entry, XrefEntry::Compressed { .. })
                })
                .count()
        })
        .unwrap_or(0);
    let objects_out = objects.len();
    let objects_renumbered = remap.iter().filter(|(old, new)| *old != *new).count();

    Ok((
        out,
        BuildInfo {
            objects_in,
            objects_out,
            objects_dropped: objects_in.saturating_sub(objects_out),
            objects_renumbered,
        },
    ))
}

/// Build and verify a rewritten document from `src`, with test options.
///
/// The reachable object graph from the newest trailer `/Root` (and `/Info`,
/// when present) is collected, renumbered from 1, and serialised as a fresh
/// single-revision PDF. Unreachable objects are dropped. The result is returned
/// only after [`verify_rewrite`] has compared it against `src`.
///
/// # Errors
///
/// [`Code::SignatureWouldBreak`] if `src` already carries a digital signature
/// (a rewrite would change every `/ByteRange`; use an incremental save),
/// [`Code::ObjUnexpected`] for an unparseable/empty source or a missing root,
/// [`Code::MutationInvalid`] for an encrypted source (EDIT.06 owns encryption),
/// [`Code::VerifyFailed`] naming any verification discrepancy, and propagated
/// parse / write / budget errors. No bytes are returned on any refusal.
///
/// # Budget
///
/// Charges parse, object collection, serialisation and both verification passes,
/// plus the materialise + inspect charges of
/// [`crate::signature_guard::refuse_if_signed`].
///
/// # Malformed Input
///
/// See above. A refused rewrite never touches the filesystem and leaves the
/// caller's source and session intact.
pub fn save_rewritten_with(
    src: &[u8],
    options: &RewriteOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, RewriteReport)> {
    // A rewrite renumbers objects and re-emits every byte, so every existing
    // signature's /ByteRange offsets would change. Refuse before building any
    // output; this also guards the AESV3-upgrade rewrite path, which goes
    // through this same entry point.
    crate::signature_guard::refuse_if_signed(src, budget, g)?;
    let (out, info) = build_rewritten(src, options, budget, g)?;
    let verification = verify_rewrite(src, &out, budget, g)?;
    let bytes_out = u64::try_from(out.len()).unwrap_or(u64::MAX);
    Ok((
        out,
        RewriteReport {
            bytes_in: u64::try_from(src.len()).unwrap_or(u64::MAX),
            bytes_out,
            objects_in: info.objects_in,
            objects_out: info.objects_out,
            objects_dropped: info.objects_dropped,
            objects_renumbered: info.objects_renumbered,
            verification,
        },
    ))
}

/// Build and verify a rewritten document from `src`.
///
/// Production entry point with no fault injection.
///
/// # Errors / Budget / Malformed Input
///
/// As [`save_rewritten_with`] with [`RewriteOptions::default`].
pub fn save_rewritten(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, RewriteReport)> {
    save_rewritten_with(src, &RewriteOptions::default(), budget, g)
}

/// Verify a rewritten document and atomically commit it to `path`, with the
/// kill-test hook. Verification happens before any byte reaches disk, and the
/// commit itself is temp + fsync + atomic rename + directory fsync.
///
/// # Errors
///
/// Any refusal from [`save_rewritten_with`], or [`Code::WriteFailed`] from the
/// atomic commit (including every injected fault). The destination is untouched
/// on every error path.
///
/// # Budget
///
/// As [`save_rewritten_with`], plus a `tick()` immediately before the commit.
///
/// # Malformed Input
///
/// See above. A refused rewrite produces no file and leaves the source intact.
pub fn save_rewritten_atomic_with(
    path: &Path,
    src: &[u8],
    options: &RewriteOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<RewriteCommitReport> {
    let (candidate, report) = save_rewritten_with(src, options, budget, g)?;
    g.tick()?;
    let receipt = selis_io::commit_atomic(
        path,
        &candidate,
        &selis_io::CommitOptions {
            fault: options.commit_fault,
        },
    )?;
    Ok(RewriteCommitReport {
        bytes: receipt.bytes,
        directory_fsynced: receipt.directory_fsynced,
        verification: report.verification,
    })
}

/// Production entry point: verified, crash-safe full rewrite with no faults.
///
/// # Errors / Budget / Malformed Input
///
/// As [`save_rewritten_atomic_with`] with [`RewriteOptions::default`].
pub fn save_rewritten_atomic(
    path: &Path,
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<RewriteCommitReport> {
    save_rewritten_atomic_with(path, src, &RewriteOptions::default(), budget, g)
}
