//! SL-5.FORM.06 — form creation and field authoring.
//!
//! The read/fill half of this crate assumes the field tree already exists.
//! This module adds the write half that creates one: a caller describes the
//! fields it wants (kind, name, page, box, options) and this code builds the
//! `/AcroForm` dictionary, the field/widget dictionaries, their `/AP`
//! appearance XObjects and the page `/Annots` wiring, rewrites the document,
//! then **re-opens the bytes it produced** and asserts every field's type,
//! name and rectangle before returning them. A field that does not survive
//! that round trip is a [`FormError::ReadBack`], not a file.
//!
//! This is intentionally a full rewrite (the same verified path `fill_form`
//! uses), not an in-place byte splice: the object graph is copied and
//! renumbered, every new object gets a unique number, and the result is
//! serialised by the COS writer. No filesystem or network is touched.

use super::{
    decode_text, dict_get, dict_remove, dict_set, is_encrypted, num_obj, object_dict_mut, open_doc,
    page_refs, read_form_fields, read_rect, resolve_deep, text_value, walk_records, FieldKind,
    FormError, FormField, MAX_FIELD_NAME_BYTES, MAX_FIELD_VALUE_BYTES, MAX_FORM_EDITS,
};
use selis_bytes::Bytes;
use selis_pdf_cos::copy::resolve_ref;
use selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer;
use selis_pdf_cos::reconcile::catalog_entry;
use selis_pdf_cos::{Doc, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The kind of field to author. The signature-only and push-button kinds of
/// [`FieldKind`] are deliberately absent: creating a signature or an action
/// button is a different, privileged operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorFieldKind {
    /// A text field (`/FT /Tx`).
    Text,
    /// A checkbox (`/FT /Btn`, no radio/push flags).
    Checkbox,
    /// A radio group (`/FT /Btn` with the radio flag).
    Radio,
    /// A choice field (`/FT /Ch`).
    Choice,
}

/// One field to create.
///
/// Every field is authored as a merged field/widget dictionary (ISO 32000-1
/// §12.7.3.3): one object carries `/FT`, `/T`, `/Rect`, `/DA` and the widget
/// `/AP`. Radios are a single on/off button; the first [`Self::options`] entry,
/// or `Yes`, is its on-state name. Choice fields use every option as `/Opt`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSpec {
    /// The fully qualified field name (written to `/T`). Must be unique in the
    /// request and in the document.
    pub name: String,
    /// The field type to create.
    pub kind: AuthorFieldKind,
    /// The 1-based page number the widget is placed on.
    pub page: u32,
    /// The widget rectangle `[x0, y0, x1, y1]` in default user space. Both
    /// dimensions must be positive and every coordinate finite.
    pub rect: [f64; 4],
    /// Choice options (written to `/Opt`, first one becomes `/V`), or the
    /// on-state name for a checkbox/radio. Ignored for a text field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// The fields to add to a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CreateFormOptions {
    /// The fields to author. An empty list is refused rather than silently
    /// rewriting the document.
    pub fields: Vec<FieldSpec>,
}

/// What a creation produced: the bytes plus a read-back of the field tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateFormReport {
    /// How many fields were created.
    pub created: usize,
    /// The fields read back from the produced bytes, in `/Fields` order.
    pub fields: Vec<FormField>,
    /// `(name, rectangle)` of every field, read back from the produced bytes.
    /// [`FormField`] deliberately carries no geometry, so callers that need to
    /// confirm placement use this list.
    pub rects: Vec<(String, [f64; 4])>,
    /// True when the document carries a signature the rewrite invalidates.
    pub has_signature: bool,
}

/// The default `/DA` written to a new `/AcroForm`: Helvetica, auto-size, black.
const CREATED_DA: &[u8] = b"/Helv 0 Tf 0 g";
/// The per-field `/DA` (a concrete size so viewers that ignore auto-size still
/// draw something).
const WIDGET_DA: &[u8] = b"/Helv 12 Tf 0 g";
/// Create AcroForm fields on `src` and return the rewritten bytes plus a
/// read-back report.
///
/// Works on a document with no form (it builds `/AcroForm` from scratch) and on
/// one that already has fields (it appends, refusing duplicate names). An
/// encrypted document, a live-XFA document, an invalid spec or a field that
/// does not round-trip is a typed error; no partial file is returned.
///
/// # Budget
///
/// One document parse, one reachable-graph copy and one serialisation, all
/// charged to `budget`; the read-back reparses the produced bytes. Allocations
/// are bounded by the copied object graph and the caller-bounded field list.
///
/// # Malformed Input
///
/// Invalid names, boxes, pages or options are rejected before any object is
/// written. A document whose catalog/page objects cannot be resolved, or whose
/// output does not reparse with the fields intact, is a typed [`FormError`].
///
/// # Errors
///
/// [`FormError::Invalid`]/[`FormError::Limit`] for a bad request,
/// [`FormError::Encrypted`]/[`FormError::Xfa`] for the document class, and
/// [`FormError::Unresolvable`]/[`FormError::Malformed`]/[`FormError::ReadBack`]
/// when the rewrite cannot be produced and verified honestly.
pub fn create_form_fields(
    src: &[u8],
    options: &CreateFormOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, CreateFormReport), FormError> {
    validate_options(options)?;

    let doc = open_doc(src, budget, g)?;
    if is_encrypted(&doc) {
        return Err(FormError::Encrypted);
    }
    let walked = walk_records(src, &doc, budget, g)?;
    if walked.has_xfa {
        return Err(FormError::Xfa);
    }
    let pages = page_refs(src, &doc, budget, g)?;
    for spec in &options.fields {
        let page = usize::try_from(spec.page).map_err(|_| {
            FormError::Invalid(format!("page number for {:?} is too large", spec.name))
        })?;
        if page == 0 || page > pages.len() {
            return Err(FormError::Invalid(format!(
                "field {:?} names page {} but the document has {} page(s)",
                spec.name,
                spec.page,
                pages.len()
            )));
        }
    }
    let mut names: Vec<String> = walked
        .records
        .iter()
        .map(|record| record.view.name.clone())
        .collect();
    for spec in &options.fields {
        if names.iter().any(|name| name == &spec.name) {
            return Err(FormError::Invalid(format!(
                "a field named {:?} already exists in this document",
                spec.name
            )));
        }
        names.push(spec.name.clone());
    }

    let revision = doc
        .revisions()
        .last()
        .ok_or_else(|| FormError::Malformed("no revisions".to_string()))?;
    let root = revision
        .root
        .ok_or_else(|| FormError::Malformed("no /Root in trailer".to_string()))?;
    let info = revision
        .trailer
        .iter()
        .find(|(key, _)| key.as_slice() == b"Info")
        .and_then(|(_, value)| match value {
            Obj::Ref(reference) => Some(*reference),
            _ => None,
        });
    // Three cases matter: an indirect form (append to it), an inline form in
    // the catalog (append into that dictionary, never overwrite its fields),
    // and no form at all (build one).
    let existing_acro = catalog_entry(src, &doc, b"AcroForm", budget, g)
        .ok()
        .flatten();
    let acroform_ref = match &existing_acro {
        Some(Obj::Ref(reference)) => Some(*reference),
        _ => None,
    };
    let inline_acro = matches!(&existing_acro, Some(Obj::Dict(_)));
    let mut roots = vec![root];
    if let Some(info) = info {
        roots.push(info);
    }
    let mut next_num = 1u32;
    let (mut objects, remap) =
        selis_pdf_cos::copy::collect_objects(src, &roots, &mut next_num, budget, g)
            .map_err(|e| FormError::Unresolvable(format!("cannot copy the document: {e}")))?;
    let catalog_num = remap.get(&root.num).copied().unwrap_or(root.num);

    // A standard-14 font resource every created field's /DA and /AP can use.
    let font_num = alloc(
        &mut objects,
        &mut next_num,
        Obj::Dict(vec![
            (key(b"Type"), name(b"Font")),
            (key(b"Subtype"), name(b"Type1")),
            (key(b"BaseFont"), name(b"Helvetica")),
        ]),
    );

    let mut field_refs: Vec<Ref> = Vec::with_capacity(options.fields.len());
    for spec in &options.fields {
        let page_index = usize::try_from(spec.page)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        let source_page = pages.get(page_index).ok_or_else(|| {
            FormError::Invalid(format!(
                "field {:?} names a page that does not exist",
                spec.name
            ))
        })?;
        let page_num = remap
            .get(&source_page.num)
            .copied()
            .unwrap_or(source_page.num);
        let field_num = build_field(spec, page_num, font_num, &mut objects, &mut next_num);
        let field_ref = Ref::new(field_num, 0);
        field_refs.push(field_ref);
        let page_dict = object_dict_mut(&mut objects, page_num).ok_or_else(|| {
            FormError::Malformed(format!(
                "page {} is not a dictionary in the output",
                spec.page
            ))
        })?;
        append_ref_array(page_dict, b"Annots", field_ref);
    }

    match acroform_ref {
        Some(reference) => {
            let num = remap.get(&reference.num).copied().ok_or_else(|| {
                FormError::Malformed("the existing /AcroForm was not copied".to_string())
            })?;
            let acro = object_dict_mut(&mut objects, num).ok_or_else(|| {
                FormError::Malformed("the existing /AcroForm is not a dictionary".to_string())
            })?;
            for field_ref in field_refs {
                append_ref_array(acro, b"Fields", field_ref);
            }
            // Make sure a font our /DA names resolves, and ask viewers to
            // rebuild any variable text we did not draw ourselves.
            ensure_helv_resource(acro, font_num);
            dict_set(acro, b"NeedAppearances", Obj::Bool(true));
        }
        None if inline_acro => {
            // The form is an inline catalog dictionary; its field objects were
            // copied with the catalog. Mutate a clone of that dictionary and
            // put it back, so no existing field is dropped.
            let catalog = object_dict_mut(&mut objects, catalog_num).ok_or_else(|| {
                FormError::Malformed("the catalog is not a dictionary in the output".to_string())
            })?;
            let mut acro = match dict_get(catalog, b"AcroForm") {
                Some(Obj::Dict(acro)) => acro.clone(),
                _ => {
                    return Err(FormError::Malformed(
                        "the inline /AcroForm disappeared from the output".to_string(),
                    ))
                }
            };
            for field_ref in field_refs {
                append_ref_array(&mut acro, b"Fields", field_ref);
            }
            ensure_helv_resource(&mut acro, font_num);
            dict_set(&mut acro, b"NeedAppearances", Obj::Bool(true));
            dict_set(catalog, b"AcroForm", Obj::Dict(acro));
        }
        None => {
            let mut acro = vec![
                (
                    key(b"Fields"),
                    Obj::Array(field_refs.iter().copied().map(Obj::Ref).collect()),
                ),
                (key(b"DA"), Obj::String(Bytes::copy_from_slice(CREATED_DA))),
                (key(b"NeedAppearances"), Obj::Bool(true)),
            ];
            ensure_helv_resource(&mut acro, font_num);
            let acro_num = alloc(&mut objects, &mut next_num, Obj::Dict(acro));
            let catalog = object_dict_mut(&mut objects, catalog_num).ok_or_else(|| {
                FormError::Malformed("the catalog is not a dictionary in the output".to_string())
            })?;
            dict_set(catalog, b"AcroForm", Obj::Ref(Ref::new(acro_num, 0)));
        }
    }

    let root_ref = Ref::new(remap.get(&root.num).copied().unwrap_or(root.num), 0);
    let mut trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some(info) = info {
        if let Some(&new_num) = remap.get(&info.num) {
            trailer.push((b"Info".to_vec(), Obj::Ref(Ref::new(new_num, 0))));
        }
    }
    let bytes = write_objects_as_document_with_trailer(&objects, root_ref, &trailer, budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot write the document: {e}")))?;

    let report = read_back(&bytes, options, walked.has_signature, budget, g)?;
    Ok((bytes, report))
}

/// Validate the caller's field list before touching any object.
///
/// # Budget
///
/// Linear in the number and size of the caller's specs; no document objects.
///
/// # Malformed Input
///
/// An empty list, a duplicate/empty/over-long name, a degenerate or infinite
/// box, a page below 1, a choice with no options or an over-long/NUL-bearing
/// option is an [`FormError`]; nothing is partially planned.
fn validate_options(options: &CreateFormOptions) -> Result<(), FormError> {
    if options.fields.is_empty() {
        return Err(FormError::Invalid(
            "no fields to create; name at least one field".to_string(),
        ));
    }
    if options.fields.len() > MAX_FORM_EDITS {
        return Err(FormError::Limit(format!(
            "a creation may add at most {MAX_FORM_EDITS} fields (got {})",
            options.fields.len()
        )));
    }
    let mut seen: Vec<&str> = Vec::with_capacity(options.fields.len());
    for spec in &options.fields {
        if spec.name.is_empty() {
            return Err(FormError::Invalid("a field name is empty".to_string()));
        }
        if spec.name.len() > MAX_FIELD_NAME_BYTES {
            return Err(FormError::Limit(format!(
                "field name {:?} is longer than {MAX_FIELD_NAME_BYTES} bytes",
                spec.name
            )));
        }
        if spec.name.contains('\0') {
            return Err(FormError::Invalid(format!(
                "field name {:?} contains a NUL",
                spec.name
            )));
        }
        if seen.contains(&spec.name.as_str()) {
            return Err(FormError::Invalid(format!(
                "the field {:?} is named twice in one request",
                spec.name
            )));
        }
        seen.push(spec.name.as_str());
        let [x0, y0, x1, y1] = spec.rect;
        if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
            return Err(FormError::Invalid(format!(
                "the box for {:?} has a non-finite coordinate",
                spec.name
            )));
        }
        if x1 <= x0 || y1 <= y0 {
            return Err(FormError::Invalid(format!(
                "the box for {:?} is degenerate (x1>x0 and y1>y0 required)",
                spec.name
            )));
        }
        if spec.page == 0 {
            return Err(FormError::Invalid(format!(
                "field {:?} has page 0; pages are 1-based",
                spec.name
            )));
        }
        if spec.kind == AuthorFieldKind::Choice && spec.options.is_empty() {
            return Err(FormError::Invalid(format!(
                "the choice field {:?} needs at least one option",
                spec.name
            )));
        }
        for option in &spec.options {
            if option.is_empty() || option.contains('\0') {
                return Err(FormError::Invalid(format!(
                    "an option for {:?} is empty or contains a NUL",
                    spec.name
                )));
            }
            if option.len() > MAX_FIELD_VALUE_BYTES {
                return Err(FormError::Limit(format!(
                    "an option for {:?} is longer than {MAX_FIELD_VALUE_BYTES} bytes",
                    spec.name
                )));
            }
        }
    }
    Ok(())
}

/// Allocate one object and return its new number.
fn alloc(objects: &mut Vec<(u32, Obj)>, next_num: &mut u32, obj: Obj) -> u32 {
    let num = *next_num;
    *next_num = next_num.saturating_add(1);
    objects.push((num, obj));
    num
}

/// A dictionary key.
fn key(bytes: &[u8]) -> Bytes {
    Bytes::copy_from_slice(bytes)
}

/// A name object.
fn name(bytes: &[u8]) -> Obj {
    Obj::Name(Bytes::copy_from_slice(bytes))
}

/// Append a reference to an array-valued dictionary key, creating it if
/// absent. A non-array value is replaced rather than guessed at.
fn append_ref_array(pairs: &mut Vec<(Bytes, Obj)>, key_bytes: &[u8], reference: Ref) {
    let mut items = match dict_get(pairs, key_bytes) {
        Some(Obj::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    items.push(Obj::Ref(reference));
    dict_set(pairs, key_bytes, Obj::Array(items));
}

/// Ensure `/DR << /Font << /Helv <font> >> >>` exists on a form dictionary so
/// the created fields' `/DA` and appearance resources resolve.
fn ensure_helv_resource(acro: &mut Vec<(Bytes, Obj)>, font_num: u32) {
    let mut dr = match dict_get(acro, b"DR") {
        Some(Obj::Dict(dr)) => dr.clone(),
        _ => Vec::new(),
    };
    let mut fonts = match dict_get(&dr, b"Font") {
        Some(Obj::Dict(fonts)) => fonts.clone(),
        _ => Vec::new(),
    };
    dict_remove(&mut fonts, b"Helv");
    fonts.push((key(b"Helv"), Obj::Ref(Ref::new(font_num, 0))));
    dict_set(&mut dr, b"Font", Obj::Dict(fonts));
    dict_set(acro, b"DR", Obj::Dict(dr));
    if dict_get(acro, b"DA").is_none() {
        dict_set(acro, b"DA", Obj::String(Bytes::copy_from_slice(CREATED_DA)));
    }
}

/// A PDF name token safe to write after `/`. Delimiters and whitespace become
/// underscores; an empty or fully-stripped name falls back to `Yes`.
fn safe_state_name(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("Yes");
    }
    out
}

/// A minimal Form XObject used as a widget appearance. `content` is the
/// (already escaped) drawing operators; a `Helv` font resource is always
/// included so later fills can synthesize over it.
fn appearance_stream(rect: [f64; 4], content: &[u8], font_num: u32) -> Obj {
    let [x0, y0, x1, y1] = rect;
    let width = (x1 - x0).abs();
    let height = (y1 - y0).abs();
    Obj::Stream {
        dict: vec![
            (key(b"Type"), name(b"XObject")),
            (key(b"Subtype"), name(b"Form")),
            (key(b"FormType"), Obj::Int(1)),
            (
                key(b"BBox"),
                Obj::Array(vec![
                    Obj::Int(0),
                    Obj::Int(0),
                    num_obj(width),
                    num_obj(height),
                ]),
            ),
            (
                key(b"Resources"),
                Obj::Dict(vec![(
                    key(b"Font"),
                    Obj::Dict(vec![(key(b"Helv"), Obj::Ref(Ref::new(font_num, 0)))]),
                )]),
            ),
            (
                key(b"Length"),
                Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
            ),
        ],
        data: Bytes::copy_from_slice(content),
    }
}

/// Build one merged field/widget object (plus its appearance streams) and
/// return its new object number.
///
/// # Budget
///
/// At most three new objects per field; allocations are bounded by the
/// caller-bounded options list.
///
/// # Malformed Input
///
/// Every spec has already been validated; a name token is sanitized before it
/// is written so no delimiter can break the PDF syntax.
fn build_field(
    spec: &FieldSpec,
    page_num: u32,
    font_num: u32,
    objects: &mut Vec<(u32, Obj)>,
    next_num: &mut u32,
) -> u32 {
    let mut pairs = vec![
        (key(b"Type"), name(b"Annot")),
        (key(b"Subtype"), name(b"Widget")),
        (key(b"F"), Obj::Int(4)), // print
        (key(b"T"), text_value(&spec.name)),
        (
            key(b"Rect"),
            Obj::Array(spec.rect.iter().map(|v| num_obj(*v)).collect()),
        ),
        (key(b"P"), Obj::Ref(Ref::new(page_num, 0))),
        (key(b"DA"), Obj::String(Bytes::copy_from_slice(WIDGET_DA))),
    ];
    match spec.kind {
        AuthorFieldKind::Text => {
            let ap = alloc(
                objects,
                next_num,
                appearance_stream(spec.rect, b"q\nQ\n", font_num),
            );
            pairs.push((key(b"FT"), name(b"Tx")));
            pairs.push((key(b"V"), text_value("")));
            pairs.push((
                key(b"AP"),
                Obj::Dict(vec![(key(b"N"), Obj::Ref(Ref::new(ap, 0)))]),
            ));
        }
        AuthorFieldKind::Choice => {
            let ap = alloc(
                objects,
                next_num,
                appearance_stream(spec.rect, b"q\nQ\n", font_num),
            );
            pairs.push((key(b"FT"), name(b"Ch")));
            pairs.push((
                key(b"Opt"),
                Obj::Array(spec.options.iter().map(|o| text_value(o)).collect()),
            ));
            let first = spec.options.first().map_or("", String::as_str);
            pairs.push((key(b"V"), text_value(first)));
            pairs.push((
                key(b"AP"),
                Obj::Dict(vec![(key(b"N"), Obj::Ref(Ref::new(ap, 0)))]),
            ));
        }
        AuthorFieldKind::Checkbox | AuthorFieldKind::Radio => {
            let on = spec
                .options
                .first()
                .map_or_else(|| "Yes".to_string(), |s| safe_state_name(s));
            let on = safe_state_name(&on);
            if spec.kind == AuthorFieldKind::Radio {
                pairs.push((key(b"Ff"), Obj::Int(1 << 15))); // radio flag
            }
            let off_stream = b"q\nQ\n";
            // A tick drawn in the widget's own coordinate space: visible, but
            // deliberately simple — this is an authoring default, not a
            // designer's artwork.
            let on_content = b"q\n1 0 0 1 0 0 cm\n2 2 m 8 10 l 16 2 l S\nQ\n";
            let off = alloc(
                objects,
                next_num,
                appearance_stream(spec.rect, off_stream, font_num),
            );
            let on_ref = alloc(
                objects,
                next_num,
                appearance_stream(spec.rect, on_content, font_num),
            );
            pairs.push((key(b"FT"), name(b"Btn")));
            pairs.push((key(b"V"), name(b"Off")));
            pairs.push((key(b"AS"), name(b"Off")));
            pairs.push((
                key(b"AP"),
                Obj::Dict(vec![(
                    key(b"N"),
                    Obj::Dict(vec![
                        (key(b"Off"), Obj::Ref(Ref::new(off, 0))),
                        (key(on.as_bytes()), Obj::Ref(Ref::new(on_ref, 0))),
                    ]),
                )]),
            ));
        }
    }
    alloc(objects, next_num, Obj::Dict(pairs))
}

/// The field kind a caller asked for, as the reader reports it.
fn expected_kind(kind: AuthorFieldKind) -> FieldKind {
    match kind {
        AuthorFieldKind::Text => FieldKind::Text,
        AuthorFieldKind::Checkbox => FieldKind::Checkbox,
        AuthorFieldKind::Radio => FieldKind::Radio,
        AuthorFieldKind::Choice => FieldKind::Choice,
    }
}

/// Re-open the produced bytes, list every AcroForm field and read each one's
/// `/Rect`, then assert the created fields match the request.
///
/// # Budget
///
/// One parse plus a linear walk of the copied field dictionaries; allocations
/// are bounded by the field list.
///
/// # Malformed Input
///
/// A document that does not reparse, a field that vanished, or a rectangle
/// that is missing or moved is a typed [`FormError`] (never a partial report).
fn read_back(
    bytes: &[u8],
    options: &CreateFormOptions,
    has_signature: bool,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<CreateFormReport, FormError> {
    let report = read_form_fields(bytes, budget, g)?;
    let rects = field_rects(bytes, budget, g)?;
    for spec in &options.fields {
        let field = report.fields.iter().find(|f| f.name == spec.name);
        let Some(field) = field else {
            return Err(FormError::ReadBack {
                name: spec.name.clone(),
                wanted: format!("{:?}", spec.kind),
                read: None,
            });
        };
        if field.kind != expected_kind(spec.kind) {
            return Err(FormError::ReadBack {
                name: spec.name.clone(),
                wanted: format!("{:?}", spec.kind),
                read: Some(format!("{:?}", field.kind)),
            });
        }
        let Some(rect) = rects.get(&spec.name) else {
            return Err(FormError::ReadBack {
                name: spec.name.clone(),
                wanted: format!("{:?}", spec.rect),
                read: None,
            });
        };
        if !rect_close(*rect, spec.rect) {
            return Err(FormError::ReadBack {
                name: spec.name.clone(),
                wanted: format!("{:?}", spec.rect),
                read: Some(format!("{rect:?}")),
            });
        }
    }
    let mut out_rects: Vec<(String, [f64; 4])> = options
        .fields
        .iter()
        .filter_map(|spec| rects.get(&spec.name).map(|r| (spec.name.clone(), *r)))
        .collect();
    out_rects.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(CreateFormReport {
        created: options.fields.len(),
        fields: report.fields,
        rects: out_rects,
        has_signature,
    })
}

/// True when two rectangles agree to within a rounding tolerance (numbers are
/// serialised as 3-decimal reals).
fn rect_close(a: [f64; 4], b: [f64; 4]) -> bool {
    let tolerance = 0.002f64;
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| (x - y).abs() <= tolerance)
}

/// Read `(name, /Rect)` of every top-level AcroForm field from raw bytes.
fn field_rects(
    bytes: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<HashMap<String, [f64; 4]>, FormError> {
    let mut out = HashMap::new();
    let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
    let doc: Doc = selis_pdf_cos::parse_revisions(bytes, startxref, budget, g)
        .map_err(|e| FormError::Malformed(format!("the output does not reparse: {e}")))?;
    let acro = catalog_entry(bytes, &doc, b"AcroForm", budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot read the output form: {e}")))?
        .ok_or_else(|| FormError::Malformed("the output has no /AcroForm".to_string()))?;
    let acro = resolve_deep(bytes, &doc, acro, 0, budget, g);
    let Obj::Dict(acro_pairs) = &acro else {
        return Err(FormError::Malformed(
            "the output /AcroForm is not a dict".to_string(),
        ));
    };
    let Some(Obj::Array(fields)) = dict_get(acro_pairs, b"Fields") else {
        return Err(FormError::Malformed(
            "the output /AcroForm has no /Fields".to_string(),
        ));
    };
    for field in fields {
        let Obj::Ref(reference) = field else { continue };
        let resolved = resolve_ref(bytes, &doc, *reference, budget, g)
            .map_err(|e| FormError::Unresolvable(format!("cannot read a field: {e}")))?;
        let Obj::Dict(pairs) = &resolved else {
            continue;
        };
        let Some(name) = dict_get(pairs, b"T").and_then(decode_text) else {
            continue;
        };
        if let Some((x0, y0, x1, y1)) = read_rect(pairs) {
            out.insert(name, [x0, y0, x1, y1]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::type_complexity
    )]

    use super::*;

    /// A minimal one-page PDF with no form and no annotations.
    fn flat_pdf() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = vec![0u64; max as usize + 1];
        for (num, body) in objects {
            offsets[*num as usize] = out.len() as u64;
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len() as u64;
        out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for index in 1..=max {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
                max + 1
            )
            .as_bytes(),
        );
        out
    }

    fn spec(name: &str, kind: AuthorFieldKind, rect: [f64; 4]) -> FieldSpec {
        FieldSpec {
            name: name.to_string(),
            kind,
            page: 1,
            rect,
            options: Vec::new(),
        }
    }

    fn create(fixture: &[u8], fields: Vec<FieldSpec>) -> (Vec<u8>, CreateFormReport) {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        create_form_fields(fixture, &CreateFormOptions { fields }, &budget, &mut g).expect("create")
    }

    #[test]
    fn creates_all_four_kinds_and_reads_them_back() {
        let mut choice = spec(
            "Color",
            AuthorFieldKind::Choice,
            [10.0, 200.0, 130.0, 220.0],
        );
        choice.options = vec!["Red".to_string(), "Green".to_string()];
        let mut radio = spec("Size", AuthorFieldKind::Radio, [10.0, 230.0, 30.0, 250.0]);
        radio.options = vec!["Small".to_string()];
        let mut checkbox = spec(
            "Agree",
            AuthorFieldKind::Checkbox,
            [10.0, 260.0, 30.0, 280.0],
        );
        checkbox.options = vec!["Yes".to_string()];

        let fields = vec![
            spec("Name", AuthorFieldKind::Text, [10.0, 10.0, 200.0, 30.0]),
            checkbox,
            choice,
            radio,
        ];
        let expected: Vec<(String, FieldKind, [f64; 4])> = fields
            .iter()
            .map(|f| (f.name.clone(), expected_kind(f.kind), f.rect))
            .collect();
        let (bytes, report) = create(&flat_pdf(), fields);
        assert_eq!(report.created, 4);
        assert_eq!(report.fields.len(), 4);
        assert_eq!(report.rects.len(), 4);
        assert!(!report.has_signature);

        for (name, kind, rect) in &expected {
            let field = report
                .fields
                .iter()
                .find(|f| &f.name == name)
                .expect("field");
            assert_eq!(field.kind, *kind, "kind for {name}");
            assert_eq!(field.page, Some(1), "page for {name}");
            let (_, read_rect) = report.rects.iter().find(|(n, _)| n == name).expect("rect");
            assert!(
                rect_close(*read_rect, *rect),
                "rect for {name}: {read_rect:?}"
            );
        }

        // The bytes really carry an /AcroForm with the four fields and their
        // widgets, not just the report.
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/AcroForm"), "an /AcroForm was built");
        assert!(text.contains("/Annots"), "the page carries /Annots");
        assert!(text.contains("/FT /Tx") && text.contains("/FT /Btn"));
        assert!(text.contains("/FT /Ch"));
        assert!(text.contains("/NeedAppearances true"));
        assert!(text.contains("/Ff 32768"), "the radio flag was written");
        assert!(
            text.contains("(Red)") && text.contains("(Green)"),
            "choices"
        );

        // The produced document is itself readable by the ordinary reader.
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let reread = read_form_fields(&bytes, &budget, &mut g).expect("reread");
        assert_eq!(reread.fields.len(), 4);
    }

    #[test]
    fn refuses_bad_specs_and_duplicates() {
        let fixture = flat_pdf();
        let budget = Budget::unlimited();
        let cases: Vec<(Vec<FieldSpec>, &str)> = vec![
            (vec![], "no fields"),
            (
                vec![spec("", AuthorFieldKind::Text, [0.0, 0.0, 10.0, 10.0])],
                "empty",
            ),
            (
                vec![spec("A", AuthorFieldKind::Text, [0.0, 0.0, 0.0, 10.0])],
                "degenerate",
            ),
            (
                vec![spec("A", AuthorFieldKind::Choice, [0.0, 0.0, 10.0, 10.0])],
                "at least one option",
            ),
            (
                vec![
                    spec("A", AuthorFieldKind::Text, [0.0, 0.0, 10.0, 10.0]),
                    spec("A", AuthorFieldKind::Text, [0.0, 0.0, 10.0, 10.0]),
                ],
                "twice",
            ),
        ];
        for (fields, needle) in cases {
            let mut g = budget.guard();
            let error =
                create_form_fields(&fixture, &CreateFormOptions { fields }, &budget, &mut g)
                    .expect_err("refused");
            assert!(
                error.to_string().contains(needle),
                "error {error:?} did not mention {needle:?}"
            );
        }
    }

    #[test]
    fn appends_to_an_inline_acroform_without_losing_fields() {
        let objects: &[(u32, &[u8])] = &[
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] >> >>",
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
            ),
            (
                5,
                b"<< /FT /Tx /T (Original) /Subtype /Widget /Rect [0 0 10 10] /P 3 0 R >>",
            ),
        ];
        let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = vec![0u64; max as usize + 1];
        for (num, body) in objects {
            offsets[*num as usize] = out.len() as u64;
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len() as u64;
        out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for index in 1..=max {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[index as usize]).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
                max + 1
            )
            .as_bytes(),
        );

        let (_, report) = create(
            &out,
            vec![spec(
                "Added",
                AuthorFieldKind::Text,
                [0.0, 20.0, 10.0, 30.0],
            )],
        );
        assert_eq!(report.fields.len(), 2, "both fields survive");
        assert!(report.fields.iter().any(|f| f.name == "Original"));
        assert!(report.fields.iter().any(|f| f.name == "Added"));
    }

    #[test]
    fn appends_to_an_existing_form() {
        // A one-field form created above can be added to without losing the
        // original field.
        let first = create(
            &flat_pdf(),
            vec![spec("First", AuthorFieldKind::Text, [0.0, 0.0, 10.0, 10.0])],
        )
        .0;
        let (_, report) = create(
            &first,
            vec![spec(
                "Second",
                AuthorFieldKind::Text,
                [0.0, 20.0, 10.0, 30.0],
            )],
        );
        assert_eq!(report.created, 1);
        assert_eq!(report.fields.len(), 2, "the original field survives");
        assert!(
            report.fields.iter().any(|f| f.name == "First")
                && report.fields.iter().any(|f| f.name == "Second")
        );
    }
}
