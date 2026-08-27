//! The `selis` document tools: merge, split, set-metadata, redact, and the
//! page operations (rotate, delete, reorder — SL-1A.TOOL.03).
//!
//! Built on the full-document writer + object-graph copy (WRITE.01/03/04).

#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use selis_pdf_cos::{self, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::{read_file, CliError, CliResult};

/// Merge several PDFs into one (SL-1A.TOOL.01).
pub(crate) fn merge(inputs: &[String], output: &str) -> CliResult<()> {
    if inputs.len() < 2 {
        return Err(CliError("merge needs at least two input files".to_string()));
    }
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let mut merged = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;

    for path in inputs {
        let src = read_file(path)?;
        let page_refs =
            open_page_refs(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
        for page_ref in page_refs {
            copy_page(&mut merged, &src, page_ref, &budget, &mut g, &mut next_num)
                .map_err(|e| CliError(format!("{path}: {e}")))?;
        }
    }

    // WRITE.03: identical resources across inputs collapse to one object.
    let deduplicated = merged.dedup(&budget, &mut g);
    let bytes = merged
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    eprintln!(
        "merged {} file(s) into {output} ({} duplicate object(s) removed)",
        inputs.len(),
        deduplicated
    );
    Ok(())
}

/// Split a PDF: extract the pages in `page_range` (inclusive, 0-based) into a
/// new file (SL-1A.TOOL.02).
pub(crate) fn split(path: &str, first: usize, last: usize, output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let page_refs =
        open_page_refs(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    if last >= page_refs.len() || first > last {
        return Err(CliError(format!(
            "page range {first}..{last} out of range (document has {} pages)",
            page_refs.len()
        )));
    }
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for page_ref in page_refs[first..=last].iter() {
        copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
    }
    let bytes = out
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    eprintln!("split pages {first}..{last} into {output}");
    Ok(())
}

/// Set metadata fields (Info dictionary) and rewrite the document
/// (SL-1A.TOOL.09).
pub(crate) fn set_metadata(path: &str, fields: &[(&str, &str)], output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let page_refs =
        open_page_refs(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for (k, v) in fields {
        out.set_info(k.as_bytes(), v);
    }
    for page_ref in &page_refs {
        copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
    }
    let bytes = out
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    eprintln!("set {} metadata field(s) -> {output}", fields.len());
    Ok(())
}

/// Redact regions: cover them with black and strip text that falls within
/// them (SL-0.LEGAL.05).
pub(crate) fn redact(path: &str, rects: &[(f64, f64, f64, f64)], output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let page_refs =
        open_page_refs(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for page_ref in &page_refs {
        copy_page_redacted(
            &mut out,
            &src,
            *page_ref,
            rects,
            &budget,
            &mut g,
            &mut next_num,
        )
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    }
    let bytes = out
        .write(&budget, &mut g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
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
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| "no revisions".to_string())?;
    let catalog = selis_pdf_cos::copy::resolve_ref(
        src,
        &doc,
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
    walk_pages(src, &doc, pages_ref, budget, g, 0)
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
/// optional extra page-dictionary entries (e.g. `/Rotate`).
fn copy_page(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    _next_num: &mut u32,
) -> Result<(), String> {
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
) -> Result<(), String> {
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
    merged.add_page_with_extra(media.0, media.1, &new_contents, new_resources, extra);
    Ok(())
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
/// falls within them.
fn copy_page_redacted(
    merged: &mut selis_pdf_cos::doc_writer::DocumentBuilder,
    src: &[u8],
    page_ref: Ref,
    rects: &[(f64, f64, f64, f64)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    next_num: &mut u32,
) -> Result<(), String> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| format!("cannot open: {e}"))?;
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| "no revisions".to_string())?;
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
    merged.add_page_with_extra(
        media.0,
        media.1,
        &[Ref::new(content_num, 0)],
        new_resources,
        extra,
    );
    Ok(())
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
    let page_refs =
        open_page_refs(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let selected = match pages {
        Some(spec) => parse_page_selection(spec, page_refs.len())
            .map_err(|e| CliError(format!("{path}: {e}")))?,
        None => (0..page_refs.len()).collect(),
    };
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for (idx, page_ref) in page_refs.iter().enumerate() {
        let existing = page_rotate(&src, &budget, &mut g, *page_ref)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        let extra = if selected.contains(&idx) {
            rotate_extra(existing.saturating_add(norm))
        } else {
            rotate_extra(existing)
        };
        copy_page_extra(
            &mut out,
            &src,
            *page_ref,
            &budget,
            &mut g,
            &mut next_num,
            extra,
        )
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    }
    write_document(out, output, &budget, &mut g)?;
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
    let page_refs =
        open_page_refs(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
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
    for (idx, page_ref) in page_refs.iter().enumerate() {
        if removed.contains(&idx) {
            continue;
        }
        copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept += 1;
    }
    write_document(out, output, &budget, &mut g)?;
    eprintln!("deleted {} page(s), kept {kept} -> {output}", removed.len());
    Ok(())
}

/// Reorder the pages of a PDF (SL-1A.TOOL.03). `order` is a permutation such
/// as `2,0,1` or `3-5,0-2`.
pub(crate) fn reorder(path: &str, order: &str, output: &str) -> CliResult<()> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let page_refs =
        open_page_refs(&src, &budget, &mut g).map_err(|e| CliError(format!("{path}: {e}")))?;
    let order =
        parse_page_order(order, page_refs.len()).map_err(|e| CliError(format!("{path}: {e}")))?;
    let mut out = selis_pdf_cos::doc_writer::DocumentBuilder::new();
    let mut next_num = 3u32;
    for idx in &order {
        let page_ref = page_refs
            .get(*idx)
            .copied()
            .ok_or_else(|| CliError(format!("page {idx} out of range")))?;
        copy_page(&mut out, &src, page_ref, &budget, &mut g, &mut next_num)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
    }
    write_document(out, output, &budget, &mut g)?;
    eprintln!("reordered {} page(s) -> {output}", order.len());
    Ok(())
}

/// Serialise and write a built document to `output`.
fn write_document(
    mut builder: selis_pdf_cos::doc_writer::DocumentBuilder,
    output: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<()> {
    let bytes = builder
        .write(budget, g)
        .map_err(|e| CliError(format!("write failed: {e}")))?;
    std::fs::write(output, &bytes).map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    Ok(())
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
        assert!(text.contains("/Annots"), "annotations carried into the merge");
        // The output parses and the annotation subgraph resolved (two
        // annotations — one per page).
        assert_eq!(text.matches("/Type /Annot").count(), 2);
    }
}
