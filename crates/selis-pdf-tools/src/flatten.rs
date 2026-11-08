//! Flatten annotations: bake an annotation's appearance stream into the page
//! content, so the mark becomes part of the page a renderer draws rather than a
//! separate object a viewer may or may not show.
//!
//! Only non-widget annotations that carry a normal appearance (`/AP /N`) are
//! baked; widget annotations (form fields) are left for the form-flatten
//! operation, and an annotation with no appearance is kept as it was rather than
//! dropped. The appearance is placed by the ISO 32000-1 §12.5.5 algorithm: its
//! `/BBox` is transformed by its `/Matrix`, and the result is mapped into the
//! annotation's `/Rect`, so the baked mark lands exactly where the viewer drew
//! it. Encrypted inputs are refused; remove the password with unlock first.
//!
//! "Never claim flatten without a render check": the structural test here proves
//! the appearance's own drawing is carried into the output and the annotation is
//! gone from `/Annots`, and the CLI render check (in `apps/cli`) renders the
//! output and reads the baked mark back from the pixels.

use selis_pdf_cos::copy::collect_objects_with_doc;
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::fingerprint::{find_name, resolve_inline};
use crate::write_gate::Preserved;
use crate::{read_file, CliError, ToolResult, Verification};

/// The counts a flatten preserves: everything except the annotation count,
/// which drops as marks move from `/Annots` into the page content.
const PRESERVE_EXCEPT_ANNOTATIONS: Preserved = Preserved {
    pages: true,
    annotations: false,
    fields: true,
    ocgs: true,
    outlines: true,
    embedded_files: true,
};

/// The `/F` flag bit that hides an annotation (ISO 32000-1 Table 165).
const ANNOT_FLAG_HIDDEN: i64 = 1 << 1;

/// Bake every eligible annotation's appearance into the page content and write
/// the result to `output`.
///
/// # Errors
///
/// A typed [`CliError`] when the input cannot be read or parsed, the document is
/// encrypted, or the output fails its structural verification.
pub fn flatten_annotations(path: &str, output: &str) -> ToolResult<Verification> {
    let src = read_file(path)?;
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    crate::tools::refuse_encrypted_input(path, &doc)?;
    let page_refs = crate::tools::page_refs_of(&src, &doc, &budget, &mut g).map_err(CliError)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);

    let mut out = DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<crate::tools::KeptPage> = Vec::with_capacity(page_refs.len());

    for (page_index, page_ref) in page_refs.iter().enumerate() {
        let (_, _, _, _, annots) = crate::tools::page_info(&src, &doc, *page_ref, &budget, &mut g)
            .map_err(|e| CliError(format!("{path}: {e}")))?;
        let merged_ref = crate::tools::copy_page_without_annots(
            &mut out,
            &src,
            *page_ref,
            &budget,
            &mut g,
            &mut next_num,
        )
        .map_err(|e| CliError(format!("{path}: {e}")))?;

        let mut keep: Vec<Ref> = Vec::new();
        for (local, annot_ref) in annotation_refs(&src, &doc, annots, &budget, &mut g)
            .into_iter()
            .enumerate()
        {
            let Obj::Dict(annot) = resolve_inline(&src, &doc, Obj::Ref(annot_ref), &budget, &mut g)
            else {
                continue;
            };
            match bake_target(&src, &doc, &annot, &budget, &mut g) {
                Some((appearance_ref, rect, bbox, matrix)) => {
                    bake(
                        &mut out,
                        &src,
                        &doc,
                        merged_ref,
                        page_index,
                        local,
                        appearance_ref,
                        rect,
                        bbox,
                        matrix,
                        &budget,
                        &mut g,
                    )
                    .map_err(|e| CliError(format!("{path}: {e}")))?;
                }
                // Widgets, hidden annotations, and anything without an
                // appearance are kept exactly as they were.
                None => keep.push(annot_ref),
            }
        }

        if !keep.is_empty() {
            let (objects, remap) =
                collect_objects_with_doc(&src, &doc, &keep, out.next_num_mut(), &budget, &mut g)
                    .map_err(|e| CliError(format!("{path}: copy kept annotations: {e}")))?;
            for (num, obj) in objects {
                out.add_object(num, obj);
            }
            let rebuilt: Vec<Obj> = keep
                .iter()
                .map(|r| Obj::Ref(Ref::new(remap.get(&r.num).copied().unwrap_or(r.num), 0)))
                .collect();
            crate::overlay::set_page_entry(&mut out, merged_ref, b"Annots", Obj::Array(rebuilt))
                .map_err(|e| CliError(format!("{path}: {e}")))?;
        }

        kept.push((page_index, page_ref.num, merged_ref));
    }

    crate::tools::reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, true, &budget, &mut g,
    )
    .map_err(CliError)?;
    crate::tools::write_document(
        out,
        output,
        Some(&src),
        PRESERVE_EXCEPT_ANNOTATIONS,
        Some(u64::try_from(page_refs.len()).unwrap_or(u64::MAX)),
        None,
        in_bytes,
        &budget,
        &mut g,
    )
}

/// The annotation references on a page, from its (possibly indirect) `/Annots`.
fn annotation_refs(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    annots: Option<Obj>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Vec<Ref> {
    let Some(value) = annots else {
        return Vec::new();
    };
    let Obj::Array(items) = resolve_inline(src, doc, value, budget, g) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            Obj::Ref(r) => Some(r),
            _ => None,
        })
        .collect()
}

/// Decide whether an annotation should be baked, and gather what that needs:
/// the appearance stream reference, the `/Rect`, and the appearance's `/BBox`
/// and `/Matrix`. Returns `None` for a widget, a hidden annotation, or one with
/// no usable normal appearance.
fn bake_target(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    annot: &[(selis_bytes::Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<(Ref, [f64; 4], [f64; 4], [f64; 6])> {
    if find_name(annot, b"Subtype").as_deref() == Some("Widget") {
        return None;
    }
    if let Some((_, Obj::Int(flags))) = annot.iter().find(|(k, _)| k.as_slice() == b"F") {
        if flags & ANNOT_FLAG_HIDDEN != 0 {
            return None;
        }
    }
    let rect = array4(annot, b"Rect")?;
    let appearance_ref = normal_appearance_ref(src, doc, annot, budget, g)?;
    let Obj::Stream { dict, .. } = resolve_inline(src, doc, Obj::Ref(appearance_ref), budget, g)
    else {
        return None;
    };
    let bbox = array4(&dict, b"BBox")?;
    let matrix = array6(&dict, b"Matrix").unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    Some((appearance_ref, rect, bbox, matrix))
}

/// The reference of an annotation's normal appearance stream, resolving an
/// appearance subdictionary through the annotation's `/AS` state when needed.
fn normal_appearance_ref(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    annot: &[(selis_bytes::Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<Ref> {
    let (_, ap_value) = annot.iter().find(|(k, _)| k.as_slice() == b"AP")?;
    let Obj::Dict(ap) = resolve_inline(src, doc, ap_value.clone(), budget, g) else {
        return None;
    };
    let (_, normal) = ap.iter().find(|(k, _)| k.as_slice() == b"N")?;
    match normal {
        Obj::Ref(r) => Some(*r),
        // A subdictionary keyed by appearance state: pick the annotation's /AS.
        Obj::Dict(states) => {
            let state = find_name(annot, b"AS")?;
            states.iter().find_map(|(key, value)| {
                if key.as_slice() == state.as_bytes() {
                    match value {
                        Obj::Ref(r) => Some(*r),
                        _ => None,
                    }
                } else {
                    None
                }
            })
        }
        _ => None,
    }
}

/// Copy the appearance into `out`, add it to the page's XObject resources, and
/// append a content stream that draws it mapped into the annotation's `/Rect`.
#[allow(clippy::too_many_arguments)]
fn bake(
    out: &mut DocumentBuilder,
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    page: Ref,
    page_index: usize,
    local: usize,
    appearance_ref: Ref,
    rect: [f64; 4],
    bbox: [f64; 4],
    matrix: [f64; 6],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), String> {
    let (objects, remap) =
        collect_objects_with_doc(src, doc, &[appearance_ref], out.next_num_mut(), budget, g)
            .map_err(|e| format!("copy appearance: {e}"))?;
    for (num, obj) in objects {
        out.add_object(num, obj);
    }
    let new_num = remap
        .get(&appearance_ref.num)
        .copied()
        .unwrap_or(appearance_ref.num);
    let name = format!("SelisFlat{page_index}_{local}");
    let resources = crate::overlay::ensure_resources_ref(out, page)?;
    crate::overlay::add_subdict_entry(
        out,
        resources,
        b"XObject",
        &name,
        Obj::Ref(Ref::new(new_num, 0)),
    )?;
    let placement = placement_matrix(rect, bbox, matrix);
    let content = format!("q\n{} cm\n/{name} Do\nQ\n", placement.map(fmt).join(" "));
    let content_ref = crate::overlay::add_stream(out, content.as_bytes());
    crate::overlay::append_content(out, page, content_ref)
}

/// The ISO 32000-1 §12.5.5 matrix that maps the appearance (its `/BBox` after
/// its `/Matrix`) into the annotation's `/Rect`.
fn placement_matrix(rect: [f64; 4], bbox: [f64; 4], matrix: [f64; 6]) -> [f64; 6] {
    let corners = [
        (bbox[0], bbox[1]),
        (bbox[2], bbox[1]),
        (bbox[2], bbox[3]),
        (bbox[0], bbox[3]),
    ];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in corners {
        let tx = matrix[0] * x + matrix[2] * y + matrix[4];
        let ty = matrix[1] * x + matrix[3] * y + matrix[5];
        min_x = min_x.min(tx);
        min_y = min_y.min(ty);
        max_x = max_x.max(tx);
        max_y = max_y.max(ty);
    }
    let transformed_w = (max_x - min_x).max(1e-6);
    let transformed_h = (max_y - min_y).max(1e-6);
    let rect_x0 = rect[0].min(rect[2]);
    let rect_y0 = rect[1].min(rect[3]);
    let rect_w = (rect[0] - rect[2]).abs();
    let rect_h = (rect[1] - rect[3]).abs();
    let scale_x = rect_w / transformed_w;
    let scale_y = rect_h / transformed_h;
    [
        scale_x,
        0.0,
        0.0,
        scale_y,
        rect_x0 - scale_x * min_x,
        rect_y0 - scale_y * min_y,
    ]
}

/// A PDF number as `f64`, from an `Int` or a `Real`.
fn number(obj: &Obj) -> Option<f64> {
    match obj {
        Obj::Int(n) => Some(*n as f64),
        Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(i32::from(*scale))),
        _ => None,
    }
}

/// Four direct numbers under `key`.
fn array4(dict: &[(selis_bytes::Bytes, Obj)], key: &[u8]) -> Option<[f64; 4]> {
    let (_, Obj::Array(items)) = dict.iter().find(|(k, _)| k.as_slice() == key)? else {
        return None;
    };
    let [a, b, c, d] = items.as_slice() else {
        return None;
    };
    Some([number(a)?, number(b)?, number(c)?, number(d)?])
}

/// Six direct numbers under `key`.
fn array6(dict: &[(selis_bytes::Bytes, Obj)], key: &[u8]) -> Option<[f64; 6]> {
    let (_, Obj::Array(items)) = dict.iter().find(|(k, _)| k.as_slice() == key)? else {
        return None;
    };
    let [a, b, c, d, e, f] = items.as_slice() else {
        return None;
    };
    Some([
        number(a)?,
        number(b)?,
        number(c)?,
        number(d)?,
        number(e)?,
        number(f)?,
    ])
}

/// Format a coordinate without exponent notation or a trailing point.
fn fmt(value: f64) -> String {
    let text = format!("{value:.4}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.').to_string();
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_input, take_output, MemorySession};

    fn stream_object(number: usize, dict: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{number} 0 obj\n{dict}\nstream\n").as_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out
    }

    fn build_xref(objects: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(&object);
        }
        let xref = out.len();
        let size = offsets.len() + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// A one-page document with one link annotation whose normal appearance is
    /// a form XObject that fills its box red (a marker the test can find).
    fn annotated_pdf() -> Vec<u8> {
        let appearance = b"1 0 0 rg\n0 0 20 20 re\nf\n";
        let objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [4 0 R] >>\nendobj\n".to_vec(),
            b"4 0 obj\n<< /Type /Annot /Subtype /Link /Rect [50 50 70 70] /AP << /N 5 0 R >> >>\nendobj\n".to_vec(),
            stream_object(
                5,
                "<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 20 >>",
                appearance,
            ),
        ];
        build_xref(objects)
    }

    #[test]
    fn a_link_appearance_is_baked_and_the_annotation_dropped() {
        let _session = MemorySession::activate();
        insert_input("flatten/link/in.pdf", annotated_pdf());

        let verification =
            flatten_annotations("flatten/link/in.pdf", "flatten/link/out.pdf").expect("flatten");
        assert_eq!(verification.pages_out, 1, "the page survives");
        assert_eq!(verification.annotations_out, 0, "the annotation is gone");

        let out = take_output("flatten/link/out.pdf").expect("output");
        let text = String::from_utf8_lossy(&out);
        assert!(out.starts_with(b"%PDF-"));
        // The appearance's own drawing is carried into the document, and a Do
        // places it; the page no longer carries an /Annots array.
        assert!(text.contains("0 0 20 20 re"), "the appearance is embedded");
        assert!(text.contains(" Do"), "a draw of the appearance is appended");
        assert!(
            !text.contains("/Annots"),
            "no annotation array remains: {text}"
        );
    }

    #[test]
    fn a_widget_annotation_is_left_for_the_form_flatten() {
        let _session = MemorySession::activate();
        // The same document, but the annotation is a Widget (a form field).
        let appearance = b"0 g\n0 0 20 20 re\nf\n";
        let objects = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [4 0 R] >>\nendobj\n".to_vec(),
            b"4 0 obj\n<< /Type /Annot /Subtype /Widget /Rect [50 50 70 70] /AP << /N 5 0 R >> >>\nendobj\n".to_vec(),
            stream_object(
                5,
                "<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 14 >>",
                appearance,
            ),
        ];
        insert_input("flatten/widget/in.pdf", build_xref(objects));

        let verification = flatten_annotations("flatten/widget/in.pdf", "flatten/widget/out.pdf")
            .expect("flatten");
        // The widget is kept, so the annotation count does not drop to zero.
        assert_eq!(verification.annotations_out, 1, "the widget is preserved");
        let out = take_output("flatten/widget/out.pdf").expect("output");
        assert!(
            String::from_utf8_lossy(&out).contains("/Widget"),
            "the widget survives"
        );
    }

    #[test]
    fn placement_maps_the_bbox_into_the_rect() {
        // A 20x20 appearance box, identity matrix, into a 40-wide, 80-tall rect
        // offset to (100, 10): scale 2 horizontally, 4 vertically, origin moved.
        let m = placement_matrix(
            [100.0, 10.0, 140.0, 90.0],
            [0.0, 0.0, 20.0, 20.0],
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        );
        assert!((m[0] - 2.0).abs() < 1e-9, "scale x");
        assert!((m[3] - 4.0).abs() < 1e-9, "scale y");
        assert!((m[4] - 100.0).abs() < 1e-9, "origin x");
        assert!((m[5] - 10.0).abs() < 1e-9, "origin y");
    }
}
