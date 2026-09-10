//! Structural verification of writer output (SL-1A.WRITE.05).
//!
//! `save_rewritten()`'s full verification obligation (`23-EDIT-MODEL-SPEC.md
//! §7`) — render every page and compare against the pre-save render — requires
//! a renderer, which does not exist until G2. Until then, tool outputs are
//! verified **structurally**: the bytes are reparsed through the xref (no
//! scan-based recovery — a writer output must not need repair), every
//! reference reachable from the trailer roots must resolve, the page tree must
//! be consistent, and the observed page/annotation/field/OCG counts must match
//! the caller's expectations.
//!
//! # Scope warning — do not reuse for content-rewriting operations
//!
//! This is deliberately the **pre-G2 weaker standard**. It proves the output
//! parses and its object graph is complete; it does **not** prove that pages
//! still render correctly or that text is preserved. It is sufficient for the
//! Phase 1A operations because none of them alter page *content*, only page
//! *selection* and document structure. **Any operation that rewrites content
//! streams (text editing, redaction of content, content re-encoding) must NOT
//! rely on this standard** — it must use the full render + text verification
//! filed as the G2 follow-up (`SL-1A.WRITE.08` in
//! `pdf-plan/11A-PHASE-1A-tools.md`).
//!
//! # Output Guarantees
//!
//! A `Verdict` with `ok == true` means: the bytes parse through their
//! cross-reference chain, the trailer `/Root` resolves to a catalog with a
//! usable `/Pages` tree, every indirect reference reachable from `/Root` (and
//! the trailer `/Info`, when present) resolves, the declared page-tree
//! `/Count` matches the walked leaf count, and every expectation the caller
//! supplied matched. It does not mean the document renders.

use std::collections::HashSet;

use selis_error::Result;
use selis_sandbox::{Budget, BudgetGuard, DepthGuard, Resource};

use crate::copy::resolve_ref;
use crate::obj::{Obj, Ref};
use crate::revision::{parse_revisions, parse_revisions_resilient, Doc};
use crate::xref;

/// Maximum reference-walk / page-tree depth before the walk is declared
/// pathological. Mirrors the tools' own walk cap.
pub const MAX_WALK_DEPTH: usize = 48;

/// What the caller expects the verified bytes to contain. `None` = unchecked.
///
/// Counts follow one definition, shared by [`survey`] so expectations can be
/// captured from an input document and asserted on its output:
///
/// * `pages` — leaf `/Type /Page` objects under the catalog's `/Pages` tree;
/// * `annotations` — total items across every page's `/Annots` array;
/// * `fields` — form-field dictionaries under `/AcroForm`/`/Fields`, counted
///   recursively through `/Kids` (roots and children each count one);
/// * `ocgs` — items in `/OCProperties`' `/OCGs` array;
/// * `outlines` — outline items reachable from the catalog's `/Outlines`
///   root over every `/First`/`/Next` chain at every nesting level;
/// * `embedded_files` — key/value pairs across the
///   `/Names`/`/EmbeddedFiles` name tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expectations {
    /// Expected page count.
    pub pages: Option<u64>,
    /// Expected total `/Annots` items across all pages.
    pub annotations: Option<u64>,
    /// Expected recursive form-field count under `/AcroForm`.
    pub fields: Option<u64>,
    /// Expected item count in `/OCProperties`/`/OCGs`.
    pub ocgs: Option<u64>,
    /// Expected outline entry count.
    pub outlines: Option<u64>,
    /// Expected embedded-file entry count.
    pub embedded_files: Option<u64>,
}

impl Expectations {
    /// No expectations: every structural check still runs, nothing is compared.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            pages: None,
            annotations: None,
            fields: None,
            ocgs: None,
            outlines: None,
            embedded_files: None,
        }
    }
}

/// Counts observed while walking the verified bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Observed {
    /// Walked leaf page count.
    pub pages: u64,
    /// Total `/Annots` items across the walked pages.
    pub annotations: u64,
    /// Recursive form-field count under `/AcroForm`.
    pub fields: u64,
    /// Item count in `/OCProperties`/`/OCGs`.
    pub ocgs: u64,
    /// Outline entries reachable from the catalog's `/Outlines` root.
    pub outlines: u64,
    /// Key/value pairs across the `/Names`/`/EmbeddedFiles` name tree.
    pub embedded_files: u64,
}

/// One structural fault found in the verified bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// The tail window holds no `startxref` at all: the bytes are not a
    /// parseable document.
    NoStartxref,
    /// The newest revision's xref chain failed to parse. A writer output must
    /// never need repair, so this is always a fault here.
    Unparsable {
        /// The parser's typed error, rendered.
        detail: String,
    },
    /// The newest trailer carries no `/Root`.
    RootMissing,
    /// `/Root` did not resolve.
    RootUnresolvable {
        /// The root reference that failed.
        root: Ref,
    },
    /// The catalog has no `/Pages` entry (an unresolvable `/Pages` reference
    /// is reported as [`Fault::UnresolvedRef`]).
    PagesMissing,
    /// An indirect reference reachable from `/Root` or the trailer `/Info`
    /// does not resolve. "No object reachable-that-should-be-reachable is
    /// missing" — this fault is that check.
    UnresolvedRef {
        /// The object holding the reference (`0 0 R` when the reference comes
        /// from a trailer or a count pass).
        from: Ref,
        /// The referenced (missing) object.
        target: Ref,
    },
    /// The walk exceeded the depth cap — a pathologically nested structure.
    WalkDepthExceeded {
        /// The depth cap that was hit.
        cap: usize,
    },
    /// The declared page-tree `/Count` disagrees with the walked leaf count.
    PageTreeCountMismatch {
        /// `/Count` as declared on the root `/Pages` node.
        declared: u64,
        /// Leaves actually walked.
        walked: u64,
    },
    /// The caller expected a different page count.
    PageCountMismatch {
        /// Expected pages.
        expected: u64,
        /// Observed pages.
        actual: u64,
    },
    /// The caller expected a different total annotation count.
    AnnotationCountMismatch {
        /// Expected `/Annots` items.
        expected: u64,
        /// Observed `/Annots` items.
        actual: u64,
    },
    /// The caller expected a different form-field count.
    FieldCountMismatch {
        /// Expected fields.
        expected: u64,
        /// Observed fields.
        actual: u64,
    },
    /// The caller expected a different optional-content group count.
    OcgCountMismatch {
        /// Expected OCGs.
        expected: u64,
        /// Observed OCGs.
        actual: u64,
    },
    /// The caller expected a different outline entry count.
    OutlineCountMismatch {
        /// Expected outline entries.
        expected: u64,
        /// Observed outline entries.
        actual: u64,
    },
    /// The caller expected a different embedded-file count.
    EmbeddedFileCountMismatch {
        /// Expected embedded files.
        expected: u64,
        /// Observed embedded files.
        actual: u64,
    },
}

impl Fault {
    /// A short stable kind label (for tool output / oplog lines).
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Fault::NoStartxref => "no-startxref",
            Fault::Unparsable { .. } => "unparsable",
            Fault::RootMissing => "root-missing",
            Fault::RootUnresolvable { .. } => "root-unresolvable",
            Fault::PagesMissing => "pages-missing",
            Fault::UnresolvedRef { .. } => "unresolved-ref",
            Fault::WalkDepthExceeded { .. } => "walk-depth",
            Fault::PageTreeCountMismatch { .. } => "page-tree-count",
            Fault::PageCountMismatch { .. } => "page-count",
            Fault::AnnotationCountMismatch { .. } => "annotation-count",
            Fault::FieldCountMismatch { .. } => "field-count",
            Fault::OcgCountMismatch { .. } => "ocg-count",
            Fault::OutlineCountMismatch { .. } => "outline-count",
            Fault::EmbeddedFileCountMismatch { .. } => "embedded-file-count",
        }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::NoStartxref => write!(f, "no startxref found in the tail window"),
            Fault::Unparsable { detail } => {
                write!(f, "xref chain failed to parse: {detail}")
            }
            Fault::RootMissing => write!(f, "trailer carries no /Root"),
            Fault::RootUnresolvable { root } => {
                write!(f, "/Root {} {} R does not resolve", root.num, root.gen)
            }
            Fault::PagesMissing => write!(f, "catalog has no /Pages"),
            Fault::UnresolvedRef { from, target } => write!(
                f,
                "reference {} {} R held by object {} {} R does not resolve",
                target.num, target.gen, from.num, from.gen
            ),
            Fault::WalkDepthExceeded { cap } => {
                write!(f, "reference walk exceeded depth {cap}")
            }
            Fault::PageTreeCountMismatch { declared, walked } => write!(
                f,
                "page tree /Count says {declared} but the walk found {walked} leaves"
            ),
            Fault::PageCountMismatch { expected, actual } => {
                write!(f, "expected {expected} page(s), output has {actual}")
            }
            Fault::AnnotationCountMismatch { expected, actual } => {
                write!(f, "expected {expected} annotation(s), output has {actual}")
            }
            Fault::FieldCountMismatch { expected, actual } => {
                write!(f, "expected {expected} form field(s), output has {actual}")
            }
            Fault::OcgCountMismatch { expected, actual } => write!(
                f,
                "expected {expected} optional-content group(s), output has {actual}"
            ),
            Fault::OutlineCountMismatch { expected, actual } => write!(
                f,
                "expected {expected} outline entr(y/ies), output has {actual}"
            ),
            Fault::EmbeddedFileCountMismatch { expected, actual } => write!(
                f,
                "expected {expected} embedded file(s), output has {actual}"
            ),
        }
    }
}

/// The outcome of [`verify_structural`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// `true` when no fault was found.
    pub ok: bool,
    /// Counts observed during the walk.
    pub observed: Observed,
    /// Every fault found (empty when `ok`).
    pub faults: Vec<Fault>,
}

/// Verify writer output structurally (SL-1A.WRITE.05).
///
/// Reparses `doc_bytes` through its xref chain — **no scan-based recovery**:
/// a tool output that cannot be parsed from its own cross-reference data is a
/// fault, not something to repair. Walks every object reachable from the
/// trailer `/Root` (plus the trailer `/Info` when present) and checks:
///
/// 1. every indirect reference resolves (nothing reachable is missing);
/// 2. the `/Pages` tree is walkable, its declared `/Count` matches the walked
///    leaf count, and the leaf count matches `expected.pages` when given;
/// 3. annotation / form-field / optional-content-group counts match the
///    caller's expectations when given.
///
/// The source buffer is never mutated (I1); the function is read-only.
///
/// Callers capture expectations from an input document with [`survey`] and
/// assert them on the output — that is how the tools compare pre/post counts.
///
/// # Budget
///
/// Every parse and every object resolution is charged to the caller's guard
/// (objects, depth, ticks). Budget exhaustion returns a typed `BUDGET_*`
/// error — never a partial-but-unmarked verdict.
///
/// # Malformed Input
///
/// Malformed bytes are *not* errors here — they are faults in the returned
/// [`Verdict`] (`NoStartxref`, `Unparsable`, `UnresolvedRef`, …) so callers
/// can name every problem in one pass. Only budget/cancellation/pending
/// errors propagate as `Err`.
///
/// # Output Guarantees
///
/// See the module docs: this is the pre-G2 weaker standard. It must not be
/// reused for content-rewriting operations (SL-1A.WRITE.05 risk note).
pub fn verify_structural(
    doc_bytes: &[u8],
    expected: &Expectations,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Verdict> {
    let mut faults: Vec<Fault> = Vec::new();

    // A writer output must parse from its own xref. When the newest revision
    // is unreadable, try the I5 rollback (an earlier complete revision) — the
    // walk then proceeds so the caller gets a full report, but the verdict
    // still fails: a writer output is complete or it is wrong.
    let Some(startxref) = xref::find_startxref(doc_bytes, 4096) else {
        faults.push(Fault::NoStartxref);
        return Ok(Verdict {
            ok: false,
            observed: Observed::default(),
            faults,
        });
    };
    let (doc, unparsable) = match parse_revisions(doc_bytes, startxref, budget, g) {
        Ok(doc) => (doc, None),
        Err(first_err) => {
            if first_err.is_budget() || first_err.is_cancelled() || first_err.is_pending() {
                return Err(first_err);
            }
            match parse_revisions_resilient(doc_bytes, startxref, budget, g) {
                Ok(Some(doc)) => (doc, Some(first_err.to_string())),
                Ok(None) => {
                    faults.push(Fault::Unparsable {
                        detail: first_err.to_string(),
                    });
                    return Ok(Verdict {
                        ok: false,
                        observed: Observed::default(),
                        faults,
                    });
                }
                Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => return Err(e),
                Err(_) => {
                    faults.push(Fault::Unparsable {
                        detail: first_err.to_string(),
                    });
                    return Ok(Verdict {
                        ok: false,
                        observed: Observed::default(),
                        faults,
                    });
                }
            }
        }
    };
    if let Some(detail) = unparsable {
        faults.push(Fault::Unparsable { detail });
    }

    let Some(root) = doc.revisions().iter().rev().find_map(|rev| rev.root) else {
        faults.push(Fault::RootMissing);
        return Ok(Verdict {
            ok: false,
            observed: Observed::default(),
            faults,
        });
    };

    let mut w = Walker {
        src: doc_bytes,
        doc: &doc,
        faults: Vec::new(),
        seen: HashSet::new(),
        reported_missing: HashSet::new(),
    };

    let catalog = match w.resolve(g, Ref::new(0, 0), root)? {
        Some(c) => c,
        None => {
            w.faults.push(Fault::RootUnresolvable { root });
            faults.append(&mut w.faults);
            return Ok(Verdict {
                ok: false,
                observed: Observed::default(),
                faults,
            });
        }
    };
    w.seen.insert((root.num, root.gen));

    // The reference walk from /Root (and trailer /Info): every indirect
    // reference reachable must resolve.
    w.walk_refs(g, &catalog, root, 0)?;
    if let Some((_, Obj::Ref(info))) = doc
        .revisions()
        .last()
        .and_then(|rev| rev.trailer.iter().find(|(k, _)| k.as_slice() == b"Info"))
    {
        if w.seen.insert((info.num, info.gen)) {
            if let Some(info_obj) = w.resolve(g, Ref::new(0, 0), *info)? {
                w.walk_refs(g, &info_obj, *info, 0)?;
            }
        }
    }

    let mut observed = Observed::default();

    // Page tree: count leaves, compare the declared /Count.
    let Some(pages_ref) = dict_ref(&catalog, b"Pages") else {
        w.faults.push(Fault::PagesMissing);
        faults.append(&mut w.faults);
        let ok = faults.is_empty();
        return Ok(Verdict {
            ok,
            observed,
            faults,
        });
    };
    let mut leaves: Vec<Obj> = Vec::new();
    let mut declared: Option<u64> = None;
    w.walk_page_tree(g, pages_ref, root, 0, &mut leaves, &mut declared)?;
    observed.pages = u64::try_from(leaves.len()).unwrap_or(u64::MAX);
    if let Some(declared) = declared {
        if declared != observed.pages {
            w.faults.push(Fault::PageTreeCountMismatch {
                declared,
                walked: observed.pages,
            });
        }
    }
    // Annotation count: /Annots items across the walked leaves.
    for leaf in &leaves {
        observed.annotations = observed
            .annotations
            .saturating_add(annots_in_page(g, leaf, &mut w)?);
    }

    // Form fields: /AcroForm /Fields, recursive through /Kids.
    if let Some(acro_ref) = dict_ref(&catalog, b"AcroForm") {
        if let Some(acro) = w.resolve(g, root, acro_ref)? {
            if let Some(fields_val) = dict_get(&acro, b"Fields") {
                let fields_val = resolve_inline(g, fields_val, &mut w)?;
                if let Obj::Array(items) = fields_val {
                    for item in &items {
                        observed.fields = observed
                            .fields
                            .saturating_add(count_fields(g, item, &mut w, 0)?);
                    }
                }
            }
        }
    }

    // Optional-content groups: /OCProperties /OCGs.
    if let Some(oc_ref) = dict_ref(&catalog, b"OCProperties") {
        if let Some(oc) = w.resolve(g, root, oc_ref)? {
            if let Some(ocgs_val) = dict_get(&oc, b"OCGs") {
                let ocgs_val = resolve_inline(g, ocgs_val, &mut w)?;
                if let Obj::Array(items) = ocgs_val {
                    observed.ocgs = u64::try_from(items.len()).unwrap_or(u64::MAX);
                }
            }
        }
    }

    // Outline entries and embedded files. The count definitions are the
    // survey's by construction (same walk, same helpers), so an expectation
    // captured from an input document is asserted with identical semantics.
    observed.outlines = count_outline_items(g, &mut w, &catalog)?;
    observed.embedded_files = count_embedded_files(g, &mut w, &catalog)?;

    // Expectation comparisons.
    if let Some(exp) = expected.pages {
        if exp != observed.pages {
            faults.push(Fault::PageCountMismatch {
                expected: exp,
                actual: observed.pages,
            });
        }
    }
    if let Some(exp) = expected.annotations {
        if exp != observed.annotations {
            faults.push(Fault::AnnotationCountMismatch {
                expected: exp,
                actual: observed.annotations,
            });
        }
    }
    if let Some(exp) = expected.fields {
        if exp != observed.fields {
            faults.push(Fault::FieldCountMismatch {
                expected: exp,
                actual: observed.fields,
            });
        }
    }
    if let Some(exp) = expected.ocgs {
        if exp != observed.ocgs {
            faults.push(Fault::OcgCountMismatch {
                expected: exp,
                actual: observed.ocgs,
            });
        }
    }
    if let Some(exp) = expected.outlines {
        if exp != observed.outlines {
            faults.push(Fault::OutlineCountMismatch {
                expected: exp,
                actual: observed.outlines,
            });
        }
    }
    if let Some(exp) = expected.embedded_files {
        if exp != observed.embedded_files {
            faults.push(Fault::EmbeddedFileCountMismatch {
                expected: exp,
                actual: observed.embedded_files,
            });
        }
    }

    // Merge the walker's faults (unresolved refs, depth breaches) into the
    // verdict's fault list.
    faults.append(&mut w.faults);
    let ok = faults.is_empty();
    Ok(Verdict {
        ok,
        observed,
        faults,
    })
}

/// Capture expectations from a document: run [`verify_structural`] with no
/// expectations and return the observed counts. Used by the tools to compare
/// pre-operation and post-operation counts.
///
/// Faults in the *input* are irrelevant here (the input is not what we
/// promise); only the observed counts are used.
///
/// # Budget
///
/// As [`verify_structural`].
///
/// # Malformed Input
///
/// As [`verify_structural`] — faults are discarded; counts may be partial
/// when the walk could not proceed.
pub fn survey(doc_bytes: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<Observed> {
    let verdict = verify_structural(doc_bytes, &Expectations::none(), budget, g)?;
    Ok(verdict.observed)
}

/// The walk state. `seen` prevents re-walking shared objects (diamond graphs)
/// and cycles (outlines are doubly-linked: `Prev`/`Next`/`Parent` form
/// cycles); `reported_missing` de-duplicates unresolvable-reference faults.
/// The walker owns its faults; the caller merges them into the verdict.
struct Walker<'a> {
    src: &'a [u8],
    doc: &'a Doc,
    faults: Vec<Fault>,
    seen: HashSet<(u32, u16)>,
    reported_missing: HashSet<(u32, u16)>,
}

impl Walker<'_> {
    /// Resolve `target`, recording an [`Fault::UnresolvedRef`] (attributed to
    /// `from`) on failure. Budget/cancel propagate.
    fn resolve(&mut self, g: &mut BudgetGuard<'_>, from: Ref, target: Ref) -> Result<Option<Obj>> {
        g.tick()?;
        g.charge_one(Resource::Objects)?;
        match resolve_ref(self.src, self.doc, target, &Budget::unlimited(), g) {
            Ok(obj) => Ok(Some(obj)),
            Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => Err(e),
            Err(_) => {
                if self.reported_missing.insert((target.num, target.gen)) {
                    self.faults.push(Fault::UnresolvedRef { from, target });
                }
                Ok(None)
            }
        }
    }

    /// Walk every reference reachable from `obj` (owned by `owner`),
    /// resolving each exactly once. Stream payloads are opaque here; their
    /// dictionaries are walked.
    ///
    /// Each nesting level is an RAII [`DepthGuard`]: the depth charge is
    /// released on every return path (an early `?` cannot leak it — a leaked
    /// level would make the caller's depth budget a count of *total* walk
    /// steps instead of true nesting).
    fn walk_refs(
        &mut self,
        g: &mut BudgetGuard<'_>,
        obj: &Obj,
        owner: Ref,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_WALK_DEPTH {
            self.faults.push(Fault::WalkDepthExceeded {
                cap: MAX_WALK_DEPTH,
            });
            return Ok(());
        }
        let mut level = DepthGuard::enter(g)?;
        let g = level.guard();
        match obj {
            Obj::Ref(r) => {
                if self.seen.insert((r.num, r.gen)) {
                    if let Some(resolved) = self.resolve(g, owner, *r)? {
                        self.walk_refs(g, &resolved, *r, depth.saturating_add(1))?;
                    }
                }
            }
            Obj::Array(items) => {
                for item in items {
                    self.walk_refs(g, item, owner, depth.saturating_add(1))?;
                }
            }
            Obj::Dict(pairs) | Obj::Stream { dict: pairs, .. } => {
                for (_, v) in pairs {
                    self.walk_refs(g, v, owner, depth.saturating_add(1))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Walk a `/Pages` tree collecting resolved leaf page dicts (and the root
    /// node's declared `/Count`). Unresolvable nodes are faults from
    /// [`Self::resolve`]. The nesting level is an RAII [`DepthGuard`] (see
    /// [`Self::walk_refs`]).
    fn walk_page_tree(
        &mut self,
        g: &mut BudgetGuard<'_>,
        node: Ref,
        parent: Ref,
        depth: usize,
        leaves: &mut Vec<Obj>,
        declared: &mut Option<u64>,
    ) -> Result<()> {
        if depth > MAX_WALK_DEPTH {
            self.faults.push(Fault::WalkDepthExceeded {
                cap: MAX_WALK_DEPTH,
            });
            return Ok(());
        }
        let mut level = DepthGuard::enter(g)?;
        let g = level.guard();
        let Some(obj) = self.resolve(g, parent, node)? else {
            return Ok(());
        };
        let Obj::Dict(pairs) = &obj else {
            return Ok(());
        };
        let is_page = pairs.iter().any(|(k, v)| {
            k.as_slice() == b"Type" && matches!(v, Obj::Name(n) if n.as_slice() == b"Page")
        });
        if is_page {
            leaves.push(obj);
            return Ok(());
        }
        if depth == 0 {
            if let Some((_, Obj::Int(n))) = pairs.iter().find(|(k, _)| k.as_slice() == b"Count") {
                *declared = Some(u64::try_from(*n).unwrap_or(u64::MAX));
            }
        }
        if let Some((_, kids)) = pairs.iter().find(|(k, _)| k.as_slice() == b"Kids") {
            let kids = resolve_inline(g, kids, self)?;
            if let Obj::Array(items) = kids {
                for item in &items {
                    if let Obj::Ref(kid) = item {
                        self.walk_page_tree(
                            g,
                            *kid,
                            node,
                            depth.saturating_add(1),
                            leaves,
                            declared,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Resolve one level: an indirect reference becomes its object; everything
/// else passes through. Unresolvable refs are recorded by [`Walker::resolve`].
fn resolve_inline(g: &mut BudgetGuard<'_>, obj: &Obj, w: &mut Walker<'_>) -> Result<Obj> {
    match obj {
        Obj::Ref(r) => Ok(w.resolve(g, *r, *r)?.unwrap_or(Obj::Null)),
        other => Ok(other.clone()),
    }
}

/// The annotations `/Annots` item count of one page dict (the array may be
/// direct or indirect).
fn annots_in_page(g: &mut BudgetGuard<'_>, page: &Obj, w: &mut Walker<'_>) -> Result<u64> {
    let Some(annots) = dict_get(page, b"Annots") else {
        return Ok(0);
    };
    let annots = resolve_inline(g, annots, w)?;
    match &annots {
        Obj::Array(items) => Ok(u64::try_from(items.len()).unwrap_or(u64::MAX)),
        _ => Ok(0),
    }
}

/// Recursively count field dictionaries under a `/Fields` item.
fn count_fields(
    g: &mut BudgetGuard<'_>,
    item: &Obj,
    w: &mut Walker<'_>,
    depth: usize,
) -> Result<u64> {
    if depth > 32 {
        return Ok(0);
    }
    let resolved = resolve_inline(g, item, w)?;
    let Obj::Dict(pairs) = &resolved else {
        return Ok(0);
    };
    let mut count = 1u64;
    if let Some((_, kids)) = pairs.iter().find(|(k, _)| k.as_slice() == b"Kids") {
        let kids = resolve_inline(g, kids, w)?;
        if let Obj::Array(items) = kids {
            for kid in &items {
                count = count.saturating_add(count_fields(g, kid, w, depth.saturating_add(1))?);
            }
        }
    }
    Ok(count)
}

/// A dictionary entry by key.
fn dict_get<'a>(obj: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match obj {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

/// A dictionary entry that is an indirect reference.
fn dict_ref(obj: &Obj, key: &[u8]) -> Option<Ref> {
    match dict_get(obj, key) {
        Some(Obj::Ref(r)) => Some(*r),
        _ => None,
    }
}

/// Hard cap on outline entries and name-tree keys counted for one document.
/// A hostile `/Next` cycle is already terminated by the seen-set; this bound
/// keeps the count itself cheap on pathological (but acyclic) chains.
const MAX_COUNTED_ENTRIES: u64 = 1_000_000;

/// Total outline entries reachable from the catalog's `/Outlines` root: every
/// item on every `/First`/`/Next` chain at every nesting level. Items stored
/// inline (not as indirect references) are pathological and count zero; the
/// reference-closure walk reports whatever else is wrong with them.
///
/// # Budget
///
/// Charged to the caller's guard like every resolution (objects, depth,
/// ticks). Cycles terminate via a seen-set; nesting is capped at 32 levels.
fn count_outline_items(g: &mut BudgetGuard<'_>, w: &mut Walker<'_>, catalog: &Obj) -> Result<u64> {
    let Some(outlines_ref) = dict_ref(catalog, b"Outlines") else {
        return Ok(0);
    };
    let Some(root) = w.resolve(g, Ref::new(0, 0), outlines_ref)? else {
        return Ok(0);
    };
    let mut count = 0u64;
    count_outline_chain(g, w, &root, &mut HashSet::new(), 0, &mut count)?;
    Ok(count)
}

/// Walk one outline chain (`/First` → `/Next` under `node`), recursing into
/// each item's children. `seen` terminates cyclic chains.
fn count_outline_chain(
    g: &mut BudgetGuard<'_>,
    w: &mut Walker<'_>,
    node: &Obj,
    seen: &mut HashSet<(u32, u16)>,
    depth: usize,
    count: &mut u64,
) -> Result<()> {
    if depth > 32 || *count >= MAX_COUNTED_ENTRIES {
        return Ok(());
    }
    let Some(mut cur) = dict_ref(node, b"First") else {
        return Ok(());
    };
    loop {
        if *count >= MAX_COUNTED_ENTRIES || !seen.insert((cur.num, cur.gen)) {
            return Ok(());
        }
        let Some(item) = w.resolve(g, Ref::new(0, 0), cur)? else {
            return Ok(());
        };
        *count = count.saturating_add(1);
        if dict_ref(&item, b"First").is_some() {
            count_outline_chain(g, w, &item, seen, depth.saturating_add(1), count)?;
        }
        match dict_ref(&item, b"Next") {
            Some(next) => cur = next,
            None => return Ok(()),
        }
    }
}

/// Embedded-file entries in the catalog's `/Names`/`/EmbeddedFiles` name
/// tree: the number of key/value pairs across every node of the tree.
fn count_embedded_files(g: &mut BudgetGuard<'_>, w: &mut Walker<'_>, catalog: &Obj) -> Result<u64> {
    let Some(names_ref) = dict_ref(catalog, b"Names") else {
        return Ok(0);
    };
    let Some(names) = w.resolve(g, Ref::new(0, 0), names_ref)? else {
        return Ok(0);
    };
    let Some(files_val) = dict_get(&names, b"EmbeddedFiles") else {
        return Ok(0);
    };
    let files = resolve_inline(g, files_val, w)?;
    let mut count = 0u64;
    count_name_tree_keys(g, w, &files, 0, &mut count)?;
    Ok(count)
}

/// Count the key/value pairs of one name-tree node, recursing through
/// `/Kids`. Depth is capped; the count itself is bounded.
fn count_name_tree_keys(
    g: &mut BudgetGuard<'_>,
    w: &mut Walker<'_>,
    node: &Obj,
    depth: usize,
    count: &mut u64,
) -> Result<()> {
    if depth > 32 || *count >= MAX_COUNTED_ENTRIES {
        return Ok(());
    }
    if let Some(pairs_val) = dict_get(node, b"Names") {
        let pairs = resolve_inline(g, pairs_val, w)?;
        if let Obj::Array(items) = &pairs {
            // Key/value pairs: chunk into twos (an odd trailing item is a
            // malformed tree; `chunks(2)` counts the partial pair, which is
            // the conservative reading).
            let pairs = items.chunks(2).count();
            *count = count.saturating_add(u64::try_from(pairs).unwrap_or(u64::MAX));
        }
    }
    if let Some(kids_val) = dict_get(node, b"Kids") {
        let kids = resolve_inline(g, kids_val, w)?;
        if let Obj::Array(items) = &kids {
            for kid in items {
                if *count >= MAX_COUNTED_ENTRIES {
                    return Ok(());
                }
                let kid = resolve_inline(g, kid, w)?;
                count_name_tree_keys(g, w, &kid, depth.saturating_add(1), count)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use crate::doc_writer::{write_objects_as_document, ContentBuilder, DocumentBuilder};
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn bytes(v: &[u8]) -> selis_bytes::Bytes {
        selis_bytes::Bytes::copy_from_slice(v)
    }

    /// A formed + layered + annotated two-page document (the shape the tools
    /// produce): one annotation on each page, an AcroForm with one root field
    /// and one kid, one OCG. Object numbers: 1 catalog, 2 pages, 3 page,
    /// 4 content, 5 page, 6 content, 7 annot, 8 annot, 9 AcroForm, 10 field,
    /// 11 kid field, 12 OCProperties, 13 OCG.
    fn formed_document() -> Vec<u8> {
        let page = |parent: u32, content: u32, annots: u32| {
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Page"))),
                (bytes(b"Parent"), Obj::Ref(Ref::new(parent, 0))),
                (
                    bytes(b"MediaBox"),
                    Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(100), Obj::Int(100)]),
                ),
                (bytes(b"Contents"), Obj::Ref(Ref::new(content, 0))),
                (
                    bytes(b"Annots"),
                    Obj::Array(vec![Obj::Ref(Ref::new(annots, 0))]),
                ),
            ])
        };
        let objects = vec![
            (
                1,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Catalog"))),
                    (bytes(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                    (bytes(b"AcroForm"), Obj::Ref(Ref::new(9, 0))),
                    (bytes(b"OCProperties"), Obj::Ref(Ref::new(12, 0))),
                ]),
            ),
            (
                2,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Pages"))),
                    (
                        bytes(b"Kids"),
                        Obj::Array(vec![Obj::Ref(Ref::new(3, 0)), Obj::Ref(Ref::new(5, 0))]),
                    ),
                    (bytes(b"Count"), Obj::Int(2)),
                ]),
            ),
            (3, page(2, 4, 7)),
            (5, page(2, 6, 8)),
            (
                4,
                Obj::Stream {
                    dict: vec![(bytes(b"Length"), Obj::Int(2))],
                    data: selis_bytes::Bytes::copy_from_slice(b"q\n"),
                },
            ),
            (
                6,
                Obj::Stream {
                    dict: vec![(bytes(b"Length"), Obj::Int(2))],
                    data: selis_bytes::Bytes::copy_from_slice(b"Q\n"),
                },
            ),
            (
                7,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Annot"))),
                    (bytes(b"Subtype"), Obj::Name(bytes(b"Square"))),
                ]),
            ),
            (
                8,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Annot"))),
                    (bytes(b"Subtype"), Obj::Name(bytes(b"Square"))),
                ]),
            ),
            (
                9,
                Obj::Dict(vec![
                    (
                        bytes(b"Fields"),
                        Obj::Array(vec![Obj::Ref(Ref::new(10, 0))]),
                    ),
                    (
                        bytes(b"DA"),
                        Obj::String(selis_bytes::Bytes::copy_from_slice(b"/Helv 0 Tf")),
                    ),
                ]),
            ),
            (
                10,
                Obj::Dict(vec![
                    (
                        bytes(b"T"),
                        Obj::String(selis_bytes::Bytes::copy_from_slice(b"top")),
                    ),
                    (bytes(b"Kids"), Obj::Array(vec![Obj::Ref(Ref::new(11, 0))])),
                ]),
            ),
            (
                11,
                Obj::Dict(vec![(
                    bytes(b"T"),
                    Obj::String(selis_bytes::Bytes::copy_from_slice(b"kid")),
                )]),
            ),
            (
                12,
                Obj::Dict(vec![(
                    bytes(b"OCGs"),
                    Obj::Array(vec![Obj::Ref(Ref::new(13, 0))]),
                )]),
            ),
            (
                13,
                Obj::Dict(vec![(bytes(b"Type"), Obj::Name(bytes(b"OCG")))]),
            ),
        ];
        write_objects_as_document(&objects, Ref::new(1, 0), &Budget::unlimited(), &mut guard())
            .expect("write")
    }

    fn formed_expectations() -> Expectations {
        Expectations {
            pages: Some(2),
            annotations: Some(2),
            fields: Some(2),
            ocgs: Some(1),
            outlines: None,
            embedded_files: None,
        }
    }

    #[test]
    fn clean_writer_output_verifies() {
        let doc = formed_document();
        let budget = Budget::unlimited();
        let mut g = guard();
        let verdict =
            verify_structural(&doc, &formed_expectations(), &budget, &mut g).expect("verify runs");
        assert!(verdict.ok, "clean output must verify: {:?}", verdict.faults);
        assert_eq!(verdict.observed.pages, 2);
        assert_eq!(verdict.observed.annotations, 2);
        assert_eq!(verdict.observed.fields, 2, "fields count roots + kids");
        assert_eq!(verdict.observed.ocgs, 1);
    }

    #[test]
    fn document_builder_output_verifies() {
        let mut b = DocumentBuilder::new();
        let mut c = ContentBuilder::new();
        c.set_fill(1.0, 0.0, 0.0).fill_rect(0.0, 0.0, 10.0, 10.0);
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
        let doc = b.write(&Budget::unlimited(), &mut guard()).expect("write");
        let mut g = guard();
        let verdict = verify_structural(
            &doc,
            &Expectations {
                pages: Some(2),
                ..Expectations::none()
            },
            &Budget::unlimited(),
            &mut g,
        )
        .expect("verify runs");
        assert!(
            verdict.ok,
            "builder output must verify: {:?}",
            verdict.faults
        );
    }

    /// DoD negative test: a deliberately-corrupted writer output is caught.
    /// Corruptions are byte-level edits of a genuinely written document.
    #[test]
    fn corrupted_writer_output_is_caught() {
        let budget = Budget::unlimited();
        let mut g = guard();
        let doc = formed_document();

        // (1) Truncate the tail: no startxref at all.
        let cut = doc.len().saturating_sub(24);
        let torn_tail = doc[..cut].to_vec();
        let verdict = verify_structural(&torn_tail, &Expectations::none(), &budget, &mut g)
            .expect("verify runs");
        assert!(!verdict.ok, "tail-truncated output must fail");
        assert!(
            matches!(verdict.faults.first(), Some(Fault::NoStartxref)),
            "no-startxref fault: {:?}",
            verdict.faults
        );

        // (2) Truncate mid-body: the startxref survives but xref offsets point
        // past the end — objects cannot resolve.
        let cut = doc.len() >> 1;
        let torn_body = doc[..cut].to_vec();
        let verdict = verify_structural(&torn_body, &Expectations::none(), &budget, &mut g)
            .expect("verify runs");
        assert!(
            !verdict.ok,
            "mid-body truncation must fail: {:?}",
            verdict.faults
        );

        // (3) Dangling reference: a document whose page's /Contents points at
        // an object that was never written (sparse xref, free entry). The
        // "reachable-that-should-be-reachable is missing" check catches it.
        let with_gap = |contents: Ref| {
            vec![
                (
                    1u32,
                    Obj::Dict(vec![
                        (bytes(b"Type"), Obj::Name(bytes(b"Catalog"))),
                        (bytes(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                    ]),
                ),
                (
                    2,
                    Obj::Dict(vec![
                        (bytes(b"Type"), Obj::Name(bytes(b"Pages"))),
                        (bytes(b"Kids"), Obj::Array(vec![Obj::Ref(Ref::new(3, 0))])),
                        (bytes(b"Count"), Obj::Int(1)),
                    ]),
                ),
                (
                    3,
                    Obj::Dict(vec![
                        (bytes(b"Type"), Obj::Name(bytes(b"Page"))),
                        (bytes(b"Parent"), Obj::Ref(Ref::new(2, 0))),
                        (
                            bytes(b"MediaBox"),
                            Obj::Array(vec![
                                Obj::Int(0),
                                Obj::Int(0),
                                Obj::Int(100),
                                Obj::Int(100),
                            ]),
                        ),
                        (bytes(b"Contents"), Obj::Ref(contents)),
                    ]),
                ),
            ]
        };
        let _ = write_objects_as_document(
            &with_gap(Ref::new(4, 0)),
            Ref::new(1, 0),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("write");
        // Control: object 4 exists (a tiny stream) — the doc verifies.
        let mut complete = with_gap(Ref::new(4, 0));
        complete.push((
            4,
            Obj::Stream {
                dict: vec![(bytes(b"Length"), Obj::Int(2))],
                data: selis_bytes::Bytes::copy_from_slice(b"q\n"),
            },
        ));
        let complete_bytes = write_objects_as_document(
            &complete,
            Ref::new(1, 0),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("write");
        let verdict = verify_structural(&complete_bytes, &Expectations::none(), &budget, &mut g)
            .expect("verify runs");
        assert!(
            verdict.ok,
            "complete control must verify: {:?}",
            verdict.faults
        );

        // Now the same document *without* object 4: the reference dangles.
        let dangling = write_objects_as_document(
            &with_gap(Ref::new(4, 0)),
            Ref::new(1, 0),
            &Budget::unlimited(),
            &mut guard(),
        )
        .expect("write");
        let verdict = verify_structural(&dangling, &Expectations::none(), &budget, &mut g)
            .expect("verify runs");
        assert!(
            !verdict.ok,
            "dangling reference must fail: {:?}",
            verdict.faults
        );
        assert!(
            verdict
                .faults
                .iter()
                .any(|f| matches!(f, Fault::UnresolvedRef { target, .. } if target.num == 4)),
            "unresolved-ref fault for object 4: {:?}",
            verdict.faults
        );

        // (4) Expectation mismatch: the output is fine, the caller's page
        // count expectation is not.
        let verdict = verify_structural(
            &doc,
            &Expectations {
                pages: Some(5),
                ..Expectations::none()
            },
            &budget,
            &mut g,
        )
        .expect("verify runs");
        assert!(!verdict.ok, "expectation mismatch must fail");
        assert!(
            matches!(
                verdict.faults.first(),
                Some(Fault::PageCountMismatch {
                    expected: 5,
                    actual: 2
                })
            ),
            "page-count fault: {:?}",
            verdict.faults
        );

        // (5) Annotation count mismatch caught.
        let verdict = verify_structural(
            &doc,
            &Expectations {
                annotations: Some(7),
                ..Expectations::none()
            },
            &budget,
            &mut g,
        )
        .expect("verify runs");
        assert!(
            matches!(
                verdict.faults.first(),
                Some(Fault::AnnotationCountMismatch {
                    expected: 7,
                    actual: 2
                })
            ),
            "annotation-count fault: {:?}",
            verdict.faults
        );

        // (6) Field and OCG count mismatches caught.
        let verdict = verify_structural(
            &doc,
            &Expectations {
                fields: Some(1),
                ocgs: Some(4),
                ..Expectations::none()
            },
            &budget,
            &mut g,
        )
        .expect("verify runs");
        assert!(
            verdict
                .faults
                .iter()
                .any(|f| matches!(f, Fault::FieldCountMismatch { .. }))
                && verdict
                    .faults
                    .iter()
                    .any(|f| matches!(f, Fault::OcgCountMismatch { .. })),
            "field/ocg faults: {:?}",
            verdict.faults
        );
    }

    /// The page-tree `/Count` check: a writer output whose declared count
    /// disagrees with its `Kids` array is caught.
    #[test]
    fn page_tree_count_mismatch_is_caught() {
        let doc = formed_document();
        let pos = find_subslice(&doc, b"/Count 2").expect("count present");
        let mut doctored = doc.clone();
        // Same length: "/Count 2" -> "/Count 9".
        doctored[pos + 7..pos + 8].copy_from_slice(b"9");
        let budget = Budget::unlimited();
        let mut g = guard();
        let verdict = verify_structural(&doctored, &Expectations::none(), &budget, &mut g)
            .expect("verify runs");
        assert!(
            matches!(
                verdict.faults.first(),
                Some(Fault::PageTreeCountMismatch {
                    declared: 9,
                    walked: 2
                })
            ),
            "tree-count fault: {:?}",
            verdict.faults
        );
    }

    /// Incremental-update output (two revisions) verifies, and the original
    /// bytes remain a byte-identical prefix (I2 holds through verification).
    #[test]
    fn incremental_output_verifies() {
        let mut b = DocumentBuilder::new();
        let mut c = ContentBuilder::new();
        c.set_fill(1.0, 0.0, 0.0).fill_rect(0.0, 0.0, 10.0, 10.0);
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
        let budget = Budget::unlimited();
        let mut g = guard();
        let original = b.write(&budget, &mut g).expect("write");

        let rotated = Obj::Dict(vec![
            (bytes(b"Type"), Obj::Name(bytes(b"Page"))),
            (bytes(b"Parent"), Obj::Ref(Ref::new(2, 0))),
            (
                bytes(b"MediaBox"),
                Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(100), Obj::Int(100)]),
            ),
            (bytes(b"Contents"), Obj::Ref(Ref::new(3, 0))),
            (bytes(b"Resources"), Obj::Dict(Vec::new())),
            (bytes(b"Rotate"), Obj::Int(90)),
        ]);
        let updated = crate::doc_writer::write_incremental_update(
            &original,
            &[(4, rotated)],
            &[(b"Root".to_vec(), Obj::Ref(Ref::new(1, 0)))],
            &budget,
            &mut g,
        )
        .expect("incr");
        assert!(updated.starts_with(&original), "I2: prefix preserved");

        let verdict = verify_structural(
            &updated,
            &Expectations {
                pages: Some(1),
                ..Expectations::none()
            },
            &budget,
            &mut g,
        )
        .expect("verify runs");
        assert!(
            verdict.ok,
            "incremental output must verify: {:?}",
            verdict.faults
        );
    }

    /// A cyclic object graph (outlines are doubly-linked) does not blow the
    /// depth cap: the walk visits each object once.
    #[test]
    fn cyclic_graph_walks_terminates() {
        let objects = vec![
            (
                1,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Catalog"))),
                    (bytes(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                    (bytes(b"Outlines"), Obj::Ref(Ref::new(3, 0))),
                ]),
            ),
            (
                2,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Pages"))),
                    (bytes(b"Kids"), Obj::Array(vec![Obj::Ref(Ref::new(4, 0))])),
                    (bytes(b"Count"), Obj::Int(1)),
                ]),
            ),
            (
                3,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Outlines"))),
                    (bytes(b"First"), Obj::Ref(Ref::new(5, 0))),
                    (bytes(b"Last"), Obj::Ref(Ref::new(5, 0))),
                    (bytes(b"Count"), Obj::Int(1)),
                ]),
            ),
            (
                5,
                Obj::Dict(vec![
                    (bytes(b"Title"), Obj::String(bytes(b"a"))),
                    (bytes(b"Parent"), Obj::Ref(Ref::new(3, 0))),
                    (bytes(b"Next"), Obj::Ref(Ref::new(5, 0))),
                    (bytes(b"Prev"), Obj::Ref(Ref::new(5, 0))),
                    (bytes(b"Dest"), Obj::Ref(Ref::new(4, 0))),
                ]),
            ),
            (
                4,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Page"))),
                    (bytes(b"Parent"), Obj::Ref(Ref::new(2, 0))),
                    (
                        bytes(b"MediaBox"),
                        Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(10), Obj::Int(10)]),
                    ),
                ]),
            ),
        ];
        let doc =
            write_objects_as_document(&objects, Ref::new(1, 0), &Budget::unlimited(), &mut guard())
                .expect("write");
        let budget = Budget::unlimited();
        let mut g = guard();
        let verdict =
            verify_structural(&doc, &Expectations::none(), &budget, &mut g).expect("verify runs");
        assert!(verdict.ok, "cyclic graph must verify: {:?}", verdict.faults);
    }

    /// A document with an outline tree (two top-level items, one carrying a
    /// child) and a two-node `/Names`/`/EmbeddedFiles` tree: the survey
    /// observes 3 outline entries and 2 embedded files, and wrong
    /// expectations for either are caught with the right fault.
    #[test]
    fn outline_and_embedded_file_counts_are_observed_and_enforced() {
        let files = |a: &str, b: &str| {
            Obj::Array(vec![
                Obj::String(bytes(a.as_bytes())),
                Obj::Dict(vec![(bytes(b"F"), Obj::String(bytes(b"f.pdf")))]),
                Obj::String(bytes(b.as_bytes())),
                Obj::Dict(vec![(bytes(b"F"), Obj::String(bytes(b"f.pdf")))]),
            ])
        };
        let objects = vec![
            (
                1,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Catalog"))),
                    (bytes(b"Pages"), Obj::Ref(Ref::new(2, 0))),
                    (bytes(b"Outlines"), Obj::Ref(Ref::new(3, 0))),
                    (bytes(b"Names"), Obj::Ref(Ref::new(6, 0))),
                ]),
            ),
            (
                2,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Pages"))),
                    (bytes(b"Kids"), Obj::Array(vec![Obj::Ref(Ref::new(4, 0))])),
                    (bytes(b"Count"), Obj::Int(1)),
                ]),
            ),
            (
                3,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Outlines"))),
                    (bytes(b"First"), Obj::Ref(Ref::new(5, 0))),
                    (bytes(b"Last"), Obj::Ref(Ref::new(7, 0))),
                    (bytes(b"Count"), Obj::Int(3)),
                ]),
            ),
            (
                4,
                Obj::Dict(vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Page"))),
                    (bytes(b"Parent"), Obj::Ref(Ref::new(2, 0))),
                    (
                        bytes(b"MediaBox"),
                        Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(10), Obj::Int(10)]),
                    ),
                ]),
            ),
            (
                5,
                Obj::Dict(vec![
                    (bytes(b"Title"), Obj::String(bytes(b"a"))),
                    (bytes(b"Parent"), Obj::Ref(Ref::new(3, 0))),
                    (bytes(b"Next"), Obj::Ref(Ref::new(7, 0))),
                    (bytes(b"First"), Obj::Ref(Ref::new(8, 0))),
                    (bytes(b"Last"), Obj::Ref(Ref::new(8, 0))),
                    (bytes(b"Count"), Obj::Int(1)),
                    (bytes(b"Dest"), Obj::Ref(Ref::new(4, 0))),
                ]),
            ),
            (
                6,
                Obj::Dict(vec![(bytes(b"EmbeddedFiles"), Obj::Ref(Ref::new(9, 0)))]),
            ),
            (
                7,
                Obj::Dict(vec![
                    (bytes(b"Title"), Obj::String(bytes(b"b"))),
                    (bytes(b"Parent"), Obj::Ref(Ref::new(3, 0))),
                    (bytes(b"Prev"), Obj::Ref(Ref::new(5, 0))),
                    (bytes(b"Dest"), Obj::Ref(Ref::new(4, 0))),
                ]),
            ),
            (
                8,
                Obj::Dict(vec![
                    (bytes(b"Title"), Obj::String(bytes(b"a1"))),
                    (bytes(b"Parent"), Obj::Ref(Ref::new(5, 0))),
                    (bytes(b"Dest"), Obj::Ref(Ref::new(4, 0))),
                ]),
            ),
            (
                9,
                Obj::Dict(vec![(
                    bytes(b"Kids"),
                    Obj::Array(vec![Obj::Ref(Ref::new(10, 0))]),
                )]),
            ),
            (
                10,
                Obj::Dict(vec![(bytes(b"Names"), files("file1", "file2"))]),
            ),
        ];
        let doc =
            write_objects_as_document(&objects, Ref::new(1, 0), &Budget::unlimited(), &mut guard())
                .expect("write");
        let budget = Budget::unlimited();
        let mut g = guard();

        let observed = survey(&doc, &budget, &mut g).expect("survey runs");
        assert_eq!(observed.outlines, 3, "two top-level items + one child");
        assert_eq!(observed.embedded_files, 2, "both name-tree keys counted");

        let verdict = verify_structural(
            &doc,
            &Expectations {
                outlines: Some(3),
                embedded_files: Some(2),
                ..Expectations::none()
            },
            &budget,
            &mut g,
        )
        .expect("verify runs");
        assert!(
            verdict.ok,
            "matching counts must verify: {:?}",
            verdict.faults
        );

        // Wrong outline expectation → the outline-count fault.
        let verdict = verify_structural(
            &doc,
            &Expectations {
                outlines: Some(9),
                ..Expectations::none()
            },
            &budget,
            &mut g,
        )
        .expect("verify runs");
        assert!(!verdict.ok, "wrong outline count must fail");
        assert!(matches!(
            verdict.faults.first(),
            Some(Fault::OutlineCountMismatch {
                expected: 9,
                actual: 3
            })
        ));

        // Wrong embedded-file expectation → the embedded-file-count fault.
        let verdict = verify_structural(
            &doc,
            &Expectations {
                embedded_files: Some(1),
                ..Expectations::none()
            },
            &budget,
            &mut g,
        )
        .expect("verify runs");
        assert!(!verdict.ok, "wrong embedded-file count must fail");
        assert!(matches!(
            verdict.faults.first(),
            Some(Fault::EmbeddedFileCountMismatch {
                expected: 1,
                actual: 2
            })
        ));
    }

    /// Find a byte subslice (test helper; same-length corruptions need it).
    fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }
}
