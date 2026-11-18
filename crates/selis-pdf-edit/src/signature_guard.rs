//! Signature preservation and DocMDP enforcement for incremental saves
//! (SL-5.EDIT.07, spec 15-PHASE-5-edit.md lines 52–57, 23-EDIT-MODEL-SPEC.md §6).
//!
//! # Why this exists
//!
//! [`crate::incremental::save_incremental`] appends a revision and keeps the
//! source bytes verbatim as a prefix (I3). A signature's `/ByteRange` names
//! FILE offsets, so those offsets keep pointing at the same bytes after the
//! append: the CMS digest still matches **by construction**. That claim is
//! load-bearing, so it is verified, not assumed:
//!
//! 1. **Before** writing, [`check_mutation_allowed`] refuses any mutation that
//!    redefines an object which is part of an existing signature (its field,
//!    its signature dictionary, its `/Reference` chain). Such a redefinition
//!    would change what a reader resolves even though the old bytes survive,
//!    and would break the signature. It also parses the certification
//!    signature's `/Reference → /TransformParams → /P` and refuses mutations
//!    that exceed the certified permission level.
//! 2. **After** writing, [`verify_signatures_preserved`] re-runs the real
//!    signature report ([`selis_pdf_sign::signatures`]) on the output and checks
//!    that every signature which verified over the source still verifies over
//!    the unchanged byte ranges, and that those ranges end inside the preserved
//!    prefix. A failure produces no output.
//!
//! # Budget
//!
//! [`check_mutation_allowed`] charges one [`Resource::Objects`] unit per
//! mutation and per signature object it inspects.
//! [`verify_signatures_preserved`] runs two full signature reports; their own
//! budget charges apply, plus one [`Resource::Objects`] unit per source
//! signature checked. The scan of an already-materialised
//! [`ObjectModel`](crate::incremental::ObjectModel) is not charged again.
//!
//! # Malformed Input
//!
//! A signature dictionary whose `/Reference`, `/TransformParams` or `/P` is the
//! wrong shape is ignored for DocMDP purposes (an approval signature has no
//! `/Reference`); its object is still protected. A signature whose byte range
//! is malformed, does not verify, or does not stay inside the prefix is a typed
//! `SIGNATURE_INVALID` error and no bytes are returned. An encrypted document
//! is handled by the caller: signature verification is skipped there because
//! the ciphertext cannot be reported on without a decryption policy.
//!
//! # Honest gap
//!
//! DocMDP levels are advisory in the file: a viewer enforces them, and so do
//! we, but a caller could in principle bundle a signature creation with an
//! unrelated edit. We refuse the bundle when any mutation is a plain content
//! or page edit (the most restrictive classification wins), which closes the
//! obvious bypass. Acrobat's own validation of a saved signed file remains a
//! **manual** check; the scriptable oracles here are our signature report and
//! `qpdf --check`.

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::incremental::{ObjectModel, ObjectMutation};

/// The `/P` transform level of a DocMDP certification signature.
///
/// ISO 32000-2 §12.8.2.2: 1 is "no changes to the document shall be permitted",
/// 2 adds form fill-in and signing, 3 adds annotation creation and editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocMdpLevel {
    /// `/P 1` — no changes permitted (signing itself is still allowed).
    NoChanges,
    /// `/P 2` — form fill-in and signing only.
    FillForms,
    /// `/P 3` — fill-in, annotations and signing.
    FillAndAnnotate,
}

impl DocMdpLevel {
    /// Interpret a `/P` integer, returning `None` for an unknown level.
    #[must_use]
    pub const fn from_p(p: i64) -> Option<Self> {
        match p {
            1 => Some(Self::NoChanges),
            2 => Some(Self::FillForms),
            3 => Some(Self::FillAndAnnotate),
            _ => None,
        }
    }

    /// The `/P` integer this level represents.
    #[must_use]
    pub const fn p(self) -> i64 {
        match self {
            Self::NoChanges => 1,
            Self::FillForms => 2,
            Self::FillAndAnnotate => 3,
        }
    }

    /// A short human label, used verbatim in refusal messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoChanges => "no changes permitted",
            Self::FillForms => "fill forms and sign only",
            Self::FillAndAnnotate => "fill, annotate and sign",
        }
    }
}

/// The permission-relevant class of a mutation batch.
///
/// Ordered from least to most restrictive: `Edit` is the strongest refusal and
/// wins when a batch mixes several kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MutationKind {
    /// Adding a signature field / dictionary (and its containers/appearance).
    Sign,
    /// Setting a form field's value.
    FormFill,
    /// Adding or editing an annotation (and its page container).
    Annotate,
    /// Any other object edit: content streams, page tree, arbitrary objects.
    Edit,
}

impl MutationKind {
    /// A short human label, used verbatim in refusal messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sign => "signing",
            Self::FormFill => "form-field fill",
            Self::Annotate => "annotation",
            Self::Edit => "content/page edit",
        }
    }
}

/// What the signature objects in a document look like to the guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureFinding {
    /// The certification level, when a DocMDP signature was found.
    pub certification_level: Option<DocMdpLevel>,
    /// Object numbers that belong to an existing signature and must not be
    /// redefined by a later mutation.
    pub signature_objects: Vec<u32>,
    /// How many signature dictionaries were found in the model.
    pub signature_count: usize,
}

/// Evidence that an incremental save preserved its existing signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignaturePreservationReport {
    /// Signatures found in the source document.
    pub source_signatures: usize,
    /// Source signatures that still verify over unchanged ranges in the output.
    pub preserved_signatures: usize,
    /// The `/ByteRange` of every preserved signature, all ending in the prefix.
    pub byte_ranges: Vec<[u64; 4]>,
    /// Length of the output, for the record that every range ends before it.
    pub output_len: u64,
}

/// Look a key up in a COS dictionary without allocating.
fn dict_get<'a>(pairs: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

/// Whether an object is a name equal to `name`.
fn name_is(obj: &Obj, name: &[u8]) -> bool {
    matches!(obj, Obj::Name(n) if n.as_slice() == name)
}

/// The pairs of a dictionary object (streams carry a dictionary too).
fn as_dict(obj: &Obj) -> Option<&[(Bytes, Obj)]> {
    match obj {
        Obj::Dict(pairs) | Obj::Stream { dict: pairs, .. } => Some(pairs.as_slice()),
        _ => None,
    }
}

/// Resolve an object through the materialised model (one level, enough for the
/// signature reference chain).
fn resolve_model<'a>(value: &'a Obj, model: &'a ObjectModel) -> Option<&'a Obj> {
    match value {
        Obj::Ref(reference) => model.get(&reference.num),
        other => Some(other),
    }
}

/// A dictionary's `/P` DocMDP level, if it is a `/DocMDP` transform.
fn docmdp_level(pairs: &[(Bytes, Obj)], model: &ObjectModel) -> Option<DocMdpLevel> {
    let reference = resolve_model(dict_get(pairs, b"Reference")?, model)?;
    let Obj::Array(entries) = reference else {
        return None;
    };
    for entry in entries {
        let Some(entry_pairs) = as_dict(resolve_model(entry, model)?) else {
            continue;
        };
        if !dict_get(entry_pairs, b"TransformMethod").is_some_and(|m| name_is(m, b"DocMDP")) {
            continue;
        }
        let params = resolve_model(dict_get(entry_pairs, b"TransformParams")?, model)?;
        let Some(param_pairs) = as_dict(params) else {
            continue;
        };
        if let Some(Obj::Int(p)) = dict_get(param_pairs, b"P") {
            if let Some(level) = DocMdpLevel::from_p(*p) {
                return Some(level);
            }
        }
    }
    None
}

/// Whether a value is an existing signature dictionary or signature field
/// whose redefinition would break that signature. The AcroForm carries
/// `/SigFlags` but is a shared container, not a signature object, so it is
/// deliberately absent here (it is still recognised as signing intent by
/// [`is_signature_value`]).
fn is_protected_signature_object(value: &Obj) -> bool {
    let Some(pairs) = as_dict(value) else {
        return false;
    };
    if dict_get(pairs, b"FT").is_some_and(|ft| name_is(ft, b"Sig")) {
        return true;
    }
    dict_get(pairs, b"ByteRange").is_some() && dict_get(pairs, b"Contents").is_some()
}

/// Whether a value expresses signing intent (a field, dictionary, appearance
/// or the form's `/SigFlags` update). Used for mutation classification.
fn is_signature_value(value: &Obj) -> bool {
    let Some(pairs) = as_dict(value) else {
        return false;
    };
    if dict_get(pairs, b"FT").is_some_and(|ft| name_is(ft, b"Sig")) {
        return true;
    }
    if dict_get(pairs, b"Type").is_some_and(|t| name_is(t, b"Sig")) {
        return true;
    }
    if dict_get(pairs, b"SigFlags").is_some() {
        return true;
    }
    dict_get(pairs, b"ByteRange").is_some() && dict_get(pairs, b"Contents").is_some()
}

/// Whether a stream is a signature appearance (a form XObject).
fn is_signature_appearance(value: &Obj) -> bool {
    let Obj::Stream { dict, .. } = value else {
        return false;
    };
    dict_get(dict, b"Type").is_some_and(|t| name_is(t, b"XObject"))
        && dict_get(dict, b"Subtype").is_some_and(|s| name_is(s, b"Form"))
}

/// Whether a value is an annotation other than a signature widget.
fn is_annotation_value(value: &Obj) -> bool {
    let Some(pairs) = as_dict(value) else {
        return false;
    };
    if dict_get(pairs, b"FT").is_some_and(|ft| name_is(ft, b"Sig")) {
        return false;
    }
    if dict_get(pairs, b"Type").is_some_and(|t| name_is(t, b"Annot")) {
        return true;
    }
    dict_get(pairs, b"Subtype").is_some() && dict_get(pairs, b"Rect").is_some()
}

/// Whether a value is a document container touched by several operations.
fn is_container_value(value: &Obj) -> bool {
    let Some(pairs) = as_dict(value) else {
        return false;
    };
    dict_get(pairs, b"Fields").is_some()
        || dict_get(pairs, b"Kids").is_some()
        || dict_get(pairs, b"Type")
            .is_some_and(|t| name_is(t, b"Catalog") || name_is(t, b"Pages") || name_is(t, b"Page"))
}

/// Maximum number of `/Parent` links followed while resolving an inherited
/// `/FT`. Mirrors the bounded field walk in `selis-pdf-form` / `selis-pdf-sign`
/// and breaks a malicious parent cycle.
const MAX_FIELD_PARENT_DEPTH: usize = 32;

/// Resolve a field object's effective `/FT`: its own `/FT`, else the first
/// `/FT` found by walking the `/Parent` chain. A widget frequently inherits its
/// type from its parent field (ISO 32000-2 §12.7.3), so filling such a widget is
/// still a form fill. This mirrors `selis-pdf-form`'s inherited-`/FT` resolution
/// (own value wins, otherwise inherit from the parent).
///
/// # Budget / Malformed Input
///
/// Walks an already-materialised model (no extra charge) and is bounded by
/// [`MAX_FIELD_PARENT_DEPTH`]; a cycle, a missing parent, or a non-name `/FT`
/// simply yields no type (the caller then classifies the mutation as `Edit`).
fn inherited_field_type(model: &ObjectModel, num: u32) -> Option<&[u8]> {
    let mut current = num;
    let mut seen = Vec::new();
    for _ in 0..MAX_FIELD_PARENT_DEPTH {
        if seen.contains(&current) {
            return None;
        }
        seen.push(current);
        let pairs = as_dict(model.get(&current)?)?;
        if let Some(Obj::Name(name)) = dict_get(pairs, b"FT") {
            return Some(name.as_slice());
        }
        match dict_get(pairs, b"Parent") {
            Some(Obj::Ref(reference)) => current = reference.num,
            _ => return None,
        }
    }
    None
}

/// Whether `num` is a fillable (non-signature) form field in the base model.
/// The field's own `/FT` wins; a widget that only inherits `/FT` from a parent
/// is still recognised (the DocMDP P=2 gap this closes). A `/FT /Sig` inherited
/// from a parent keeps the object a signature field, never a fillable one.
fn is_form_field(model: &ObjectModel, num: u32) -> bool {
    inherited_field_type(model, num).is_some_and(|ft| ft != b"Sig".as_slice())
}

impl ObjectMutation {
    /// The new value a `SetObject` mutation writes, when there is one.
    fn written_value(&self) -> Option<&Obj> {
        match self {
            Self::SetObject { value, .. } => Some(value),
            Self::DeleteObject { .. } => None,
        }
    }
}

/// Classify one mutation against the base model.
fn classify_one(model: &ObjectModel, mutation: &ObjectMutation) -> MutationKind {
    let Some(value) = mutation.written_value() else {
        return MutationKind::Edit;
    };
    if is_signature_value(value) || is_signature_appearance(value) {
        return MutationKind::Sign;
    }
    if is_annotation_value(value) {
        return MutationKind::Annotate;
    }
    if is_form_field(model, mutation.object_number()) {
        return MutationKind::FormFill;
    }
    MutationKind::Edit
}

/// Classify a whole mutation batch, using its intent to resolve shared
/// containers (a page or form dictionary is touched by signing, annotating and
/// page edits alike).
fn classify_batch(model: &ObjectModel, mutations: &[ObjectMutation]) -> MutationKind {
    let has_sign = mutations.iter().any(|m| {
        m.written_value()
            .is_some_and(|v| is_signature_value(v) || is_signature_appearance(v))
    });
    let has_annot = mutations
        .iter()
        .any(|m| m.written_value().is_some_and(is_annotation_value));
    let mut worst = MutationKind::Sign;
    for mutation in mutations {
        let kind = classify_one(model, mutation);
        let kind = if is_container_value_opt(mutation.written_value()) {
            if has_sign {
                MutationKind::Sign
            } else if has_annot {
                MutationKind::Annotate
            } else {
                kind
            }
        } else {
            kind
        };
        if kind > worst {
            worst = kind;
        }
    }
    worst
}

fn is_container_value_opt(value: Option<&Obj>) -> bool {
    value.is_some_and(is_container_value)
}

/// Whether a certification level permits this class of mutation.
fn allowed(level: DocMdpLevel, kind: MutationKind) -> bool {
    match level {
        DocMdpLevel::NoChanges => matches!(kind, MutationKind::Sign),
        DocMdpLevel::FillForms => {
            matches!(kind, MutationKind::Sign | MutationKind::FormFill)
        }
        DocMdpLevel::FillAndAnnotate => !matches!(kind, MutationKind::Edit),
    }
}

/// Inspect the model for signature objects and a DocMDP certification level.
///
/// # Budget
///
/// Charges one [`Resource::Objects`] unit per signature dictionary found. The
/// scan itself walks an already-materialised model and allocates only the
/// returned object-number list.
///
/// # Malformed Input
///
/// A malformed `/Reference` or `/TransformParams` is ignored: the document
/// still has a signature to protect, just no usable DocMDP level.
pub fn inspect_signatures(
    model: &ObjectModel,
    _budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<SignatureFinding> {
    let mut signature_objects = Vec::new();
    let mut signature_count = 0usize;
    let mut certification_level = None;
    for (num, value) in model {
        if !is_protected_signature_object(value) {
            continue;
        }
        g.charge_one(Resource::Objects)?;
        signature_count = signature_count.saturating_add(1);
        signature_objects.push(*num);
        if let Some(pairs) = as_dict(value) {
            if certification_level.is_none() {
                certification_level = docmdp_level(pairs, model);
            }
            // Protect objects referenced from the signature dictionary (the
            // `/Reference` chain and the `/V` of a signature field).
            for key in [
                b"Reference".as_slice(),
                b"TransformParams".as_slice(),
                b"V".as_slice(),
            ] {
                if let Some(target) = dict_get(pairs, key) {
                    collect_refs(target, model, &mut signature_objects);
                }
            }
        }
    }
    // Protect fields that point at a signature dictionary via `/V`.
    for (num, value) in model {
        if let Some(pairs) = as_dict(value) {
            if let Some(Obj::Ref(target)) = dict_get(pairs, b"V") {
                if model
                    .get(&target.num)
                    .is_some_and(is_protected_signature_object)
                {
                    signature_objects.push(*num);
                    if !signature_objects.contains(&target.num) {
                        signature_objects.push(target.num);
                    }
                }
            }
        }
    }
    signature_objects.sort_unstable();
    signature_objects.dedup();
    Ok(SignatureFinding {
        certification_level,
        signature_objects,
        signature_count,
    })
}

/// Add direct references found under `value` to `out`, one level deep.
fn collect_refs(value: &Obj, model: &ObjectModel, out: &mut Vec<u32>) {
    match value {
        Obj::Ref(reference) => {
            if model.contains_key(&reference.num) && !out.contains(&reference.num) {
                out.push(reference.num);
            }
        }
        Obj::Array(items) => {
            for item in items {
                collect_refs(item, model, out);
            }
        }
        Obj::Dict(pairs) => {
            for (_, item) in pairs {
                if let Obj::Ref(reference) = item {
                    if model.contains_key(&reference.num) && !out.contains(&reference.num) {
                        out.push(reference.num);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Check a mutation sequence against existing signatures and DocMDP.
///
/// Called by [`crate::incremental::save_incremental`] before any bytes are
/// written, so a refusal produces no output.
///
/// # Errors
///
/// * `SIGNATURE_WOULD_BREAK` if a mutation redefines a signature object, or if
///   the certification level forbids the mutation class; the detail names the
///   level, the refused operation and the object where relevant;
/// * a budget error if the guard is exhausted.
///
/// # Budget
///
/// Charges one [`Resource::Objects`] unit per mutation, plus the charges of
/// [`inspect_signatures`].
pub fn check_mutation_allowed(
    model: &ObjectModel,
    mutations: &[ObjectMutation],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    for _ in mutations {
        g.charge_one(Resource::Objects)?;
    }
    let finding = inspect_signatures(model, budget, g)?;

    for mutation in mutations {
        let num = mutation.object_number();
        if finding.signature_objects.contains(&num) {
            return Err(err!(
                Code::SignatureWouldBreak,
                during = "signature-guard",
                object = num,
                detail = format!(
                    "object {num} is part of an existing digital signature and cannot be redefined"
                )
            ));
        }
    }

    let kind = classify_batch(model, mutations);
    if let Some(level) = finding.certification_level {
        if !allowed(level, kind) {
            return Err(err!(
                Code::SignatureWouldBreak,
                during = "signature-guard",
                detail = format!(
                    "DocMDP level {} ({}) forbids {} on a certified document",
                    level.p(),
                    level.label(),
                    kind.label()
                )
            ));
        }
    }
    Ok(())
}

/// Refuse any operation that would rewrite the whole file when `src` already
/// carries a digital signature. A full rewrite (EDIT.05 `save_rewritten`,
/// including the AESV3-upgrade rewrite path) renumbers objects and re-emits
/// every byte, so a signature's `/ByteRange` FILE offsets change and the CMS
/// digest can no longer match. Only an incremental append preserves them.
///
/// # Errors
///
/// * `SIGNATURE_WOULD_BREAK` if [`inspect_signatures`] finds at least one
///   existing signature; the detail names the count and points to the
///   incremental alternative;
/// * an `ObjUnexpected` / parse / budget error if `src` cannot be materialised.
///
/// # Budget
///
/// The charges of [`crate::incremental::materialise`] plus those of
/// [`inspect_signatures`]. No bytes are written or returned on any path.
///
/// # Malformed Input
///
/// An unparseable source is refused by `materialise`; an encrypted source is
/// refused by the caller before this runs (EDIT.06 owns encryption). A source
/// with no signature objects is allowed. A malformed signature reference is
/// ignored by `inspect_signatures`, but a well-formed signature dictionary
/// (`/ByteRange` + `/Contents`) is still detected and refused.
pub fn refuse_if_signed(src: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<()> {
    let model = crate::incremental::materialise(src, budget, g)?;
    let finding = inspect_signatures(&model, budget, g)?;
    if finding.signature_count > 0 {
        return Err(err!(
            Code::SignatureWouldBreak,
            during = "signature-guard",
            detail = format!(
                "full rewrite would break {} existing digital signature(s); use an incremental save instead",
                finding.signature_count
            )
        ));
    }
    Ok(())
}

/// Re-run the signature report on an incrementally saved output and prove the
/// source signatures survived unchanged.
///
/// The output necessarily has bytes appended after the signature's `/ByteRange`
/// (that is what an incremental save is), so `covers_whole_document` on the
/// **output** is false by design. What matters for preservation is that each
/// source signature verified over a range ending at the source length, and
/// that the same four offsets in the output still verify — which they do only
/// because the source bytes are a verbatim prefix.
///
/// # Errors
///
/// * `SIGNATURE_INVALID` if a source signature has no well-formed byte range or
///   one ending outside the preserved prefix, if a report cannot be produced,
///   if a signature's byte range changed in the output, or if a signature that
///   verified over the whole source no longer verifies; a signature already
///   invalid on the source is carried over unchanged, not counted as preserved;
/// * a budget error from either signature report.
///
/// # Budget
///
/// Two full [`selis_pdf_sign::signatures`] reports plus one
/// [`Resource::Objects`] charge per source signature checked.
pub fn verify_signatures_preserved(
    src: &[u8],
    output: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<SignaturePreservationReport> {
    // The core has no clock. Trust is irrelevant to byte-range preservation;
    // a fixed instant keeps the report deterministic.
    const REPORT_NOW: i64 = 0;
    let before = selis_pdf_sign::signatures(src, &[], REPORT_NOW, budget, g).map_err(|e| {
        err!(
            Code::SignatureInvalid,
            during = "signature-guard",
            detail = format!("cannot read source signatures: {e}")
        )
    })?;
    let after = selis_pdf_sign::signatures(output, &[], REPORT_NOW, budget, g).map_err(|e| {
        err!(
            Code::SignatureInvalid,
            during = "signature-guard",
            detail = format!("cannot read output signatures: {e}")
        )
    })?;
    let src_len = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let out_len = u64::try_from(output.len()).unwrap_or(u64::MAX);

    let mut byte_ranges = Vec::new();
    let mut preserved = 0usize;
    for signature in &before.signatures {
        let Some(range) = signature.byte_range else {
            return Err(err!(
                Code::SignatureInvalid,
                during = "signature-guard",
                detail = format!(
                    "signature {:?} has no well-formed /ByteRange",
                    signature.field
                )
            ));
        };
        let range_end = range[2].saturating_add(range[3]);
        if range_end > src_len {
            return Err(err!(
                Code::SignatureInvalid,
                during = "signature-guard",
                detail = format!(
                    "signature {:?} byte range ends past the preserved prefix",
                    signature.field
                )
            ));
        }
        g.charge_one(Resource::Objects)?;
        let Some(matching) = after.signatures.iter().find(|candidate| {
            candidate.field == signature.field && candidate.byte_range == Some(range)
        }) else {
            return Err(err!(
                Code::SignatureInvalid,
                during = "signature-guard",
                detail = format!(
                    "signature {:?} byte range changed in the saved output",
                    signature.field
                )
            ));
        };
        if matching
            .byte_range
            .is_some_and(|r| r[2].saturating_add(r[3]) > src_len)
        {
            return Err(err!(
                Code::SignatureInvalid,
                during = "signature-guard",
                detail = format!(
                    "signature {:?} byte range no longer ends inside the preserved prefix",
                    signature.field
                )
            ));
        }
        // A signature that was already broken or unsupported on the source is
        // not made worse by an append; it simply is not "preserved valid".
        // Only a signature that verified over the whole source must keep doing
        // so — that is the case the incremental-save invariant guarantees.
        if signature.verified && signature.covers_whole_document {
            if !matching.verified {
                return Err(err!(
                    Code::SignatureInvalid,
                    during = "signature-guard",
                    detail = format!(
                        "signature {:?} no longer verifies over its original byte range in the saved output",
                        signature.field
                    )
                ));
            }
            byte_ranges.push(range);
            preserved = preserved.saturating_add(1);
        }
    }

    Ok(SignaturePreservationReport {
        source_signatures: before.signatures.len(),
        preserved_signatures: preserved,
        byte_ranges,
        output_len: out_len,
    })
}
