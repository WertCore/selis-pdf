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

use crate::write_gate::{self, Preserved, PRESERVE_ALL, PRESERVE_EXCEPT_PAGES};
use crate::{read_file, CliError, CliResult};

/// A parsed merge input with its source-page → merged-page mapping.
struct MergeInput {
    src: Vec<u8>,
    doc: selis_pdf_cos::Doc,
    page_map: std::collections::HashMap<u32, Ref>,
    pages: usize,
}

/// Merge several PDFs into one (SL-1A.TOOL.01 + SL-1A.WRITE.04: outlines,
/// named destinations, page labels, and embedded files survive and are
/// reconciled across the inputs).
pub(crate) fn merge(inputs: &[String], output: &str) -> CliResult<()> {
    if inputs.len() < 2 {
        return Err(CliError("merge needs at least two input files".to_string()));
    }
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let mut merged = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;

    let mut parsed: Vec<MergeInput> = Vec::with_capacity(inputs.len());
    for path in inputs {
        let src = read_file(path)?;
        let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
        let page_refs =
            open_page_refs(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
        let mut page_map = std::collections::HashMap::new();
        for page_ref in &page_refs {
            let merged_ref =
                copy_page(&mut merged, &src, *page_ref, &budget, &mut g, &mut next_num)
                    .map_err(|e| CliError(format!("{path}: {e}")))?;
            page_map.insert(page_ref.num, merged_ref);
        }
        parsed.push(MergeInput {
            src,
            doc,
            page_map,
            pages: page_refs.len(),
        });
    }

    // WRITE.04: reconcile document-level structures across the inputs.
    reconcile_inputs(&mut merged, &parsed, &budget, &mut g).map_err(CliError)?;

    // WRITE.03: identical resources across inputs collapse to one object.
    let deduplicated = merged.dedup(&budget, &mut g);
    let bytes = merged
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    // WRITE.05: structural verification before the output reaches disk.
    // Merge copies every input's pages, so the expectation is the sum of the
    // inputs' pages; other counts follow each input's survey.
    let mut expected_pages = 0u64;
    for input in &parsed {
        let observed = selis_pdf_cos::verify::survey(&input.src, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: {e}", path = inputs[0], e = e)))?;
        expected_pages = expected_pages.saturating_add(observed.pages);
    }
    write_gate::write_verified(
        &bytes,
        output,
        &selis_pdf_cos::verify::Expectations {
            pages: Some(expected_pages),
            ..selis_pdf_cos::verify::Expectations::none()
        },
        &budget,
        &mut g,
    )?;
    eprintln!(
        "merged {} file(s) into {output} ({} duplicate object(s) removed)",
        inputs.len(),
        deduplicated
    );
    Ok(())
}

/// Reconcile the merged document's catalog-level structures (WRITE.04):
/// outlines (bookmarks), page labels, named destinations, embedded files,
/// form fields, the structure tree (ADR-P0031 — a merged document stays
/// tagged), optional-content groups, and the trailer `/ID`. Each input's
/// page references are redirected at the merged pages.
fn reconcile_inputs(
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

        // Page labels: shift each input's keys by the pages merged before it.
        if let Some(labels) = rec::catalog_entry(&input.src, &input.doc, b"PageLabels", budget, g)
            .map_err(|e| e.to_string())?
        {
            if let Ok(pairs) = rec::number_tree_pairs(&labels, &input.src, &input.doc, budget, g) {
                if !pairs.is_empty() {
                    label_trees.push((pairs, page_offset));
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
fn reconcile_single_input(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    all_pages: &[Ref],
    kept: &[(usize, u32, Ref)],
    carry_metadata: bool,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    use selis_pdf_cos::reconcile as rec;

    let page_map: std::collections::HashMap<u32, Ref> =
        kept.iter().map(|(_, old, new)| (*old, *new)).collect();
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

    // Page labels: re-keyed at the surviving pages' new indices.
    let mut new_idx_by_old: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for (new_idx, (old_idx, _, _)) in kept.iter().enumerate() {
        new_idx_by_old.insert(*old_idx, new_idx);
    }
    if let Some(labels) =
        rec::catalog_entry(src, doc, b"PageLabels", budget, g).map_err(|e| e.to_string())?
    {
        let labels = resolve_shallow(src, doc, labels, budget, g);
        if let Ok(pairs) = rec::number_tree_pairs(&labels, src, doc, budget, g) {
            let remapped = remap_page_label_keys(&pairs, &new_idx_by_old, all_pages.len());
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
                    if let Ok(found) = rec::name_tree_pairs(&dests, src, doc, budget, g) {
                        pairs.extend(found);
                    }
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

    /// The page object number a named destination targets, when decidable.
    fn page_of(&self, name: &[u8]) -> Option<u32> {
        let (_, value) = self.pairs.iter().find(|(k, _)| k.as_slice() == name)?;
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

/// Carry the input's outlines into `out`, dropping items whose destination
/// page was deleted (WRITE.07). Chain links (`First`/`Last`/`Next`/`Prev`/
/// `Parent`/`Count`) are rebuilt over the kept items; every other entry of
/// a kept item is copied through `copy_value_into`, so destinations remap
/// at the surviving pages.
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
fn carry_struct_tree_single(
    out: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    kept: &[(usize, u32, Ref)],
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
    let mut kept_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for (_, old_num, new_ref) in kept {
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
    if let Some((_, obj)) = merged.objects_mut().iter_mut().find(|(n, _)| *n == num) {
        if let Obj::Dict(pairs) = obj {
            let key_b = selis_bytes::Bytes::copy_from_slice(key);
            if let Some(slot) = pairs.iter_mut().find(|(k, _)| k.as_slice() == key) {
                slot.1 = value;
            } else {
                pairs.push((key_b, value));
            }
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

/// Split a PDF: extract the pages in `page_range` (inclusive, 0-based) into a
/// new file (SL-1A.TOOL.02).
pub(crate) fn split(path: &str, first: usize, last: usize, output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    if last >= page_refs.len() || first > last {
        return Err(CliError(format!(
            "page range {first}..{last} out of range (document has {} pages)",
            page_refs.len()
        )));
    }
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<(usize, u32, Ref)> = Vec::new();
    for (idx, page_ref) in page_refs.iter().enumerate() {
        if idx < first || idx > last {
            continue;
        }
        let merged_ref = copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept.push((idx, page_ref.num, merged_ref));
    }
    // WRITE.07: carry the input's catalog-level structures through the page
    // selection (structure tree, output intents, metadata, …), pruning what
    // only referenced the deleted pages.
    reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, true, &budget, &mut g,
    )
    .map_err(CliError)?;
    let expected_pages = u64::try_from(kept.len()).unwrap_or(u64::MAX);
    let kept_annots = annots_on_pages(
        &src,
        &doc,
        &kept.iter().map(|(_, _, r)| *r).collect::<Vec<_>>(),
        &budget,
        &mut g,
    );
    write_document(
        out,
        output,
        Some(&src),
        PRESERVE_EXCEPT_PAGES,
        Some(expected_pages),
        Some(kept_annots),
        &budget,
        &mut g,
    )?;
    eprintln!("split pages {first}..{last} into {output}");
    Ok(())
}

/// Set metadata fields (Info dictionary) and rewrite the document
/// (SL-1A.TOOL.09).
pub(crate) fn set_metadata(path: &str, fields: &[(&str, &str)], output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
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
    write_document(
        out,
        output,
        Some(&src),
        PRESERVE_ALL,
        Some(u64::try_from(kept.len()).unwrap_or(u64::MAX)),
        None,
        &budget,
        &mut g,
    )?;
    eprintln!("set {} metadata field(s) -> {output}", fields.len());
    Ok(())
}

/// Redact regions: cover them with black and strip text that falls within
/// them (SL-0.LEGAL.05).
pub(crate) fn redact(path: &str, rects: &[(f64, f64, f64, f64)], output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
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
    write_document(
        out,
        output,
        Some(&src),
        PRESERVE_ALL,
        Some(u64::try_from(kept.len()).unwrap_or(u64::MAX)),
        None,
        &budget,
        &mut g,
    )?;
    eprintln!("redacted {} region(s) -> {output}", rects.len());
    Ok(())
}

/// Open a document and return its leaf page references.
fn open_page_refs(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Ref>, String> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    page_refs_of(src, &doc, budget, g)
}

/// The leaf page references of an already-parsed document.
fn page_refs_of(
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
fn copy_page(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    _next_num: &mut u32,
) -> Result<Ref, String> {
    copy_page_extra(merged, src, page_ref, budget, g, _next_num, Vec::new())
}

/// Copy a leaf page with extra page-dictionary entries appended.
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
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| "no revisions".to_string())?;
    let _ = rev;
    let (media, content_refs, resources, _rotate, annots) =
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
    // Carry the page's annotations (WRITE.03 DoD) alongside the caller's extra
    // page-dictionary entries.
    let mut extra = extra;
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
fn page_info(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<((f64, f64), Vec<Ref>, Option<Obj>, i64, Option<Obj>), String> {
    let page = resolve_ref_obj(src, doc, page_ref, budget, g)?;
    let Obj::Dict(pairs) = &page else {
        return Err("page is not a dict".to_string());
    };
    let media = match pairs.iter().find(|(k, _)| k.as_slice() == b"MediaBox") {
        Some((_, Obj::Array(items))) if items.len() >= 4 => {
            let n = |i: usize| match items.get(i) {
                Some(Obj::Int(v)) => Some(*v as f64),
                Some(Obj::Real { scaled, scale }) => {
                    Some(*scaled as f64 / 10f64.powi(*scale as i32))
                }
                _ => None,
            };
            (
                n(2).unwrap_or(0.0).max(n(0).unwrap_or(0.0)),
                n(3).unwrap_or(0.0).max(n(1).unwrap_or(0.0)),
            )
        }
        _ => (612.0, 792.0),
    };
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
    let resources = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Resources")
        .map(|(_, v)| v.clone());
    let annots = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Annots")
        .map(|(_, v)| v.clone());
    let rotate = pairs
        .iter()
        .find(|(k, _)| k.as_slice() == b"Rotate")
        .and_then(|(_, v)| match v {
            Obj::Int(v) => Some(*v),
            _ => None,
        })
        .unwrap_or(0);
    Ok((media, contents, resources, rotate, annots))
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
/// `/Rotate` (mod 360).
pub(crate) fn rotate(path: &str, angle: i64, pages: Option<&str>, output: &str) -> CliResult<()> {
    let norm = angle.rem_euclid(360);
    if norm != 90 && norm != 180 && norm != 270 {
        return Err(CliError(format!(
            "rotation angle must be 90, 180 or 270 (got {angle})"
        )));
    }
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
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
    write_document(
        out,
        output,
        Some(&src),
        PRESERVE_ALL,
        Some(u64::try_from(kept.len()).unwrap_or(u64::MAX)),
        None,
        &budget,
        &mut g,
    )?;
    eprintln!(
        "rotated {} page(s) by {}° -> {output}",
        selected.len(),
        norm
    );
    Ok(())
}

/// Delete pages from a PDF (SL-1A.TOOL.03). `pages` is a `0,2,5-7` selection.
pub(crate) fn delete(path: &str, pages: &str, output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
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
    let kept_annots = annots_on_pages(
        &src,
        &doc,
        &kept_pages.iter().map(|(_, _, r)| *r).collect::<Vec<_>>(),
        &budget,
        &mut g,
    );
    write_document(
        out,
        output,
        Some(&src),
        PRESERVE_EXCEPT_PAGES,
        Some(u64::try_from(kept_pages.len()).unwrap_or(u64::MAX)),
        Some(kept_annots),
        &budget,
        &mut g,
    )?;
    eprintln!("deleted {} page(s), kept {kept} -> {output}", removed.len());
    Ok(())
}

/// Reorder the pages of a PDF (SL-1A.TOOL.03). `order` is a permutation such
/// as `2,0,1` or `3-5,0-2`.
pub(crate) fn reorder(path: &str, order: &str, output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    let page_refs =
        page_refs_of(&src, &doc, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let order =
        parse_page_order(order, page_refs.len()).map_err(|e| CliError(format!("{path}: {e}")))?;
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<(usize, u32, Ref)> = Vec::new();
    for idx in &order {
        let page_ref = page_refs
            .get(*idx)
            .copied()
            .ok_or_else(|| CliError(format!("page {idx} out of range")))?;
        let merged_ref = copy_page(&mut out, &src, page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept.push((*idx, page_ref.num, merged_ref));
    }
    // WRITE.07: catalog-level structures carry through the rewrite; page
    // labels re-key at the new indices, destinations remap at the pages.
    reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, true, &budget, &mut g,
    )
    .map_err(CliError)?;
    write_document(
        out,
        output,
        Some(&src),
        PRESERVE_ALL,
        Some(u64::try_from(order.len()).unwrap_or(u64::MAX)),
        None,
        &budget,
        &mut g,
    )?;
    eprintln!("reordered {} page(s) -> {output}", order.len());
    Ok(())
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
/// verification and an atomic commit. `preserved` selects which counts the
/// operation promises to keep from the input's survey; `expected_pages` is
/// asserted directly when the caller knows the number (split, reorder);
/// `annotations_override` replaces the input-total annotation expectation
/// when only a subset of pages survives (split, delete).
fn write_document(
    builder: selis_pdf_cos::doc_writer::DocumentBuilder,
    output: &str,
    input: Option<&[u8]>,
    preserved: Preserved,
    expected_pages: Option<u64>,
    annotations_override: Option<u64>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<()> {
    let mut builder = builder;
    let bytes = builder
        .write(budget, g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    let mut expected = match input {
        Some(src) => {
            let observed = selis_pdf_cos::verify::survey(src, budget, g)
                .map_err(|e| CliError(format!("input survey failed: {e}")))?;
            write_gate::expectations_from(&observed, &preserved)
        }
        None => selis_pdf_cos::verify::Expectations::none(),
    };
    if let Some(pages) = expected_pages {
        expected.pages = Some(pages);
    }
    if let Some(annots) = annotations_override {
        expected.annotations = Some(annots);
    }
    write_gate::write_verified(&bytes, output, &expected, budget, g)
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
    let mut g = budget.guard();
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

    use super::{Obj, Ref};

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
}
