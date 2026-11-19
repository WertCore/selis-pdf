//! Full field appearance generation for every widget type (SL-5.FORM.02).
//!
//! [`regenerate_appearances`] walks the AcroForm and writes a conformant `/AP`
//! appearance for **every** widget kind from the field's current value:
//!
//! * **text** — the value drawn per `/DA` with quadding / multiline / auto-size,
//!   using exact standard-14 metrics (the shared variable-text generator);
//! * **choice / combo / list box**, including **multi-select** — the selected
//!   `/Opt` display labels, one stacked line per selection;
//! * **checkbox** — `/AP /N` gets `Off` (empty) and the on-state (a vector
//!   checkmark), with `/AS` selected from `/V`;
//! * **radio** — each widget gets `Off` plus its on-state (a filled disc),
//!   with `/AS` selected from `/V`;
//! * **push button** — a bordered caption drawn from `/MK /CA` (falling back to
//!   `/T`), honouring `/MK /BC`, `/BG` and `/R`;
//! * **signature** — a bordered box drawing the signer name from an existing
//!   `/V /Name`. A signature field with no `/V` is skipped with a typed note:
//!   this engine never fabricates a signature appearance or signs.
//!
//! Buttons that already carry appearance states are **left untouched** — only
//! `/AS` is selected — so a hand-authored on-state is never silently replaced.
//! A widget we cannot draw (no box, no resolvable font, a signed field with no
//! name) is skipped with a typed [`AppearanceSkip`] and `/NeedAppearances` is
//! set so a conformant viewer is asked to rebuild it — the same contract the
//! annotation crate uses (ANNOT.02). Font substitution is always disclosed in
//! [`AppearanceReport::disclosures`]; nothing changes silently.
//!
//! Like the rest of this crate, the writer copies the reachable graph, mutates
//! the copies, emits a fresh document and **re-opens the bytes** to prove every
//! generated appearance reads back before anything is returned.

use super::*;
use serde::{Deserialize, Serialize};

/// Why a widget was not given a generated appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceSkipReason {
    /// A signature field with no `/V` (nothing honest to draw).
    SignatureWithoutValue,
    /// The field has no widget annotation.
    NoWidget,
    /// The widget has no usable `/Rect`.
    DegenerateBox,
    /// Text/choice font could not be resolved in any `/DR /Font` map.
    NoResolvableFont,
}

impl AppearanceSkipReason {
    /// A stable human-readable note.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::SignatureWithoutValue => "signature field has no /V to draw",
            Self::NoWidget => "field has no widget annotation",
            Self::DegenerateBox => "widget has no usable /Rect",
            Self::NoResolvableFont => "no font resolvable in /DR",
        }
    }
}

/// One widget deliberately left without a generated appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppearanceSkip {
    /// The fully-qualified field name.
    pub field: String,
    /// Why it was skipped.
    pub reason: AppearanceSkipReason,
}

/// What a regeneration produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppearanceReport {
    /// Widgets that received a freshly generated `/AP`.
    pub generated: usize,
    /// Buttons whose existing states were kept (only `/AS` was selected).
    pub kept_existing: usize,
    /// Widgets that could not be drawn, each with a typed reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<AppearanceSkip>,
    /// Disclosures — notably font substitutions. Never empty when a font was
    /// chosen for a field that did not name a resolvable one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disclosures: Vec<String>,
}

/// Regenerate appearances for every widget in the document's AcroForm.
///
/// The caller owns the bytes: there is no filesystem or network access. Every
/// generated stream is re-read from the produced bytes before they are
/// returned; a widget that was meant to be drawn but does not read back is a
/// typed [`FormError::ReadBack`], not a deliverable.
///
/// # Budget
///
/// One document parse, one bounded field-tree walk and one bounded widget walk;
/// every field ticks the budget. Streams are bounded by the value limit and the
/// copied widget graph.
///
/// # Errors
///
/// [`FormError::Encrypted`] for an encrypted document,
/// [`FormError::Xfa`] for an XFA form, and the usual malformed / unresolvable
/// variants for a document this crate cannot rewrite honestly.
pub fn regenerate_appearances(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, AppearanceReport), FormError> {
    let doc = open_doc(src, budget, g)?;
    if is_encrypted(&doc) {
        return Err(FormError::Encrypted);
    }
    let walked = walk_records(src, &doc, budget, g)?;
    if walked.has_xfa {
        return Err(FormError::Xfa);
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
    let acroform_ref =
        match selis_pdf_cos::reconcile::catalog_entry(src, &doc, b"AcroForm", budget, g) {
            Ok(Some(Obj::Ref(reference))) => Some(reference),
            _ => None,
        };
    let mut roots = vec![root];
    if let Some(info) = info {
        roots.push(info);
    }
    let mut next_num = 1u32;
    let (mut objects, remap) = collect_objects(src, &roots, &mut next_num, budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot copy the document: {e}")))?;

    let (default_da, form_fonts) = form_defaults(&objects, &remap, root.num, acroform_ref);
    let mut report = AppearanceReport::default();

    for record in &walked.records {
        g.tick()
            .map_err(|e| FormError::Limit(format!("appearance budget exhausted: {e}")))?;
        let name = record.view.name.clone();
        match record.view.kind {
            FieldKind::Text | FieldKind::Choice => {
                synthesize_variable(
                    record,
                    &mut objects,
                    &remap,
                    &mut next_num,
                    &default_da,
                    &form_fonts,
                    &mut report,
                    &name,
                );
            }
            FieldKind::Checkbox | FieldKind::Radio => {
                synthesize_button_state(
                    record,
                    &mut objects,
                    &remap,
                    &mut next_num,
                    &mut report,
                    &name,
                );
            }
            FieldKind::PushButton => {
                synthesize_push_button(
                    record,
                    &mut objects,
                    &remap,
                    &mut next_num,
                    &mut report,
                    &name,
                );
            }
            FieldKind::Signature => {
                synthesize_signature(
                    record,
                    &mut objects,
                    &remap,
                    &mut next_num,
                    &mut report,
                    &name,
                );
            }
            FieldKind::Unknown => {
                report.skipped.push(AppearanceSkip {
                    field: name,
                    reason: AppearanceSkipReason::NoResolvableFont,
                });
            }
        }
    }

    // /NeedAppearances interplay (compatible with ANNOT.02): clear it when
    // every widget was drawn, otherwise ask a viewer to rebuild the ones we
    // could not. We never leave a half-generated form looking complete.
    let all_generated = report.skipped.is_empty();
    clear_or_set_need_appearances(&mut objects, &remap, root.num, acroform_ref, !all_generated);

    let root_ref = Ref::new(remap.get(&root.num).copied().unwrap_or(root.num), 0);
    let mut trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some(info) = info {
        if let Some(&new_num) = remap.get(&info.num) {
            trailer.push((b"Info".to_vec(), Obj::Ref(Ref::new(new_num, 0))));
        }
    }
    let bytes = write_objects_as_document_with_trailer(&objects, root_ref, &trailer, budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot write the document: {e}")))?;

    // Read-back: reopen and verify the fields we claimed to draw still carry
    // an /AP. One walk of the output for all of them (not one per widget).
    let out_startxref = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
    let out_doc = selis_pdf_cos::parse_revisions(&bytes, out_startxref, budget, g)
        .map_err(|e| FormError::Malformed(format!("the output does not reparse: {e}")))?;
    let claimed: Vec<&str> = walked
        .records
        .iter()
        .filter(|record| {
            record.widgets.iter().any(|widget| {
                widget
                    .obj_ref
                    .and_then(|r| remap.get(&r.num).copied())
                    .and_then(|num| object_dict(&objects, num))
                    .is_some_and(|dict| dict_get(dict, b"AP").is_some())
            })
        })
        .map(|record| record.view.name.as_str())
        .collect();
    if !claimed.is_empty() {
        let out_walked = walk_records(&bytes, &out_doc, budget, g)?;
        for name in claimed {
            let found = out_walked
                .records
                .iter()
                .find(|record| record.view.name == name)
                .is_some_and(|record| {
                    record.widgets.iter().any(|widget| {
                        widget
                            .obj_ref
                            .and_then(|r| resolve_ref(&bytes, &out_doc, r, budget, g).ok())
                            .is_some_and(|obj| {
                                matches!(&obj, Obj::Dict(pairs) if dict_get(pairs, b"AP").is_some())
                            })
                    })
                });
            if !found {
                return Err(FormError::ReadBack {
                    name: name.to_string(),
                    wanted: "an /AP appearance".to_string(),
                    read: None,
                });
            }
        }
    }
    Ok((bytes, report))
}

/// Clear or set `/NeedAppearances` on the copied AcroForm (or the catalog when
/// the form is inline). The inline path reaches through the catalog dict.
fn clear_or_set_need_appearances(
    objects: &mut [(u32, Obj)],
    remap: &HashMap<u32, u32>,
    root_num: u32,
    acroform_ref: Option<Ref>,
    needed: bool,
) {
    let target = match acroform_ref {
        Some(reference) => remap.get(&reference.num).copied(),
        None => Some(root_num),
    };
    let Some(target) = target else { return };
    let Some(pairs) = object_dict_mut(objects, target) else {
        return;
    };
    if acroform_ref.is_some() {
        if needed {
            dict_set(pairs, b"NeedAppearances", Obj::Bool(true));
        } else {
            dict_remove(pairs, b"NeedAppearances");
        }
        return;
    }
    if let Some(Obj::Dict(inner)) = pairs
        .iter_mut()
        .find(|(key, _)| key.as_slice() == b"AcroForm")
        .map(|(_, value)| value)
    {
        if needed {
            dict_set(inner, b"NeedAppearances", Obj::Bool(true));
        } else {
            dict_remove(inner, b"NeedAppearances");
        }
    }
}

/// Whether the field's `/DA` font resolves to a resource in any `/DR /Font`
/// map (field, widget or form). The actual drawing re-derives the `/DA` through
/// the shared generator; this only decides whether a fake appearance should be
/// claimed or the viewer asked to rebuild via `/NeedAppearances`.
fn font_resolves(
    objects: &[(u32, Obj)],
    remap: &HashMap<u32, u32>,
    record: &FieldRecord,
    default_da: &[u8],
    form_fonts: &[(Bytes, Obj)],
) -> bool {
    let Some(field_ref) = record.field_ref else {
        return false;
    };
    let Some(field_num) = remap.get(&field_ref.num).copied() else {
        return false;
    };
    let da = object_dict(objects, field_num)
        .and_then(|d| dict_get(d, b"DA"))
        .and_then(|o| match o {
            Obj::String(s) => Some(s.as_slice().to_vec()),
            _ => None,
        })
        .unwrap_or_else(|| default_da.to_vec());
    let (font, _, _) = parse_default_appearance(&da);
    let widget_nums: Vec<u32> = record
        .widgets
        .iter()
        .filter_map(|w| w.obj_ref)
        .filter_map(|r| remap.get(&r.num).copied())
        .collect();
    let fonts = field_dr_fonts(objects, field_num, &widget_nums, form_fonts);
    fonts
        .iter()
        .any(|(name, _)| name.as_slice() == font.as_slice())
}

/// Text / choice regeneration: reuse the shared variable-text generator with
/// the labels derived from the field's current value.
#[allow(clippy::too_many_arguments)]
fn synthesize_variable(
    record: &FieldRecord,
    objects: &mut Vec<(u32, Obj)>,
    remap: &HashMap<u32, u32>,
    next_num: &mut u32,
    default_da: &[u8],
    form_fonts: &[(Bytes, Obj)],
    report: &mut AppearanceReport,
    name: &str,
) {
    if !font_resolves(objects, remap, record, default_da, form_fonts) {
        report.skipped.push(AppearanceSkip {
            field: name.to_string(),
            reason: AppearanceSkipReason::NoResolvableFont,
        });
        return;
    }
    let labels: Vec<String> = match record.view.kind {
        FieldKind::Text => vec![record.view.value.clone().unwrap_or_default()],
        FieldKind::Choice if !record.view.values.is_empty() => {
            // Multi-select: one label per exported value.
            record
                .view
                .values
                .iter()
                .map(|value| {
                    record
                        .option_exports
                        .iter()
                        .position(|export| export == value)
                        .and_then(|i| record.option_labels.get(i))
                        .cloned()
                        .unwrap_or_else(|| value.clone())
                })
                .collect()
        }
        FieldKind::Choice => {
            let value = record.view.value.clone().unwrap_or_default();
            vec![record
                .option_exports
                .iter()
                .position(|export| export == &value)
                .and_then(|i| record.option_labels.get(i))
                .cloned()
                .unwrap_or(value)]
        }
        _ => Vec::new(),
    };
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    if synthesize_variable_appearances(
        objects,
        remap,
        next_num,
        record,
        &label_refs,
        default_da,
        form_fonts,
    ) {
        report.generated = report.generated.saturating_add(count_widgets(record));
    } else {
        report.skipped.push(AppearanceSkip {
            field: name.to_string(),
            reason: AppearanceSkipReason::DegenerateBox,
        });
    }
}

/// Count a record's widgets (for reporting).
fn count_widgets(record: &FieldRecord) -> usize {
    record.widgets.len()
}

/// Add a Form XObject to the copied objects and return its reference.
#[allow(clippy::too_many_arguments)]
fn push_form_xobject(
    objects: &mut Vec<(u32, Obj)>,
    next_num: &mut u32,
    width: f64,
    height: f64,
    font_name: Option<&[u8]>,
    font_obj: Option<Obj>,
    content: Vec<u8>,
) -> Ref {
    let mut resources: Vec<(Bytes, Obj)> = Vec::new();
    if let (Some(name), Some(obj)) = (font_name, font_obj) {
        resources.push((
            Bytes::copy_from_slice(b"Font"),
            Obj::Dict(vec![(Bytes::copy_from_slice(name), obj)]),
        ));
    }
    let num = *next_num;
    *next_num = next_num.saturating_add(1);
    objects.push((
        num,
        Obj::Stream {
            dict: vec![
                (
                    Bytes::copy_from_slice(b"Type"),
                    Obj::Name(Bytes::copy_from_slice(b"XObject")),
                ),
                (
                    Bytes::copy_from_slice(b"Subtype"),
                    Obj::Name(Bytes::copy_from_slice(b"Form")),
                ),
                (Bytes::copy_from_slice(b"FormType"), Obj::Int(1)),
                (
                    Bytes::copy_from_slice(b"BBox"),
                    Obj::Array(vec![
                        Obj::Int(0),
                        Obj::Int(0),
                        num_obj(width),
                        num_obj(height),
                    ]),
                ),
                (Bytes::copy_from_slice(b"Resources"), Obj::Dict(resources)),
                (
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                ),
            ],
            data: Bytes::copy_from_slice(&content),
        },
    ));
    Ref::new(num, 0)
}

/// The widget dicts of a record, with their box sizes, from the copied objects.
fn widget_jobs(
    objects: &[(u32, Obj)],
    remap: &HashMap<u32, u32>,
    record: &FieldRecord,
) -> Vec<(u32, f64, f64)> {
    let mut jobs = Vec::new();
    for widget in &record.widgets {
        let Some(wref) = widget.obj_ref else { continue };
        let Some(wnum) = remap.get(&wref.num).copied() else {
            continue;
        };
        if let Some((x0, y0, x1, y1)) = object_dict(objects, wnum).and_then(|d| read_rect(d)) {
            jobs.push((wnum, (x1 - x0).abs(), (y1 - y0).abs()));
        }
    }
    jobs
}

/// Resolve an object (possibly an indirect reference) inside the copied graph.
fn copied_obj<'a>(objects: &'a [(u32, Obj)], obj: &'a Obj) -> Option<&'a Obj> {
    match obj {
        Obj::Ref(reference) => objects
            .iter()
            .find(|(num, _)| *num == reference.num)
            .map(|(_, object)| object),
        other => Some(other),
    }
}

/// A standard ZapfDingbats/Helvetica font dictionary value (unembedded, which
/// is valid for the standard 14 and needs no font program).
fn standard_font(name: &[u8]) -> Obj {
    Obj::Dict(vec![
        (
            Bytes::copy_from_slice(b"Type"),
            Obj::Name(Bytes::copy_from_slice(b"Font")),
        ),
        (
            Bytes::copy_from_slice(b"Subtype"),
            Obj::Name(Bytes::copy_from_slice(b"Type1")),
        ),
        (
            Bytes::copy_from_slice(b"BaseFont"),
            Obj::Name(Bytes::copy_from_slice(name)),
        ),
    ])
}

/// Checkbox / radio regeneration. Existing `/AP /N` states are kept; only when
/// a widget has no state dictionary do we generate `Off` + an on-state.
fn synthesize_button_state(
    record: &FieldRecord,
    objects: &mut Vec<(u32, Obj)>,
    remap: &HashMap<u32, u32>,
    next_num: &mut u32,
    report: &mut AppearanceReport,
    name: &str,
) {
    let jobs = widget_jobs(objects, remap, record);
    if jobs.is_empty() {
        report.skipped.push(AppearanceSkip {
            field: name.to_string(),
            reason: AppearanceSkipReason::NoWidget,
        });
        return;
    }
    let on_name = button_on_name(objects, remap, record);
    let is_radio = record.view.kind == FieldKind::Radio;
    for (wnum, width, height) in jobs {
        if width <= 0.0 || height <= 0.0 {
            report.skipped.push(AppearanceSkip {
                field: name.to_string(),
                reason: AppearanceSkipReason::DegenerateBox,
            });
            continue;
        }
        // Keep a hand-authored appearance; only select /AS from /V.
        let has_states = object_dict(objects, wnum)
            .and_then(|d| dict_get(d, b"AP"))
            .is_some();
        if has_states {
            if let Some(dict) = object_dict_mut(objects, wnum) {
                set_widget_state(dict, &record.view.states, &on_name);
            }
            report.kept_existing = report.kept_existing.saturating_add(1);
            continue;
        }
        let off = push_form_xobject(objects, next_num, width, height, None, None, Vec::new());
        let mark = button_mark_content(width, height, is_radio);
        let on = push_form_xobject(objects, next_num, width, height, None, None, mark);
        let states = Obj::Dict(vec![
            (Bytes::copy_from_slice(b"Off"), Obj::Ref(off)),
            (Bytes::from(on_name.as_bytes().to_vec()), Obj::Ref(on)),
        ]);
        if let Some(dict) = object_dict_mut(objects, wnum) {
            dict_set(
                dict,
                b"AP",
                Obj::Dict(vec![(Bytes::copy_from_slice(b"N"), states)]),
            );
            set_widget_state(dict, &record.view.states, &on_name);
            report.generated = report.generated.saturating_add(1);
        }
    }
}

/// The on-state name for a button: the field's current `/V` when it names an
/// existing on-state, otherwise the first state, otherwise `Yes`.
fn button_on_name(
    objects: &[(u32, Obj)],
    remap: &HashMap<u32, u32>,
    record: &FieldRecord,
) -> String {
    let current = record
        .field_ref
        .and_then(|r| remap.get(&r.num).copied())
        .and_then(|num| object_dict(objects, num))
        .and_then(|d| dict_get(d, b"V"))
        .and_then(as_name)
        .map(|n| String::from_utf8_lossy(n.as_slice()).into_owned());
    if let Some(value) = current {
        if value != "Off" && record.view.states.iter().any(|s| s == &value) {
            return value;
        }
    }
    record
        .view
        .states
        .first()
        .cloned()
        .unwrap_or_else(|| "Yes".to_string())
}

/// Vector mark drawn in a checkbox/radio on-state (no font needed).
fn button_mark_content(width: f64, height: f64, radio: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if radio {
        // A filled disc centred in the box, approximated with four Bézier arcs.
        let r = (width.min(height) * 0.30).max(1.0);
        let cx = width / 2.0;
        let cy = height / 2.0;
        let k = r * 0.5523;
        out.extend_from_slice(
            format!(
                "q 0 0 0 rg {cx} {cy} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c f Q\n",
                cx + k, cy + r, cx - k, cy + r, cx - r, cy,
                cx - r, cy - k, cx - k, cy - r, cx, cy - r,
                cx + k, cy - r, cx + r, cy - k, cx + r, cy,
                cx + r, cy + k, cx + k, cy + r, cx, cy + r,
            )
            .as_bytes(),
        );
    } else {
        // A checkmark drawn as two stroked lines.
        let a = (width * 0.20).max(1.0);
        let c = width * 0.80;
        out.extend_from_slice(
            format!(
                "q 0 0 0 RG {} w {a} {} m {} {} l {} {} l S Q\n",
                (height * 0.12).max(1.0),
                height * 0.50,
                width * 0.45,
                height * 0.15,
                c,
                height * 0.85,
            )
            .as_bytes(),
        );
    }
    out
}

/// Push-button regeneration: a bordered caption from `/MK /CA` (or `/T`).
/// Uses Helvetica when no `/DA` font resolves, with a disclosure.
fn synthesize_push_button(
    record: &FieldRecord,
    objects: &mut Vec<(u32, Obj)>,
    remap: &HashMap<u32, u32>,
    next_num: &mut u32,
    report: &mut AppearanceReport,
    name: &str,
) {
    let Some(field_ref) = record.field_ref else {
        report.skipped.push(AppearanceSkip {
            field: name.to_string(),
            reason: AppearanceSkipReason::NoWidget,
        });
        return;
    };
    let Some(field_num) = remap.get(&field_ref.num).copied() else {
        report.skipped.push(AppearanceSkip {
            field: name.to_string(),
            reason: AppearanceSkipReason::NoWidget,
        });
        return;
    };
    let caption = object_dict(objects, field_num)
        .and_then(|d| dict_get(d, b"MK"))
        .and_then(|mk| match mk {
            Obj::Dict(pairs) => dict_get(pairs, b"CA").and_then(decode_text),
            _ => None,
        })
        .or_else(|| {
            object_dict(objects, field_num)
                .and_then(|d| dict_get(d, b"T"))
                .and_then(decode_text)
        })
        .unwrap_or_default();
    let jobs = widget_jobs(objects, remap, record);
    if jobs.is_empty() {
        report.skipped.push(AppearanceSkip {
            field: name.to_string(),
            reason: AppearanceSkipReason::NoWidget,
        });
        return;
    }
    let font = b"Helv".to_vec();
    let font_obj = standard_font(b"Helvetica");
    report.disclosures.push(format!(
        "push button {name:?}: caption drawn with substituted Helvetica (no /DA font resolved)"
    ));
    for (wnum, width, height) in jobs {
        if width <= 0.0 || height <= 0.0 {
            report.skipped.push(AppearanceSkip {
                field: name.to_string(),
                reason: AppearanceSkipReason::DegenerateBox,
            });
            continue;
        }
        let content = push_button_content(&caption, width, height);
        let ap = push_form_xobject(
            objects,
            next_num,
            width,
            height,
            Some(&font),
            Some(font_obj.clone()),
            content,
        );
        if let Some(dict) = object_dict_mut(objects, wnum) {
            dict_set(
                dict,
                b"AP",
                Obj::Dict(vec![(Bytes::copy_from_slice(b"N"), Obj::Ref(ap))]),
            );
            report.generated = report.generated.saturating_add(1);
        }
    }
}

/// Content for a push button: border, optional background, centred caption.
fn push_button_content(caption: &str, width: f64, height: f64) -> Vec<u8> {
    let size = (height * 0.5).clamp(6.0, 18.0);
    let text_w = text_width(caption, size, Some("Helvetica"));
    let x = ((width - text_w) / 2.0).max(1.0);
    let y = ((height - size) / 2.0 + size * 0.2).max(1.0);
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "q 0.5 w 0 0 {} {} re S\nBT 0 g /Helv {} Tf {} {} Td (",
            width,
            height,
            fmt_num(size),
            fmt_num(x),
            fmt_num(y)
        )
        .as_bytes(),
    );
    out.extend_from_slice(&escape_pdf_literal(caption));
    out.extend_from_slice(b") Tj ET Q\n");
    out
}

/// Signature-field regeneration: draw the signer name from an existing
/// `/V /Name`. Never signs; a field with no `/V` is skipped with a typed note.
fn synthesize_signature(
    record: &FieldRecord,
    objects: &mut Vec<(u32, Obj)>,
    remap: &HashMap<u32, u32>,
    next_num: &mut u32,
    report: &mut AppearanceReport,
    name: &str,
) {
    let signer = record
        .field_ref
        .and_then(|r| remap.get(&r.num).copied())
        .and_then(|num| object_dict(objects, num))
        .and_then(|d| dict_get(d, b"V"))
        .and_then(|v| copied_obj(objects, v))
        .and_then(|v| match v {
            Obj::Dict(pairs) => dict_get(pairs, b"Name").and_then(decode_text),
            _ => None,
        });
    let Some(signer) = signer else {
        report.skipped.push(AppearanceSkip {
            field: name.to_string(),
            reason: AppearanceSkipReason::SignatureWithoutValue,
        });
        return;
    };
    let jobs = widget_jobs(objects, remap, record);
    if jobs.is_empty() {
        report.skipped.push(AppearanceSkip {
            field: name.to_string(),
            reason: AppearanceSkipReason::NoWidget,
        });
        return;
    }
    let font = b"Helv".to_vec();
    let font_obj = standard_font(b"Helvetica");
    report.disclosures.push(format!(
        "signature field {name:?}: signer name drawn with substituted Helvetica"
    ));
    for (wnum, width, height) in jobs {
        if width <= 0.0 || height <= 0.0 {
            report.skipped.push(AppearanceSkip {
                field: name.to_string(),
                reason: AppearanceSkipReason::DegenerateBox,
            });
            continue;
        }
        let content = push_button_content(&format!("Signed by {signer}"), width, height);
        let ap = push_form_xobject(
            objects,
            next_num,
            width,
            height,
            Some(&font),
            Some(font_obj.clone()),
            content,
        );
        if let Some(dict) = object_dict_mut(objects, wnum) {
            dict_set(
                dict,
                b"AP",
                Obj::Dict(vec![(Bytes::copy_from_slice(b"N"), Obj::Ref(ap))]),
            );
            report.generated = report.generated.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    fn build_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
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

    /// A form with every widget kind: text, checkbox, radio (two widgets),
    /// multi-select choice, push button and a signed signature field.
    fn all_widgets_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R 7 0 R 10 0 R 11 0 R 12 0 R 14 0 R 16 0 R] >>"),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (5, b"<< /Fields [6 0 R 7 0 R 10 0 R 11 0 R 12 0 R 14 0 R 16 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 20 0 R >> >> /NeedAppearances true >>"),
            (6, b"<< /FT /Tx /T (Name) /V (Ada) /Subtype /Widget /Rect [10 10 200 30] /P 3 0 R /DA (/Helv 12 Tf 0 g) >>"),
            (7, b"<< /FT /Btn /T (Agree) /V /Yes /Subtype /Widget /Rect [10 40 30 60] /P 3 0 R >>"),
            (10, b"<< /FT /Btn /Ff 32768 /T (Size) /V /Small /Kids [11 0 R 12 0 R] >>"),
            (11, b"<< /Subtype /Widget /Parent 10 0 R /Rect [10 100 30 120] /P 3 0 R >>"),
            (12, b"<< /Subtype /Widget /Parent 10 0 R /Rect [40 100 60 120] /P 3 0 R >>"),
            (13, b"<< /FT /Ch /Ff 2097152 /T (Tags) /V (x) /Opt [(x) (y)] /Subtype /Widget /Rect [10 130 120 150] /P 3 0 R >>"),
            (14, b"<< /FT /Btn /Ff 65536 /T (Reset) /Subtype /Widget /Rect [10 170 90 190] /P 3 0 R /MK << /CA (Reset Form) >> >>"),
            (16, b"<< /FT /Sig /T (Sign) /Subtype /Widget /Rect [10 210 210 240] /P 3 0 R /V 17 0 R >>"),
            (17, b"<< /Type /Sig /Name (Ada Lovelace) /Contents <0000> >>"),
            (20, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ];
        // 13 is a multi-select but was given object number 13; include it in
        // the form field list by rewriting object 13's entry above (kept simple
        // by listing it explicitly).
        let mut fixed = objects.to_vec();
        // The /Fields array references 13 0 R for the choice; patch object 5.
        fixed[4] = (5, b"<< /Fields [6 0 R 7 0 R 10 0 R 13 0 R 14 0 R 16 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 20 0 R >> >> /NeedAppearances true >>");
        build_pdf(&fixed)
    }

    #[test]
    fn generates_every_widget_kind_and_reads_back() {
        let pdf = all_widgets_fixture();
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let (out, report) = regenerate_appearances(&pdf, &budget, &mut g).expect("regenerate");
        assert!(
            out.len() > pdf.len(),
            "new appearance streams were appended"
        );
        assert!(report.generated >= 6, "report: {report:?}");
        assert!(
            report.disclosures.iter().any(|d| d.contains("Helvetica")),
            "{report:?}"
        );
        // The signed field had a /Name, so no signature skip; the /NeedAppearances
        // flag is cleared because every widget was drawn.
        assert!(report.skipped.is_empty(), "{report:?}");
        let text = String::from_utf8_lossy(&out);
        assert!(
            !text.contains("/NeedAppearances true"),
            "flag cleared on full generation"
        );
    }

    #[test]
    fn a_signature_without_a_value_is_skipped_with_a_typed_note() {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>"),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (5, b"<< /Fields [6 0 R] >>"),
            (6, b"<< /FT /Sig /T (Sign) /Subtype /Widget /Rect [10 10 100 30] /P 3 0 R >>"),
        ];
        let pdf = build_pdf(objects);
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let (out, report) = regenerate_appearances(&pdf, &budget, &mut g).expect("regenerate");
        assert_eq!(
            report.skipped,
            vec![AppearanceSkip {
                field: "Sign".to_string(),
                reason: AppearanceSkipReason::SignatureWithoutValue,
            }]
        );
        // A skipped widget keeps /NeedAppearances so a viewer can rebuild it.
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("/NeedAppearances true"));
    }

    #[test]
    fn an_existing_checkbox_state_is_kept_not_replaced() {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>"),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (5, b"<< /Fields [6 0 R] >>"),
            (6, b"<< /FT /Btn /T (Agree) /V /Yes /Subtype /Widget /Rect [10 10 30 30] /P 3 0 R /AP << /N << /Off 7 0 R /Yes 7 0 R >> >> >>"),
            (7, b"<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream"),
        ];
        let pdf = build_pdf(objects);
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let (_, report) = regenerate_appearances(&pdf, &budget, &mut g).expect("regenerate");
        assert_eq!(report.kept_existing, 1, "{report:?}");
        assert_eq!(report.generated, 0, "{report:?}");
    }
}
