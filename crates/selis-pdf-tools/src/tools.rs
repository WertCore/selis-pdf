//! The `selis` document tools: merge, split, set-metadata, redact, and the
//! page operations (rotate, delete, reorder — SL-1A.TOOL.03).
//!
//! Built on the full-document writer + object-graph copy (WRITE.01/03/04),
//! with conformance-friendly defaults (WRITE.07): the tools carry the
//! input's tagged structure, output intents, metadata, outlines, page
//! labels, form fields, and named destinations through the rewrite,
//! pruning what only referenced deleted pages, and never introduce
//! JavaScript, launch actions, or external references.

#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use selis_pdf_cos::{self, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::verify_report::Verification;
use crate::write_gate::{self, Preserved, PRESERVE_ALL, PRESERVE_EXCEPT_PAGES};
use crate::{read_file, CliError, CliResult, SplitPart, MAX_SPLIT_OUTPUTS};

/// A parsed merge input with its source-page → merged-page mapping.
pub(crate) struct MergeInput {
    pub(crate) path: String,
    pub(crate) src: Vec<u8>,
    pub(crate) doc: selis_pdf_cos::Doc,
    pub(crate) page_refs: Vec<Ref>,
    pub(crate) page_map: std::collections::HashMap<u32, Ref>,
    pub(crate) pages: usize,
    /// For a non-contiguous output plan (page mixing), the output index of
    /// each source page, in source order. Merge leaves this `None` and shifts
    /// page-label keys by the running page offset instead.
    pub(crate) page_label_indices: Option<Vec<usize>>,
    /// When mixing documents that have page labels, an input without labels
    /// contributes its own default decimal numbering so another input's label
    /// style cannot leak across the interleaved pages.
    pub(crate) materialize_default_labels: bool,
}

/// Parse one merge input and collect its leaf pages without copying anything.
///
/// Page copying is deliberately separate: merge copies the inputs one after
/// another, while page mixing copies them round-robin.
pub(crate) fn open_merge_input(
    path: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<MergeInput> {
    let src = read_file(path)?;
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, budget, g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, budget, g).map_err(|e| CliError(format!("{path}: {e}")))?;
    Ok(MergeInput {
        path: path.to_string(),
        pages: page_refs.len(),
        src,
        doc,
        page_refs,
        page_map: std::collections::HashMap::new(),
        page_label_indices: None,
        materialize_default_labels: false,
    })
}

/// Merge several PDFs into one (SL-1A.TOOL.01 + SL-1A.WRITE.04: outlines,
/// named destinations, page labels, and embedded files survive and are
/// reconciled across the inputs). Returns the operation's verification
/// display (SL-1A.UI.02).
pub(crate) fn merge(inputs: &[String], output: &str) -> CliResult<Verification> {
    if inputs.len() < 2 {
        return Err(CliError("merge needs at least two input files".to_string()));
    }
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let mut merged = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;

    let mut parsed: Vec<MergeInput> = Vec::with_capacity(inputs.len());
    for path in inputs {
        parsed.push(open_merge_input(path, &budget, &mut g)?);
    }
    for input in &mut parsed {
        for &page_ref in &input.page_refs {
            let merged_ref = copy_page(
                &mut merged,
                &input.src,
                page_ref,
                &budget,
                &mut g,
                &mut next_num,
            )
            .map_err(|e| CliError(format!("{}: {e}", input.path)))?;
            input.page_map.insert(page_ref.num, merged_ref);
        }
    }

    // WRITE.04: reconcile document-level structures across the inputs.
    reconcile_inputs(&mut merged, &parsed, &budget, &mut g).map_err(CliError)?;

    let (verification, deduplicated) = write_merged(&mut merged, &parsed, output, &budget, &mut g)?;
    eprintln!(
        "merged {} file(s) into {output} ({} duplicate object(s) removed)",
        inputs.len(),
        deduplicated
    );
    verification.emit_line();
    Ok(verification)
}

/// Serialise a reconciled multi-input document through the verification gate.
///
/// Merge and page mixing share this commit surface: deduplicate identical
/// resources, write the full document, sum the inputs' surveyed structures as
/// the preservation expectation, verify, and build the display. Mixing reuses
/// the merge reconciliation guarantees by construction — it differs only in
/// the page order the inputs were copied in.
pub(crate) fn write_merged(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    inputs: &[MergeInput],
    output: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<(Verification, usize)> {
    // WRITE.03: identical resources across inputs collapse to one object.
    let deduplicated = merged.dedup(budget, g);
    let bytes = merged
        .write(budget, g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    // WRITE.05: structural verification before the output reaches disk.
    // Merge and mix preserve every input's structure (collisions are renamed,
    // never dropped), so the expectation is the sum of the inputs' surveyed
    // counts.
    let mut input_observed = selis_pdf_cos::verify::Observed::default();
    for input in inputs {
        let observed = selis_pdf_cos::verify::survey(&input.src, budget, g)
            .map_err(|e| CliError(format!("{}: {e}", input.path)))?;
        input_observed.pages = input_observed.pages.saturating_add(observed.pages);
        input_observed.annotations = input_observed
            .annotations
            .saturating_add(observed.annotations);
        input_observed.fields = input_observed.fields.saturating_add(observed.fields);
        input_observed.ocgs = input_observed.ocgs.saturating_add(observed.ocgs);
        input_observed.outlines = input_observed.outlines.saturating_add(observed.outlines);
        input_observed.embedded_files = input_observed
            .embedded_files
            .saturating_add(observed.embedded_files);
    }
    let expected = write_gate::expectations_from(&input_observed, &PRESERVE_ALL);
    let in_bytes = inputs
        .iter()
        .map(|i| i.src.len())
        .fold(0usize, usize::saturating_add);
    let verdict = write_gate::write_verified(&bytes, output, &expected, budget, g)?;
    let verification = Verification::from_gate(
        Some(&input_observed),
        &verdict,
        &expected,
        u64::try_from(in_bytes).unwrap_or(u64::MAX),
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    Ok((verification, deduplicated))
}

/// Interleave the pages of two or more PDFs.
///
/// Round one contributes page 1 of every input, round two page 2, and so on;
/// shorter inputs simply stop contributing. The page copy, document-level
/// reconciliation, and verification gate are the merge path, so annotations,
/// forms, outlines, named destinations, embedded files, and structure survive
/// with the same guarantees.
///
/// Page labels cannot use merge's contiguous offset once pages are interleaved,
/// so every page gets its own materialised label. When any input carries
/// `/PageLabels`, inputs without labels contribute explicit decimal defaults;
/// otherwise another input's prefix or numbering style would leak across the
/// mixed pages.
pub(crate) fn mix(inputs: &[String], output: &str) -> CliResult<Verification> {
    if inputs.len() < 2 {
        return Err(CliError("mix needs at least two input files".to_string()));
    }
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let mut merged = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;

    let mut parsed: Vec<MergeInput> = Vec::with_capacity(inputs.len());
    for path in inputs {
        parsed.push(open_merge_input(path, &budget, &mut g)?);
    }

    let has_labels = parsed.iter().any(|input| {
        matches!(
            selis_pdf_cos::reconcile::catalog_entry(
                &input.src,
                &input.doc,
                b"PageLabels",
                &budget,
                &mut g,
            ),
            Ok(Some(_))
        )
    });

    let max_pages = parsed.iter().map(|input| input.pages).max().unwrap_or(0);
    let mut indices: Vec<Vec<usize>> = parsed
        .iter()
        .map(|input| vec![usize::MAX; input.pages])
        .collect();
    let mut plan: Vec<(usize, usize)> = Vec::new();
    for page_idx in 0..max_pages {
        for (input_idx, input) in parsed.iter().enumerate() {
            if page_idx >= input.pages {
                continue;
            }
            let output_idx = plan.len();
            if let Some(slot) = indices
                .get_mut(input_idx)
                .and_then(|row| row.get_mut(page_idx))
            {
                *slot = output_idx;
            }
            plan.push((input_idx, page_idx));
        }
    }

    for (input_idx, input) in parsed.iter_mut().enumerate() {
        if let Some(row) = indices.get_mut(input_idx) {
            input.page_label_indices = Some(std::mem::take(row));
        }
        input.materialize_default_labels = has_labels;
    }

    for (input_idx, page_idx) in plan {
        let input = parsed
            .get_mut(input_idx)
            .ok_or_else(|| CliError("mix page plan references a missing input".to_string()))?;
        let page_ref = input.page_refs.get(page_idx).copied().ok_or_else(|| {
            CliError(format!("{}: page {} disappeared", input.path, page_idx + 1))
        })?;
        let merged_ref = copy_page(
            &mut merged,
            &input.src,
            page_ref,
            &budget,
            &mut g,
            &mut next_num,
        )
        .map_err(|e| CliError(format!("{}: {e}", input.path)))?;
        input.page_map.insert(page_ref.num, merged_ref);
    }

    reconcile_inputs(&mut merged, &parsed, &budget, &mut g).map_err(CliError)?;
    let (verification, deduplicated) = write_merged(&mut merged, &parsed, output, &budget, &mut g)?;
    eprintln!(
        "mixed {} file(s) into {output} ({} duplicate object(s) removed)",
        inputs.len(),
        deduplicated
    );
    verification.emit_line();
    Ok(verification)
}

/// Warn on stderr when the document carries a digital signature that a full
/// rewrite will invalidate (TOOL.04 engineering note: detect this *before*
/// running and tell the user, rather than silently handing back a document
/// whose signature has quietly died). Applies to every structure-rewriting
/// tool — unlock, clear-permissions, protect.
///
/// Detection is deliberately narrow and honest: interactive signature fields
/// under `/AcroForm` (`/FT /Sig`) and the document time-stamp / MDP entry
/// under a catalog `/Perms` *dictionary* (`/Signatures`). It is a warning,
/// not a refusal — the user may hold the signing key and re-sign the output.
///
/// # Budget
///
/// Charged to the caller's guard like every parse: a handful of object
/// resolutions bounded by the `/AcroForm` field list. Never walks page trees.
///
/// # Malformed Input
///
/// A missing or damaged `/AcroForm`/`/Perms` simply yields no warning; the
/// helper never fails the operation it guards.
pub(crate) fn warn_if_digital_signature(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    path: &str,
) {
    use selis_pdf_cos::copy::resolve_ref;
    use selis_pdf_cos::reconcile as rec;

    // Signature fields: /AcroForm → /Fields → any field with /FT /Sig.
    if let Ok(Some(Obj::Ref(acro_ref))) = rec::catalog_entry(src, doc, b"AcroForm", budget, g) {
        if let Ok(Obj::Dict(form_pairs)) = resolve_ref(src, doc, acro_ref, budget, g) {
            if let Some((_, Obj::Array(fields))) =
                form_pairs.iter().find(|(k, _)| k.as_slice() == b"Fields")
            {
                let has_sig_field = fields.iter().any(|field| {
                    let resolved = match field {
                        Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g),
                        other => Ok(other.clone()),
                    };
                    matches!(&resolved, Ok(Obj::Dict(field_pairs)) if field_pairs
                    .iter()
                    .any(|(k, v)| {
                        k.as_slice() == b"FT"
                            && matches!(v, Obj::Name(n) if n.as_slice() == b"Sig")
                    }))
                });
                if has_sig_field {
                    eprintln!(
                        "warning: {path} carries a digital signature; this rewrite invalidates \
                         it (re-sign the output to restore one)"
                    );
                    return;
                }
            }
        }
    }

    // Document-level: a catalog /Perms *dictionary* (the MDP form) carrying
    // /Signatures. (The permission-bits form of /Perms is an integer and
    // carries no signatures.)
    if let Ok(Some(perms)) = rec::catalog_entry(src, doc, b"Perms", budget, g) {
        let resolved = match &perms {
            Obj::Ref(r) => resolve_ref(src, doc, *r, budget, g),
            other => Ok(other.clone()),
        };
        if let Ok(Obj::Dict(perms_pairs)) = resolved {
            if perms_pairs
                .iter()
                .any(|(k, _)| k.as_slice() == b"Signatures")
            {
                eprintln!(
                    "warning: {path} carries a digital signature; this rewrite invalidates it \
                     (re-sign the output to restore one)"
                );
            }
        }
    }
}

/// Reconcile the merged document's catalog-level structures (WRITE.04):
/// outlines (bookmarks), page labels, named destinations, embedded files,
/// form fields, the structure tree (ADR-P0031 — a merged document stays
/// tagged), optional-content groups, and the trailer `/ID`. Each input's
/// page references are redirected at the merged pages.
pub(crate) fn reconcile_inputs(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    inputs: &[MergeInput],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    use selis_pdf_cos::reconcile as rec;

    let mut chains: Vec<OutlineChain> = Vec::new();
    let mut old_outline_roots: Vec<u32> = Vec::new();
    let mut label_trees: Vec<(Vec<(i64, Obj)>, i64)> = Vec::new();
    let mut dest_pairs: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    let mut file_pairs: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    let mut page_offset = 0i64;

    // Form fields (WRITE.04 DoD).
    let mut field_refs: Vec<Ref> = Vec::new();
    let mut seen_field_names: std::collections::HashMap<Vec<u8>, u32> =
        std::collections::HashMap::new();
    let mut default_appearance: Option<Obj> = None;
    let mut need_appearances = false;
    let mut form_resources: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    let mut calc_order: Vec<Obj> = Vec::new();

    // Structure tree (ADR-P0031): a merged document must stay tagged.
    let mut struct_kids: Vec<Obj> = Vec::new();
    let mut parent_pairs: Vec<(i64, Obj)> = Vec::new();
    let mut role_map: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    let mut class_map: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    let mut old_struct_roots: Vec<u32> = Vec::new();
    let mut id_base = 0i64;

    // Optional-content groups.
    let mut ocg_refs: Vec<Obj> = Vec::new();
    let mut oc_defaults: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();

    // WRITE.07: catalog-level conformance state carried through the merge.
    // `/Lang`, `/MarkInfo`, `/ViewerPreferences`, `/PageMode`, `/PageLayout`
    // come from the first input that has them (first-wins); `/OutputIntents`
    // concatenate across the inputs. `/Metadata` (XMP) is deliberately not
    // carried: no single XMP packet can describe a multi-document merge
    // truthfully, and the merged document carries no Info dictionary either,
    // so no Info/XMP conflict is introduced.
    let mut first_scalars: Vec<(Vec<u8>, Obj)> = Vec::new();
    let mut output_intents: Vec<Obj> = Vec::new();

    for input in inputs {
        let mut cache = std::collections::HashMap::new();

        // Outlines: copy the tree, harvesting its top-level chain.
        if let Some(Obj::Ref(root_ref)) =
            rec::catalog_entry(&input.src, &input.doc, b"Outlines", budget, g)
                .map_err(|e| e.to_string())?
        {
            let copied = rec::copy_value_into(
                merged,
                &input.src,
                &input.doc,
                &input.page_map,
                &mut cache,
                &Obj::Ref(root_ref),
                budget,
                g,
            )
            .map_err(|e| e.to_string())?;
            if let Obj::Ref(new_root) = copied {
                if let Some(chain) = harvest_outline_chain(merged, new_root.num) {
                    old_outline_roots.push(new_root.num);
                    chains.push(chain);
                }
            }
        }

        // Page labels: merge shifts each input's keys by the pages merged
        // before it. Mixing interleaves the pages, so it materialises one
        // label per output page instead: a shifted range would let one
        // input's prefix/style leak into the other input's pages.
        let mut labels_materialized = false;
        if let Some(labels) = rec::catalog_entry(&input.src, &input.doc, b"PageLabels", budget, g)
            .map_err(|e| e.to_string())?
        {
            if let Ok(pairs) = rec::number_tree_pairs(&labels, &input.src, &input.doc, budget, g) {
                if !pairs.is_empty() {
                    if let Some(indices) = &input.page_label_indices {
                        let remapped = materialize_page_labels(&pairs, indices, input.pages);
                        if !remapped.is_empty() {
                            label_trees.push((remapped, 0));
                            labels_materialized = true;
                        }
                    } else {
                        label_trees.push((pairs, page_offset));
                    }
                }
            }
        }
        if !labels_materialized {
            if let Some(indices) = &input.page_label_indices {
                if input.materialize_default_labels {
                    let defaults = default_page_labels(indices);
                    if !defaults.is_empty() {
                        label_trees.push((defaults, 0));
                    }
                }
            }
        }

        // Named destinations and embedded files from the /Names tree.
        if let Some(names) = rec::catalog_entry(&input.src, &input.doc, b"Names", budget, g)
            .map_err(|e| e.to_string())?
        {
            let names = resolve_shallow(&input.src, &input.doc, names, budget, g);
            if let Obj::Dict(pairs) = &names {
                for (k, v) in pairs {
                    if k.as_slice() == b"Dests" {
                        let v = resolve_shallow(&input.src, &input.doc, v.clone(), budget, g);
                        if let Ok(found) =
                            rec::name_tree_pairs(&v, &input.src, &input.doc, budget, g)
                        {
                            for (name, value) in found {
                                let value = rec::copy_value_into(
                                    merged,
                                    &input.src,
                                    &input.doc,
                                    &input.page_map,
                                    &mut cache,
                                    &value,
                                    budget,
                                    g,
                                )
                                .map_err(|e| e.to_string())?;
                                dest_pairs.push((name, value));
                            }
                        }
                    } else if k.as_slice() == b"EmbeddedFiles" {
                        let v = resolve_shallow(&input.src, &input.doc, v.clone(), budget, g);
                        if let Ok(found) =
                            rec::name_tree_pairs(&v, &input.src, &input.doc, budget, g)
                        {
                            for (name, value) in found {
                                let value = rec::copy_value_into(
                                    merged,
                                    &input.src,
                                    &input.doc,
                                    &input.page_map,
                                    &mut cache,
                                    &value,
                                    budget,
                                    g,
                                )
                                .map_err(|e| e.to_string())?;
                                file_pairs.push((name, value));
                            }
                        }
                    }
                }
            }
        }

        // Legacy direct /Dests dictionary.
        if let Some(dests) = rec::catalog_entry(&input.src, &input.doc, b"Dests", budget, g)
            .map_err(|e| e.to_string())?
        {
            let dests = resolve_shallow(&input.src, &input.doc, dests, budget, g);
            if let Obj::Dict(pairs) = &dests {
                for (name, v) in pairs {
                    let value = rec::copy_value_into(
                        merged,
                        &input.src,
                        &input.doc,
                        &input.page_map,
                        &mut cache,
                        v,
                        budget,
                        g,
                    )
                    .map_err(|e| e.to_string())?;
                    dest_pairs.push((name.clone(), value));
                }
            }
        }

        // Form fields (WRITE.04 DoD): field roots are copied with their page
        // references remapped at the merged pages, colliding field names are
        // renamed, and default appearance / resources are merged first-wins.
        if let Some(form) = rec::catalog_entry(&input.src, &input.doc, b"AcroForm", budget, g)
            .map_err(|e| e.to_string())?
        {
            let form = resolve_shallow(&input.src, &input.doc, form, budget, g);
            if let Obj::Dict(pairs) = &form {
                for (k, v) in pairs {
                    match k.as_slice() {
                        b"Fields" => {
                            if let Obj::Array(items) = v {
                                for f in items {
                                    let copied = rec::copy_value_into(
                                        merged,
                                        &input.src,
                                        &input.doc,
                                        &input.page_map,
                                        &mut cache,
                                        f,
                                        budget,
                                        g,
                                    )
                                    .map_err(|e| e.to_string())?;
                                    if let Obj::Ref(r) = copied {
                                        rename_field_collision(
                                            merged,
                                            r.num,
                                            &mut seen_field_names,
                                        );
                                        field_refs.push(r);
                                    }
                                }
                            }
                        }
                        b"DA" if default_appearance.is_none() => {
                            default_appearance = Some(v.clone());
                        }
                        b"NeedAppearances" => {
                            if matches!(v, Obj::Bool(true)) {
                                need_appearances = true;
                            }
                        }
                        b"CO" => {
                            let copied = rec::copy_value_into(
                                merged,
                                &input.src,
                                &input.doc,
                                &input.page_map,
                                &mut cache,
                                v,
                                budget,
                                g,
                            )
                            .map_err(|e| e.to_string())?;
                            if let Obj::Array(items) = copied {
                                calc_order.extend(items);
                            }
                        }
                        b"DR" => {
                            let copied = rec::copy_value_into(
                                merged,
                                &input.src,
                                &input.doc,
                                &input.page_map,
                                &mut cache,
                                v,
                                budget,
                                g,
                            )
                            .map_err(|e| e.to_string())?;
                            if let Obj::Dict(dr) = copied {
                                union_dicts(&mut form_resources, dr);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Structure tree (ADR-P0031): `/K` is copied with page references
        // remapped, `/ParentTree` keys shift by the running struct-parents
        // id base, and each merged page's `/StructParents` is shifted to
        // match so marked-content stays registered with the parent tree.
        if let Some(struct_root) =
            rec::catalog_entry(&input.src, &input.doc, b"StructTreeRoot", budget, g)
                .map_err(|e| e.to_string())?
        {
            let src_root_ref = match &struct_root {
                Obj::Ref(r) => Some(*r),
                _ => None,
            };
            let struct_root = resolve_shallow(&input.src, &input.doc, struct_root, budget, g);
            if let Obj::Dict(pairs) = &struct_root {
                // Shift each merged page's struct-parents id and find the
                // input's highest id.
                let mut max_sp = 0i64;
                for (&src_num, &merged_ref) in &input.page_map {
                    let page = resolve_shallow(
                        &input.src,
                        &input.doc,
                        Obj::Ref(Ref::new(src_num, 0)),
                        budget,
                        g,
                    );
                    let Obj::Dict(pp) = &page else { continue };
                    let Some(sp) = pp
                        .iter()
                        .find(|(k, _)| k.as_slice() == b"StructParents")
                        .and_then(|(_, v)| match v {
                            Obj::Int(n) => Some(*n),
                            _ => None,
                        })
                    else {
                        continue;
                    };
                    max_sp = max_sp.max(sp);
                    edit_dict_entry(
                        merged,
                        merged_ref.num,
                        b"StructParents",
                        Obj::Int(sp.saturating_add(id_base)),
                    );
                }
                // Copy `/K` first so its elements register in the cache; the
                // `/ParentTree` values then redirect at the same objects.
                if let Some((_, k)) = pairs.iter().find(|(k, _)| k.as_slice() == b"K") {
                    let copied = rec::copy_value_into(
                        merged,
                        &input.src,
                        &input.doc,
                        &input.page_map,
                        &mut cache,
                        k,
                        budget,
                        g,
                    )
                    .map_err(|e| e.to_string())?;
                    match copied {
                        Obj::Array(items) => struct_kids.extend(items),
                        other => struct_kids.push(other),
                    }
                }
                if let Some((_, pt)) = pairs.iter().find(|(k, _)| k.as_slice() == b"ParentTree") {
                    if let Ok(found) =
                        rec::number_tree_pairs_unresolved(pt, &input.src, &input.doc, budget, g)
                    {
                        for (key, value) in found {
                            let copied = rec::copy_value_into(
                                merged,
                                &input.src,
                                &input.doc,
                                &input.page_map,
                                &mut cache,
                                &value,
                                budget,
                                g,
                            )
                            .map_err(|e| e.to_string())?;
                            parent_pairs.push((key.saturating_add(id_base), copied));
                        }
                    }
                }
                if let Some((_, rm)) = pairs.iter().find(|(k, _)| k.as_slice() == b"RoleMap") {
                    let copied = rec::copy_value_into(
                        merged,
                        &input.src,
                        &input.doc,
                        &input.page_map,
                        &mut cache,
                        rm,
                        budget,
                        g,
                    )
                    .map_err(|e| e.to_string())?;
                    if let Obj::Dict(d) = copied {
                        union_dicts(&mut role_map, d);
                    }
                }
                if let Some((_, cm)) = pairs.iter().find(|(k, _)| k.as_slice() == b"ClassMap") {
                    let copied = rec::copy_value_into(
                        merged,
                        &input.src,
                        &input.doc,
                        &input.page_map,
                        &mut cache,
                        cm,
                        budget,
                        g,
                    )
                    .map_err(|e| e.to_string())?;
                    if let Obj::Dict(d) = copied {
                        union_dicts(&mut class_map, d);
                    }
                }
                id_base = id_base.saturating_add(max_sp.saturating_add(1));
                // The copied source root's kids are re-parented at the merged
                // root below; record it for removal so it does not linger.
                if let Some(r) = src_root_ref {
                    if let Some(&copied_num) = cache.get(&(r.num, r.gen)) {
                        old_struct_roots.push(copied_num);
                    }
                }
            }
        }

        // Optional-content groups: OCGs are copied and concatenated; the
        // default configuration's arrays (Order, ON, OFF, …) are
        // concatenated across inputs, scalars kept first-wins.
        if let Some(oc) = rec::catalog_entry(&input.src, &input.doc, b"OCProperties", budget, g)
            .map_err(|e| e.to_string())?
        {
            let oc = resolve_shallow(&input.src, &input.doc, oc, budget, g);
            if let Obj::Dict(pairs) = &oc {
                if let Some((_, Obj::Array(items))) =
                    pairs.iter().find(|(k, _)| k.as_slice() == b"OCGs")
                {
                    for item in items {
                        let copied = rec::copy_value_into(
                            merged,
                            &input.src,
                            &input.doc,
                            &input.page_map,
                            &mut cache,
                            item,
                            budget,
                            g,
                        )
                        .map_err(|e| e.to_string())?;
                        ocg_refs.push(copied);
                    }
                }
                if let Some((_, d)) = pairs.iter().find(|(k, _)| k.as_slice() == b"D") {
                    let d = resolve_shallow(&input.src, &input.doc, d.clone(), budget, g);
                    if let Obj::Dict(dpairs) = &d {
                        for (k, v) in dpairs {
                            let copied = rec::copy_value_into(
                                merged,
                                &input.src,
                                &input.doc,
                                &input.page_map,
                                &mut cache,
                                v,
                                budget,
                                g,
                            )
                            .map_err(|e| e.to_string())?;
                            if let Some((_, slot)) = oc_defaults
                                .iter_mut()
                                .find(|(ak, _)| ak.as_slice() == k.as_slice())
                            {
                                if let (Obj::Array(a), Obj::Array(items)) = (&mut *slot, &copied) {
                                    a.extend(items.iter().cloned());
                                }
                            } else {
                                oc_defaults.push((k.clone(), copied));
                            }
                        }
                    }
                }
            }
        }

        // WRITE.07: first-wins catalog scalars and concatenated
        // `/OutputIntents` (see the declaration above).
        for key in [
            b"Lang".as_slice(),
            b"MarkInfo".as_slice(),
            b"ViewerPreferences".as_slice(),
            b"PageMode".as_slice(),
            b"PageLayout".as_slice(),
        ] {
            if first_scalars.iter().any(|(k, _)| k.as_slice() == key) {
                continue;
            }
            if let Some(v) = rec::catalog_entry(&input.src, &input.doc, key, budget, g)
                .map_err(|e| e.to_string())?
            {
                let copied = rec::copy_value_into(
                    merged,
                    &input.src,
                    &input.doc,
                    &input.page_map,
                    &mut cache,
                    &v,
                    budget,
                    g,
                )
                .map_err(|e| e.to_string())?;
                first_scalars.push((key.to_vec(), copied));
            }
        }
        if let Some(oi) = rec::catalog_entry(&input.src, &input.doc, b"OutputIntents", budget, g)
            .map_err(|e| e.to_string())?
        {
            let oi = resolve_shallow(&input.src, &input.doc, oi, budget, g);
            if let Obj::Array(items) = oi {
                for item in items {
                    let copied = rec::copy_value_into(
                        merged,
                        &input.src,
                        &input.doc,
                        &input.page_map,
                        &mut cache,
                        &item,
                        budget,
                        g,
                    )
                    .map_err(|e| e.to_string())?;
                    output_intents.push(copied);
                }
            }
        }

        page_offset = page_offset.saturating_add(i64::try_from(input.pages).unwrap_or(i64::MAX));
    }

    attach_outlines(merged, &chains, &old_outline_roots);
    attach_page_labels(merged, &label_trees);
    attach_names(merged, dest_pairs, file_pairs);
    attach_acroform(
        merged,
        &field_refs,
        default_appearance,
        need_appearances,
        form_resources,
        calc_order,
    );
    let top_level_elements: Vec<u32> = struct_kids
        .iter()
        .filter_map(|k| match k {
            Obj::Ref(r) => Some(r.num),
            _ => None,
        })
        .collect();
    let struct_root_num =
        attach_struct_tree(merged, struct_kids, parent_pairs, role_map, class_map);
    if let Some(root_num) = struct_root_num {
        // Top-level elements' `/P` pointed at the copied source roots; repoint
        // it at the merged root, then drop the source roots.
        for num in top_level_elements {
            edit_dict_entry(merged, num, b"P", Obj::Ref(Ref::new(root_num, 0)));
        }
        merged
            .objects_mut()
            .retain(|(num, _)| !old_struct_roots.contains(num));
    }
    attach_ocproperties(merged, ocg_refs, oc_defaults);

    // WRITE.07: attach the first-wins catalog scalars and the concatenated
    // output intents collected above.
    for (key, value) in first_scalars {
        merged.add_catalog_entry(&key, value);
    }
    if !output_intents.is_empty() {
        let num = merged.allocate();
        merged.add_object(num, Obj::Array(output_intents));
        merged.add_catalog_entry(b"OutputIntents", Obj::Ref(Ref::new(num, 0)));
    }

    // A fresh /ID: the merged document is a new identity (WRITE.04).
    let id = document_id(inputs);
    merged.set_id(id.to_vec(), id.to_vec());
    Ok(())
}

/// Resolve a value one level: indirect references become their object,
/// everything else passes through.
fn resolve_shallow(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    obj: Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Obj {
    match obj {
        Obj::Ref(r) => {
            selis_pdf_cos::copy::resolve_ref(src, doc, r, budget, g).unwrap_or(Obj::Null)
        }
        other => other,
    }
}

/// A harvested top-level outline chain: its first/last items, visible count,
/// and the item object numbers in order.
struct OutlineChain {
    first: u32,
    last: u32,
    count: i64,
    items: Vec<u32>,
}

/// Walk the top-level items of a copied outline root, returning the chain.
fn harvest_outline_chain(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    root_num: u32,
) -> Option<OutlineChain> {
    let (first, count) = {
        let (_, root) = merged
            .objects_mut()
            .iter()
            .find(|(num, _)| *num == root_num)?;
        let Obj::Dict(pairs) = root else { return None };
        let first = pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"First")
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            })?;
        let count = pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"Count")
            .and_then(|(_, v)| match v {
                Obj::Int(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        (first, count)
    };
    let mut items = Vec::new();
    let mut cur = first;
    let mut last = first.num;
    for _ in 0..10_000 {
        items.push(cur.num);
        last = cur.num;
        let next = {
            let Some((_, obj)) = merged.objects_mut().iter().find(|(num, _)| *num == cur.num)
            else {
                break;
            };
            let Obj::Dict(pairs) = obj else { break };
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == b"Next")
                .and_then(|(_, v)| match v {
                    Obj::Ref(r) => Some(*r),
                    _ => None,
                })
        };
        let Some(next) = next else { break };
        if items.contains(&next.num) {
            break;
        }
        cur = next;
    }
    Some(OutlineChain {
        first: first.num,
        last,
        count,
        items,
    })
}

/// Build the merged outlines root and stitch each input's chain into one
/// top-level sequence, re-pointing `/Parent` entries at the new root. The
/// per-input copied outline roots are then dropped: their items have been
/// re-parented at the merged root, so nothing references them anymore.
fn attach_outlines(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    chains: &[OutlineChain],
    old_roots: &[u32],
) {
    if chains.is_empty() {
        return;
    }
    let root_num = merged.allocate();
    merged.add_object(root_num, Obj::Dict(Vec::new()));

    let mut prev_last: Option<u32> = None;
    for chain in chains {
        // Re-parent every top-level item at the merged root.
        for &item_num in &chain.items {
            edit_dict_entry(merged, item_num, b"Parent", Obj::Ref(Ref::new(root_num, 0)));
        }
        // Stitch onto the previous chain.
        if let Some(pl) = prev_last {
            edit_dict_entry(merged, pl, b"Next", Obj::Ref(Ref::new(chain.first, 0)));
            if let Some((_, prev_obj)) = merged
                .objects_mut()
                .iter_mut()
                .find(|(num, _)| *num == chain.first)
            {
                if !dict_has(prev_obj, b"Prev") {
                    if let Obj::Dict(pairs) = prev_obj {
                        pairs.push((
                            selis_bytes::Bytes::copy_from_slice(b"Prev"),
                            Obj::Ref(Ref::new(pl, 0)),
                        ));
                    }
                }
            }
        }
        prev_last = Some(chain.last);
    }

    let total: i64 = chains
        .iter()
        .map(|c| {
            if c.count > 0 {
                c.count
            } else {
                i64::try_from(c.items.len()).unwrap_or(i64::MAX)
            }
        })
        .sum();
    let first = chains.first().map(|c| c.first).unwrap_or(0);
    let last = chains.last().map(|c| c.last).unwrap_or(0);
    edit_dict_entry(
        merged,
        root_num,
        b"Type",
        Obj::Name(selis_bytes::Bytes::copy_from_slice(b"Outlines")),
    );
    edit_dict_entry(merged, root_num, b"First", Obj::Ref(Ref::new(first, 0)));
    edit_dict_entry(merged, root_num, b"Last", Obj::Ref(Ref::new(last, 0)));
    edit_dict_entry(merged, root_num, b"Count", Obj::Int(total));
    merged.add_catalog_entry(b"Outlines", Obj::Ref(Ref::new(root_num, 0)));

    // Drop the per-input copied outline roots (now unreferenced).
    merged
        .objects_mut()
        .retain(|(num, _)| !old_roots.contains(num));
}

/// Merge the inputs' page-label number trees into one, offset by page index.
fn attach_page_labels(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    trees: &[(Vec<(i64, Obj)>, i64)],
) {
    if trees.is_empty() {
        return;
    }
    let tree = selis_pdf_cos::reconcile::merge_number_trees(trees);
    let num = merged.allocate();
    merged.add_object(num, tree);
    merged.add_catalog_entry(b"PageLabels", Obj::Ref(Ref::new(num, 0)));
}

/// Build the merged `/Names` tree (named destinations + embedded files),
/// renaming collisions so no name is lost.
fn attach_names(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    dests: Vec<(selis_bytes::Bytes, Obj)>,
    files: Vec<(selis_bytes::Bytes, Obj)>,
) {
    let mut names_pairs: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    if !dests.is_empty() {
        let tree = selis_pdf_cos::reconcile::build_name_tree(unique_names(dests));
        let num = merged.allocate();
        merged.add_object(num, tree);
        names_pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"Dests"),
            Obj::Ref(Ref::new(num, 0)),
        ));
    }
    if !files.is_empty() {
        let tree = selis_pdf_cos::reconcile::build_name_tree(unique_names(files));
        let num = merged.allocate();
        merged.add_object(num, tree);
        names_pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"EmbeddedFiles"),
            Obj::Ref(Ref::new(num, 0)),
        ));
    }
    if !names_pairs.is_empty() {
        let names = Obj::Dict(names_pairs);
        let num = merged.allocate();
        merged.add_object(num, names);
        merged.add_catalog_entry(b"Names", Obj::Ref(Ref::new(num, 0)));
    }
}

/// Rename a copied field root when its `/T` (partial name) collides with an
/// earlier input's field, mirroring the name-tree rename: no field is lost.
fn rename_field_collision(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    num: u32,
    seen: &mut std::collections::HashMap<Vec<u8>, u32>,
) {
    let name = {
        let Some((_, obj)) = merged.objects_mut().iter().find(|(n, _)| *n == num) else {
            return;
        };
        let Obj::Dict(pairs) = obj else { return };
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"T")
            .and_then(|(_, v)| match v {
                Obj::String(s) => Some(s.as_slice().to_vec()),
                Obj::Name(n) => Some(n.as_slice().to_vec()),
                _ => None,
            })
    };
    let Some(name) = name else {
        return;
    };
    let count = seen.entry(name.clone()).or_insert(0);
    *count = count.saturating_add(1);
    if *count > 1 {
        let renamed = format!("{}-{}", String::from_utf8_lossy(&name), count);
        edit_dict_entry(
            merged,
            num,
            b"T",
            Obj::String(selis_bytes::Bytes::copy_from_slice(renamed.as_bytes())),
        );
    }
}

/// Union `extra` into `acc` (first-wins per key); colliding sub-dictionaries
/// are unioned entry-wise so resources like `/DR` or `/RoleMap` keep entries
/// from every input.
fn union_dicts(acc: &mut Vec<(selis_bytes::Bytes, Obj)>, extra: Vec<(selis_bytes::Bytes, Obj)>) {
    for (k, v) in extra {
        if let Some((_, slot)) = acc.iter_mut().find(|(ak, _)| ak.as_slice() == k.as_slice()) {
            if let (Obj::Dict(existing), Obj::Dict(incoming)) = (&mut *slot, &v) {
                for (ik, iv) in incoming {
                    if !existing
                        .iter()
                        .any(|(ek, _)| ek.as_slice() == ik.as_slice())
                    {
                        existing.push((ik.clone(), iv.clone()));
                    }
                }
            }
        } else {
            acc.push((k, v));
        }
    }
}

/// Build the merged `/AcroForm` from the reconciled pieces (WRITE.04): one
/// field list spanning all inputs, first-wins default appearance,
/// any-true `/NeedAppearances`, unioned default resources.
fn attach_acroform(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    field_refs: &[Ref],
    default_appearance: Option<Obj>,
    need_appearances: bool,
    form_resources: Vec<(selis_bytes::Bytes, Obj)>,
    calc_order: Vec<Obj>,
) {
    if field_refs.is_empty() {
        return;
    }
    let mut pairs: Vec<(selis_bytes::Bytes, Obj)> = vec![(
        selis_bytes::Bytes::copy_from_slice(b"Fields"),
        Obj::Array(field_refs.iter().map(|r| Obj::Ref(*r)).collect()),
    )];
    if let Some(da) = default_appearance {
        pairs.push((selis_bytes::Bytes::copy_from_slice(b"DA"), da));
    }
    if need_appearances {
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"NeedAppearances"),
            Obj::Bool(true),
        ));
    }
    if !form_resources.is_empty() {
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"DR"),
            Obj::Dict(form_resources),
        ));
    }
    if !calc_order.is_empty() {
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"CO"),
            Obj::Array(calc_order),
        ));
    }
    let num = merged.allocate();
    merged.add_object(num, Obj::Dict(pairs));
    merged.add_catalog_entry(b"AcroForm", Obj::Ref(Ref::new(num, 0)));
}

/// Build the merged `/StructTreeRoot` from the reconciled pieces
/// (ADR-P0031): `/K` kids from every input, the shifted `/ParentTree`,
/// unioned role and class maps. Returns the merged root's object number so
/// callers can re-point the top-level elements' `/P` at it.
fn attach_struct_tree(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    struct_kids: Vec<Obj>,
    parent_pairs: Vec<(i64, Obj)>,
    role_map: Vec<(selis_bytes::Bytes, Obj)>,
    class_map: Vec<(selis_bytes::Bytes, Obj)>,
) -> Option<u32> {
    if struct_kids.is_empty() && parent_pairs.is_empty() {
        return None;
    }
    let mut pairs: Vec<(selis_bytes::Bytes, Obj)> = vec![(
        selis_bytes::Bytes::copy_from_slice(b"Type"),
        Obj::Name(selis_bytes::Bytes::copy_from_slice(b"StructTreeRoot")),
    )];
    if !struct_kids.is_empty() {
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"K"),
            Obj::Array(struct_kids),
        ));
    }
    if !parent_pairs.is_empty() {
        let next_key = parent_pairs
            .iter()
            .map(|(k, _)| *k)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let tree = selis_pdf_cos::reconcile::merge_number_trees(&[(parent_pairs, 0)]);
        let num = merged.allocate();
        merged.add_object(num, tree);
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"ParentTree"),
            Obj::Ref(Ref::new(num, 0)),
        ));
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"ParentTreeNextKey"),
            Obj::Int(next_key),
        ));
    }
    if !role_map.is_empty() {
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"RoleMap"),
            Obj::Dict(role_map),
        ));
    }
    if !class_map.is_empty() {
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"ClassMap"),
            Obj::Dict(class_map),
        ));
    }
    let num = merged.allocate();
    merged.add_object(num, Obj::Dict(pairs));
    merged.add_catalog_entry(b"StructTreeRoot", Obj::Ref(Ref::new(num, 0)));
    Some(num)
}

/// Build the merged `/OCProperties` from the reconciled pieces: one OCG
/// list, default configuration with concatenated arrays.
fn attach_ocproperties(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    ocg_refs: Vec<Obj>,
    defaults: Vec<(selis_bytes::Bytes, Obj)>,
) {
    if ocg_refs.is_empty() {
        return;
    }
    let mut pairs: Vec<(selis_bytes::Bytes, Obj)> = vec![(
        selis_bytes::Bytes::copy_from_slice(b"OCGs"),
        Obj::Array(ocg_refs),
    )];
    if !defaults.is_empty() {
        pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"D"),
            Obj::Dict(defaults),
        ));
    }
    let num = merged.allocate();
    merged.add_object(num, Obj::Dict(pairs));
    merged.add_catalog_entry(b"OCProperties", Obj::Ref(Ref::new(num, 0)));
}

/// Carry one input's catalog-level structures into the output document
/// (SL-1A.WRITE.07 — conformance-friendly writer defaults): a structure
/// tree that was present on input is never dropped (ADR-P0031), output
/// intents pass through, `/Lang` and the marking/viewer preferences
/// survive, and outlines, page labels, form fields, and named destinations
/// are kept where possible and pruned where they only reference deleted
/// pages. The writer introduces nothing that degrades conformance: no
/// JavaScript, no `/Launch` actions, no external references — the inputs'
/// action-bearing `/Names` subtrees are never copied.
///
/// `all_pages` lists every source page; `kept` lists the surviving pages
/// in output order as (old page index, old page object number, merged page
/// reference). `carry_metadata` is false when the caller rewrote the
/// `/Info` dictionary (`set-metadata`): the input's XMP packet is then
/// stripped rather than left inconsistent with the new Info values (XMP
/// editing is a TOOL.09 refinement).
// The conformance carry/prune pass is wide because WRITE.07's contract is
// wide: the output builder, the source bytes, the parsed source, the page
// remap, the kept-page list, the flags, and the two budget handles are all
// needed at each step, and a context struct would hide which of them a given
// carry actually reads. The arity stays and the allow is scoped to this
// function (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconcile_single_input(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    all_pages: &[Ref],
    kept: &[KeptPage],
    carry_metadata: bool,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    let new_idx_by_old = output_index_by_source(kept);
    reconcile_single_input_mapped(
        out,
        src,
        doc,
        all_pages,
        kept,
        &new_idx_by_old,
        carry_metadata,
        budget,
        g,
    )
}

/// The output index of each source page's first occurrence.
///
/// Reorder and delete have one output per source page, so this is simply a
/// permutation/injection. Duplicate-page plans deliberately repeat a source
/// page; destinations and labels then point at the first copy, while the
/// later copies remain independently visible pages.
fn output_index_by_source(kept: &[KeptPage]) -> std::collections::HashMap<usize, usize> {
    let mut map = std::collections::HashMap::new();
    for (new_idx, (old_idx, _, _)) in kept.iter().enumerate() {
        map.entry(*old_idx).or_insert(new_idx);
    }
    map
}

/// Reconcile against an explicit source-to-output page map.
///
/// `new_idx_by_old` is separate from `kept` because duplicate and blank-page
/// plans change the output page count without changing the source page count.
/// It maps each source page to its first output occurrence; source pages that
/// were removed are absent.
#[allow(clippy::too_many_arguments)]
fn reconcile_single_input_mapped(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    all_pages: &[Ref],
    kept: &[KeptPage],
    new_idx_by_old: &std::collections::HashMap<usize, usize>,
    carry_metadata: bool,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    use selis_pdf_cos::reconcile as rec;

    let mut page_map: std::collections::HashMap<u32, Ref> = std::collections::HashMap::new();
    for (_, old, new) in kept {
        page_map.entry(*old).or_insert(*new);
    }
    let deleted: std::collections::HashSet<u32> = all_pages
        .iter()
        .map(|r| r.num)
        .filter(|num| !page_map.contains_key(num))
        .collect();
    let mut cache = std::collections::HashMap::new();

    // Simple carry-throughs (values that never reference pages).
    let mut carry_keys: Vec<&[u8]> = vec![
        b"Lang",
        b"MarkInfo",
        b"ViewerPreferences",
        b"PageMode",
        b"PageLayout",
        b"OutputIntents",
    ];
    if carry_metadata {
        carry_keys.push(b"Metadata");
    }
    for key in carry_keys {
        if let Some(v) = rec::catalog_entry(src, doc, key, budget, g).map_err(|e| e.to_string())? {
            let copied = rec::copy_value_into(out, src, doc, &page_map, &mut cache, &v, budget, g)
                .map_err(|e| e.to_string())?;
            out.add_catalog_entry(key, copied);
        }
    }

    // Optional-content groups (WRITE.07): `/OCProperties` never references
    // pages, so the whole dictionary carries through every page selection —
    // dropping it would silently lose the document's layers. (The
    // verification display asserts OCG counts on rewrite tools; this carry
    // is what makes that promise true.)
    if let Some(oc) =
        rec::catalog_entry(src, doc, b"OCProperties", budget, g).map_err(|e| e.to_string())?
    {
        let copied = rec::copy_value_into(out, src, doc, &page_map, &mut cache, &oc, budget, g)
            .map_err(|e| e.to_string())?;
        out.add_catalog_entry(b"OCProperties", copied);
    }

    // Page labels: re-keyed at the surviving pages' new indices.
    if let Some(labels) =
        rec::catalog_entry(src, doc, b"PageLabels", budget, g).map_err(|e| e.to_string())?
    {
        let labels = resolve_shallow(src, doc, labels, budget, g);
        if let Ok(pairs) = rec::number_tree_pairs(&labels, src, doc, budget, g) {
            let remapped = remap_page_label_keys(&pairs, new_idx_by_old, all_pages.len());
            if !remapped.is_empty() {
                let tree = rec::merge_number_trees(&[(remapped, 0)]);
                let num = out.allocate();
                out.add_object(num, tree);
                out.add_catalog_entry(b"PageLabels", Obj::Ref(Ref::new(num, 0)));
            }
        }
    }

    // Outlines: items destinating deleted pages are pruned.
    prune_outlines(out, src, doc, &page_map, &deleted, &mut cache, budget, g)?;

    // Form fields: roots whose widget page was deleted are dropped.
    carry_acroform_single(out, src, doc, &page_map, &deleted, &mut cache, budget, g)?;

    // Named destinations (pruned) and embedded files (kept); never actions.
    carry_names_single(out, src, doc, &page_map, &deleted, &mut cache, budget, g)?;

    // The structure tree (ADR-P0031): pruned at the deleted pages.
    carry_struct_tree_single(
        out, src, doc, kept, &page_map, &deleted, &mut cache, budget, g,
    )?;

    Ok(())
}

/// Re-key a single input's page-label pairs for a page selection: a label
/// starting at key `k` covers `[k, next key)`; it survives anchored at the
/// first kept page in that span, under that page's new index. Labels whose
/// span holds no kept page are dropped.
fn remap_page_label_keys(
    pairs: &[(i64, Obj)],
    new_idx_by_old: &std::collections::HashMap<usize, usize>,
    page_count: usize,
) -> Vec<(i64, Obj)> {
    let mut sorted: Vec<(i64, Obj)> = pairs.to_vec();
    sorted.sort_by_key(|(k, _)| *k);
    let page_max = i64::try_from(page_count)
        .unwrap_or(i64::MAX)
        .saturating_sub(1);
    let mut out = Vec::new();
    for (i, (key, value)) in sorted.iter().enumerate() {
        let start = (*key).max(0);
        let end = match sorted.get(i.saturating_add(1)) {
            Some((next, _)) => next.saturating_sub(1),
            None => i64::MAX,
        }
        .min(page_max);
        let mut old = start;
        while old <= end {
            let idx = usize::try_from(old).unwrap_or(usize::MAX);
            if let Some(&new_idx) = new_idx_by_old.get(&idx) {
                out.push((i64::try_from(new_idx).unwrap_or(i64::MAX), value.clone()));
                break;
            }
            old = old.saturating_add(1);
        }
    }
    out
}

/// Materialise one page-label dictionary per output page for an interleaved
/// plan.
///
/// A page-label range keyed at `k` covers every page until the next key, and
/// its `/St` number advances with the page's offset from `k`. Interleaving
/// destroys that contiguity: a range from one input would otherwise also
/// cover the other input's pages. Expanding every source page to its own
/// output entry — with `/St` advanced by `page - k` — preserves each page's
/// label exactly, and leaves no gap for a neighbouring input's style to leak
/// into.
pub(crate) fn materialize_page_labels(
    pairs: &[(i64, Obj)],
    output_indices: &[usize],
    page_count: usize,
) -> Vec<(i64, Obj)> {
    let mut sorted: Vec<(i64, Obj)> = pairs.to_vec();
    sorted.sort_by_key(|(key, _)| *key);
    let mut out = Vec::new();
    for src_idx in 0..page_count {
        let Some(&out_idx) = output_indices.get(src_idx) else {
            continue;
        };
        if out_idx == usize::MAX {
            continue;
        }
        let Ok(out_key) = i64::try_from(out_idx) else {
            continue;
        };
        let target = i64::try_from(src_idx).unwrap_or(i64::MAX);
        let label = match sorted.iter().rev().find(|(key, _)| *key <= target) {
            Some((key, Obj::Dict(dict))) => {
                let mut dict = dict.clone();
                advance_page_label(&mut dict, target.saturating_sub(*key));
                Obj::Dict(dict)
            }
            // No entry covers this page (or the covering value is malformed):
            // fall back to the default decimal label for the source position.
            _ => default_page_label(src_idx),
        };
        out.push((out_key, label));
    }
    out
}

/// Default decimal page labels for every page in a mixed input.
pub(crate) fn default_page_labels(output_indices: &[usize]) -> Vec<(i64, Obj)> {
    let mut out = Vec::new();
    for (src_idx, &out_idx) in output_indices.iter().enumerate() {
        if out_idx == usize::MAX {
            continue;
        }
        if let Ok(key) = i64::try_from(out_idx) {
            out.push((key, default_page_label(src_idx)));
        }
    }
    out
}

/// The label a page would carry without an explicit `/PageLabels` entry:
/// decimal numbering starting at 1 for the input's first page.
fn default_page_label(src_idx: usize) -> Obj {
    let start = i64::try_from(src_idx).unwrap_or(i64::MAX).saturating_add(1);
    Obj::Dict(vec![
        (
            selis_bytes::Bytes::copy_from_slice(b"S"),
            Obj::Name(selis_bytes::Bytes::copy_from_slice(b"D")),
        ),
        (selis_bytes::Bytes::copy_from_slice(b"St"), Obj::Int(start)),
    ])
}

/// Advance a page-label dictionary's `/St` by `offset` pages.
///
/// `/S /None` generates no number, so its start value is left untouched.
/// An entry without `/S` is decimal by default and is made explicit; an entry
/// without `/St` starts at 1. A malformed `/S` is replaced rather than
/// duplicated, so the materialised dictionary has one authoritative style.
fn advance_page_label(dict: &mut Vec<(selis_bytes::Bytes, Obj)>, offset: i64) {
    let style = dict
        .iter()
        .find(|(key, _)| key.as_slice() == b"S")
        .and_then(|(_, value)| match value {
            Obj::Name(name) => Some(name.as_slice().to_vec()),
            _ => None,
        });
    if style.as_deref() == Some(b"None") {
        return;
    }
    if style.is_none() {
        let decimal = Obj::Name(selis_bytes::Bytes::copy_from_slice(b"D"));
        if let Some((_, slot)) = dict.iter_mut().find(|(key, _)| key.as_slice() == b"S") {
            *slot = decimal;
        } else {
            dict.push((selis_bytes::Bytes::copy_from_slice(b"S"), decimal));
        }
    }
    let base = dict
        .iter()
        .find(|(key, _)| key.as_slice() == b"St")
        .and_then(|(_, value)| match value {
            Obj::Int(start) => Some(*start),
            _ => None,
        })
        .unwrap_or(1);
    let advanced = base.saturating_add(offset);
    if let Some((_, slot)) = dict.iter_mut().find(|(key, _)| key.as_slice() == b"St") {
        *slot = Obj::Int(advanced);
    } else {
        dict.push((
            selis_bytes::Bytes::copy_from_slice(b"St"),
            Obj::Int(advanced),
        ));
    }
}

/// Flatten a name tree without native recursion or trusting document-chosen
/// nesting.
///
/// A `/Kids` cycle is stopped by the visited-reference set, and both the node
/// and pair counts are capped. The values stay unresolved; destination
/// lookups resolve them through the same bounded chain as `/Dest` values.
fn flatten_name_tree(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    root: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Vec<(selis_bytes::Bytes, Obj)> {
    const MAX_TREE_NODES: usize = 10_000;
    const MAX_TREE_PAIRS: usize = 100_000;
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    let mut visited = std::collections::HashSet::new();
    let mut nodes = 0usize;
    while let Some(value) = stack.pop() {
        if nodes >= MAX_TREE_NODES || out.len() >= MAX_TREE_PAIRS {
            break;
        }
        nodes = nodes.saturating_add(1);
        let node = match value {
            Obj::Ref(r) => {
                if !visited.insert(r.num) {
                    continue;
                }
                match resolve_ref_obj(src, doc, r, budget, g) {
                    Ok(obj) => obj,
                    Err(_) => continue,
                }
            }
            other => other,
        };
        let Obj::Dict(pairs) = node else {
            continue;
        };
        for (key, value) in &pairs {
            if key.as_slice() == b"Names" {
                if let Obj::Array(items) = value {
                    let mut it = items.iter();
                    while let Some(name) = it.next() {
                        let Some(dest) = it.next() else { break };
                        if let Obj::String(name) = name {
                            // Match the old `/Names` reader: resolve one
                            // indirect hop so the outline pruner's named
                            // destination lookup keeps working. The bookmark
                            // resolver can follow any remaining chain itself.
                            let dest = match dest {
                                Obj::Ref(r) => resolve_ref_obj(src, doc, *r, budget, g)
                                    .unwrap_or_else(|_| dest.clone()),
                                other => other.clone(),
                            };
                            out.push((name.clone(), dest));
                            if out.len() >= MAX_TREE_PAIRS {
                                break;
                            }
                        }
                    }
                }
            } else if key.as_slice() == b"Kids" {
                if let Obj::Array(items) = value {
                    for kid in items.iter().rev() {
                        stack.push(kid.clone());
                    }
                }
            }
        }
    }
    out
}

/// The document's named-destination tables flattened for destination
/// lookups: the `/Names/Dests` name tree plus the legacy direct `/Dests`
/// dictionary.
struct NamedDests {
    pairs: Vec<(selis_bytes::Bytes, Obj)>,
}

impl NamedDests {
    /// Flatten both destination tables (best-effort: a damaged tree simply
    /// contributes nothing and destinations are conservatively kept).
    fn collect(
        src: &[u8],
        doc: &selis_pdf_cos::Doc,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Self {
        use selis_pdf_cos::reconcile as rec;
        let mut pairs = Vec::new();
        if let Ok(Some(names)) = rec::catalog_entry(src, doc, b"Names", budget, g) {
            let names = resolve_shallow(src, doc, names, budget, g);
            if let Obj::Dict(np) = &names {
                if let Some((_, dests)) = np.iter().find(|(k, _)| k.as_slice() == b"Dests") {
                    let dests = resolve_shallow(src, doc, dests.clone(), budget, g);
                    pairs.extend(flatten_name_tree(src, doc, &dests, budget, g));
                }
            }
        }
        if let Ok(Some(dests)) = rec::catalog_entry(src, doc, b"Dests", budget, g) {
            let dests = resolve_shallow(src, doc, dests, budget, g);
            if let Obj::Dict(dp) = &dests {
                for (k, v) in dp {
                    pairs.push((k.clone(), v.clone()));
                }
            }
        }
        NamedDests { pairs }
    }

    /// The destination value stored under `name`, when present.
    fn value_of(&self, name: &[u8]) -> Option<&Obj> {
        self.pairs
            .iter()
            .find(|(k, _)| k.as_slice() == name)
            .map(|(_, v)| v)
    }

    /// The page object number a named destination targets, when decidable.
    fn page_of(&self, name: &[u8]) -> Option<u32> {
        let value = self.value_of(name)?;
        match value {
            Obj::Array(items) => match items.first() {
                Some(Obj::Ref(r)) => Some(r.num),
                _ => None,
            },
            Obj::Dict(pairs) => {
                pairs
                    .iter()
                    .find(|(k, _)| k.as_slice() == b"D")
                    .and_then(|(_, v)| match v {
                        Obj::Array(items) => match items.first() {
                            Some(Obj::Ref(r)) => Some(r.num),
                            _ => None,
                        },
                        _ => None,
                    })
            }
            _ => None,
        }
    }
}

/// The page object number a destination value targets (an explicit
/// `[page …]` array, or a named destination), when decidable.
fn dest_page_num(dest: &Obj, named: &NamedDests) -> Option<u32> {
    match dest {
        Obj::Array(items) => match items.first() {
            Some(Obj::Ref(r)) => Some(r.num),
            _ => None,
        },
        Obj::String(s) => named.page_of(s.as_slice()),
        Obj::Name(n) => named.page_of(n.as_slice()),
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"D")
            .and_then(|(_, v)| dest_page_num(v, named)),
        _ => None,
    }
}

/// Follow an indirect object chain with a fixed cap.
fn resolve_indirect(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    mut value: Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj, String> {
    const MAX_INDIRECT: usize = 32;
    for _ in 0..MAX_INDIRECT {
        match value {
            Obj::Ref(r) => value = resolve_ref_obj(src, doc, r, budget, g)?,
            other => return Ok(other),
        }
    }
    Err("destination reference chain is too deep".to_string())
}

/// Resolve a destination value (explicit array, destination dictionary, or
/// named destination) to a zero-based page index.
fn destination_page_index(
    value: Obj,
    named: &NamedDests,
    page_index: &std::collections::HashMap<u32, usize>,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<usize>, String> {
    const MAX_DESTINATION_CHAIN: usize = 32;
    let mut current = value;
    let mut seen_names: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    for _ in 0..MAX_DESTINATION_CHAIN {
        match current {
            Obj::Ref(r) => current = resolve_ref_obj(src, doc, r, budget, g)?,
            Obj::Array(items) => {
                let Some(Obj::Ref(page_ref)) = items.first() else {
                    return Err("bookmark destination does not name a page".to_string());
                };
                return page_index
                    .get(&page_ref.num)
                    .copied()
                    .map(Some)
                    .ok_or_else(|| {
                        format!(
                        "bookmark destination names page object {}, which is not in the page tree",
                        page_ref.num
                    )
                    });
            }
            Obj::Dict(pairs) => {
                let Some((_, dest)) = pairs.iter().find(|(k, _)| k.as_slice() == b"D") else {
                    return Err("bookmark destination dictionary has no /D".to_string());
                };
                current = dest.clone();
            }
            Obj::String(name) | Obj::Name(name) => {
                if !seen_names.insert(name.as_slice().to_vec()) {
                    return Err("bookmark named destinations contain a cycle".to_string());
                }
                let Some(next) = named.value_of(name.as_slice()) else {
                    return Err(format!(
                        "bookmark names the missing destination `{}`",
                        String::from_utf8_lossy(name.as_slice())
                    ));
                };
                current = next.clone();
            }
            _ => return Err("bookmark destination is not a local page".to_string()),
        }
    }
    Err("bookmark destination chain is too deep".to_string())
}

/// The page a top-level bookmark item points at, following `/A /S /GoTo`
/// first and `/Dest` only when no action is present (the PDF precedence rule).
fn outline_item_page_index(
    item: &Obj,
    named: &NamedDests,
    page_index: &std::collections::HashMap<u32, usize>,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<usize>, String> {
    let Obj::Dict(pairs) = item else {
        return Err("bookmark outline item is not a dictionary".to_string());
    };
    if let Some((_, action)) = pairs.iter().find(|(k, _)| k.as_slice() == b"A") {
        let action = resolve_indirect(src, doc, action.clone(), budget, g)?;
        let Obj::Dict(action_pairs) = action else {
            // An action class this reader does not model is not a local
            // boundary, and /A wins over /Dest even when it is unusable.
            return Ok(None);
        };
        let goto = action_pairs.iter().find(|(k, _)| k.as_slice() == b"S");
        let Some((_, Obj::Name(kind))) = goto else {
            return Ok(None);
        };
        if kind.as_slice() != b"GoTo" {
            return Ok(None);
        }
        let Some((_, dest)) = action_pairs.iter().find(|(k, _)| k.as_slice() == b"D") else {
            return Err("bookmark GoTo action has no destination".to_string());
        };
        return destination_page_index(dest.clone(), named, page_index, src, doc, budget, g);
    }
    if let Some((_, dest)) = pairs.iter().find(|(k, _)| k.as_slice() == b"Dest") {
        return destination_page_index(dest.clone(), named, page_index, src, doc, budget, g);
    }
    Ok(None)
}

/// Collect the page starts named by the document's top-level bookmarks.
///
/// Only the first outline level is read. External actions are skipped (they
/// are not boundaries in this document); a local destination that cannot be
/// resolved is a truncated outline and refuses the split rather than silently
/// dropping a section.
fn bookmark_page_starts(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_refs: &[Ref],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<usize>, String> {
    use selis_pdf_cos::reconcile as rec;
    const MAX_TOP_LEVEL_ITEMS: usize = 10_000;
    let Some(outlines) =
        rec::catalog_entry(src, doc, b"Outlines", budget, g).map_err(|e| e.to_string())?
    else {
        return Err("this PDF has no bookmarks to split by".to_string());
    };
    let root = resolve_indirect(src, doc, outlines, budget, g)?;
    let Obj::Dict(root_pairs) = root else {
        return Err("the bookmark outline is unreadable".to_string());
    };
    let Some((_, first)) = root_pairs.iter().find(|(k, _)| k.as_slice() == b"First") else {
        return Err("this PDF has no top-level bookmarks to split by".to_string());
    };
    let named = NamedDests::collect(src, doc, budget, g);
    let mut page_index = std::collections::HashMap::new();
    for (index, page_ref) in page_refs.iter().enumerate() {
        page_index.entry(page_ref.num).or_insert(index);
    }

    let mut starts = std::collections::BTreeSet::new();
    let mut visited = std::collections::HashSet::new();
    let mut pending = Some(first.clone());
    let mut item_count = 0usize;
    while let Some(value) = pending.take() {
        item_count = item_count.saturating_add(1);
        if item_count > MAX_TOP_LEVEL_ITEMS {
            return Err("the bookmark outline has too many top-level items".to_string());
        }
        let item = match value {
            Obj::Ref(r) => {
                if !visited.insert(r.num) {
                    return Err("the bookmark outline contains a cycle".to_string());
                }
                resolve_ref_obj(src, doc, r, budget, g)?
            }
            Obj::Dict(_) => value,
            _ => return Err("the bookmark outline is unreadable".to_string()),
        };
        if let Some(page) =
            outline_item_page_index(&item, &named, &page_index, src, doc, budget, g)?
        {
            starts.insert(page);
        }
        let next = match &item {
            Obj::Dict(pairs) => pairs
                .iter()
                .find(|(k, _)| k.as_slice() == b"Next")
                .map(|(_, v)| v.clone()),
            _ => None,
        };
        match next {
            None | Some(Obj::Null) => break,
            Some(next @ (Obj::Ref(_) | Obj::Dict(_))) => pending = Some(next),
            Some(_) => return Err("the bookmark outline has an invalid next link".to_string()),
        }
    }
    if starts.is_empty() {
        return Err("the top-level bookmarks do not point to local pages".to_string());
    }
    Ok(starts.into_iter().collect())
}

/// Turn sorted bookmark page starts into contiguous output ranges. Pages
/// before the first bookmark form a front-matter part when there are any.
fn bookmark_ranges(starts: &[usize], page_count: usize) -> Result<Vec<(usize, usize)>, String> {
    if starts.is_empty() {
        return Err("this PDF has no local bookmark targets".to_string());
    }
    if let Some(&last) = starts.last() {
        if last >= page_count {
            return Err("a bookmark targets a page outside the document".to_string());
        }
    }
    let mut ranges = Vec::new();
    if let Some(&first) = starts.first() {
        if first > 0 {
            ranges.push((0, first.saturating_sub(1)));
        }
    }
    for (index, &start) in starts.iter().enumerate() {
        let end = starts
            .get(index.saturating_add(1))
            .map_or(page_count.saturating_sub(1), |next| next.saturating_sub(1));
        if start <= end {
            ranges.push((start, end));
        }
    }
    if ranges.len() <= 1 {
        return Err("the bookmarks do not divide this PDF into two or more parts".to_string());
    }
    Ok(ranges)
}

/// Split `path` at top-level bookmark boundaries.
pub(crate) fn split_by_bookmarks(path: &str, output_prefix: &str) -> CliResult<Vec<SplitPart>> {
    let (src, doc, page_refs, budget) = open_for_split(path)?;
    let count = page_refs.len();
    if count == 0 {
        return Err(CliError(format!("{path}: document has no pages")));
    }
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let starts = bookmark_page_starts(&src, &doc, &page_refs, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let ranges = bookmark_ranges(&starts, count).map_err(|e| CliError(format!("{path}: {e}")))?;
    if ranges.len() > MAX_SPLIT_OUTPUTS {
        return Err(CliError(format!(
            "bookmark split would produce {} parts; the limit is {MAX_SPLIT_OUTPUTS}",
            ranges.len()
        )));
    }
    write_ranges(
        path,
        &src,
        &doc,
        &page_refs,
        &ranges,
        output_prefix,
        in_bytes,
        &budget,
        &mut g,
    )
}

/// Carry the input's outlines into `out`, dropping items whose destination
/// page was deleted (WRITE.07). Chain links (`First`/`Last`/`Next`/`Prev`/
/// `Parent`/`Count`) are rebuilt over the kept items; every other entry of
/// a kept item is copied through `copy_value_into`, so destinations remap
/// at the surviving pages.
// Same WRITE.07 carry/prune arity as `reconcile_single_input`; see the
// justification there (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
fn prune_outlines(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_map: &std::collections::HashMap<u32, Ref>,
    deleted: &std::collections::HashSet<u32>,
    cache: &mut std::collections::HashMap<(u32, u16), u32>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    use selis_pdf_cos::reconcile as rec;
    let Some(root) =
        rec::catalog_entry(src, doc, b"Outlines", budget, g).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let root_ref = match root {
        Obj::Ref(r) => r,
        _ => return Ok(()),
    };
    let named = NamedDests::collect(src, doc, budget, g);
    let root_num = out.allocate();
    out.add_object(root_num, Obj::Dict(Vec::new()));
    let items = prune_outline_chain(
        out, src, doc, page_map, deleted, cache, &named, root_ref, root_num, budget, g,
    )?;
    if items.is_empty() {
        // Nothing survived: drop the empty root again.
        out.objects_mut().retain(|(num, _)| *num != root_num);
        return Ok(());
    }
    link_outline_chain(out, root_num, &items, true);
    out.add_catalog_entry(b"Outlines", Obj::Ref(Ref::new(root_num, 0)));
    Ok(())
}

/// Walk one chain of outline items (the `/First`/`/Next` sequence under
/// `parent_ref`), keeping items whose destination survives, and return the
/// kept items' new object numbers in order. `parent_num` is the object the
/// caller will link the returned chain under.
// Same WRITE.07 carry/prune arity as `reconcile_single_input`; see the
// justification there (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
fn prune_outline_chain(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_map: &std::collections::HashMap<u32, Ref>,
    deleted: &std::collections::HashSet<u32>,
    cache: &mut std::collections::HashMap<(u32, u16), u32>,
    named: &NamedDests,
    parent_ref: Ref,
    _parent_num: u32,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u32>, String> {
    use selis_pdf_cos::reconcile as rec;
    let parent = resolve_ref_obj(src, doc, parent_ref, budget, g)?;
    let Obj::Dict(pairs) = &parent else {
        return Ok(Vec::new());
    };
    let Some(Obj::Ref(first)) = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"First")
        .map(|(_, v)| v)
    else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    let mut cur = *first;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..10_000 {
        if !seen.insert((cur.num, cur.gen)) {
            break;
        }
        let obj = resolve_ref_obj(src, doc, cur, budget, g)?;
        let next = match &obj {
            Obj::Dict(p) => p
                .iter()
                .find(|(k, _)| k.as_slice() == b"Next")
                .and_then(|(_, v)| match v {
                    Obj::Ref(r) => Some(*r),
                    _ => None,
                }),
            _ => None,
        };
        if outline_item_kept(&obj, deleted, named) {
            let original_count = match &obj {
                Obj::Dict(p) => p
                    .iter()
                    .find(|(k, _)| k.as_slice() == b"Count")
                    .and_then(|(_, v)| match v {
                        Obj::Int(n) => Some(*n),
                        _ => None,
                    })
                    .unwrap_or(1),
                _ => 1,
            };
            // Build the kept item without its chain links (rebuilt below).
            let num = out.allocate();
            cache.insert((cur.num, cur.gen), num);
            let mut new_pairs: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
            if let Obj::Dict(p) = &obj {
                for (k, v) in p {
                    if matches!(
                        k.as_slice(),
                        b"First" | b"Last" | b"Next" | b"Prev" | b"Parent" | b"Count"
                    ) {
                        continue;
                    }
                    let copied = rec::copy_value_into(out, src, doc, page_map, cache, v, budget, g)
                        .map_err(|e| e.to_string())?;
                    new_pairs.push((k.clone(), copied));
                }
            }
            out.add_object(num, Obj::Dict(new_pairs));
            let children = prune_outline_chain(
                out, src, doc, page_map, deleted, cache, named, cur, num, budget, g,
            )?;
            if !children.is_empty() {
                link_outline_chain(out, num, &children, original_count >= 0);
            }
            items.push(num);
        }
        let Some(next) = next else { break };
        cur = next;
    }
    Ok(items)
}

/// True when an outline item's destination survives the page selection.
/// Items without a decidable destination are kept (never drop what cannot
/// be classified).
fn outline_item_kept(
    obj: &Obj,
    deleted: &std::collections::HashSet<u32>,
    named: &NamedDests,
) -> bool {
    let Obj::Dict(pairs) = obj else {
        return true;
    };
    let dest = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Dest")
        .map(|(_, v)| v.clone())
        .or_else(|| {
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == b"A")
                .and_then(|(_, v)| match v {
                    Obj::Dict(a) => {
                        let goto = a.iter().any(|(k, v)| {
                            let is_action_type = k.as_slice() == b"S";
                            is_action_type && matches!(v, Obj::Name(n) if n.as_slice() == b"GoTo")
                        });
                        if !goto {
                            return None;
                        }
                        a.iter()
                            .find(|(k, _)| k.as_slice() == b"D")
                            .map(|(_, v)| v.clone())
                    }
                    _ => None,
                })
        });
    let Some(dest) = dest else {
        return true;
    };
    match dest_page_num(&dest, named) {
        Some(num) => !deleted.contains(&num),
        None => true,
    }
}

/// Link a kept outline chain under `parent_num`: `/Parent` for each item,
/// `/Next`/`/Prev` between items, and `/First`/`/Last`/`/Count` on the
/// parent. `positive` keeps the original open/closed state on `/Count`.
fn link_outline_chain(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    parent_num: u32,
    items: &[u32],
    positive: bool,
) {
    let Some((&first, _)) = items.split_first() else {
        return;
    };
    let last = *items.last().unwrap_or(&first);
    let mut prev: Option<u32> = None;
    for &item in items {
        edit_dict_entry(out, item, b"Parent", Obj::Ref(Ref::new(parent_num, 0)));
        if let Some(p) = prev {
            edit_dict_entry(out, item, b"Prev", Obj::Ref(Ref::new(p, 0)));
        }
        prev = Some(item);
    }
    for i in 0..items.len().saturating_sub(1) {
        let cur = items.get(i).copied().unwrap_or(0);
        let nxt = items.get(i.saturating_add(1)).copied().unwrap_or(cur);
        edit_dict_entry(out, cur, b"Next", Obj::Ref(Ref::new(nxt, 0)));
    }
    edit_dict_entry(out, parent_num, b"First", Obj::Ref(Ref::new(first, 0)));
    edit_dict_entry(out, parent_num, b"Last", Obj::Ref(Ref::new(last, 0)));
    let count = i64::try_from(items.len()).unwrap_or(i64::MAX);
    edit_dict_entry(
        out,
        parent_num,
        b"Count",
        Obj::Int(if positive {
            count
        } else {
            count.saturating_neg()
        }),
    );
}

/// Carry the input's `/AcroForm` into `out`, dropping field roots whose
/// widget page was deleted (WRITE.07). Default appearance, default
/// resources, need-appearances, and calculation order pass through;
/// `/XFA` is never carried (deprecated in PDF 2.0).
// Same WRITE.07 carry/prune arity as `reconcile_single_input`; see the
// justification there (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
fn carry_acroform_single(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_map: &std::collections::HashMap<u32, Ref>,
    deleted: &std::collections::HashSet<u32>,
    cache: &mut std::collections::HashMap<(u32, u16), u32>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    use selis_pdf_cos::reconcile as rec;
    let Some(form) =
        rec::catalog_entry(src, doc, b"AcroForm", budget, g).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let form = resolve_shallow(src, doc, form, budget, g);
    let Obj::Dict(pairs) = &form else {
        return Ok(());
    };
    let mut field_refs: Vec<Ref> = Vec::new();
    let mut default_appearance: Option<Obj> = None;
    let mut need_appearances = false;
    let mut form_resources: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    let mut calc_order: Vec<Obj> = Vec::new();
    for (k, v) in pairs {
        match k.as_slice() {
            b"Fields" => {
                if let Obj::Array(items) = v {
                    for f in items {
                        if !field_survives(src, doc, f, deleted, 0, budget, g) {
                            continue;
                        }
                        let copied =
                            rec::copy_value_into(out, src, doc, page_map, cache, f, budget, g)
                                .map_err(|e| e.to_string())?;
                        if let Obj::Ref(r) = copied {
                            field_refs.push(r);
                        }
                    }
                }
            }
            b"DA" => {
                default_appearance = Some(
                    rec::copy_value_into(out, src, doc, page_map, cache, v, budget, g)
                        .map_err(|e| e.to_string())?,
                );
            }
            b"NeedAppearances" => {
                if matches!(v, Obj::Bool(true)) {
                    need_appearances = true;
                }
            }
            b"CO" => {
                let copied = rec::copy_value_into(out, src, doc, page_map, cache, v, budget, g)
                    .map_err(|e| e.to_string())?;
                if let Obj::Array(items) = copied {
                    calc_order.extend(items);
                }
            }
            b"DR" => {
                let copied = rec::copy_value_into(out, src, doc, page_map, cache, v, budget, g)
                    .map_err(|e| e.to_string())?;
                if let Obj::Dict(d) = copied {
                    union_dicts(&mut form_resources, d);
                }
            }
            b"XFA" => {} // deprecated in PDF 2.0; never introduced by the writer
            _ => {}
        }
    }
    if field_refs.is_empty() {
        return Ok(());
    }
    attach_acroform(
        out,
        &field_refs,
        default_appearance,
        need_appearances,
        form_resources,
        calc_order,
    );
    Ok(())
}

/// True when a form field still has a home after the page selection: its
/// `/P` page survived, or (for pure container fields) at least one
/// descendant does. Unresolvable fields are kept — the writer never drops
/// what it cannot classify.
fn field_survives(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    obj: &Obj,
    deleted: &std::collections::HashSet<u32>,
    depth: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> bool {
    if depth > 32 {
        return true;
    }
    let resolved = resolve_shallow(src, doc, obj.clone(), budget, g);
    let Obj::Dict(pairs) = &resolved else {
        return true;
    };
    if let Some((_, Obj::Ref(p))) = pairs.iter().find(|(k, _)| k.as_slice() == b"P") {
        return !deleted.contains(&p.num);
    }
    if let Some((_, Obj::Array(kids))) = pairs.iter().find(|(k, _)| k.as_slice() == b"Kids") {
        return kids
            .iter()
            .any(|kid| field_survives(src, doc, kid, deleted, depth.saturating_add(1), budget, g));
    }
    true
}

/// Carry the input's `/Names` tree into `out` (WRITE.07): named
/// destinations whose target page was deleted are pruned, embedded files
/// pass through, and everything else (`/JavaScript`, `/Launch`-bearing
/// subtrees, …) is never copied — the writer introduces no actions.
// Same WRITE.07 carry/prune arity as `reconcile_single_input`; see the
// justification there (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
fn carry_names_single(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_map: &std::collections::HashMap<u32, Ref>,
    deleted: &std::collections::HashSet<u32>,
    cache: &mut std::collections::HashMap<(u32, u16), u32>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    use selis_pdf_cos::reconcile as rec;
    let Some(names) =
        rec::catalog_entry(src, doc, b"Names", budget, g).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let names = resolve_shallow(src, doc, names, budget, g);
    let Obj::Dict(pairs) = &names else {
        return Ok(());
    };
    let named = NamedDests::collect(src, doc, budget, g);
    let mut dest_pairs: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    let mut file_pairs: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    for (k, v) in pairs {
        match k.as_slice() {
            b"Dests" => {
                let v = resolve_shallow(src, doc, v.clone(), budget, g);
                let Ok(found) = rec::name_tree_pairs(&v, src, doc, budget, g) else {
                    continue;
                };
                for (name, value) in found {
                    if let Some(num) = dest_page_num(&value, &named) {
                        if deleted.contains(&num) {
                            continue; // the destination's page was deleted
                        }
                    }
                    let copied =
                        rec::copy_value_into(out, src, doc, page_map, cache, &value, budget, g)
                            .map_err(|e| e.to_string())?;
                    dest_pairs.push((name, copied));
                }
            }
            b"EmbeddedFiles" => {
                let v = resolve_shallow(src, doc, v.clone(), budget, g);
                let Ok(found) = rec::name_tree_pairs(&v, src, doc, budget, g) else {
                    continue;
                };
                for (name, value) in found {
                    let copied =
                        rec::copy_value_into(out, src, doc, page_map, cache, &value, budget, g)
                            .map_err(|e| e.to_string())?;
                    file_pairs.push((name, copied));
                }
            }
            _ => {} // JavaScript, AP, …: never carried into tool output
        }
    }
    attach_names(out, dest_pairs, file_pairs);
    Ok(())
}

/// Carry the input's `/StructTreeRoot` into `out` (WRITE.07, ADR-P0031 —
/// a rewritten document stays tagged): elements whose page was deleted are
/// pruned (their marked content no longer exists), kept pages retain their
/// `/StructParents` ids, and the `/ParentTree` keeps exactly those ids'
/// entries with their values redirected at the pruned elements via the
/// shared copy cache.
// Same WRITE.07 carry/prune arity as `reconcile_single_input`; see the
// justification there (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
fn carry_struct_tree_single(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    kept: &[KeptPage],
    page_map: &std::collections::HashMap<u32, Ref>,
    deleted: &std::collections::HashSet<u32>,
    cache: &mut std::collections::HashMap<(u32, u16), u32>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    use selis_pdf_cos::reconcile as rec;
    let Some(raw) =
        rec::catalog_entry(src, doc, b"StructTreeRoot", budget, g).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let raw_ref = match &raw {
        Obj::Ref(r) => Some(*r),
        _ => None,
    };
    let root = resolve_shallow(src, doc, raw, budget, g);
    let Obj::Dict(pairs) = &root else {
        return Ok(());
    };

    // Reserve the merged root's number and pre-register the source root in
    // the cache: copied elements' `/P` entries repoint at the merged root
    // and the source root itself is never copied.
    let root_num = out.allocate();
    if let Some(r) = raw_ref {
        cache.insert((r.num, r.gen), root_num);
    }

    // Kept pages retain their struct-parents ids (single input: no shift).
    // A duplicated source page appears more than once in `kept`; only its first
    // output copy owns the original tagged structure, because the structure
    // tree still points at that copy. Reusing the id on a later duplicate would
    // name a parent-tree entry anchored at another page.
    let mut kept_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut tagged_pages: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (_, old_num, new_ref) in kept {
        if !tagged_pages.insert(*old_num) {
            continue;
        }
        let page = resolve_shallow(src, doc, Obj::Ref(Ref::new(*old_num, 0)), budget, g);
        let Obj::Dict(pp) = &page else { continue };
        let Some(sp) = pp
            .iter()
            .find(|(k, _)| k.as_slice() == b"StructParents")
            .and_then(|(_, v)| match v {
                Obj::Int(n) => Some(*n),
                _ => None,
            })
        else {
            continue;
        };
        kept_ids.insert(sp);
        edit_dict_entry(out, new_ref.num, b"StructParents", Obj::Int(sp));
    }

    let mut dropped: std::collections::HashSet<(u32, u16)> = std::collections::HashSet::new();

    // `/K` first so kept elements register in the cache before the
    // `/ParentTree` values are copied (they must redirect at the same
    // objects, not resurrect pruned ones). The source shape (array vs a
    // single element) is recorded and preserved on write: wrapping a lone
    // element in a fresh array would nest the rewritten document one level
    // deeper than the input, which trips the depth budget on deeply nested
    // type-4/function documents.
    let mut struct_kids: Vec<Obj> = Vec::new();
    let mut k_was_array = true;
    if let Some((_, k)) = pairs.iter().find(|(k, _)| k.as_slice() == b"K") {
        let (kids, was_array): (Vec<Obj>, bool) = match k {
            Obj::Array(items) => (items.clone(), true),
            other => (vec![other.clone()], false),
        };
        k_was_array = was_array;
        for kid in &kids {
            if let Some(copied) = copy_struct_value(
                out,
                src,
                doc,
                page_map,
                deleted,
                cache,
                &mut dropped,
                kid,
                budget,
                g,
            )? {
                struct_kids.push(copied);
            }
        }
    }

    // `/ParentTree`: keep exactly the surviving pages' ids.
    let mut parent_pairs: Vec<(i64, Obj)> = Vec::new();
    if let Some((_, pt)) = pairs.iter().find(|(k, _)| k.as_slice() == b"ParentTree") {
        if let Ok(found) = rec::number_tree_pairs_unresolved(pt, src, doc, budget, g) {
            for (key, value) in found {
                if !kept_ids.contains(&key) {
                    continue;
                }
                if let Some(copied) = copy_struct_value(
                    out,
                    src,
                    doc,
                    page_map,
                    deleted,
                    cache,
                    &mut dropped,
                    &value,
                    budget,
                    g,
                )? {
                    parent_pairs.push((key, copied));
                }
            }
        }
    }

    let mut role_map: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    if let Some((_, rm)) = pairs.iter().find(|(k, _)| k.as_slice() == b"RoleMap") {
        let copied = rec::copy_value_into(out, src, doc, page_map, cache, rm, budget, g)
            .map_err(|e| e.to_string())?;
        if let Obj::Dict(d) = copied {
            union_dicts(&mut role_map, d);
        }
    }
    let mut class_map: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
    if let Some((_, cm)) = pairs.iter().find(|(k, _)| k.as_slice() == b"ClassMap") {
        let copied = rec::copy_value_into(out, src, doc, page_map, cache, cm, budget, g)
            .map_err(|e| e.to_string())?;
        if let Obj::Dict(d) = copied {
            union_dicts(&mut class_map, d);
        }
    }

    // The input had a structure tree: the output keeps one, even when the
    // page selection pruned every element (WRITE.07 — never drop a
    // structure tree that was present on input).
    let mut root_pairs: Vec<(selis_bytes::Bytes, Obj)> = vec![(
        selis_bytes::Bytes::copy_from_slice(b"Type"),
        Obj::Name(selis_bytes::Bytes::copy_from_slice(b"StructTreeRoot")),
    )];
    if !struct_kids.is_empty() {
        let k_value = if k_was_array || struct_kids.len() != 1 {
            Obj::Array(struct_kids)
        } else {
            // Preserve the source's single-element shape (see above).
            struct_kids
                .into_iter()
                .next()
                .unwrap_or_else(|| Obj::Array(Vec::new()))
        };
        root_pairs.push((selis_bytes::Bytes::copy_from_slice(b"K"), k_value));
    }
    if !parent_pairs.is_empty() {
        let next_key = parent_pairs
            .iter()
            .map(|(k, _)| *k)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let tree = selis_pdf_cos::reconcile::merge_number_trees(&[(parent_pairs, 0)]);
        let num = out.allocate();
        out.add_object(num, tree);
        root_pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"ParentTree"),
            Obj::Ref(Ref::new(num, 0)),
        ));
        root_pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"ParentTreeNextKey"),
            Obj::Int(next_key),
        ));
    }
    if !role_map.is_empty() {
        root_pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"RoleMap"),
            Obj::Dict(role_map),
        ));
    }
    if !class_map.is_empty() {
        root_pairs.push((
            selis_bytes::Bytes::copy_from_slice(b"ClassMap"),
            Obj::Dict(class_map),
        ));
    }
    out.add_object(root_num, Obj::Dict(root_pairs));
    out.add_catalog_entry(b"StructTreeRoot", Obj::Ref(Ref::new(root_num, 0)));
    Ok(())
}

/// Copy a structure-tree value into `out`, pruning everything that belongs
/// to a deleted page (WRITE.07): an element whose `/Pg` page was deleted
/// is dropped (its marked content is gone), an element whose `/K` kids are
/// all pruned drops with them, and pairs referencing deleted pages are
/// omitted. `dropped` remembers pruned source objects so the
/// `/ParentTree` pass cannot resurrect them.
// Same WRITE.07 carry/prune arity as `reconcile_single_input`; see the
// justification there (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
fn copy_struct_value(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_map: &std::collections::HashMap<u32, Ref>,
    deleted: &std::collections::HashSet<u32>,
    cache: &mut std::collections::HashMap<(u32, u16), u32>,
    dropped: &mut std::collections::HashSet<(u32, u16)>,
    obj: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Obj>, String> {
    match obj {
        Obj::Ref(r) => {
            if deleted.contains(&r.num) || dropped.contains(&(r.num, r.gen)) {
                return Ok(None);
            }
            if let Some(&merged) = page_map.get(&r.num) {
                return Ok(Some(Obj::Ref(merged)));
            }
            if let Some(&num) = cache.get(&(r.num, r.gen)) {
                return Ok(Some(Obj::Ref(Ref::new(num, 0))));
            }
            let resolved = resolve_ref_obj(src, doc, *r, budget, g)?;
            let num = out.allocate();
            cache.insert((r.num, r.gen), num);
            let copied = copy_struct_value(
                out, src, doc, page_map, deleted, cache, dropped, &resolved, budget, g,
            )?;
            match copied {
                Some(c) => {
                    out.add_object(num, c);
                    Ok(Some(Obj::Ref(Ref::new(num, 0))))
                }
                None => {
                    cache.remove(&(r.num, r.gen));
                    dropped.insert((r.num, r.gen));
                    Ok(None)
                }
            }
        }
        Obj::Array(items) => {
            let mut kept = Vec::new();
            for item in items {
                if let Some(c) = copy_struct_value(
                    out, src, doc, page_map, deleted, cache, dropped, item, budget, g,
                )? {
                    kept.push(c);
                }
            }
            Ok(Some(Obj::Array(kept)))
        }
        Obj::Dict(pairs) => {
            // An element anchored at a deleted page vanishes with the page.
            if let Some((_, Obj::Ref(pg))) = pairs.iter().find(|(k, _)| k.as_slice() == b"Pg") {
                if deleted.contains(&pg.num) {
                    return Ok(None);
                }
            }
            let mut new_pairs: Vec<(selis_bytes::Bytes, Obj)> = Vec::new();
            let mut has_kids_key = false;
            let mut kids_kept = 0usize;
            for (k, v) in pairs {
                let Ok(copied) = copy_struct_value(
                    out, src, doc, page_map, deleted, cache, dropped, v, budget, g,
                ) else {
                    continue;
                };
                let Some(copied) = copied else {
                    continue;
                };
                if k.as_slice() == b"K" {
                    has_kids_key = true;
                    match &copied {
                        Obj::Array(items) => {
                            kids_kept = kids_kept.saturating_add(items.len());
                        }
                        _ => kids_kept = kids_kept.saturating_add(1),
                    }
                }
                new_pairs.push((k.clone(), copied));
            }
            if has_kids_key && kids_kept == 0 {
                // Every kid was pruned: the element is empty.
                return Ok(None);
            }
            Ok(Some(Obj::Dict(new_pairs)))
        }
        other => Ok(Some(other.clone())),
    }
}

/// Rename duplicate names with a `-2`, `-3`, … suffix so every entry keeps a
/// unique key in the merged name tree.
fn unique_names(pairs: Vec<(selis_bytes::Bytes, Obj)>) -> Vec<(selis_bytes::Bytes, Obj)> {
    let mut seen: std::collections::HashMap<Vec<u8>, u32> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        let count = seen.entry(name.as_slice().to_vec()).or_insert(0);
        *count = count.saturating_add(1);
        let unique = if *count == 1 {
            name
        } else {
            selis_bytes::Bytes::copy_from_slice(
                format!("{}-{}", String::from_utf8_lossy(name.as_slice()), count).as_bytes(),
            )
        };
        out.push((unique, value));
    }
    out
}

/// Set or replace a key in the dictionary object numbered `num`.
fn edit_dict_entry(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    num: u32,
    key: &[u8],
    value: Obj,
) {
    if let Some((_, Obj::Dict(pairs))) = merged.objects_mut().iter_mut().find(|(n, _)| *n == num) {
        let key_b = selis_bytes::Bytes::copy_from_slice(key);
        if let Some(slot) = pairs.iter_mut().find(|(k, _)| k.as_slice() == key) {
            slot.1 = value;
        } else {
            pairs.push((key_b, value));
        }
    }
}

/// True when the dictionary object carries `key`.
fn dict_has(obj: &Obj, key: &[u8]) -> bool {
    matches!(obj, Obj::Dict(pairs) if pairs.iter().any(|(k, _)| k.as_slice() == key))
}

/// A deterministic 16-byte document ID derived from the merged inputs (file
/// identities are stable across runs for the same inputs).
fn document_id(inputs: &[MergeInput]) -> [u8; 16] {
    fn fnv(data: &[u8], seed: u64) -> u64 {
        let mut h = seed ^ 0xcbf2_9ce4_8422_2325;
        for &b in data {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
    let mut blob = Vec::new();
    for input in inputs {
        blob.extend_from_slice(format!("{};", input.src.len()).as_bytes());
        blob.extend_from_slice(&input.src[..input.src.len().min(4096)]);
    }
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&fnv(&blob, 1).to_le_bytes());
    id[8..].copy_from_slice(&fnv(&blob, 2).to_le_bytes());
    id
}

/// Open a document once for a page selection or split.
///
/// The returned budget is reusable by the caller's guard; it owns no borrow
/// of the source bytes, and the parsed document/page list stay valid for the
/// rest of the operation.
fn open_for_split(path: &str) -> CliResult<(Vec<u8>, selis_pdf_cos::Doc, Vec<Ref>, Budget)> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    drop(g);
    Ok((src, doc, page_refs, budget))
}

/// Build one page selection without committing it.
///
/// `selected` is a list of zero-based source page indexes in output order. The
/// caller has already validated every index against `page_refs`. The returned
/// builder is fresh for each call because [`DocumentBuilder::write`] mutates
/// the document it serialises.
#[allow(clippy::too_many_arguments)]
fn build_selection(
    path: &str,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_refs: &[Ref],
    selected: &[usize],
    count_annots: bool,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<(selis_pdf_cos::doc_writer::DocumentBuilder, u64, Option<u64>)> {
    if selected.is_empty() {
        return Err(CliError("no pages were selected".to_string()));
    }
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<(usize, u32, Ref)> = Vec::with_capacity(selected.len());
    for &idx in selected {
        let page_ref = page_refs.get(idx).ok_or_else(|| {
            CliError(format!(
                "page {idx} out of range (document has {} pages)",
                page_refs.len()
            ))
        })?;
        let merged_ref = copy_page(&mut out, src, *page_ref, budget, g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept.push((idx, page_ref.num, merged_ref));
    }
    // WRITE.07: carry the input's catalog-level structures through the page
    // selection, pruning what only referenced the deleted pages.
    reconcile_single_input(&mut out, src, doc, page_refs, &kept, true, budget, g)
        .map_err(CliError)?;
    let expected_pages = u64::try_from(kept.len()).unwrap_or(u64::MAX);
    // The annotation expectation counts the surviving ORIGINAL pages (the
    // merged page numbers do not exist in the input document). Size probes do
    // not need the count, so skipping the walk keeps those probes cheaper.
    let kept_annots = if count_annots {
        Some(annots_on_pages(
            src,
            doc,
            &kept
                .iter()
                .map(|(_, old_num, _)| Ref::new(*old_num, 0))
                .collect::<Vec<_>>(),
            budget,
            g,
        ))
    } else {
        None
    };
    Ok((out, expected_pages, kept_annots))
}

/// Write one page selection through the full writer and verification gate.
#[allow(clippy::too_many_arguments)]
fn write_selection(
    path: &str,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_refs: &[Ref],
    selected: &[usize],
    output: &str,
    in_bytes: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Verification> {
    let (out, expected_pages, kept_annots) =
        build_selection(path, src, doc, page_refs, selected, true, budget, g)?;
    write_document(
        out,
        output,
        Some(src),
        PRESERVE_EXCEPT_PAGES,
        Some(expected_pages),
        kept_annots,
        in_bytes,
        budget,
        g,
    )
}

/// Serialise a page selection and return the exact output size without
/// committing it. This is the measurement used by split-by-size.
#[allow(clippy::too_many_arguments)]
fn measure_selection(
    path: &str,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_refs: &[Ref],
    selected: &[usize],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<u64> {
    let (mut out, _, _) = build_selection(path, src, doc, page_refs, selected, false, budget, g)?;
    let bytes = out
        .write(budget, g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    Ok(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
}

/// Write consecutive page ranges as independent verified PDFs.
#[allow(clippy::too_many_arguments)]
fn write_ranges(
    path: &str,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_refs: &[Ref],
    ranges: &[(usize, usize)],
    output_prefix: &str,
    in_bytes: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Vec<SplitPart>> {
    if ranges.is_empty() {
        return Err(CliError("split needs at least one page range".to_string()));
    }
    if ranges.len() > MAX_SPLIT_OUTPUTS {
        return Err(CliError(format!(
            "split would produce {} parts; the limit is {MAX_SPLIT_OUTPUTS}",
            ranges.len()
        )));
    }
    let count = page_refs.len();
    let mut parts = Vec::with_capacity(ranges.len());
    for (index, &(first, last)) in ranges.iter().enumerate() {
        if first > last || last >= count {
            return Err(CliError(format!(
                "page range {first}..{last} out of range (document has {count} pages)"
            )));
        }
        let output = format!("{output_prefix}{index}.pdf");
        let selected: Vec<usize> = (first..=last).collect();
        let verification = write_selection(
            path, src, doc, page_refs, &selected, &output, in_bytes, budget, g,
        )?;
        eprintln!("split pages {first}..{last} into {output}");
        verification.emit_line();
        parts.push(SplitPart {
            first,
            last,
            output,
            verification,
            over_target: false,
        });
    }
    Ok(parts)
}

/// Split a PDF: extract the pages in `page_range` (inclusive, 0-based) into a
/// new file (SL-1A.TOOL.02). Returns the operation's verification display.
///
/// `last == usize::MAX` means "through the final page"; that is the CLI's
/// default and the browser omits its `to` option for the same case.
pub(crate) fn split(
    path: &str,
    first: usize,
    last: usize,
    output: &str,
) -> CliResult<Verification> {
    let (src, doc, page_refs, budget) = open_for_split(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let last = if last == usize::MAX {
        page_refs.len().saturating_sub(1)
    } else {
        last
    };
    if last >= page_refs.len() || first > last {
        return Err(CliError(format!(
            "page range {first}..{last} out of range (document has {} pages)",
            page_refs.len()
        )));
    }
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let selected: Vec<usize> = (first..=last).collect();
    let verification = write_selection(
        path, &src, &doc, &page_refs, &selected, output, in_bytes, &budget, &mut g,
    )?;
    eprintln!("split pages {first}..{last} into {output}");
    verification.emit_line();
    Ok(verification)
}

/// Split `path` into consecutive groups of `every` pages (SL-1A.TOOL.02
/// every-N mode). Each part is separately verified; the caller receives the
/// backend key and source range for naming and download.
pub(crate) fn split_every(
    path: &str,
    every: usize,
    output_prefix: &str,
) -> CliResult<Vec<SplitPart>> {
    if every == 0 {
        return Err(CliError(
            "split interval must be at least 1 page".to_string(),
        ));
    }
    let (src, doc, page_refs, budget) = open_for_split(path)?;
    let count = page_refs.len();
    if count == 0 {
        return Err(CliError(format!("{path}: document has no pages")));
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    while start < count {
        let end = start.saturating_add(every).saturating_sub(1).min(count - 1);
        ranges.push((start, end));
        if ranges.len() > MAX_SPLIT_OUTPUTS {
            return Err(CliError(format!(
                "split every {every} page(s) would produce more than {MAX_SPLIT_OUTPUTS} parts"
            )));
        }
        start = end.saturating_add(1);
    }
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    write_ranges(
        path,
        &src,
        &doc,
        &page_refs,
        &ranges,
        output_prefix,
        in_bytes,
        &budget,
        &mut g,
    )
}

/// Split `path` in half at the middle page (SL-1A.TOOL.02 half mode).
pub(crate) fn split_half(path: &str, output_prefix: &str) -> CliResult<Vec<SplitPart>> {
    let (src, doc, page_refs, budget) = open_for_split(path)?;
    let count = page_refs.len();
    if count < 2 {
        return Err(CliError(
            "split in half needs a document with at least two pages".to_string(),
        ));
    }
    let middle = count
        .checked_div(2)
        .ok_or_else(|| CliError("cannot split this page count".to_string()))?;
    let ranges = [(0, middle.saturating_sub(1)), (middle, count - 1)];
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    write_ranges(
        path,
        &src,
        &doc,
        &page_refs,
        &ranges,
        output_prefix,
        in_bytes,
        &budget,
        &mut g,
    )
}

/// Find the largest `end` for which the measured `start..=end` group fits
/// `target_bytes`.
///
/// Adding a page to a contiguous group never removes output bytes, so the
/// measured size is monotonic in `end`. A gallop finds an upper bound with
/// O(log n) probes, then a binary search finds the last fitting end.
#[allow(clippy::too_many_arguments)]
fn largest_fitting_end(
    path: &str,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_refs: &[Ref],
    start: usize,
    target_bytes: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<usize> {
    let count = page_refs.len();
    if start >= count {
        return Err(CliError(format!(
            "page {start} out of range (document has {count} pages)"
        )));
    }
    let mut fit = start;
    let mut step = 1usize;
    let mut high = loop {
        let end = start.saturating_add(step).min(count - 1);
        let selected: Vec<usize> = (start..=end).collect();
        let measured = measure_selection(path, src, doc, page_refs, &selected, budget, g)?;
        if measured > target_bytes {
            break end;
        }
        fit = end;
        if end == count - 1 {
            return Ok(end);
        }
        step = step.saturating_mul(2);
    };
    // `fit` fits and `high` does not; keep the invariant while closing the gap.
    while fit.saturating_add(1) < high {
        let mid = fit.midpoint(high);
        let selected: Vec<usize> = (start..=mid).collect();
        let measured = measure_selection(path, src, doc, page_refs, &selected, budget, g)?;
        if measured > target_bytes {
            high = mid;
        } else {
            fit = mid;
        }
    }
    Ok(fit)
}

/// Split `path` into contiguous groups whose exact PDF output stays within
/// `target_bytes` (SL-1A.TOOL.02 size mode).
///
/// A one-page group that cannot fit the target is emitted alone with
/// [`SplitPart::over_target`] set. Any larger group exceeding the target is a
/// bug in the measurement and aborts the request.
pub(crate) fn split_by_size(
    path: &str,
    target_bytes: u64,
    output_prefix: &str,
) -> CliResult<Vec<SplitPart>> {
    if target_bytes == 0 {
        return Err(CliError(
            "split target size must be at least one byte".to_string(),
        ));
    }
    let (src, doc, page_refs, budget) = open_for_split(path)?;
    let count = page_refs.len();
    if count == 0 {
        return Err(CliError(format!("{path}: document has no pages")));
    }
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    while start < count {
        let single = measure_selection(path, &src, &doc, &page_refs, &[start], &budget, &mut g)?;
        let end = if single > target_bytes {
            start
        } else {
            largest_fitting_end(
                path,
                &src,
                &doc,
                &page_refs,
                start,
                target_bytes,
                &budget,
                &mut g,
            )?
        };
        ranges.push((start, end));
        if ranges.len() > MAX_SPLIT_OUTPUTS {
            return Err(CliError(format!(
                "split by {target_bytes} bytes would produce more than {MAX_SPLIT_OUTPUTS} parts"
            )));
        }
        start = end.saturating_add(1);
    }

    let mut parts = write_ranges(
        path,
        &src,
        &doc,
        &page_refs,
        &ranges,
        output_prefix,
        in_bytes,
        &budget,
        &mut g,
    )?;
    for part in &mut parts {
        let over = part.verification.bytes_out > target_bytes;
        if over && part.first != part.last {
            return Err(CliError(format!(
                "split pages {}..{} produced {} bytes, above the {} byte target",
                part.first, part.last, part.verification.bytes_out, target_bytes
            )));
        }
        part.over_target = over;
    }
    Ok(parts)
}

/// Extract the pages in `selection` into one PDF, keeping document order
/// (SL-1A.TOOL.02 extract mode).
pub(crate) fn extract_pages(path: &str, selection: &str, output: &str) -> CliResult<Verification> {
    let (src, doc, page_refs, budget) = open_for_split(path)?;
    let count = page_refs.len();
    let mut selected = parse_page_selection(selection, count).map_err(CliError)?;
    selected.sort_unstable();
    selected.dedup();
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let verification = write_selection(
        path, &src, &doc, &page_refs, &selected, output, in_bytes, &budget, &mut g,
    )?;
    eprintln!("extracted {} page(s) into {output}", selected.len());
    verification.emit_line();
    Ok(verification)
}

/// Set metadata fields (Info dictionary) and rewrite the document
/// (SL-1A.TOOL.09). Returns the operation's verification display.
pub(crate) fn set_metadata(
    path: &str,
    fields: &[(&str, &str)],
    output: &str,
) -> CliResult<Verification> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for (k, v) in fields {
        out.set_info(k.as_bytes(), v);
    }
    let mut kept: Vec<(usize, u32, Ref)> = Vec::new();
    for (idx, page_ref) in page_refs.iter().enumerate() {
        let merged_ref = copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept.push((idx, page_ref.num, merged_ref));
    }
    // WRITE.07: catalog structures carry through; the input's XMP packet is
    // stripped (`carry_metadata: false`) — the caller rewrote the Info
    // dictionary, and stale XMP would contradict the new values (updating
    // XMP in place is the TOOL.09 refinement).
    reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, false, &budget, &mut g,
    )
    .map_err(CliError)?;
    let verification = write_document(
        out,
        output,
        Some(&src),
        PRESERVE_ALL,
        Some(u64::try_from(kept.len()).unwrap_or(u64::MAX)),
        None,
        in_bytes,
        &budget,
        &mut g,
    )?;
    eprintln!("set {} metadata field(s) -> {output}", fields.len());
    verification.emit_line();
    Ok(verification)
}

/// Redact regions: cover them with black and strip text that falls within
/// them (SL-0.LEGAL.05). Returns the operation's verification display.
pub(crate) fn redact(
    path: &str,
    rects: &[(f64, f64, f64, f64)],
    output: &str,
) -> CliResult<Verification> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<(usize, u32, Ref)> = Vec::new();
    for (idx, page_ref) in page_refs.iter().enumerate() {
        let merged_ref = copy_page_redacted(
            &mut out,
            &src,
            *page_ref,
            rects,
            &budget,
            &mut g,
            &mut next_num,
        )
        .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept.push((idx, page_ref.num, merged_ref));
    }
    // WRITE.07: catalog-level structures carry through the rewrite.
    reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, true, &budget, &mut g,
    )
    .map_err(CliError)?;
    let verification = write_document(
        out,
        output,
        Some(&src),
        PRESERVE_ALL,
        Some(u64::try_from(kept.len()).unwrap_or(u64::MAX)),
        None,
        in_bytes,
        &budget,
        &mut g,
    )?;
    eprintln!("redacted {} region(s) -> {output}", rects.len());
    verification.emit_line();
    Ok(verification)
}

/// The leaf page references of an already-parsed document.
pub(crate) fn page_refs_of(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Ref>, String> {
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| "no revisions".to_string())?;
    let catalog = selis_pdf_cos::copy::resolve_ref(
        src,
        doc,
        rev.root.ok_or_else(|| "no /Root".to_string())?,
        budget,
        g,
    )
    .map_err(|e| format!("resolve /Root: {e}"))?;
    let pages_ref = match catalog {
        Obj::Dict(ref pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == b"Pages")
            .and_then(|(_, v)| match v {
                Obj::Ref(r) => Some(*r),
                _ => None,
            }),
        _ => None,
    }
    .ok_or_else(|| "catalog has no /Pages".to_string())?;
    walk_pages(src, doc, pages_ref, budget, g, 0)
}

/// Resolve an object by its revision xref entry (handles compressed objects).
fn resolve_ref_obj(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    r: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Obj, String> {
    selis_pdf_cos::copy::resolve_ref(src, doc, r, budget, g).map_err(|e| e.to_string())
}

/// Walk a pages-tree node, collecting leaf page refs.
fn walk_pages(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    node: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    depth: usize,
) -> Result<Vec<Ref>, String> {
    if depth > 32 {
        return Err("page tree too deep".to_string());
    }
    let obj =
        selis_pdf_cos::copy::resolve_ref(src, doc, node, budget, g).map_err(|e| e.to_string())?;
    let Obj::Dict(pairs) = &obj else {
        return Ok(Vec::new());
    };
    let ty = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Type")
        .and_then(|(_, v)| match v {
            Obj::Name(n) => Some(n.as_slice().to_vec()),
            _ => None,
        });
    match ty.as_deref() {
        Some(b"Page") => Ok(vec![node]),
        Some(b"Pages") | None => {
            let kids: Vec<Ref> = pairs
                .iter()
                .find(|(k, _)| k.as_slice() == b"Kids")
                .and_then(|(_, v)| match v {
                    Obj::Array(items) => Some(
                        items
                            .iter()
                            .filter_map(|o| match o {
                                Obj::Ref(r) => Some(*r),
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            let mut out = Vec::new();
            for kid in kids {
                out.extend(walk_pages(src, doc, kid, budget, g, depth + 1)?);
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// Copy a leaf page (content + resources) into the output document, with
/// optional extra page-dictionary entries (e.g. `/Rotate`). Returns the
/// merged page's reference (needed by reconciliation, SL-1A.WRITE.04).
pub(crate) fn copy_page(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    _next_num: &mut u32,
) -> Result<Ref, String> {
    copy_page_inner(
        merged,
        src,
        page_ref,
        budget,
        g,
        _next_num,
        Vec::new(),
        true,
    )
}

/// Copy a leaf page with caller-supplied page-dictionary entries appended.
/// The caller owns the rotation entry (the rotate operation computes the new
/// value), so this path does not copy the source `/Rotate` automatically.
#[allow(unused_variables)]
fn copy_page_extra(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    _next_num: &mut u32,
    extra: Vec<(Vec<u8>, Obj)>,
) -> Result<Ref, String> {
    copy_page_inner(merged, src, page_ref, budget, g, _next_num, extra, false)
}

#[allow(clippy::too_many_arguments)]
fn copy_page_inner(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    _next_num: &mut u32,
    extra: Vec<(Vec<u8>, Obj)>,
    preserve_rotation: bool,
) -> Result<Ref, String> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| "no revisions".to_string())?;
    let _ = rev;
    let (media, content_refs, resources, rotate, annots) =
        page_info(src, &doc, page_ref, budget, g)?;
    let (objects, remap) =
        selis_pdf_cos::copy::collect_objects(src, &content_refs, merged.next_num_mut(), budget, g)
            .map_err(|e| format!("copy objects: {e}"))?;
    for (num, obj) in objects {
        merged.add_object(num, obj);
    }
    let new_contents: Vec<Ref> = content_refs
        .iter()
        .map(|r| Ref::new(remap.get(&r.num).copied().unwrap_or(r.num), r.gen))
        .collect();
    let new_resources = materialize_resources(merged, src, resources, budget, g)?;
    // Page order must not change how a page is read, so a plain copy keeps
    // its rotation. The rotate operation supplies its own computed value and
    // opts out to avoid writing `/Rotate` twice.
    let mut extra = extra;
    if preserve_rotation {
        extra.extend(rotate_extra(rotate));
    }
    if let Some(annots_ref) = materialize_object(merged, src, annots, budget, g)? {
        extra.push((b"Annots".to_vec(), Obj::Ref(annots_ref)));
    }
    Ok(merged.add_page_with_extra(media.0, media.1, &new_contents, new_resources, extra))
}

/// Carry a page's `/Resources` into `merged`, returning the reference the
/// copied page should point at (WRITE.03). See [`materialize_object`].
fn materialize_resources(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    resources: Option<Obj>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Ref>, String> {
    materialize_object(merged, src, resources, budget, g)
}

/// Carry an arbitrary page-level value (resources, annotations, group) into
/// `merged` and return a reference to it, or `None` when absent (WRITE.03).
///
/// Handles the forms a page entry may take: an indirect reference (copy its
/// subgraph and remap), or an inline value — a dictionary or an array of
/// references (copy the objects it references, renumber the value, and store
/// it as its own object so the page holds a valid indirect reference).
/// Dropping these loses the page's fonts/XObjects/annotations.
fn materialize_object(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    value: Option<Obj>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Ref>, String> {
    let Some(val) = value else {
        return Ok(None);
    };
    match val {
        Obj::Ref(r) => {
            let (objects, remap) =
                selis_pdf_cos::copy::collect_objects(src, &[r], merged.next_num_mut(), budget, g)
                    .map_err(|e| format!("copy object: {e}"))?;
            for (num, obj) in objects {
                merged.add_object(num, obj);
            }
            Ok(Some(Ref::new(
                remap.get(&r.num).copied().unwrap_or(r.num),
                r.gen,
            )))
        }
        inline @ (Obj::Dict(_) | Obj::Array(_)) => {
            let mut sub_refs = Vec::new();
            collect_refs_in(&inline, &mut sub_refs);
            let remap = if sub_refs.is_empty() {
                std::collections::HashMap::new()
            } else {
                let (objects, remap) = selis_pdf_cos::copy::collect_objects(
                    src,
                    &sub_refs,
                    merged.next_num_mut(),
                    budget,
                    g,
                )
                .map_err(|e| format!("copy inline object: {e}"))?;
                for (num, obj) in objects {
                    merged.add_object(num, obj);
                }
                remap
            };
            let renumbered = selis_pdf_cos::copy::renumber(inline, &remap);
            let num = merged.allocate();
            merged.add_object(num, renumbered);
            Ok(Some(Ref::new(num, 0)))
        }
        // Anything else (a primitive, or an unexpected stream) is skipped.
        _ => Ok(None),
    }
}

/// Gather every reference reachable inside `obj`.
fn collect_refs_in(obj: &Obj, out: &mut Vec<Ref>) {
    match obj {
        Obj::Ref(r) => out.push(*r),
        Obj::Array(items) => {
            for item in items {
                collect_refs_in(item, out);
            }
        }
        Obj::Dict(pairs) | Obj::Stream { dict: pairs, .. } => {
            for (_, v) in pairs {
                collect_refs_in(v, out);
            }
        }
        _ => {}
    }
}

/// The page's (media box, content refs, resources value, existing /Rotate,
/// annotations value). The resources and annotations values are the raw page
/// entries: an indirect reference, an inline dictionary/array, or absent.
/// (old page index, old page object number, merged page reference).
pub(crate) type KeptPage = (usize, u32, Ref);

/// What `page_info` reads off one page (SL-0.WS.11): its media box, the
/// annotation references, the (possibly indirect) `/StructParents`, the
/// rotation, and the (possibly indirect) `/Tabs` value. Named because the
/// five-tuple is a contract every page-tree caller reads positionally.
type PageInfo = ((f64, f64), Vec<Ref>, Option<Obj>, i64, Option<Obj>);

pub(crate) fn page_info(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<PageInfo, String> {
    let page = resolve_ref_obj(src, doc, page_ref, budget, g)?;
    let Obj::Dict(pairs) = &page else {
        return Err("page is not a dict".to_string());
    };
    // `/MediaBox`, `/Resources` and `/Rotate` are inheritable: a leaf page may
    // omit them and take them from an ancestor `/Pages` node. Reading only the
    // leaf silently substituted Letter/Forest sizes, dropped fonts, and lost
    // rotation on real documents written that way.
    let media = media_size(
        src,
        doc,
        inherited_page_value(src, doc, page_ref, b"MediaBox", budget, g, 0)?,
        budget,
        g,
    )?;
    let contents = match pairs.iter().find(|(k, _)| k.as_slice() == b"Contents") {
        Some((_, Obj::Ref(r))) => vec![*r],
        Some((_, Obj::Array(items))) => items
            .iter()
            .filter_map(|o| match o {
                Obj::Ref(r) => Some(*r),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let resources = inherited_page_value(src, doc, page_ref, b"Resources", budget, g, 0)?;
    let annots = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Annots")
        .map(|(_, v)| v.clone());
    let rotate = match inherited_page_value(src, doc, page_ref, b"Rotate", budget, g, 0)? {
        Some(Obj::Int(v)) => v,
        Some(Obj::Ref(r)) => match resolve_ref_obj(src, doc, r, budget, g)? {
            Obj::Int(v) => v,
            _ => 0,
        },
        _ => 0,
    };
    Ok((media, contents, resources, rotate, annots))
}

/// Walk a leaf page and its `/Parent` chain for an inheritable page-tree
/// attribute. PDF 32000-1 7.7.3.4 makes `/Resources`, `/MediaBox`, `/CropBox`
/// and `/Rotate` inheritable; the leaf's own entry wins.
fn inherited_page_value(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    node: Ref,
    key: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    depth: usize,
) -> Result<Option<Obj>, String> {
    if depth > 32 {
        return Err("page tree too deep".to_string());
    }
    let obj = resolve_ref_obj(src, doc, node, budget, g)?;
    let Obj::Dict(pairs) = &obj else {
        return Ok(None);
    };
    if let Some((_, value)) = pairs.iter().find(|(k, _)| k.as_slice() == key) {
        return Ok(Some(value.clone()));
    }
    let parent = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Parent")
        .and_then(|(_, value)| match value {
            Obj::Ref(r) => Some(*r),
            _ => None,
        });
    match parent {
        Some(parent) => inherited_page_value(src, doc, parent, key, budget, g, depth + 1),
        None => Ok(None),
    }
}

/// The page size from a (possibly indirect) `/MediaBox`, defaulting to US
/// Letter when the tree does not carry a usable box. The box may have a
/// non-zero origin, so size is `upper - lower`, not the upper coordinate.
fn media_size(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    value: Option<Obj>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(f64, f64), String> {
    let value = match value {
        Some(Obj::Ref(r)) => resolve_ref_obj(src, doc, r, budget, g)?,
        Some(value) => value,
        None => return Ok((612.0, 792.0)),
    };
    let Obj::Array(items) = value else {
        return Ok((612.0, 792.0));
    };
    if items.len() < 4 {
        return Ok((612.0, 792.0));
    }
    let n = |i: usize| match items.get(i) {
        Some(Obj::Int(v)) => Some(*v as f64),
        Some(Obj::Real { scaled, scale }) => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
        _ => None,
    };
    let (Some(x0), Some(y0), Some(x1), Some(y1)) = (n(0), n(1), n(2), n(3)) else {
        return Ok((612.0, 792.0));
    };
    Ok(((x1 - x0).abs(), (y1 - y0).abs()))
}

/// Copy a page, appending black rects over `rects` and stripping text that
/// falls within them. Returns the redacted page's reference in `merged`.
fn copy_page_redacted(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    rects: &[(f64, f64, f64, f64)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    _next_num: &mut u32,
) -> Result<Ref, String> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    if doc.revisions().last().is_none() {
        return Err("no revisions".to_string());
    }
    let (media, content_refs, resources_ref, rotate, annots) =
        page_info(src, &doc, page_ref, budget, g)?;

    // Rebuild the content streams with redaction: strip text inside the
    // regions, then append black fills over them.
    let mut rebuilt = Vec::new();
    for content_ref in &content_refs {
        let content_obj = resolve_ref_obj(src, &doc, *content_ref, budget, g)?;
        if let Obj::Stream { data, .. } = &content_obj {
            rebuilt.extend(strip_text_in_regions(data.as_slice(), rects));
            rebuilt.push(b'\n');
        }
    }
    // Append black rects over the regions.
    let mut cb = selis_pdf_cos::doc_writer::ContentBuilder::new();
    cb.set_fill(0.0, 0.0, 0.0);
    for (x, y, w, h) in rects {
        cb.fill_rect(*x, *y, *w, *h);
    }
    rebuilt.extend_from_slice(&cb.to_bytes());

    // Copy the (rebuilt) content as a fresh stream, then the resources.
    let content_num = merged.allocate();
    merged.add_object(
        content_num,
        Obj::Stream {
            dict: vec![(
                selis_bytes::Bytes::copy_from_slice(b"Length"),
                Obj::Int(i64::try_from(rebuilt.len()).unwrap_or(i64::MAX)),
            )],
            data: selis_bytes::Bytes::from(rebuilt),
        },
    );
    let new_resources = materialize_resources(merged, src, resources_ref, budget, g)?;
    let mut extra = rotate_extra(rotate);
    if let Some(annots_ref) = materialize_object(merged, src, annots, budget, g)? {
        extra.push((b"Annots".to_vec(), Obj::Ref(annots_ref)));
    }
    Ok(merged.add_page_with_extra(
        media.0,
        media.1,
        &[Ref::new(content_num, 0)],
        new_resources,
        extra,
    ))
}

/// The `/Rotate` page-dict entry when a non-zero rotation is present.
fn rotate_extra(rotate: i64) -> Vec<(Vec<u8>, Obj)> {
    let normalized = rotate.rem_euclid(360);
    if normalized == 0 {
        Vec::new()
    } else {
        vec![(b"Rotate".to_vec(), Obj::Int(normalized))]
    }
}

/// Rotate pages of a PDF (SL-1A.TOOL.03). `pages` is `None` for all pages,
/// or a `0,2,5-7` selection; `angle` is added to each page's existing
/// `/Rotate` (mod 360). Returns the operation's verification display.
pub(crate) fn rotate(
    path: &str,
    angle: i64,
    pages: Option<&str>,
    output: &str,
) -> CliResult<Verification> {
    let norm = angle.rem_euclid(360);
    if norm != 90 && norm != 180 && norm != 270 {
        return Err(CliError(format!(
            "rotation angle must be 90, 180 or 270 (got {angle})"
        )));
    }
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let selected = match pages {
        Some(spec) => parse_page_selection(spec, page_refs.len())
            .map_err(|e| CliError(format!("{path}: {e}")))?,
        None => (0..page_refs.len()).collect(),
    };
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<(usize, u32, Ref)> = Vec::new();
    for (idx, page_ref) in page_refs.iter().enumerate() {
        let existing = page_rotate(&src, &budget, &mut g, *page_ref)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        let extra = if selected.contains(&idx) {
            rotate_extra(existing.saturating_add(norm))
        } else {
            rotate_extra(existing)
        };
        let merged_ref = copy_page_extra(
            &mut out,
            &src,
            *page_ref,
            &budget,
            &mut g,
            &mut next_num,
            extra,
        )
        .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept.push((idx, page_ref.num, merged_ref));
    }
    // WRITE.07: catalog-level structures carry through the rewrite.
    reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, true, &budget, &mut g,
    )
    .map_err(CliError)?;
    let verification = write_document(
        out,
        output,
        Some(&src),
        PRESERVE_ALL,
        Some(u64::try_from(kept.len()).unwrap_or(u64::MAX)),
        None,
        in_bytes,
        &budget,
        &mut g,
    )?;
    eprintln!(
        "rotated {} page(s) by {}° -> {output}",
        selected.len(),
        norm
    );
    verification.emit_line();
    Ok(verification)
}

/// Delete pages from a PDF (SL-1A.TOOL.03). `pages` is a `0,2,5-7` selection.
/// Returns the operation's verification display.
pub(crate) fn delete(path: &str, pages: &str, output: &str) -> CliResult<Verification> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let removed = parse_page_selection(pages, page_refs.len())
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    if removed.len() >= page_refs.len() {
        return Err(CliError(
            "refusing to delete every page; a PDF needs at least one".to_string(),
        ));
    }
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept = 0usize;
    let mut kept_pages: Vec<(usize, u32, Ref)> = Vec::new();
    for (idx, page_ref) in page_refs.iter().enumerate() {
        if removed.contains(&idx) {
            continue;
        }
        let merged_ref = copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept_pages.push((idx, page_ref.num, merged_ref));
        kept += 1;
    }
    // WRITE.07: catalog-level structures carry through the page selection,
    // pruned where they only referenced deleted pages.
    reconcile_single_input(
        &mut out,
        &src,
        &doc,
        &page_refs,
        &kept_pages,
        true,
        &budget,
        &mut g,
    )
    .map_err(CliError)?;
    // The annotation expectation counts the surviving ORIGINAL pages (the
    // merged page numbers do not exist in the input document).
    let kept_annots = annots_on_pages(
        &src,
        &doc,
        &kept_pages
            .iter()
            .map(|(_, old_num, _)| Ref::new(*old_num, 0))
            .collect::<Vec<_>>(),
        &budget,
        &mut g,
    );
    let verification = write_document(
        out,
        output,
        Some(&src),
        PRESERVE_EXCEPT_PAGES,
        Some(u64::try_from(kept_pages.len()).unwrap_or(u64::MAX)),
        Some(kept_annots),
        in_bytes,
        &budget,
        &mut g,
    )?;
    eprintln!("deleted {} page(s), kept {kept} -> {output}", removed.len());
    verification.emit_line();
    Ok(verification)
}

/// One page in an output plan: a copy of a source page, or a blank page whose
/// media box and rotation come from a source template page.
enum PlannedPage {
    /// Copy source page `index` (zero-based) into the output.
    Source(usize),
    /// Add a blank page using source page `template` for `/MediaBox` and
    /// `/Rotate`.
    Blank { template: usize },
}

/// Build one output from an explicit source-page plan.
///
/// Unlike the older one-to-one selection helpers, a plan may repeat a source
/// page (duplicate) or add pages with no source counterpart (blank). The
/// output-to-source map used by reconciliation is therefore derived from the
/// actual plan, with the first occurrence winning for destinations and labels.
#[allow(clippy::too_many_arguments)]
fn rewrite_with_plan(
    path: &str,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_refs: &[Ref],
    plan: &[PlannedPage],
    output: &str,
    action: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Verification> {
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<(usize, u32, Ref)> = Vec::new();
    let mut new_idx_by_old: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    let mut annotation_pages: Vec<Ref> = Vec::new();
    let mut blank_templates: std::collections::HashMap<usize, ((f64, f64), i64)> =
        std::collections::HashMap::new();

    for (new_idx, planned) in plan.iter().enumerate() {
        match *planned {
            PlannedPage::Source(idx) => {
                let page_ref = page_refs
                    .get(idx)
                    .copied()
                    .ok_or_else(|| CliError(format!("page {idx} out of range")))?;
                let merged_ref = copy_page(&mut out, src, page_ref, budget, g, &mut next_num)
                    .map_err(|e| CliError(format!("{path}: {e}")))?;
                kept.push((idx, page_ref.num, merged_ref));
                new_idx_by_old.entry(idx).or_insert(new_idx);
                annotation_pages.push(page_ref);
            }
            PlannedPage::Blank { template } => {
                let (media, rotate) = if let Some(spec) = blank_templates.get(&template) {
                    *spec
                } else {
                    let template_ref = page_refs.get(template).copied().ok_or_else(|| {
                        CliError(format!("blank-page template page {template} out of range"))
                    })?;
                    let (media, _, _, rotate, _) = page_info(src, doc, template_ref, budget, g)
                        .map_err(|e| CliError(format!("{path}: {e}")))?;
                    let spec = (media, rotate);
                    blank_templates.insert(template, spec);
                    spec
                };
                out.add_page_with_extra(media.0, media.1, &[], None, rotate_extra(rotate));
            }
        }
    }

    // WRITE.07: catalog-level structures carry through the rewrite; page
    // labels re-key at the new indices, destinations remap at the pages.
    reconcile_single_input_mapped(
        &mut out,
        src,
        doc,
        page_refs,
        &kept,
        &new_idx_by_old,
        true,
        budget,
        g,
    )
    .map_err(CliError)?;
    // Duplicated pages repeat their annotations, so the expectation is the
    // exact total over the planned source pages, not the input's total.
    let annotations = annots_on_pages(src, doc, &annotation_pages, budget, g);
    let verification = write_document(
        out,
        output,
        Some(src),
        PRESERVE_ALL,
        Some(u64::try_from(plan.len()).unwrap_or(u64::MAX)),
        Some(annotations),
        in_bytes,
        budget,
        g,
    )?;
    eprintln!("{action} -> {output}");
    verification.emit_line();
    Ok(verification)
}

/// Reorder the pages of a PDF (SL-1A.TOOL.03). `order` is a permutation such
/// as `2,0,1` or `3-5,0-2`. Returns the operation's verification display.
pub(crate) fn reorder(path: &str, order: &str, output: &str) -> CliResult<Verification> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let order =
        parse_page_order(order, page_refs.len()).map_err(|e| CliError(format!("{path}: {e}")))?;
    let plan: Vec<PlannedPage> = order.into_iter().map(PlannedPage::Source).collect();
    let action = format!("reordered {} page(s)", plan.len());
    rewrite_with_plan(
        path, &src, &doc, &page_refs, &plan, output, &action, &budget, &mut g,
    )
}

/// Duplicate each selected page immediately after its original (SL-1A.TOOL.03).
///
/// The selection is zero-based (`0,2,5-7`), matching the CLI's other page
/// operations. Each page is copied once, in source order, even if the selection
/// repeats an index. Content, resources, and annotations travel with the copy.
pub(crate) fn duplicate_pages(path: &str, pages: &str, output: &str) -> CliResult<Verification> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let selected = parse_page_selection(pages, page_refs.len())
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let selected_count = selected.len();
    let selected: std::collections::HashSet<usize> = selected.into_iter().collect();
    let mut plan = Vec::with_capacity(page_refs.len().saturating_add(selected_count));
    for idx in 0..page_refs.len() {
        plan.push(PlannedPage::Source(idx));
        if selected.contains(&idx) {
            plan.push(PlannedPage::Source(idx));
        }
    }
    let action = format!("duplicated {selected_count} page(s)");
    rewrite_with_plan(
        path, &src, &doc, &page_refs, &plan, output, &action, &budget, &mut g,
    )
}

/// Insert blank pages into a PDF (SL-1A.TOOL.03).
///
/// `after` is the 1-based page after which the blanks go; `0` inserts before
/// the first page and `None` appends. Every blank page uses the template page's
/// media box and rotation: the page at `after - 1`, or the first page when
/// `after == 0`, or the last page when appending. At most
/// [`crate::MAX_BLANK_PAGES`] pages may be added per request.
pub(crate) fn insert_blank_pages(
    path: &str,
    after: Option<usize>,
    count: usize,
    output: &str,
) -> CliResult<Verification> {
    if count == 0 {
        return Err(CliError("insert at least one blank page".to_string()));
    }
    if count > crate::MAX_BLANK_PAGES {
        return Err(CliError(format!(
            "at most {} blank pages can be inserted at once",
            crate::MAX_BLANK_PAGES
        )));
    }
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    if page_refs.is_empty() {
        return Err(CliError(format!("{path}: document has no pages")));
    }
    let after = after.unwrap_or(page_refs.len());
    if after > page_refs.len() {
        return Err(CliError(format!(
            "{path}: insert position {after} is out of range (document has {} pages; use 0 to insert before the first page)",
            page_refs.len()
        )));
    }
    let template = after
        .saturating_sub(1)
        .min(page_refs.len().saturating_sub(1));
    let mut plan = Vec::with_capacity(page_refs.len().saturating_add(count));
    for idx in 0..page_refs.len() {
        if idx == after {
            for _ in 0..count {
                plan.push(PlannedPage::Blank { template });
            }
        }
        plan.push(PlannedPage::Source(idx));
    }
    if after == page_refs.len() {
        for _ in 0..count {
            plan.push(PlannedPage::Blank { template });
        }
    }
    let action = format!("inserted {count} blank page(s) after page {after}");
    rewrite_with_plan(
        path, &src, &doc, &page_refs, &plan, output, &action, &budget, &mut g,
    )
}

/// Total `/Annots` items across a subset of pages (used by split/delete:
/// the output keeps only the surviving pages' annotations).
fn annots_on_pages(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    pages: &[Ref],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> u64 {
    let mut total = 0u64;
    for page_ref in pages {
        let Ok(page) = resolve_ref_obj(src, doc, *page_ref, budget, g) else {
            continue;
        };
        let annots = match &page {
            Obj::Dict(pairs) => pairs
                .iter()
                .find(|(k, _)| k.as_slice() == b"Annots")
                .map(|(_, v)| v.clone()),
            _ => None,
        };
        let Some(annots) = annots else { continue };
        let resolved = match annots {
            Obj::Ref(r) => resolve_ref_obj(src, doc, r, budget, g).unwrap_or(Obj::Null),
            other => other,
        };
        if let Obj::Array(items) = resolved {
            total = total.saturating_add(u64::try_from(items.len()).unwrap_or(u64::MAX));
        }
    }
    total
}

/// Serialise and write a built document to `output` with WRITE.05 structural
/// verification and an atomic commit, and build the operation's verification
/// display (SL-1A.UI.02). `preserved` selects which counts the operation
/// promises to keep from the input's survey; `expected_pages` is asserted
/// directly when the caller knows the number (split, reorder);
/// `annotations_override` replaces the input-total annotation expectation
/// when only a subset of pages survives (split, delete). `in_bytes` is the
/// size of what was read (the display's `bytes_in`).
// The single commit surface every tool funnels through (WRITE.05), so its
// parameters are the whole per-operation contract: the builder, the output
// and input paths and size, the preserved-count policy, the page-count and
// annotation overrides, the conformance text, and the two budget handles.
// That is the point of a commit surface — one place that knows all of it —
// so the arity stays and the allow is scoped to this function (SL-0.WS.11).
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_document(
    builder: selis_pdf_cos::doc_writer::DocumentBuilder,
    output: &str,
    input: Option<&[u8]>,
    preserved: Preserved,
    expected_pages: Option<u64>,
    annotations_override: Option<u64>,
    in_bytes: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Verification> {
    let mut builder = builder;
    let bytes = builder
        .write(budget, g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    let mut input_observed = None;
    let mut expected = match input {
        Some(src) => {
            let observed = selis_pdf_cos::verify::survey(src, budget, g)
                .map_err(|e| CliError(format!("input survey failed: {e}")))?;
            let e = write_gate::expectations_from(&observed, &preserved);
            input_observed = Some(observed);
            e
        }
        None => selis_pdf_cos::verify::Expectations::none(),
    };
    if let Some(pages) = expected_pages {
        expected.pages = Some(pages);
    }
    if let Some(annots) = annotations_override {
        expected.annotations = Some(annots);
    }
    let verdict = write_gate::write_verified(&bytes, output, &expected, budget, g)?;
    let verification = Verification::from_gate(
        input_observed.as_ref(),
        &verdict,
        &expected,
        in_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    Ok(verification)
}

/// A page's existing `/Rotate` (0 when absent).
fn page_rotate(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    page_ref: Ref,
) -> Result<i64, String> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    let (_, _, _, rotate, _) = page_info(src, &doc, page_ref, budget, g)?;
    Ok(rotate)
}

/// Parse a page selection (`0,2,5-7`; 0-based) into a deduplicated set.
fn parse_page_selection(spec: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut out: Vec<usize> = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let range: Vec<usize> = parse_part(part, count)?;
        for idx in range {
            if !out.contains(&idx) {
                out.push(idx);
            }
        }
    }
    if out.is_empty() {
        return Err(format!("`{spec}` selects no pages"));
    }
    Ok(out)
}

/// Parse a page order (`2,0,1` or `3-5,0`; 0-based) into a full permutation.
fn parse_page_order(spec: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut out: Vec<usize> = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.extend(parse_part(part, count)?);
    }
    if out.len() != count {
        return Err(format!(
            "`{spec}` lists {} pages but the document has {count}; \
             reorder needs every page exactly once",
            out.len()
        ));
    }
    let mut seen = vec![false; count];
    for &idx in &out {
        if let Some(slot) = seen.get_mut(idx) {
            if *slot {
                return Err(format!("`{spec}` lists page {idx} twice"));
            }
            *slot = true;
        }
    }
    Ok(out)
}

/// Parse one `N` or `A-B` part of a page list.
fn parse_part(part: &str, count: usize) -> Result<Vec<usize>, String> {
    let bounds_check = |idx: usize| -> Result<usize, String> {
        if idx >= count {
            Err(format!(
                "page {idx} out of range (document has {count} pages)"
            ))
        } else {
            Ok(idx)
        }
    };
    if let Some((a, b)) = part.split_once('-') {
        let start: usize = a
            .trim()
            .parse()
            .map_err(|_| format!("`{part}` is not a page range"))?;
        let end: usize = b
            .trim()
            .parse()
            .map_err(|_| format!("`{part}` is not a page range"))?;
        if start > end {
            return Err(format!("range `{part}` goes backwards"));
        }
        (start..=end).map(bounds_check).collect()
    } else {
        let idx: usize = part
            .parse()
            .map_err(|_| format!("`{part}` is not a page number"))?;
        Ok(vec![bounds_check(idx)?])
    }
}

/// A minimal content-stream rewriter: drop `Tj`/`TJ`/`'`/`"` show operations
/// whose text origin is inside one of `rects`. The token stream is walked
/// tracking the text line matrix; `BT`/`ET` and state operators pass through.
fn strip_text_in_regions(content: &[u8], rects: &[(f64, f64, f64, f64)]) -> Vec<u8> {
    use selis_pdf_cos::{Number, Token};
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let mut lexer = selis_pdf_cos::Lexer::new(content);
    let mut out: Vec<u8> = Vec::new();
    let mut tm = [1.0f64, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut in_text = false;
    let mut tokens: Vec<Token> = Vec::new();
    while let Ok(Some(t)) = lexer.next_token(&mut g) {
        tokens.push(t);
    }
    let n = |t: &Token| -> Option<f64> {
        match t {
            Token::Number(Number::Int(v)) => Some(*v as f64),
            Token::Number(Number::Real { scaled, scale }) => {
                Some(*scaled as f64 / 10f64.powi(i32::from(*scale)))
            }
            _ => None,
        }
    };
    let mut i = 0usize;
    while i < tokens.len() {
        let t = &tokens[i];
        match t {
            Token::Name(name) => {
                let op = String::from_utf8_lossy(name.as_slice()).to_string();
                match op.as_str() {
                    "BT" => {
                        in_text = true;
                        tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                        emit(&mut out, &tokens[i..=i]);
                        i += 1;
                    }
                    "ET" => {
                        in_text = false;
                        emit(&mut out, &tokens[i..=i]);
                        i += 1;
                    }
                    "Td" | "TD" => {
                        if let (Some(x), Some(y)) =
                            (tokens.get(i + 1).and_then(n), tokens.get(i + 2).and_then(n))
                        {
                            tm[4] += x;
                            tm[5] += y;
                            emit(&mut out, &tokens[i..=i.saturating_add(2)]);
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                    "Tm" => {
                        if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                            tokens.get(i + 1).and_then(n),
                            tokens.get(i + 2).and_then(n),
                            tokens.get(i + 3).and_then(n),
                            tokens.get(i + 4).and_then(n),
                            tokens.get(i + 5).and_then(n),
                            tokens.get(i + 6).and_then(n),
                        ) {
                            tm = [a, b, c, d, e, f];
                            emit(&mut out, &tokens[i..=i.saturating_add(6)]);
                            i += 7;
                        } else {
                            i += 1;
                        }
                    }
                    "Tj" => {
                        let (x, y) = (tm[4], tm[5]);
                        if in_text && inside_any(rects, x, y) {
                            i += 2;
                        } else {
                            emit(&mut out, &tokens[i..=i.saturating_add(1)]);
                            i += 2;
                        }
                    }
                    "'" | "\"" => {
                        let (x, y) = (tm[4], tm[5]);
                        if in_text && inside_any(rects, x, y) {
                            i += 2;
                        } else {
                            emit(&mut out, &tokens[i..=i.saturating_add(1)]);
                            i += 2;
                        }
                    }
                    "TJ" => {
                        let (x, y) = (tm[4], tm[5]);
                        if in_text && inside_any(rects, x, y) {
                            i += 2;
                        } else {
                            emit(&mut out, &tokens[i..=i.saturating_add(1)]);
                            i += 2;
                        }
                    }
                    _ => {
                        let mut j = i.saturating_add(1);
                        while j < tokens.len() && !matches!(tokens[j], Token::Name(_)) {
                            j = j.saturating_add(1);
                        }
                        emit(&mut out, &tokens[i..j]);
                        i = j;
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    out
}

/// Whether a point is inside any rect.
fn inside_any(rects: &[(f64, f64, f64, f64)], x: f64, y: f64) -> bool {
    rects
        .iter()
        .any(|(rx, ry, rw, rh)| x >= *rx && x <= rx + rw && y >= *ry && y <= ry + rh)
}

/// Emit a token slice back to bytes (via the object writer for strings/names).
fn emit(out: &mut Vec<u8>, toks: &[selis_pdf_cos::Token]) {
    use selis_pdf_cos::{Number, Token};
    let mut buf: Vec<u8> = Vec::new();
    for t in toks {
        match t {
            Token::Number(n) => {
                let s = match n {
                    Number::Int(v) => format!("{v}"),
                    Number::Real { scaled, scale } => {
                        let v = *scaled as f64 / 10f64.powi(i32::from(*scale));
                        format!("{v:.3}")
                            .trim_end_matches('0')
                            .trim_end_matches('.')
                            .to_string()
                    }
                };
                buf.extend_from_slice(s.as_bytes());
                buf.push(b' ');
            }
            Token::Name(b) => {
                buf.extend_from_slice(b"/");
                buf.extend_from_slice(b.as_slice());
                buf.push(b' ');
            }
            Token::String(b) => {
                buf.push(b'(');
                for &c in b.as_slice() {
                    if c == b'(' || c == b')' || c == b'\\' {
                        buf.push(b'\\');
                    }
                    buf.push(c);
                }
                buf.push(b')');
                buf.push(b' ');
            }
            _ => {}
        }
    }
    out.extend_from_slice(&buf);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::{Budget, Obj, Ref};

    /// Build a one-page PDF whose page carries an inline `/Resources` and an
    /// `/Annots` array referencing one annotation object. Offsets are computed
    /// so the bytes reparse cleanly.
    fn annotated_source() -> Vec<u8> {
        let numbered: &[(u32, &[u8])] = &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents 4 0 R /Annots [5 0 R] >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n"),
            (
                5,
                b"5 0 obj\n<< /Type /Annot /Subtype /Square /Rect [10 10 50 50] >>\nendobj\n",
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
        out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for (_, off) in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n");
        out.extend_from_slice(format!("{xref_at}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");
        out
    }

    /// A page that inherits its box from the root `/Pages` node and its
    /// resources and rotation from an intermediate `/Pages` node, as PDF
    /// 32000-1 7.7.3.4 permits.
    fn inherited_page_source() -> Vec<u8> {
        let numbered: &[(u32, &[u8])] = &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n"),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [10 20 210 320] >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /Rotate 90 /Resources << /Font << /F1 6 0 R >> >> >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Type /Page /Parent 3 0 R /Contents 5 0 R >>\nendobj\n",
            ),
            (5, b"5 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n"),
            (
                6,
                b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
            ),
        ];
        numbered_source(numbered)
    }

    /// Inheritable page attributes survive both a read and the copy path that
    /// rotation uses; a leaf-only read used to substitute Letter and lose the
    /// font or inherited rotation.
    #[test]
    fn inherited_page_attributes_are_read_and_copied() {
        let bytes = inherited_page_source();
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("parse");
        let refs = super::page_refs_of(&bytes, &doc, &budget, &mut g).expect("page refs");
        assert_eq!(refs.len(), 1);
        let (media, contents, resources, rotate, _) =
            super::page_info(&bytes, &doc, refs[0], &budget, &mut g).expect("page info");
        assert_eq!(media, (200.0, 300.0), "inherited /MediaBox size");
        assert_eq!(contents.len(), 1, "page content");
        assert_eq!(rotate, 90, "inherited /Rotate");
        let Obj::Dict(resource_pairs) = resources.expect("inherited /Resources") else {
            panic!("resources should be a dictionary");
        };
        assert!(
            resource_pairs.iter().any(|(k, _)| k.as_slice() == b"Font"),
            "inherited /Resources must carry the font dictionary"
        );

        let dir = temp_dir("inherited-page-attributes");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, &bytes).unwrap();
        super::rotate(
            &input.display().to_string(),
            90,
            None,
            &output.display().to_string(),
        )
        .expect("rotate");
        let out = std::fs::read(&output).expect("read output");
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&out, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&out, sx, &budget, &mut g).expect("parse out");
        let refs = super::page_refs_of(&out, &doc, &budget, &mut g).expect("page refs out");
        let (media, _, resources, rotate, _) =
            super::page_info(&out, &doc, refs[0], &budget, &mut g).expect("page info out");
        assert_eq!(media, (200.0, 300.0), "copied page keeps its media box");
        assert_eq!(rotate, 180, "rotation is added to the inherited value");
        // The writer materializes copied resources as an indirect object, so
        // resolve the page's `/Resources` reference before checking its keys.
        let resources = match resources.expect("copied /Resources") {
            Obj::Ref(r) => super::resolve_ref_obj(&out, &doc, r, &budget, &mut g)
                .expect("resolve copied resources"),
            value => value,
        };
        let Obj::Dict(resource_pairs) = resources else {
            panic!("copied resources should be a dictionary");
        };
        assert!(
            resource_pairs.iter().any(|(k, _)| k.as_slice() == b"Font"),
            "the copied page must keep the inherited font resources"
        );
    }

    /// Merging a page must carry its annotations (WRITE.03 DoD): the output
    /// keeps an `/Annots` entry whose target resolves to an annotation object.
    #[test]
    fn merge_carries_annotations() {
        let dir = std::env::temp_dir().join("selis-merge-annot-test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        let out = dir.join("out.pdf");
        std::fs::write(&a, annotated_source()).unwrap();
        std::fs::write(&b, annotated_source()).unwrap();
        super::merge(
            &[a.display().to_string(), b.display().to_string()],
            &out.display().to_string(),
        )
        .expect("merge");
        let bytes = std::fs::read(&out).expect("output");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("/Annots"),
            "annotations carried into the merge"
        );
        // The output parses and the annotation subgraph resolved (two
        // annotations — one per page).
        assert_eq!(text.matches("/Type /Annot").count(), 2);
    }

    /// A one-page PDF with outlines (one item destinating its page), page
    /// labels, and a named destination. Offsets computed for a valid xref.
    fn outlined_source() -> Vec<u8> {
        let numbered: &[(u32, &[u8])] = &[
            (
                1,
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /PageLabels 7 0 R /Dests << /Top [3 0 R /Fit] >> >>\nendobj\n",
            ),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n"),
            (
                5,
                b"5 0 obj\n<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>\nendobj\n",
            ),
            (
                6,
                b"6 0 obj\n<< /Title (Chapter) /Parent 5 0 R /Dest [3 0 R /Fit] >>\nendobj\n",
            ),
            (7, b"7 0 obj\n<< /Nums [0 << /S /r >>] >>\nendobj\n"),
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
        out.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        for (_, off) in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(b"trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n");
        out.extend_from_slice(format!("{xref_at}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");
        out
    }

    /// Merging outlined documents reconciles outlines, page labels, and named
    /// destinations (WRITE.04): the merged output carries one outline tree,
    /// shifted label keys, both named destinations, and a trailer /ID.
    #[test]
    fn merge_reconciles_document_structures() {
        let dir = std::env::temp_dir().join("selis-merge-reconcile-test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        let out = dir.join("out.pdf");
        std::fs::write(&a, outlined_source()).unwrap();
        std::fs::write(&b, outlined_source()).unwrap();
        super::merge(
            &[a.display().to_string(), b.display().to_string()],
            &out.display().to_string(),
        )
        .expect("merge");
        let bytes = std::fs::read(&out).expect("output");
        let text = String::from_utf8_lossy(&bytes);

        // Outlines survive: two top-level items (one per input) under one
        // merged root, each destinating its own merged page.
        assert!(text.contains("/Type /Outlines"), "merged outline root");
        assert_eq!(
            text.matches("/Type /Outlines").count(),
            1,
            "one merged outline root; per-input roots dropped"
        );
        assert_eq!(
            text.matches("(Chapter)").count(),
            2,
            "one outline item per input"
        );

        // Page labels: input 2's label shifts to page index 1.
        assert!(text.contains("/PageLabels"), "page labels carried");
        assert!(
            text.contains("[0 <</S /r>> 1 <</S /r>>]"),
            "label keys offset by page count: {text}"
        );

        // Named destinations: both inputs' /Top survive (second renamed).
        assert!(text.contains("(Top)"), "first named destination kept");
        assert!(text.contains("(Top-2)"), "collision renamed, not lost");

        // A trailer /ID is present.
        assert!(text.contains("/ID"), "merged document carries an /ID");

        // The output still parses.
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("reparses");
        assert_eq!(doc.revisions().len(), 1);
    }

    #[test]
    fn mix_interleaves_pages_in_round_robin_order() {
        let dir = temp_dir("mix-order");
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        let out = dir.join("out.pdf");
        std::fs::write(&a, sized_pages_source(&[100.0, 110.0, 120.0])).unwrap();
        std::fs::write(&b, sized_pages_source(&[200.0])).unwrap();
        super::mix(
            &[a.display().to_string(), b.display().to_string()],
            &out.display().to_string(),
        )
        .expect("mix");

        let bytes = std::fs::read(&out).expect("output");
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("parse");
        let refs = super::page_refs_of(&bytes, &doc, &budget, &mut g).expect("page refs");
        let widths: Vec<f64> = refs
            .iter()
            .map(|page| {
                super::page_info(&bytes, &doc, *page, &budget, &mut g)
                    .expect("page info")
                    .0
                     .0
            })
            .collect();
        assert_eq!(
            widths,
            vec![100.0, 200.0, 110.0, 120.0],
            "page 1 of each input, then page 2 of the inputs that still have one"
        );
    }

    #[test]
    fn mix_materializes_labels_so_styles_cannot_leak() {
        let dir = temp_dir("mix-labels");
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        let out = dir.join("out.pdf");
        std::fs::write(&a, outlined_source()).unwrap();
        std::fs::write(&b, sized_pages_source(&[100.0])).unwrap();
        super::mix(
            &[a.display().to_string(), b.display().to_string()],
            &out.display().to_string(),
        )
        .expect("mix");

        let bytes = std::fs::read(&out).expect("output");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/PageLabels"), "mixed labels are present");
        assert!(text.contains("/S /r"), "the labeled input keeps its style");
        assert!(
            text.contains("/S /D"),
            "the unlabeled input gets explicit decimal defaults"
        );
    }

    #[test]
    fn materialized_page_labels_advance_the_number_and_keep_output_keys() {
        let pairs = vec![(
            0i64,
            Obj::Dict(vec![
                (
                    selis_bytes::Bytes::copy_from_slice(b"S"),
                    Obj::Name(selis_bytes::Bytes::copy_from_slice(b"D")),
                ),
                (selis_bytes::Bytes::copy_from_slice(b"St"), Obj::Int(5)),
                (
                    selis_bytes::Bytes::copy_from_slice(b"P"),
                    Obj::String(selis_bytes::Bytes::copy_from_slice(b"A-")),
                ),
            ]),
        )];
        let out = super::materialize_page_labels(&pairs, &[0, 2, 1], 3);
        assert_eq!(
            out.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            vec![0, 2, 1]
        );
        let keyed_starts: Vec<(i64, i64)> = out
            .iter()
            .map(|(key, value)| {
                let start = match dict_entry(pairs_of(value), b"St") {
                    Some(Obj::Int(start)) => *start,
                    other => panic!("missing /St: {other:?}"),
                };
                (*key, start)
            })
            .collect();
        assert_eq!(
            keyed_starts,
            vec![(0, 5), (2, 6), (1, 7)],
            "each source page advances the range's start by its source offset, under its output key"
        );
    }

    /// A one-page tagged, formed PDF: an `/AcroForm` with one text field
    /// (widget on the page), a `/StructTreeRoot` whose element covers the
    /// page via `/Pg` and the parent tree via `/StructParents 0`, and one
    /// optional-content group. Object numbers skip (5–7, 10 unused) to
    /// exercise a sparse xref.
    fn formed_tagged_source() -> Vec<u8> {
        let numbered: &[(u32, &[u8])] = &[
            (
                1,
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm 8 0 R /StructTreeRoot 9 0 R /OCProperties 11 0 R >>\nendobj\n",
            ),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /StructParents 0 >>\nendobj\n",
            ),
            (4, b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n"),
            (
                8,
                b"8 0 obj\n<< /Fields [12 0 R] /DA (/Helv 0 Tf 0 g) /NeedAppearances true >>\nendobj\n",
            ),
            (
                9,
                b"9 0 obj\n<< /Type /StructTreeRoot /K 13 0 R /ParentTree 14 0 R /ParentTreeNextKey 2 /RoleMap << /Artifact /Artifact >> >>\nendobj\n",
            ),
            (
                11,
                b"11 0 obj\n<< /OCGs [15 0 R] /D << /Order [15 0 R] /ON [15 0 R] >> >>\nendobj\n",
            ),
            (
                12,
                b"12 0 obj\n<< /T (name) /FT /Tx /Rect [0 0 10 10] /P 3 0 R >>\nendobj\n",
            ),
            (
                13,
                b"13 0 obj\n<< /Type /StructElem /S /P /P 9 0 R /Pg 3 0 R /K << /Type /MCR /Pg 3 0 R /MCID 0 >> >>\nendobj\n",
            ),
            (14, b"14 0 obj\n<< /Nums [0 [13 0 R]] >>\nendobj\n"),
            (15, b"15 0 obj\n<< /Type /OCG /Name (Layer) >>\nendobj\n"),
        ];
        let max_num = numbered.iter().map(|(n, _)| *n).max().unwrap_or(1);
        let size = usize::try_from(max_num.saturating_add(1)).unwrap();
        let by_num: std::collections::HashMap<u32, &[u8]> = numbered.iter().copied().collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets: Vec<Option<u64>> = vec![None; size];
        for num in 1..=max_num {
            let Some(body) = by_num.get(&num) else {
                continue;
            };
            offsets[usize::try_from(num).unwrap()] = Some(u64::try_from(out.len()).unwrap_or(0));
            out.extend_from_slice(body);
        }
        let xref_at = u64::try_from(out.len()).unwrap_or(0);
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for off in offsets.iter().skip(1) {
            match off {
                Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n").as_bytes(),
        );
        out.extend_from_slice(format!("{xref_at}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");
        out
    }

    /// Merging two tagged, formed documents keeps every feature and it still
    /// validates (WRITE.04 DoD): one form with both fields (collision
    /// renamed), one structure tree with both elements, shifted
    /// struct-parents ids, parent-tree values pointing at the merged
    /// elements, and both OCGs under one configuration.
    #[test]
    fn merge_reconciles_forms_structure_and_layers() {
        let dir = std::env::temp_dir().join("selis-merge-formed-tagged-test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        let out = dir.join("out.pdf");
        std::fs::write(&a, formed_tagged_source()).unwrap();
        std::fs::write(&b, formed_tagged_source()).unwrap();
        super::merge(
            &[a.display().to_string(), b.display().to_string()],
            &out.display().to_string(),
        )
        .expect("merge");
        let bytes = std::fs::read(&out).expect("output");
        let text = String::from_utf8_lossy(&bytes);

        // Field collision renamed, not lost.
        assert!(text.contains("(name)"), "first field name kept");
        assert!(text.contains("(name-2)"), "field name collision renamed");

        // Structural validation: reparse and walk the merged graph.
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("reparses");
        assert_eq!(doc.revisions().len(), 1);
        let resolve =
            |r: Ref, g: &mut _| selis_pdf_cos::copy::resolve_ref(&bytes, &doc, r, &budget, g);

        let root_ref = doc
            .revisions()
            .iter()
            .rev()
            .find_map(|rev| rev.root)
            .expect("trailer root");
        let catalog = resolve(root_ref, &mut g).expect("catalog");
        let cat = pairs_of(&catalog);

        // Pages: two, with struct-parents ids 0 and 1 (second input shifted).
        let page_refs: Vec<Ref> = {
            let Obj::Ref(r) = dict_entry(cat, b"Pages").expect("/Pages") else {
                panic!("pages ref");
            };
            let pages = resolve(*r, &mut g).expect("pages");
            let kids = dict_entry(pairs_of(&pages), b"Kids").expect("/Kids");
            let Obj::Array(items) = kids else {
                panic!("kids array");
            };
            items
                .iter()
                .filter_map(|o| match o {
                    Obj::Ref(r) => Some(*r),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(page_refs.len(), 2, "two merged pages");
        let struct_parents: Vec<i64> = page_refs
            .iter()
            .map(|r| {
                let page = resolve(*r, &mut g).expect("page");
                let Some(Obj::Int(n)) = dict_entry(pairs_of(&page), b"StructParents") else {
                    panic!("page /StructParents");
                };
                *n
            })
            .collect();
        assert_eq!(
            struct_parents,
            vec![0, 1],
            "struct-parents shifted per input"
        );

        // Form: one /AcroForm listing both field roots, names intact.
        let acro = {
            let Obj::Ref(r) = dict_entry(cat, b"AcroForm").expect("/AcroForm") else {
                panic!("acroform ref");
            };
            resolve(*r, &mut g).expect("acroform")
        };
        let field_refs: Vec<Ref> = {
            let Obj::Array(items) = dict_entry(pairs_of(&acro), b"Fields").expect("/Fields") else {
                panic!("fields array");
            };
            items
                .iter()
                .filter_map(|o| match o {
                    Obj::Ref(r) => Some(*r),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(field_refs.len(), 2, "both fields listed");
        let mut field_names: Vec<String> = field_refs
            .iter()
            .filter_map(|r| {
                let field = resolve(*r, &mut g).ok()?;
                match dict_entry(pairs_of(&field), b"T")? {
                    Obj::String(s) => Some(String::from_utf8_lossy(s.as_slice()).into_owned()),
                    _ => None,
                }
            })
            .collect();
        field_names.sort();
        assert_eq!(field_names, vec!["name", "name-2"]);

        // Structure tree: one merged root, both elements re-parented at it,
        // the parent tree's values the very objects `/K` references.
        let st_ref = match dict_entry(cat, b"StructTreeRoot").expect("/StructTreeRoot") {
            Obj::Ref(r) => *r,
            _ => panic!("struct root ref"),
        };
        let st = resolve(st_ref, &mut g).expect("struct root");
        let st = pairs_of(&st);
        let kid_refs: Vec<Ref> = match dict_entry(st, b"K").expect("/K") {
            Obj::Array(items) => items
                .iter()
                .filter_map(|o| match o {
                    Obj::Ref(r) => Some(*r),
                    _ => None,
                })
                .collect(),
            Obj::Ref(r) => vec![*r],
            _ => panic!("kids"),
        };
        assert_eq!(kid_refs.len(), 2, "both structure elements under /K");
        for (i, kid) in kid_refs.iter().enumerate() {
            let elem = resolve(*kid, &mut g).expect("element");
            let elem = pairs_of(&elem);
            assert_eq!(
                dict_entry(elem, b"P"),
                Some(&Obj::Ref(st_ref)),
                "element {} re-parented at the merged root",
                i
            );
            assert_eq!(
                dict_entry(elem, b"Pg"),
                Some(&Obj::Ref(page_refs[i])),
                "element {} lands on its merged page",
                i
            );
        }
        let parent_tree = {
            let Obj::Ref(r) = dict_entry(st, b"ParentTree").expect("/ParentTree") else {
                panic!("parent tree ref");
            };
            resolve(*r, &mut g).expect("parent tree")
        };
        {
            let Obj::Array(nums) = dict_entry(pairs_of(&parent_tree), b"Nums").expect("/Nums")
            else {
                panic!("nums");
            };
            // [key, values] pairs: keys 0 and 1, values redirecting at the
            // merged elements (the cache-shared copy, not fresh duplicates).
            let mut it = nums.iter();
            let mut by_key: std::collections::HashMap<i64, &Obj> = std::collections::HashMap::new();
            while let Some(k) = it.next() {
                if let (Obj::Int(n), Some(v)) = (k, it.next()) {
                    by_key.insert(*n, v);
                }
            }
            for (key, kid) in [(0i64, &kid_refs[0]), (1, &kid_refs[1])] {
                let Some(Obj::Array(values)) = by_key.get(&key) else {
                    panic!("parent-tree key {key} missing");
                };
                assert!(
                    values.contains(&Obj::Ref(*kid)),
                    "parent-tree value {key} redirects at the merged element"
                );
            }
        }

        // Optional content: both groups listed under one configuration.
        let oc = {
            let Obj::Ref(r) = dict_entry(cat, b"OCProperties").expect("/OCProperties") else {
                panic!("oc ref");
            };
            resolve(*r, &mut g).expect("oc properties")
        };
        let ocgs = dict_entry(pairs_of(&oc), b"OCGs").expect("/OCGs");
        let Obj::Array(groups) = ocgs else {
            panic!("ocgs array");
        };
        assert_eq!(groups.len(), 2, "both OCG references listed");
    }

    fn dict_entry<'a>(pairs: &'a [(selis_bytes::Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
        pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    }

    fn pairs_of(obj: &Obj) -> &[(selis_bytes::Bytes, Obj)] {
        match obj {
            Obj::Dict(p) => p.as_slice(),
            other => panic!("expected dict, got {}", other.type_name()),
        }
    }

    fn multipage_source(pages: usize) -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut builder = selis_pdf_cos::doc_writer::DocumentBuilder::new();
        for index in 0..pages {
            let content = format!("q 1 0 0 1 {index} 0 cm Q");
            builder.add_page(100.0, 100.0, content.as_bytes());
        }
        builder
            .write(&budget, &mut g)
            .expect("write multipage source")
    }

    /// A PDF whose pages have distinct widths, so page order is observable
    /// after a copy operation without decoding content streams.
    fn sized_pages_source(widths: &[f64]) -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut builder = selis_pdf_cos::doc_writer::DocumentBuilder::new();
        for width in widths {
            builder.add_page(*width, 100.0, b"");
        }
        builder.write(&budget, &mut g).expect("write sized source")
    }

    /// A numbered PDF assembled with a valid xref table.
    fn numbered_source(numbered: &[(u32, &[u8])]) -> Vec<u8> {
        let max_num = numbered.iter().map(|(n, _)| *n).max().unwrap_or(1);
        let size = usize::try_from(max_num.saturating_add(1)).unwrap();
        let by_num: std::collections::HashMap<u32, &[u8]> = numbered.iter().copied().collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets: Vec<Option<u64>> = vec![None; size];
        for num in 1..=max_num {
            let Some(body) = by_num.get(&num) else {
                continue;
            };
            offsets[usize::try_from(num).unwrap()] = Some(u64::try_from(out.len()).unwrap_or(0));
            out.extend_from_slice(body);
        }
        let xref_at = u64::try_from(out.len()).unwrap_or(0);
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for off in offsets.iter().skip(1) {
            match off {
                Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n").as_bytes(),
        );
        out.extend_from_slice(format!("{xref_at}\n").as_bytes());
        out.extend_from_slice(b"%%EOF\n");
        out
    }

    /// Five pages with a front matter page and three top-level bookmarks:
    /// an explicit array (page 1), a /GoTo action (page 3), and a named
    /// destination (page 2), plus a nested item that must not create a
    /// boundary (page 0).
    fn bookmarked_source() -> Vec<u8> {
        let numbered: &[(u32, &[u8])] = &[
            (
                1,
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /Names 14 0 R >>\nendobj\n",
            ),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R] /Count 5 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 8 0 R >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 8 0 R >>\nendobj\n",
            ),
            (
                5,
                b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 8 0 R >>\nendobj\n",
            ),
            (
                6,
                b"6 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 8 0 R >>\nendobj\n",
            ),
            (
                7,
                b"7 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 8 0 R >>\nendobj\n",
            ),
            (8, b"8 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n"),
            (
                10,
                b"10 0 obj\n<< /Type /Outlines /First 11 0 R /Last 17 0 R /Count 4 >>\nendobj\n",
            ),
            (
                11,
                b"11 0 obj\n<< /Title (Start) /Parent 10 0 R /Next 12 0 R /First 16 0 R /Dest [4 0 R /Fit] >>\nendobj\n",
            ),
            (
                12,
                b"12 0 obj\n<< /Title (Middle) /Parent 10 0 R /Prev 11 0 R /Next 13 0 R /A << /S /GoTo /D [6 0 R /Fit] >> /Dest [3 0 R /Fit] >>\nendobj\n",
            ),
            (
                13,
                b"13 0 obj\n<< /Title (Named) /Parent 10 0 R /Prev 12 0 R /Next 17 0 R /Dest (There) >>\nendobj\n",
            ),
            (14, b"14 0 obj\n<< /Dests 15 0 R >>\nendobj\n"),
            (
                15,
                b"15 0 obj\n<< /Names [(There) [5 0 R /Fit]] >>\nendobj\n",
            ),
            (
                16,
                b"16 0 obj\n<< /Title (Nested) /Parent 11 0 R /Dest [3 0 R /Fit] >>\nendobj\n",
            ),
            (
                17,
                b"17 0 obj\n<< /Title (External) /Parent 10 0 R /Prev 13 0 R /A << /S /URI /URI (https://example.invalid/) >> >>\nendobj\n",
            ),
        ];
        numbered_source(numbered)
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("selis-tools-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn pages_in_file(path: &std::path::Path) -> u64 {
        let bytes = std::fs::read(path).expect("read output");
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("parse");
        let refs = super::page_refs_of(&bytes, &doc, &budget, &mut g).expect("page refs");
        u64::try_from(refs.len()).unwrap()
    }

    /// A three-page document with page labels, one outline item on page 2,
    /// and one annotation on that page. Its middle page is 200×300 and
    /// rotated 90°, so blank-page placement can be checked against a
    /// non-default template.
    fn labeled_outlined_source() -> Vec<u8> {
        let numbered: &[(u32, &[u8])] = &[
            (
                1,
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /PageLabels 12 0 R >>\nendobj\n",
            ),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>\nendobj\n",
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 8 0 R >>\nendobj\n",
            ),
            (
                4,
                b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 8 0 R /Rotate 90 /Annots [13 0 R] >>\nendobj\n",
            ),
            (
                5,
                b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 8 0 R >>\nendobj\n",
            ),
            (8, b"8 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n"),
            (
                10,
                b"10 0 obj\n<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>\nendobj\n",
            ),
            (
                11,
                b"11 0 obj\n<< /Title (Second) /Parent 10 0 R /Dest [4 0 R /Fit] >>\nendobj\n",
            ),
            (12, b"12 0 obj\n<< /Nums [0 << /S /r >> 2 << /S /D >>] >>\nendobj\n"),
            (
                13,
                b"13 0 obj\n<< /Type /Annot /Subtype /Square /Rect [10 10 50 50] >>\nendobj\n",
            ),
        ];
        numbered_source(numbered)
    }

    /// (media box, rotation, whether content streams are present, annotation
    /// count) for every output page, in page-tree order.
    fn page_summaries(path: &std::path::Path) -> Vec<((f64, f64), i64, bool, usize)> {
        let bytes = std::fs::read(path).expect("read output");
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("parse");
        let refs = super::page_refs_of(&bytes, &doc, &budget, &mut g).expect("page refs");
        refs.iter()
            .map(|page_ref| {
                let (media, contents, _, rotate, annots) =
                    super::page_info(&bytes, &doc, *page_ref, &budget, &mut g).expect("page info");
                let annot_count = match annots {
                    Some(Obj::Ref(r)) => {
                        match super::resolve_ref_obj(&bytes, &doc, r, &budget, &mut g)
                            .unwrap_or(Obj::Null)
                        {
                            Obj::Array(items) => items.len(),
                            _ => 0,
                        }
                    }
                    Some(Obj::Array(items)) => items.len(),
                    _ => 0,
                };
                (media, rotate, !contents.is_empty(), annot_count)
            })
            .collect()
    }

    /// The output page index an outline item's `/Dest` points at.
    fn outline_target_page(path: &std::path::Path) -> usize {
        let bytes = std::fs::read(path).expect("read output");
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let sx = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&bytes, sx, &budget, &mut g).expect("parse");
        let page_refs = super::page_refs_of(&bytes, &doc, &budget, &mut g).expect("page refs");
        let resolve = |r: Ref, g: &mut _| {
            selis_pdf_cos::copy::resolve_ref(&bytes, &doc, r, &budget, g).expect("resolve")
        };
        let root_ref = doc
            .revisions()
            .iter()
            .rev()
            .find_map(|rev| rev.root)
            .expect("trailer root");
        let catalog = resolve(root_ref, &mut g);
        let outlines_ref = match dict_entry(pairs_of(&catalog), b"Outlines").expect("/Outlines") {
            Obj::Ref(r) => *r,
            _ => panic!("outline root ref"),
        };
        let outlines = resolve(outlines_ref, &mut g);
        let first_ref = match dict_entry(pairs_of(&outlines), b"First").expect("/First") {
            Obj::Ref(r) => *r,
            _ => panic!("outline item ref"),
        };
        let first = resolve(first_ref, &mut g);
        let dest = dict_entry(pairs_of(&first), b"Dest").expect("/Dest");
        let target = match dest {
            Obj::Array(items) => match items.first() {
                Some(Obj::Ref(r)) => r.num,
                _ => panic!("outline destination page"),
            },
            _ => panic!("outline destination array"),
        };
        page_refs
            .iter()
            .position(|r| r.num == target)
            .expect("outline destination is a page")
    }

    #[test]
    fn duplicate_pages_places_copies_after_originals_and_remaps_structures() {
        let dir = temp_dir("duplicate-pages");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, labeled_outlined_source()).expect("write input");
        let verification =
            super::duplicate_pages(input.to_str().unwrap(), "1,1", output.to_str().unwrap())
                .expect("duplicate");
        assert_eq!(verification.verdict, "ok");
        assert_eq!(verification.pages_in, Some(3));
        assert_eq!(verification.pages_out, 4);
        assert_eq!(verification.annotations_in, Some(1));
        assert_eq!(verification.annotations_out, 2);
        assert_eq!(verification.outline_entries_out, 1);
        assert!(verification.checked.contains(&"annotations"));
        assert_eq!(pages_in_file(&output), 4);

        let pages = page_summaries(&output);
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0].0, (100.0, 100.0));
        assert_eq!(pages[1].0, (200.0, 300.0));
        assert_eq!(pages[1].1, 90);
        assert_eq!(pages[2].0, (200.0, 300.0));
        assert_eq!(pages[2].1, 90);
        assert_eq!(pages[3].0, (100.0, 100.0));
        assert_eq!(pages[1].3, 1);
        assert_eq!(pages[2].3, 1);
        assert_eq!(
            outline_target_page(&output),
            1,
            "the bookmark targets the first copy"
        );

        let out_bytes = std::fs::read(&output).expect("read output");
        let text = String::from_utf8_lossy(&out_bytes);
        assert!(
            text.contains("[0 <</S /r>> 3 <</S /D>>]"),
            "labels re-key around the duplicate: {text}"
        );
    }

    #[test]
    fn insert_blank_pages_uses_the_boundary_template_and_remaps_structures() {
        let dir = temp_dir("insert-blank-middle");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, labeled_outlined_source()).expect("write input");
        let verification = super::insert_blank_pages(
            input.to_str().unwrap(),
            Some(2),
            2,
            output.to_str().unwrap(),
        )
        .expect("insert blanks");
        assert_eq!(verification.verdict, "ok");
        assert_eq!(verification.pages_in, Some(3));
        assert_eq!(verification.pages_out, 5);
        assert_eq!(verification.annotations_out, 1);
        assert_eq!(pages_in_file(&output), 5);

        let pages = page_summaries(&output);
        assert_eq!(pages.len(), 5);
        assert_eq!(pages[0].0, (100.0, 100.0));
        assert_eq!(pages[1].0, (200.0, 300.0));
        assert_eq!(pages[1].1, 90);
        assert!(pages[1].2, "the original page keeps its content");
        assert_eq!(pages[1].3, 1);
        assert_eq!(pages[2].0, (200.0, 300.0));
        assert_eq!(pages[2].1, 90);
        assert!(!pages[2].2, "inserted page has no content");
        assert_eq!(pages[3].0, (200.0, 300.0));
        assert_eq!(pages[3].1, 90);
        assert!(!pages[3].2, "second inserted page has no content");
        assert_eq!(pages[4].0, (100.0, 100.0));
        assert_eq!(
            outline_target_page(&output),
            1,
            "the bookmark stays on its original page before the insertion"
        );

        let out_bytes = std::fs::read(&output).expect("read output");
        let text = String::from_utf8_lossy(&out_bytes);
        assert!(
            text.contains("[0 <</S /r>> 4 <</S /D>>]"),
            "labels re-key after inserted pages: {text}"
        );
    }

    #[test]
    fn insert_blank_pages_supports_start_and_append_boundaries() {
        let dir = temp_dir("insert-blank-ends");
        let input = dir.join("in.pdf");
        std::fs::write(&input, labeled_outlined_source()).expect("write input");

        let start = dir.join("start.pdf");
        let verification =
            super::insert_blank_pages(input.to_str().unwrap(), Some(0), 1, start.to_str().unwrap())
                .expect("insert at start");
        assert_eq!(verification.pages_out, 4);
        let pages = page_summaries(&start);
        assert_eq!(pages[0].0, (100.0, 100.0));
        assert!(!pages[0].2);
        assert_eq!(pages[1].0, (100.0, 100.0));
        assert_eq!(pages[3].0, (100.0, 100.0));
        assert_eq!(outline_target_page(&start), 2);

        let end = dir.join("end.pdf");
        let verification =
            super::insert_blank_pages(input.to_str().unwrap(), None, 1, end.to_str().unwrap())
                .expect("append blank");
        assert_eq!(verification.pages_out, 4);
        let pages = page_summaries(&end);
        assert_eq!(pages[3].0, (100.0, 100.0));
        assert!(!pages[3].2);
        assert_eq!(outline_target_page(&end), 1);
    }

    #[test]
    fn insert_blank_pages_rejects_invalid_counts_and_boundaries() {
        let dir = temp_dir("insert-blank-invalid");
        let input = dir.join("in.pdf");
        std::fs::write(&input, labeled_outlined_source()).expect("write input");
        let output = dir.join("out.pdf");
        let path = input.to_str().unwrap();
        let out = output.to_str().unwrap();

        let error = super::insert_blank_pages(path, Some(1), 0, out).expect_err("zero count");
        assert!(error.0.contains("at least one"));
        let error = super::insert_blank_pages(path, Some(1), crate::MAX_BLANK_PAGES + 1, out)
            .expect_err("too many blanks");
        assert!(error.0.contains("at most"));
        let error = super::insert_blank_pages(path, Some(4), 1, out).expect_err("bad boundary");
        assert!(error.0.contains("out of range"));
    }

    #[test]
    fn duplicate_pages_preserves_forms_and_keeps_one_tagged_parent() {
        let dir = temp_dir("duplicate-forms");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, formed_tagged_source()).expect("write input");
        let verification =
            super::duplicate_pages(input.to_str().unwrap(), "0", output.to_str().unwrap())
                .expect("duplicate tagged page");
        assert_eq!(verification.verdict, "ok");
        assert_eq!(verification.pages_out, 2);
        assert_eq!(verification.fields_in, Some(1));
        assert_eq!(verification.fields_out, 1);
        assert_eq!(verification.ocgs_out, 1);
        assert_eq!(pages_in_file(&output), 2);
        let out_bytes = std::fs::read(&output).expect("read output");
        let text = String::from_utf8_lossy(&out_bytes);
        assert_eq!(text.matches("/Type /StructTreeRoot").count(), 1);
        assert_eq!(
            text.matches("/StructParents").count(),
            1,
            "the duplicate does not reuse the first copy's parent-tree id: {text}"
        );
    }

    #[test]
    fn split_every_groups_pages_and_verifies_each_part() {
        let dir = temp_dir("split-every");
        let input = dir.join("in.pdf");
        std::fs::write(&input, multipage_source(5)).expect("write input");
        let prefix = format!("{}\\part-", dir.display());
        let parts = super::split_every(input.to_str().unwrap(), 2, &prefix).expect("split every");
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts
                .iter()
                .map(|part| (part.first, part.last))
                .collect::<Vec<_>>(),
            vec![(0, 1), (2, 3), (4, 4)]
        );
        assert_eq!(pages_in_file(std::path::Path::new(&parts[0].output)), 2);
        assert_eq!(pages_in_file(std::path::Path::new(&parts[1].output)), 2);
        assert_eq!(pages_in_file(std::path::Path::new(&parts[2].output)), 1);
        for part in &parts {
            assert_eq!(part.verification.verdict, "ok");
            assert_eq!(part.verification.pages_in, Some(5));
            assert_eq!(
                part.verification.pages_out,
                u64::try_from(part.last - part.first + 1).unwrap()
            );
        }
    }

    #[test]
    fn split_half_uses_the_middle_page() {
        let dir = temp_dir("split-half");
        let input = dir.join("in.pdf");
        std::fs::write(&input, multipage_source(5)).expect("write input");
        let prefix = format!("{}\\half-", dir.display());
        let parts = super::split_half(input.to_str().unwrap(), &prefix).expect("split half");
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].first, parts[0].last), (0, 1));
        assert_eq!((parts[1].first, parts[1].last), (2, 4));
        assert_eq!(pages_in_file(std::path::Path::new(&parts[0].output)), 2);
        assert_eq!(pages_in_file(std::path::Path::new(&parts[1].output)), 3);
    }

    #[test]
    fn extract_pages_deduplicates_and_keeps_document_order() {
        let dir = temp_dir("extract-pages");
        let input = dir.join("in.pdf");
        std::fs::write(&input, multipage_source(6)).expect("write input");
        let output = dir.join("extracted.pdf");
        let verification =
            super::extract_pages(input.to_str().unwrap(), "5,2-3,2", output.to_str().unwrap())
                .expect("extract");
        assert_eq!(verification.pages_in, Some(6));
        assert_eq!(verification.pages_out, 3);
        assert_eq!(pages_in_file(&output), 3);
    }

    #[test]
    fn split_by_size_uses_exact_measured_group_boundaries() {
        let dir = temp_dir("split-size-groups");
        let input = dir.join("in.pdf");
        std::fs::write(&input, multipage_source(4)).expect("write input");
        let (src, doc, page_refs, budget) =
            super::open_for_split(input.to_str().unwrap()).expect("open input");
        let clock = crate::shell_clock();
        let mut g = crate::runtime::cli_guard(&budget, &clock);
        let single = super::measure_selection(
            input.to_str().unwrap(),
            &src,
            &doc,
            &page_refs,
            &[0],
            &budget,
            &mut g,
        )
        .expect("measure single");
        let pair = super::measure_selection(
            input.to_str().unwrap(),
            &src,
            &doc,
            &page_refs,
            &[0, 1],
            &budget,
            &mut g,
        )
        .expect("measure pair");
        assert!(single < pair, "adding a page must add bytes");
        drop(g);

        let prefix = format!("{}\\size-", dir.display());
        let parts =
            super::split_by_size(input.to_str().unwrap(), pair, &prefix).expect("split by size");
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts
                .iter()
                .map(|part| (part.first, part.last))
                .collect::<Vec<_>>(),
            vec![(0, 1), (2, 3)]
        );
        for part in &parts {
            assert!(!part.over_target);
            assert!(part.verification.bytes_out <= pair);
            assert_eq!(part.verification.verdict, "ok");
            assert_eq!(pages_in_file(std::path::Path::new(&part.output)), 2);
        }
    }

    #[test]
    fn split_by_size_flags_a_single_page_that_cannot_fit() {
        let dir = temp_dir("split-size-oversize");
        let input = dir.join("in.pdf");
        std::fs::write(&input, multipage_source(3)).expect("write input");
        let (src, doc, page_refs, budget) =
            super::open_for_split(input.to_str().unwrap()).expect("open input");
        let clock = crate::shell_clock();
        let mut g = crate::runtime::cli_guard(&budget, &clock);
        let single = super::measure_selection(
            input.to_str().unwrap(),
            &src,
            &doc,
            &page_refs,
            &[0],
            &budget,
            &mut g,
        )
        .expect("measure single");
        drop(g);
        let target = single.saturating_sub(1);
        assert!(target > 0);

        let prefix = format!("{}\\oversize-", dir.display());
        let parts =
            super::split_by_size(input.to_str().unwrap(), target, &prefix).expect("split by size");
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert!(part.over_target, "a one-page part above target is flagged");
            assert!(part.verification.bytes_out > target);
            assert_eq!(part.first, part.last);
        }
        assert!(super::split_by_size(input.to_str().unwrap(), 0, &prefix).is_err());
    }

    #[test]
    fn split_by_size_refuses_more_than_the_part_limit() {
        let dir = temp_dir("split-size-limit");
        let input = dir.join("in.pdf");
        std::fs::write(&input, multipage_source(65)).expect("write input");
        let error = super::split_by_size(input.to_str().unwrap(), 1, "")
            .expect_err("65 one-page parts exceed the cap");
        assert!(error.0.contains("64"), "{error:?}");
    }

    #[test]
    fn split_by_bookmarks_uses_top_level_targets_and_front_matter() {
        let dir = temp_dir("split-bookmarks");
        let input = dir.join("in.pdf");
        std::fs::write(&input, bookmarked_source()).expect("write input");
        let prefix = format!("{}\\bookmarks-", dir.display());
        let parts =
            super::split_by_bookmarks(input.to_str().unwrap(), &prefix).expect("split bookmarks");
        assert_eq!(
            parts
                .iter()
                .map(|part| (part.first, part.last))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 1), (2, 2), (3, 4)]
        );
        assert_eq!(
            parts
                .iter()
                .map(|part| pages_in_file(std::path::Path::new(&part.output)))
                .sum::<u64>(),
            5
        );
        for part in &parts {
            assert_eq!(part.verification.verdict, "ok");
            assert!(!part.over_target);
        }
    }

    #[test]
    fn split_by_bookmarks_refuses_absent_or_single_part_outlines() {
        let dir = temp_dir("split-bookmarks-errors");
        let input = dir.join("in.pdf");
        std::fs::write(&input, multipage_source(3)).expect("write input");
        let error = super::split_by_bookmarks(input.to_str().unwrap(), "")
            .expect_err("a document without bookmarks cannot be split by them");
        assert!(error.0.contains("no bookmarks"), "{error:?}");
        assert!(super::bookmark_ranges(&[0], 3).is_err());
        assert_eq!(
            super::bookmark_ranges(&[1], 3).expect("front matter and one section"),
            vec![(0, 0), (1, 2)]
        );
    }

    #[test]
    fn split_sentinel_keeps_the_final_page_and_rejects_bad_ranges() {
        let dir = temp_dir("split-sentinel");
        let input = dir.join("in.pdf");
        std::fs::write(&input, multipage_source(5)).expect("write input");
        let output = dir.join("tail.pdf");
        let verification = super::split(
            input.to_str().unwrap(),
            2,
            usize::MAX,
            output.to_str().unwrap(),
        )
        .expect("split to the end");
        assert_eq!(verification.pages_out, 3);
        assert_eq!(pages_in_file(&output), 3);

        let error = super::split(input.to_str().unwrap(), 3, 2, output.to_str().unwrap())
            .expect_err("reversed range");
        assert!(error.0.contains("out of range"), "{error:?}");
        assert!(super::split_every(input.to_str().unwrap(), 0, "").is_err());
        let single = dir.join("single.pdf");
        std::fs::write(&single, multipage_source(1)).expect("write single-page input");
        assert!(super::split_half(single.to_str().unwrap(), "").is_err());
    }
}
