//! The AcroForm field model of the Selis PDF engine (SL-5.FORM.01, the
//! read/fill half).
//!
//! This crate owns the parts of filling a form that every shell needs: the
//! field tree (hierarchy and inheritance), the widget association, the kinds,
//! the flags, and the value encoding. The browser tools chunk and the CLI are
//! thin wrappers over [`read_form_fields`] and [`fill_form`]; the desktop
//! app can call the same functions with its own budget and its own caller
//! supplied options — nobody re-implements the field walk.
//!
//! The bar for this crate is "form values survive a round trip": the fill
//! reads the document's field tree, writes the caller's values into the
//! copied field dictionaries, then **re-opens the bytes it just produced and
//! reads every filled value back** before the caller receives them. A value
//! that does not round-trip is an error, not a file.
//!
//! What this crate does not do, and must not claim:
//!
//! * with `FillFormsOptions.synthesize_appearances` it writes a real `/AP /N`
//!   Form XObject for every filled text field, single-select choice field
//!   (combo box or list box) and multi-select choice field (SL-5.FORM.02), so
//!   the value shows without a viewer rebuild and can be flattened. Without
//!   that option, values are written to `/V` (and `/I` for choices) and
//!   `/NeedAppearances` asks a conformant viewer to build the display. Button
//!   fields are exact either way, because their appearance states already
//!   exist and this crate selects the state (`/AS`). A choice draws its
//!   selected `/Opt` display labels; a multi-select choice stacks one line per
//!   selection and carries every export value in `/V` and index in `/I`. An
//!   editable choice — single- or multi-select — may carry a typed value that
//!   matches no `/Opt` entry: it is drawn as itself, stored in `/V`, and given
//!   no `/I` index. Quadding uses exact `selis_font::standard14` advance widths
//!   where the field's font resolves to a standard-14 face (a field/widget `/DR`
//!   overrides the form map, and font values may be indirect references or
//!   direct dictionaries). Every single byte is measured by its WinAnsi AFM
//!   glyph name — including ASCII: digits use `zero`..`nine`, `'` uses
//!   `quotesingle`, and `é` (0xE9) uses `eacute` — because that is the glyph
//!   the appearance string draws. Codes with no AFM entry and
//!   embedded/non-standard faces keep an honest fallback.
//!   A field with no resolvable font, widget or box keeps `/NeedAppearances`
//!   rather than claiming an appearance it did not write.
//!   Flattening is a separate task;
//! * it does not evaluate JavaScript, `/CO` calculations, or format actions
//!   (SL-5.FORM.03) — a value is stored as given;
//! * it refuses an encrypted document, an XFA form (the `/XFA` packet is the
//!   authority in such a form, and this crate edits the AcroForm dictionaries),
//!   a signature field, a read-only field and a push button. A multi-select
//!   choice is filled when every selection is an `/Opt` export value or label,
//!   or (when the Edit flag is set) a typed value drawn as itself without an
//!   `/I` index; only an unmatched value on a non-editable choice, or more than
//!   one value on a single-select or non-choice field, is refused. Every
//!   refusal is a typed [`FormError`], never a silently dropped edit;
//! * it reports whether the document carries a signature
//!   ([`FillInfo::has_signature`]) so every shell can disclose, before the
//!   bytes are committed, that a full rewrite invalidates it.
//!
//! The rewrite copies the object graph reachable from the catalog and
//! `/Info` (the same GC the compress operation uses), so values are replaced
//! inside a clean, renumbered document with no superseded bytes left behind.
//!
//! This crate never touches the filesystem or the network; it reads the
//! bytes the caller hands it and returns the bytes the caller hands out.

#![forbid(unsafe_code)]

pub mod formdata;

#[cfg(test)]
mod formdata_tests;

use std::collections::HashMap;
use std::fmt;

use selis_bytes::Bytes;
use selis_pdf_cos::copy::{collect_objects, resolve_ref};
use selis_pdf_cos::doc_writer::write_objects_as_document_with_trailer;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};
use serde::{Deserialize, Serialize};

/// The most field edits one request may carry. A hostile form cannot turn a
/// fill into an unbounded object graph.
pub const MAX_FORM_EDITS: usize = 2_000;
/// The longest field name one edit may carry.
pub const MAX_FIELD_NAME_BYTES: usize = 512;
/// The longest value one edit may carry.
pub const MAX_FIELD_VALUE_BYTES: usize = 8 * 1024;

/// What went wrong filling a form.
///
/// Every variant carries only user-facing text; no variant carries document
/// bytes. Shells may match on the variant — the desktop app can hang its own
/// UX copy off these kinds — while simple callers can print [`Display`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormError {
    /// The bytes are not a readable document, or a value the walk needs is
    /// missing or shaped like the wrong object (e.g. a field that is not a
    /// dictionary in the output).
    Malformed(String),
    /// A referenced object could not be read while copying.
    Unresolvable(String),
    /// The document is encrypted; remove the password before filling.
    Encrypted,
    /// The form carries an XFA packet; editing AcroForm dictionaries under a
    /// live XFA is refused rather than half-applied.
    Xfa,
    /// A limit was exceeded (too many edits, an over-long name or value).
    Limit(String),
    /// The request is invalid on its own terms (an empty field list, an empty
    /// name).
    Invalid(String),
    /// A field refuses the edit: read-only, a signature or push button, a
    /// choice value outside `/Opt`, a button state that does not exist, or a
    /// single-select-only schema being asked for multiple selections.
    Refused(String),
    /// The value written did not read back from the bytes this crate
    /// produced. Carries the field name and both values.
    ReadBack {
        /// The field name.
        name: String,
        /// The value that was written.
        wanted: String,
        /// The value that read back, or nothing if the field did not.
        read: Option<String>,
    },
}

impl fmt::Display for FormError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(detail) | Self::Unresolvable(detail)
            | Self::Limit(detail) | Self::Invalid(detail) | Self::Refused(detail) => {
                f.write_str(detail)
            }
            Self::Encrypted => {
                f.write_str("document is encrypted; remove the password with unlock first")
            }
            Self::Xfa => f.write_str(
                "this form carries an XFA packet; XFA is the authority in such a form and this tool edits AcroForm dictionaries only",
            ),
            Self::ReadBack { name, wanted, read } => write!(
                f,
                "the value for {name:?} did not read back from the filled output (wanted {wanted:?}, read {read:?})"
            ),
        }
    }
}

impl std::error::Error for FormError {}

/// What a fill produced: the bytes plus the flags a caller may want to act
/// on.
///
/// `has_signature` mirrors what [`read_form_fields`] reports — a signature
/// field, or a catalog `/Perms` dictionary carrying `/Signatures` (a
/// certification over the document). Every shell should disclose, before it
/// commits these bytes, that the rewrite invalidates that signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillInfo {
    /// How many fields were set.
    pub filled: usize,
    /// How many fields the form holds.
    pub total: usize,
    /// True when the document carries a signature this rewrite invalidates.
    pub has_signature: bool,
}

/// The kind of an AcroForm field, as far as this tool understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// A text field (`/FT /Tx`).
    Text,
    /// A checkbox (`/FT /Btn` without the radio or push-button flags).
    Checkbox,
    /// A radio group (`/FT /Btn` with the radio flag), one field with one or
    /// more widget children.
    Radio,
    /// A choice field (`/FT /Ch`).
    Choice,
    /// A signature field (`/FT /Sig`). Listed, never filled.
    Signature,
    /// A push button (`/FT /Btn` with the push-button flag). Listed, never
    /// filled: it has no value.
    PushButton,
    /// A field whose `/FT` is absent or outside the standard set.
    Unknown,
}

/// One form field, as read from the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormField {
    /// The fully qualified name: the `/T` chain joined with dots.
    pub name: String,
    /// The field's kind.
    pub kind: FieldKind,
    /// The current value as text. Button fields report their state name
    /// (`Off` when unset); a field with no `/V` reports `null`. A multi-select
    /// choice whose `/V` is an array reports its selections in [`Self::values`]
    /// and leaves this as `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// A multi-select choice's selected export values, in `/V` order. Empty for
    /// every other field (and for a single-valued choice, which uses
    /// [`Self::value`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// A choice field's export values, in `/Opt` order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// A choice field's display labels, parallel to `options` when the
    /// document distinguishes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Choice fields with the edit flag: any text is accepted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub editable: bool,
    /// Choice fields with the multi-select flag: one or more values can be set.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multi_select: bool,
    /// The text field's `/MaxLen` limit, when the document sets one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u64>,
    /// A button field's selectable state names (everything the widgets offer
    /// except `Off`), in first-seen order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    /// The read-only flag (bit 1).
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// The required flag (bit 2).
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// The 1-based page of the field's first widget, when the widget names
    /// one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
}

/// The read-only field listing produced by [`read_form_fields`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormFieldsReport {
    /// `ok` for a document whose field tree was readable.
    pub status: String,
    /// The document's page count, when it could be counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u64>,
    /// True when at least one signature field exists: a subsequent full
    /// rewrite will invalidate any signature over this document.
    pub has_signature: bool,
    /// The fields, in `/Fields` walk order (parents before children).
    pub fields: Vec<FormField>,
}

impl FormFieldsReport {
    /// The report as pretty JSON.
    ///
    /// # Errors
    ///
    /// [`FormError::Malformed`] when the report cannot be encoded (never for a
    /// report this crate built).
    pub fn to_json(&self) -> Result<Vec<u8>, FormError> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| FormError::Malformed(format!("cannot encode the form-field report: {e}")))
    }
}

/// One field to set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldEdit {
    /// The fully qualified field name, exactly as [`read_form_fields`]
    /// reports it.
    pub name: String,
    /// The value: text for `/Tx`, an export value for `/Ch`, a state name
    /// (`Yes`/`Off`, or `true`/`false` for a single-state checkbox) for
    /// `/Btn`. For a multi-select choice this is the first selection.
    pub value: String,
    /// Extra selections for a multi-select choice field. Ignored for every
    /// other field kind (a non-empty list there is refused, not silently
    /// dropped). Empty for the usual single-value edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

impl FieldEdit {
    /// Every value this edit asks to store, with `value` first.
    ///
    /// # Budget
    ///
    /// One small vector proportional to the edit's (caller-bounded) value
    /// count; no document objects are touched.
    ///
    /// # Malformed Input
    ///
    /// None: an empty `value` still yields one (empty) selection, which the
    /// field's planner validates against its own rules.
    fn selections(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(self.values.len().saturating_add(1));
        out.push(self.value.as_str());
        out.extend(self.values.iter().map(String::as_str));
        out
    }
}

/// Options for [`fill_form`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FillFormsOptions {
    /// The values to set. Every name must match exactly one field; an empty
    /// list is refused rather than silently rewriting the document.
    pub fields: Vec<FieldEdit>,
    /// Write a real `/AP /N` appearance stream for each filled **text**,
    /// **single-select choice / list box** and **multi-select choice** field,
    /// not only `/V` + `/NeedAppearances` (SL-5.FORM.02). With this off (the
    /// default) a viewer that honours `/NeedAppearances` rebuilds the display
    /// and a viewer that does not shows an empty box; with it on the value is
    /// drawn into a Form XObject the widget carries, so it is visible in every
    /// viewer and survives flattening. The appearance follows the field's
    /// `/DA` (font, size, colour) resolved against its own or a widget's `/DR`
    /// when present, otherwise the form's `/DR`; honours `/Q` (quadding) and
    /// the multiline flag; and sizes auto (`/DA` size `0`) text to the box.
    /// Centring / right-alignment uses exact `selis_font::standard14` advance
    /// widths when the resolved font is a standard-14 face — including the
    /// face's WinAnsi widths for single bytes 0x80–0xFF — with an honest
    /// fallback for unmappable glyphs and a 0.5 em heuristic for embedded /
    /// non-standard faces. Font values in any `/DR /Font` map may be indirect
    /// references or direct dictionaries; both are resolved.
    ///
    /// Choice handling: `/FT /Ch` with the Combo flag (a drop-down) or without
    /// it (a list box) is drawn when one value is selected. A non-edit choice
    /// draws the selected option's **display label** from `/Opt` while `/V`
    /// keeps the export value and `/I` the index; an editable choice whose
    /// value is not an option draws that typed value itself and gives it no
    /// `/I` index. A multi-select choice (the `/Ff` MultiSelect bit) stacks one
    /// label per selection in the widget box and writes every export value to
    /// `/V` and every matched index to `/I` (`FieldEdit::values` holds the extra
    /// selections). Signature fields, push buttons and read-only fields are
    /// refused by type and never get an appearance here. An unmatched value on
    /// a non-editable choice, or multiple values on a single-select / non-choice
    /// field, is refused too. A field with no resolvable font in either `/DR`,
    /// no widget or no usable `/Rect` is not given a fake appearance:
    /// `/NeedAppearances` stays set so a viewer is asked to rebuild it instead.
    /// Button appearances already exist (the fill selects `/AS`) and are
    /// untouched by this flag.
    pub synthesize_appearances: bool,
}

/// Read the document's AcroForm fields.
///
/// This parses the document, refuses it when it is encrypted, and walks the
/// field tree. Nothing is rewritten.
///
/// # Errors
///
/// [`FormError::Malformed`] when the bytes are not a readable document,
/// [`FormError::Encrypted`] when they carry `/Encrypt` (its dictionaries are
/// still ciphertext without the file key), or [`FormError::Unresolvable`]
/// when an object the walk needs cannot be read.
pub fn read_form_fields(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<FormFieldsReport, FormError> {
    let doc = open_doc(src, budget, g)?;
    if is_encrypted(&doc) {
        return Err(FormError::Encrypted);
    }
    let pages = page_refs(src, &doc, budget, g)
        .ok()
        .map(|refs| u64::try_from(refs.len()).unwrap_or(u64::MAX));
    let walked = walk_records(src, &doc, budget, g)?;
    Ok(FormFieldsReport {
        status: "ok".to_string(),
        pages,
        has_signature: walked.has_signature,
        fields: walked
            .records
            .into_iter()
            .map(|record| record.view)
            .collect(),
    })
}

/// Fill the named fields in a document and return the rewritten bytes.
///
/// The caller owns the bytes: this function never touches a filesystem or a
/// network. Every filled value is re-read from the produced bytes before
/// they are returned; a value that does not round-trip is
/// [`FormError::ReadBack`], not a file for the caller to commit.
///
/// [`FillInfo`] carries what a caller may want to act on — in particular
/// whether the document carries a signature this rewrite invalidates.
///
/// # Errors
///
/// [`FormError::Invalid`]/[`FormError::Limit`] for the request itself,
/// [`FormError::Encrypted`]/[`FormError::Xfa`] for the document class,
/// [`FormError::Refused`] for a field that refuses the edit,
/// [`FormError::Malformed`]/[`FormError::Unresolvable`] for a byte stream the
/// rewrite cannot produce honestly.
pub fn fill_form(
    src: &[u8],
    options: &FillFormsOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, FillInfo), FormError> {
    if options.fields.is_empty() {
        return Err(FormError::Invalid(
            "no form fields to fill; name at least one field".to_string(),
        ));
    }
    if options.fields.len() > MAX_FORM_EDITS {
        return Err(FormError::Limit(format!(
            "a fill may edit at most {MAX_FORM_EDITS} fields (got {})",
            options.fields.len()
        )));
    }
    for edit in &options.fields {
        if edit.name.is_empty() {
            return Err(FormError::Invalid("a field name is empty".to_string()));
        }
        if edit.name.len() > MAX_FIELD_NAME_BYTES {
            return Err(FormError::Limit(format!(
                "field name {:?} is longer than {MAX_FIELD_NAME_BYTES} bytes",
                edit.name
            )));
        }
        if edit.value.len() > MAX_FIELD_VALUE_BYTES {
            return Err(FormError::Limit(format!(
                "the value for {:?} is longer than {MAX_FIELD_VALUE_BYTES} bytes",
                edit.name
            )));
        }
        if edit.value.contains('\0') {
            return Err(FormError::Invalid(format!(
                "the value for {:?} contains a NUL character",
                edit.name
            )));
        }
        for extra in &edit.values {
            if extra.len() > MAX_FIELD_VALUE_BYTES {
                return Err(FormError::Limit(format!(
                    "a value for {:?} is longer than {MAX_FIELD_VALUE_BYTES} bytes",
                    edit.name
                )));
            }
            if extra.contains('\0') {
                return Err(FormError::Invalid(format!(
                    "a value for {:?} contains a NUL character",
                    edit.name
                )));
            }
        }
    }

    let doc = open_doc(src, budget, g)?;
    if is_encrypted(&doc) {
        return Err(FormError::Encrypted);
    }

    let walked = walk_records(src, &doc, budget, g)?;
    if walked.has_xfa {
        return Err(FormError::Xfa);
    }

    // Plan every edit against the field model first: a refusal must leave the
    // output untouched, and no partial plan may reach the writer.
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, record) in walked.records.iter().enumerate() {
        by_name
            .entry(record.view.name.as_str())
            .or_default()
            .push(index);
    }
    let mut planned: Vec<(usize, Plan)> = Vec::with_capacity(options.fields.len());
    for edit in &options.fields {
        let matches = by_name
            .get(edit.name.as_str())
            .map_or(&[][..], Vec::as_slice);
        let index = match matches {
            [] => {
                return Err(FormError::Refused(format!(
                    "no form field is named {:?}; use the field list to see the names",
                    edit.name
                )))
            }
            [one] => *one,
            _ => {
                return Err(FormError::Refused(format!(
                    "the name {:?} matches {} fields; this document repeats the name",
                    edit.name,
                    matches.len()
                )))
            }
        };
        let record = walked.records.get(index).ok_or_else(|| {
            FormError::Malformed(format!(
                "form field {:?} disappeared from the model",
                edit.name
            ))
        })?;
        let plan = plan_edit(record, edit)?;
        if planned.iter().any(|(other, _)| *other == index) {
            return Err(FormError::Invalid(format!(
                "the field {:?} is named twice in one request",
                edit.name
            )));
        }
        planned.push((index, plan));
    }

    // Copy the reachable graph (catalog + /Info), then apply the plan to the
    // copies. References are already renumbered, so every mutation is by the
    // remapped object number.
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

    for (index, plan) in &planned {
        let record = walked.records.get(*index).ok_or_else(|| {
            FormError::Malformed("a planned field disappeared from the model".to_string())
        })?;
        apply_plan(&mut objects, &remap, record, plan)?;
    }
    // Text and choice values need an appearance a viewer can show. With
    // synthesis on we draw text fields and single-select choices/listboxes
    // ourselves (a real `/AP /N` the value is visible in, and the flattener can
    // bake); anything we do not synthesize (a field with no resolvable font, no
    // widget or a degenerate box) still asks the viewer to rebuild via
    // `/NeedAppearances` — we never claim an appearance we did not write.
    let has_variable = planned.iter().any(|(_, plan)| {
        matches!(
            plan,
            Plan::Text(_) | Plan::Choice { .. } | Plan::ChoiceMulti { .. }
        )
    });
    if has_variable {
        let mut needs_viewer_rebuild = !options.synthesize_appearances;
        if options.synthesize_appearances {
            let (default_da, dr_fonts) = form_defaults(&objects, &remap, root.num, acroform_ref);
            for (index, plan) in &planned {
                let record = walked.records.get(*index).ok_or_else(|| {
                    FormError::Malformed("a planned field disappeared from the model".to_string())
                })?;
                match plan {
                    Plan::Text(value) => {
                        if !synthesize_text_appearances(
                            &mut objects,
                            &remap,
                            &mut next_num,
                            record,
                            value,
                            &default_da,
                            &dr_fonts,
                        ) {
                            needs_viewer_rebuild = true;
                        }
                    }
                    Plan::Choice { .. } | Plan::ChoiceMulti { .. } => {
                        if !synthesize_choice_appearances(
                            &mut objects,
                            &remap,
                            &mut next_num,
                            record,
                            plan,
                            &default_da,
                            &dr_fonts,
                        ) {
                            needs_viewer_rebuild = true;
                        }
                    }
                    Plan::Button { .. } => {}
                }
            }
        }
        if needs_viewer_rebuild {
            set_need_appearances(&mut objects, &remap, root.num, acroform_ref);
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

    // Read-back: re-open the bytes this crate produced and compare every
    // filled value with what was asked for. A value that does not survive
    // the round trip is an error, never a deliverable.
    let out_startxref = selis_pdf_cos::xref::find_startxref(&bytes, 4096).unwrap_or(0);
    let out_doc = selis_pdf_cos::parse_revisions(&bytes, out_startxref, budget, g)
        .map_err(|e| FormError::Malformed(format!("the filled output does not reparse: {e}")))?;
    let out_walked = walk_records(&bytes, &out_doc, budget, g)?;
    for (index, plan) in &planned {
        let record = walked.records.get(*index).ok_or_else(|| {
            FormError::Malformed("a planned field disappeared from the model".to_string())
        })?;
        let read_back = out_walked
            .records
            .iter()
            .find(|candidate| candidate.view.name == record.view.name)
            .ok_or_else(|| FormError::ReadBack {
                name: record.view.name.clone(),
                wanted: plan_wanted(plan),
                read: None,
            })?;
        let expected = plan_wanted(plan);
        let matches = match plan {
            Plan::ChoiceMulti { values, .. } => {
                read_back.view.values.as_slice() == values.as_slice()
            }
            _ => read_back.view.value.as_deref() == Some(expected.as_str()),
        };
        if !matches {
            let read = match plan {
                Plan::ChoiceMulti { .. } => Some(read_back.view.values.join(", ")),
                _ => read_back.view.value.clone(),
            };
            return Err(FormError::ReadBack {
                name: record.view.name.clone(),
                wanted: expected,
                read,
            });
        }
    }

    let total = walked.records.len();
    Ok((
        bytes,
        FillInfo {
            filled: planned.len(),
            total,
            has_signature: walked.has_signature,
        },
    ))
}

/// Parse the document, or a typed failure — the one validation both public
/// entries share.
fn open_doc(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<selis_pdf_cos::Doc, FormError> {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).unwrap_or(0);
    selis_pdf_cos::parse_revisions(src, startxref, budget, g)
        .map_err(|e| FormError::Malformed(format!("cannot open: {e}")))
}

/// True when the document carries `/Encrypt` in its newest revision.
fn is_encrypted(doc: &selis_pdf_cos::Doc) -> bool {
    doc.revisions()
        .last()
        .is_some_and(|revision| revision.encrypt.is_some())
}

/// The 1-based page of each leaf page object, walking from the catalog.
///
/// An `/Pages` node with no resolvable `/Kids` is counted as a leaf; a page
/// the walker cannot parse simply contributes no pages, which keeps a
/// hostile tree from spinning the walk.
fn page_refs(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<Ref>, FormError> {
    let revision = doc
        .revisions()
        .last()
        .ok_or_else(|| FormError::Malformed("no revisions".to_string()))?;
    let root = revision
        .root
        .ok_or_else(|| FormError::Malformed("no /Root in trailer".to_string()))?;
    let root_obj = resolve_ref(src, doc, root, budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot resolve /Root: {e}")))?;
    let pages_ref = match &root_obj {
        Obj::Dict(pairs) => dict_get(pairs, b"Pages").and_then(|value| match value {
            Obj::Ref(reference) => Some(*reference),
            _ => None,
        }),
        _ => None,
    }
    .ok_or_else(|| FormError::Malformed("catalog has no /Pages".to_string()))?;
    let mut out = Vec::new();
    walk_pages(src, doc, pages_ref, budget, g, 0, &mut out)?;
    Ok(out)
}

/// One page-tree node, recursively.
#[allow(clippy::too_many_arguments)]
fn walk_pages(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    node: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    depth: usize,
    out: &mut Vec<Ref>,
) -> Result<(), FormError> {
    if depth > 32 {
        return Ok(());
    }
    let obj = resolve_ref(src, doc, node, budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot resolve page node: {e}")))?;
    let Obj::Dict(pairs) = &obj else {
        return Ok(());
    };
    let ty = dict_get(pairs, b"Type").and_then(as_name);
    match ty.as_deref() {
        Some(b"Page") => {
            out.push(node);
            Ok(())
        }
        Some(b"Pages") | None => {
            let Some(Obj::Array(kids)) = dict_get(pairs, b"Kids") else {
                return Ok(());
            };
            for kid in kids {
                if let Obj::Ref(reference) = kid {
                    walk_pages(
                        src,
                        doc,
                        *reference,
                        budget,
                        g,
                        depth.saturating_add(1),
                        out,
                    )?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The string a plan asks the writer to store. For a multi-select choice the
/// selections are joined for diagnostics only; read-back compares the array.
///
/// # Budget
///
/// One owned string bounded by the plan's value count.
///
/// # Malformed Input
///
/// None: every plan variant this crate builds has a representable string.
fn plan_wanted(plan: &Plan) -> String {
    match plan {
        Plan::Text(value) => value.clone(),
        Plan::Choice { value, .. } => value.clone(),
        Plan::ChoiceMulti { values, .. } => values.join(", "),
        Plan::Button { on } => on.clone(),
    }
}

/// The outcome of one field-tree walk.
struct Walked {
    records: Vec<FieldRecord>,
    has_xfa: bool,
    /// Any `/FT /Sig` field, or a catalog `/Perms` dictionary carrying
    /// `/Signatures` (a certification over the document).
    has_signature: bool,
}

/// A field plus the wiring the writer needs: object references and the
/// per-widget appearance states.
struct FieldRecord {
    view: FormField,
    /// The field dictionary, when it is an indirect object.
    field_ref: Option<Ref>,
    /// The widgets, in `/Kids` order (or the field itself when it is merged
    /// with its single widget).
    widgets: Vec<WidgetRecord>,
    /// Choice export values, in `/Opt` order.
    option_exports: Vec<String>,
    /// Choice display labels, parallel to `option_exports`.
    option_labels: Vec<String>,
    /// Choice fields with the edit flag set accept any text.
    editable: bool,
    /// Choice fields with the multi-select flag set accept several values.
    multi_select: bool,
    /// `/MaxLen` for a text field, when present.
    max_len: Option<u64>,
}

/// One widget dictionary and the appearance states it offers.
struct WidgetRecord {
    obj_ref: Option<Ref>,
    /// Named `/AP /N` states (empty when the widget has no state dictionary).
    states: Vec<String>,
    /// The object number of the widget's `/P` page, when it names one.
    page_ref: Option<u32>,
}

/// The normalized edit a plan carries into the writer.
enum Plan {
    /// A text value.
    Text(String),
    /// A choice value and, when it matched an option, the option's index.
    Choice { value: String, index: Option<usize> },
    /// Several selections on a multi-select choice: export values in `/V`
    /// order and, for each, its `/Opt` index — `None` for an editable typed
    /// value that matches no option. Such a value is stored in `/V` but has no
    /// index, so it is deliberately left out of `/I` (an honest representation
    /// rather than a fabricated index) and drawn as its own typed label.
    ChoiceMulti {
        values: Vec<String>,
        indices: Vec<Option<usize>>,
    },
    /// A button state: `Off` or an existing on-state name.
    Button { on: String },
}

/// One walk context: what a field inherits from its ancestors.
#[derive(Clone, Default)]
struct Ctx {
    name: String,
    ft: Option<Bytes>,
    flags: i64,
    value: Option<Obj>,
    opt: Option<Obj>,
    max_len: Option<i64>,
}

/// Walk `/AcroForm /Fields` into flat records.
fn walk_records(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Walked, FormError> {
    let Some(acro) = selis_pdf_cos::reconcile::catalog_entry(src, doc, b"AcroForm", budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot read /AcroForm: {e}")))?
    else {
        return Ok(Walked {
            records: Vec::new(),
            has_xfa: false,
            has_signature: false,
        });
    };
    let acro = resolve_deep(src, doc, acro, 0, budget, g);
    let Obj::Dict(pairs) = &acro else {
        return Ok(Walked {
            records: Vec::new(),
            has_xfa: false,
            has_signature: false,
        });
    };
    let has_xfa = dict_get(pairs, b"XFA").is_some();
    let page_of = page_index(src, doc, budget, g);
    let mut records = Vec::new();
    if let Some(Obj::Array(fields)) = dict_get(pairs, b"Fields") {
        for item in fields {
            walk_field(
                src,
                doc,
                item,
                &Ctx::default(),
                &page_of,
                budget,
                g,
                0,
                &mut records,
            )?;
        }
    }
    let has_signature = records
        .iter()
        .any(|record| record.view.kind == FieldKind::Signature)
        || document_carries_signature(src, doc, budget, g);
    Ok(Walked {
        records,
        has_xfa,
        has_signature,
    })
}

/// Detect a document signature: a catalog `/Perms` *dictionary* carrying
/// `/Signatures` (the permission-bits form of `/Perms` is an integer and
/// carries no signature).
fn document_carries_signature(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> bool {
    match selis_pdf_cos::reconcile::catalog_entry(src, doc, b"Perms", budget, g) {
        Ok(Some(perms)) => {
            let perms = resolve_deep(src, doc, perms, 0, budget, g);
            matches!(&perms, Obj::Dict(pairs)
                if pairs.iter().any(|(key, _)| key.as_slice() == b"Signatures"))
        }
        _ => false,
    }
}

/// The object number of a widget's `/P` page, when it names one.
fn widget_page_ref(pairs: &[(Bytes, Obj)]) -> Option<u32> {
    match dict_get(pairs, b"P")? {
        Obj::Ref(reference) => Some(reference.num),
        _ => None,
    }
}

/// A widget's page number (1-based), keyed by the page object number.
fn page_index(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    if let Ok(refs) = page_refs(src, doc, budget, g) {
        for (index, page_ref) in refs.iter().enumerate() {
            let page = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
            map.insert(page_ref.num, page);
        }
    }
    map
}

/// One field (recursively; its children become their own records).
#[allow(clippy::too_many_arguments)]
fn walk_field(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    item: &Obj,
    ctx: &Ctx,
    page_of: &HashMap<u32, u32>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    depth: usize,
    out: &mut Vec<FieldRecord>,
) -> Result<(), FormError> {
    if depth > 32 {
        return Ok(());
    }
    let item_ref = match item {
        Obj::Ref(reference) => Some(*reference),
        _ => None,
    };
    let resolved = resolve_deep(src, doc, item.clone(), 0, budget, g);
    let Obj::Dict(pairs) = &resolved else {
        return Ok(());
    };
    let own_name = dict_get(pairs, b"T").and_then(decode_text);
    let name = join_name(&ctx.name, own_name.as_deref());
    let ft = dict_get(pairs, b"FT")
        .and_then(as_name)
        .or_else(|| ctx.ft.clone());
    let flags = dict_get(pairs, b"Ff").and_then(as_i64).unwrap_or(ctx.flags);
    let value = dict_get(pairs, b"V").cloned().or_else(|| ctx.value.clone());
    let opt = dict_get(pairs, b"Opt").cloned().or_else(|| ctx.opt.clone());
    let max_len = dict_get(pairs, b"MaxLen").and_then(as_i64).or(ctx.max_len);

    let mut widgets: Vec<WidgetRecord> = Vec::new();
    let mut children: Vec<Obj> = Vec::new();
    if let Some(Obj::Array(kids)) = dict_get(pairs, b"Kids") {
        for kid in kids {
            let kid_ref = match kid {
                Obj::Ref(reference) => Some(*reference),
                _ => None,
            };
            let kid_resolved = resolve_deep(src, doc, kid.clone(), 0, budget, g);
            let Obj::Dict(kid_pairs) = &kid_resolved else {
                continue;
            };
            if is_widget(kid_pairs) {
                widgets.push(WidgetRecord {
                    obj_ref: kid_ref,
                    states: widget_states(src, doc, kid_pairs, budget, g),
                    page_ref: widget_page_ref(kid_pairs),
                });
            } else {
                children.push(kid_resolved);
            }
        }
    } else if is_widget(pairs) {
        widgets.push(WidgetRecord {
            obj_ref: item_ref,
            states: widget_states(src, doc, pairs, budget, g),
            page_ref: widget_page_ref(pairs),
        });
    }

    if !children.is_empty() {
        let sub = Ctx {
            name: name.clone(),
            ft: ft.clone(),
            flags,
            value: value.clone(),
            opt: opt.clone(),
            max_len,
        };
        for child in &children {
            walk_field(
                src,
                doc,
                child,
                &sub,
                page_of,
                budget,
                g,
                depth.saturating_add(1),
                out,
            )?;
        }
        return Ok(());
    }

    let kind = field_kind(ft.as_deref(), flags);
    let (option_exports, option_labels) = match opt.as_ref() {
        Some(value) => choice_options(value),
        None => (Vec::new(), Vec::new()),
    };
    let mut states: Vec<String> = Vec::new();
    for widget in &widgets {
        for state in &widget.states {
            if state != "Off" && !states.iter().any(|seen| seen == state) {
                states.push(state.clone());
            }
        }
    }
    let page = widgets
        .iter()
        .find_map(|widget| widget.page_ref.and_then(|num| page_of.get(&num).copied()));
    out.push(FieldRecord {
        view: FormField {
            name,
            kind,
            value: decode_field_value(kind, value.as_ref()),
            values: if kind == FieldKind::Choice {
                choice_values(value.as_ref())
            } else {
                Vec::new()
            },
            options: option_exports.clone(),
            labels: option_labels.clone(),
            editable: flags & (1 << 18) != 0,
            multi_select: flags & (1 << 21) != 0,
            max_len: max_len.and_then(|value| u64::try_from(value).ok()),
            states,
            read_only: flags & 1 != 0,
            required: flags & 2 != 0,
            page,
        },
        field_ref: item_ref,
        widgets,
        option_exports,
        option_labels,
        editable: flags & (1 << 18) != 0,
        multi_select: flags & (1 << 21) != 0,
        max_len: max_len.and_then(|value| u64::try_from(value).ok()),
    });
    Ok(())
}

/// The kind a `/FT` name and flags describe.
fn field_kind(ft: Option<&[u8]>, flags: i64) -> FieldKind {
    match ft {
        Some(b"Tx") => FieldKind::Text,
        Some(b"Ch") => FieldKind::Choice,
        Some(b"Sig") => FieldKind::Signature,
        Some(b"Btn") => {
            if flags & (1 << 16) != 0 {
                FieldKind::PushButton
            } else if flags & (1 << 15) != 0 {
                FieldKind::Radio
            } else {
                FieldKind::Checkbox
            }
        }
        _ => FieldKind::Unknown,
    }
}

/// Whether a dictionary is a widget annotation.
fn is_widget(pairs: &[(Bytes, Obj)]) -> bool {
    if let Some(Obj::Name(subtype)) = dict_get(pairs, b"Subtype") {
        return subtype.as_slice() == b"Widget";
    }
    // Merged fields occasionally omit /Subtype; /Rect plus none of the field
    // keys is a widget by shape (or a leaf container, which the record still
    // describes usefully).
    dict_get(pairs, b"Rect").is_some()
        && dict_get(pairs, b"T").is_none()
        && dict_get(pairs, b"FT").is_none()
        && dict_get(pairs, b"Kids").is_none()
}

/// The named appearance states a widget's `/AP /N` dictionary offers.
fn widget_states(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    pairs: &[(Bytes, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Vec<String> {
    let Some(ap) = dict_get(pairs, b"AP") else {
        return Vec::new();
    };
    let ap = resolve_deep(src, doc, ap.clone(), 0, budget, g);
    let Obj::Dict(ap_pairs) = &ap else {
        return Vec::new();
    };
    let Some(normal) = dict_get(ap_pairs, b"N") else {
        return Vec::new();
    };
    let normal = resolve_deep(src, doc, normal.clone(), 0, budget, g);
    let Obj::Dict(state_pairs) = &normal else {
        return Vec::new();
    };
    state_pairs
        .iter()
        .filter_map(|(key, _)| std::str::from_utf8(key.as_slice()).ok().map(str::to_string))
        .collect()
}

/// A choice field's `/Opt` array as (exports, labels).
fn choice_options(opt: &Obj) -> (Vec<String>, Vec<String>) {
    let resolved = match opt {
        Obj::Array(items) => items,
        _ => return (Vec::new(), Vec::new()),
    };
    let mut exports = Vec::new();
    let mut labels = Vec::new();
    for item in resolved {
        match item {
            Obj::String(_) | Obj::Name(_) => {
                let Some(text) = decode_text(item) else {
                    continue;
                };
                exports.push(text.clone());
                labels.push(text);
            }
            Obj::Array(pair) => {
                let export = pair.first().and_then(decode_text);
                let label = pair.get(1).and_then(decode_text);
                let Some(export) = export else { continue };
                let label = label.unwrap_or_else(|| export.clone());
                exports.push(export);
                labels.push(label);
            }
            _ => {}
        }
    }
    (exports, labels)
}

/// A field's current value as text. A multi-select choice (`/V` is an array)
/// has no single value; its selections are reported by [`choice_values`].
///
/// # Budget
///
/// One borrowed object read; no allocation beyond the decoded string.
///
/// # Malformed Input
///
/// A `/V` of an unexpected object shape decodes to `None` rather than a
/// guessed value.
fn decode_field_value(kind: FieldKind, value: Option<&Obj>) -> Option<String> {
    let value = value?;
    if matches!(value, Obj::Array(_)) {
        return None;
    }
    match kind {
        FieldKind::Checkbox | FieldKind::Radio | FieldKind::PushButton => {
            let text = decode_text(value)?;
            Some(text)
        }
        FieldKind::Text | FieldKind::Choice | FieldKind::Signature | FieldKind::Unknown => {
            decode_text(value)
        }
    }
}

/// A multi-select choice's selected values from an array `/V`. Every other
/// value shape is empty (the single value lives in `FormField::value`).
///
/// # Budget
///
/// Allocations are bounded by the number of entries in `/V`, which is one of
/// the copied document's objects.
///
/// # Malformed Input
///
/// Array entries that are not decodable text are skipped; a `/V` that is not
/// an array yields an empty list.
fn choice_values(value: Option<&Obj>) -> Vec<String> {
    let Some(Obj::Array(items)) = value else {
        return Vec::new();
    };
    items.iter().filter_map(decode_text).collect()
}

/// Join a parent's qualified name with the field's own partial name.
fn join_name(parent: &str, own: Option<&str>) -> String {
    match (parent.is_empty(), own) {
        (_, None) | (true, Some("")) => parent.to_string(),
        (true, Some(own)) => own.to_string(),
        (false, Some("")) => parent.to_string(),
        (false, Some(own)) => format!("{parent}.{own}"),
    }
}

/// Resolve one choice selection to `(export value, /Opt index)`, matching an
/// export value first and a display label second.
///
/// # Budget
///
/// Two linear scans of the field's `/Opt` arrays, bounded by the copied object
/// graph; one owned string on a match.
///
/// # Malformed Input
///
/// A selection that matches neither export nor label yields `None`, so the
/// caller refuses it instead of storing a value with no index.
fn resolve_choice_selection(record: &FieldRecord, raw: &str) -> Option<(String, usize)> {
    if let Some(index) = record
        .option_exports
        .iter()
        .position(|export| export == raw)
    {
        return Some((raw.to_string(), index));
    }
    record
        .option_labels
        .iter()
        .position(|label| label == raw)
        .map(|index| {
            (
                record
                    .option_exports
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| raw.to_string()),
                index,
            )
        })
}

/// Validate an edit against its field and normalize it.
///
/// # Budget
///
/// Allocations are bounded by the edit's value count (at most
/// [`MAX_FORM_EDITS`] times the value limit, checked by the caller) and
/// the field's `/Opt` length, both bounded by the copied object graph.
///
/// # Malformed Input
///
/// A multi-value edit on a non-multi-select field, or a selection that matches
/// no `/Opt` entry on a non-editable multi-select field, is a typed
/// [`FormError::Refused`]; nothing is partially planned. On an editable
/// multi-select field an unmatched typed value is kept with no `/I` index.
fn plan_edit(record: &FieldRecord, edit: &FieldEdit) -> Result<Plan, FormError> {
    let name = record.view.name.as_str();
    let selections = edit.selections();
    if record.view.read_only {
        return Err(FormError::Refused(format!(
            "the field {name:?} is read-only; this tool does not override the flag"
        )));
    }
    // Only a multi-select choice takes more than one value.
    if selections.len() > 1 && !(record.view.kind == FieldKind::Choice && record.multi_select) {
        return Err(FormError::Refused(format!(
            "the field {name:?} takes a single value, but {} were supplied",
            selections.len()
        )));
    }
    let raw = selections.first().copied().unwrap_or("");
    match record.view.kind {
        FieldKind::Text => {
            if let Some(max) = record.max_len {
                let length = u64::try_from(raw.chars().count()).unwrap_or(u64::MAX);
                if length > max {
                    return Err(FormError::Refused(format!(
                        "the value for {name:?} has {length} characters but the field allows {max}"
                    )));
                }
            }
            Ok(Plan::Text(raw.to_string()))
        }
        FieldKind::Checkbox => {
            if record.view.states.is_empty() {
                return Err(FormError::Refused(format!(
                    "the checkbox {name:?} carries no appearance states, so its on-state cannot be determined"
                )));
            }
            if raw.eq_ignore_ascii_case("off") || raw.eq_ignore_ascii_case("false") || raw.is_empty()
            {
                return Ok(Plan::Button {
                    on: "Off".to_string(),
                });
            }
            if let Some(state) = record.view.states.iter().find(|state| *state == raw) {
                return Ok(Plan::Button {
                    on: state.clone(),
                });
            }
            if record.view.states.len() == 1
                && (raw.eq_ignore_ascii_case("true") || raw.eq_ignore_ascii_case("yes"))
            {
                if let Some(state) = record.view.states.first() {
                    return Ok(Plan::Button {
                        on: state.clone(),
                    });
                }
            }
            Err(FormError::Refused(format!(
                "the checkbox {name:?} has no state {raw:?}; it offers {:?}",
                record.view.states
            )))
        }
        FieldKind::Radio => {
            if raw.eq_ignore_ascii_case("off") || raw.is_empty() {
                return Ok(Plan::Button {
                    on: "Off".to_string(),
                });
            }
            if let Some(state) = record.view.states.iter().find(|state| *state == raw) {
                return Ok(Plan::Button {
                    on: state.clone(),
                });
            }
            Err(FormError::Refused(format!(
                "the radio group {name:?} has no option {raw:?}; it offers {:?}",
                record.view.states
            )))
        }
        FieldKind::Choice => {
            if record.multi_select {
                let mut values = Vec::with_capacity(selections.len());
                let mut indices = Vec::with_capacity(selections.len());
                for selection in selections {
                    if let Some((value, index)) = resolve_choice_selection(record, selection) {
                        values.push(value);
                        indices.push(Some(index));
                    } else if record.editable {
                        // An editable multi-select carries typed values that are
                        // not options. Keep the typed text in /V with no /I
                        // index, and draw that text itself in the appearance.
                        values.push(selection.to_string());
                        indices.push(None);
                    } else {
                        return Err(FormError::Refused(format!(
                            "the choice field {name:?} has no option {selection:?}; it offers {:?}",
                            record.view.options
                        )));
                    }
                }
                return Ok(Plan::ChoiceMulti { values, indices });
            }
            if let Some((value, index)) = resolve_choice_selection(record, raw) {
                return Ok(Plan::Choice {
                    value,
                    index: Some(index),
                });
            }
            if record.editable {
                return Ok(Plan::Choice {
                    value: raw.to_string(),
                    index: None,
                });
            }
            Err(FormError::Refused(format!(
                "the choice field {name:?} has no option {raw:?}; it offers {:?}",
                record.view.options
            )))
        }
        FieldKind::Signature => Err(FormError::Refused(format!(
            "the field {name:?} is a signature field; filling it is a signing operation, which this tool does not perform"
        ))),
        FieldKind::PushButton => Err(FormError::Refused(format!(
            "the field {name:?} is a push button and has no value"
        ))),
        FieldKind::Unknown => Err(FormError::Refused(format!(
            "the field {name:?} has no supported field type"
        ))),
    }
}

/// Apply one planned edit to the copied objects.
///
/// # Budget
///
/// Only the copied objects are mutated; allocations (notably a multi-select
/// `/V`/`/I` array) are bounded by the edit's caller-bounded value count.
///
/// # Malformed Input
///
/// A field that was not copied, or is not a dictionary in the output, is a
/// typed [`FormError`]; no partial value is written.
fn apply_plan(
    objects: &mut [(u32, Obj)],
    remap: &HashMap<u32, u32>,
    record: &FieldRecord,
    plan: &Plan,
) -> Result<(), FormError> {
    let Some(field_ref) = record.field_ref else {
        return Err(FormError::Refused(format!(
            "the field {:?} is not an indirect object, so its value cannot be written",
            record.view.name
        )));
    };
    let Some(num) = remap.get(&field_ref.num).copied() else {
        return Err(FormError::Malformed(format!(
            "the field {:?} was not copied into the output",
            record.view.name
        )));
    };
    let Some(pairs) = object_dict_mut(objects, num) else {
        return Err(FormError::Malformed(format!(
            "the field {:?} is not a dictionary in the output",
            record.view.name
        )));
    };
    match plan {
        Plan::Text(value) => {
            dict_set(pairs, b"V", text_value(value));
            dict_remove(pairs, b"RV");
        }
        Plan::Choice { value, index } => {
            dict_set(pairs, b"V", text_value(value));
            match index {
                Some(index) => {
                    let index = i64::try_from(*index).unwrap_or(i64::MAX);
                    dict_set(pairs, b"I", Obj::Array(vec![Obj::Int(index)]));
                }
                None => dict_remove(pairs, b"I"),
            }
        }
        Plan::ChoiceMulti { values, indices } => {
            dict_set(
                pairs,
                b"V",
                Obj::Array(values.iter().map(|value| text_value(value)).collect()),
            );
            // Only matched selections have an /Opt index. An editable typed
            // value has none and is deliberately left out of /I rather than
            // being given a fabricated index.
            dict_set(
                pairs,
                b"I",
                Obj::Array(
                    indices
                        .iter()
                        .filter_map(|index| *index)
                        .map(|index| Obj::Int(i64::try_from(index).unwrap_or(i64::MAX)))
                        .collect(),
                ),
            );
        }
        Plan::Button { on } => {
            dict_set(
                pairs,
                b"V",
                Obj::Name(Bytes::copy_from_slice(on.as_bytes())),
            );
        }
    }
    if matches!(plan, Plan::Button { .. }) {
        for widget in &record.widgets {
            let Some(widget_ref) = widget.obj_ref else {
                return Err(FormError::Refused(format!(
                    "a widget of {:?} is not an indirect object, so its state cannot be selected",
                    record.view.name
                )));
            };
            let Some(widget_num) = remap.get(&widget_ref.num).copied() else {
                return Err(FormError::Malformed(format!(
                    "a widget of {:?} was not copied into the output",
                    record.view.name
                )));
            };
            if widget_num == num {
                // The field is merged with its single widget; it was just
                // written above.
                let on = match plan {
                    Plan::Button { on } => on,
                    _ => "Off",
                };
                let Some(pairs) = object_dict_mut(objects, widget_num) else {
                    continue;
                };
                set_widget_state(pairs, &widget.states, on);
                continue;
            }
            let on = match plan {
                Plan::Button { on } => on,
                _ => "Off",
            };
            let Some(pairs) = object_dict_mut(objects, widget_num) else {
                continue;
            };
            set_widget_state(pairs, &widget.states, on);
        }
    }
    Ok(())
}

/// Select one named state on a widget whose appearance offers it.
fn set_widget_state(pairs: &mut Vec<(Bytes, Obj)>, states: &[String], on: &str) {
    if states.is_empty() {
        return;
    }
    let selected = if states.iter().any(|state| state == on) {
        on
    } else {
        "Off"
    };
    dict_set(
        pairs,
        b"AS",
        Obj::Name(Bytes::copy_from_slice(selected.as_bytes())),
    );
}

/// Set `/NeedAppearances true` on the copied form so a viewer regenerates the
/// text and choice appearances. Falls back to the catalog's inline dictionary
/// when the form is not an indirect object.
fn set_need_appearances(
    objects: &mut [(u32, Obj)],
    remap: &HashMap<u32, u32>,
    root_num: u32,
    acroform_ref: Option<Ref>,
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
        dict_set(pairs, b"NeedAppearances", Obj::Bool(true));
        return;
    }
    // The /AcroForm value is inline in the catalog; reach through it.
    if let Some(Obj::Dict(inner)) = pairs
        .iter_mut()
        .find(|(key, _)| key.as_slice() == b"AcroForm")
        .map(|(_, value)| value)
    {
        dict_set(inner, b"NeedAppearances", Obj::Bool(true));
    }
}

/// The dictionary of the object numbered `num`, when it is one.
fn object_dict_mut(objects: &mut [(u32, Obj)], num: u32) -> Option<&mut Vec<(Bytes, Obj)>> {
    objects
        .iter_mut()
        .find(|(candidate, _)| *candidate == num)
        .and_then(|(_, value)| match value {
            Obj::Dict(pairs) => Some(pairs),
            _ => None,
        })
}

/// Resolve through references up to a fixed depth, returning the object
/// unchanged when nothing resolves.
fn resolve_deep(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    mut obj: Obj,
    depth: usize,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Obj {
    let mut remaining = depth;
    while let Obj::Ref(reference) = obj {
        if remaining > 32 {
            return Obj::Null;
        }
        remaining = remaining.saturating_add(1);
        match resolve_ref(src, doc, reference, budget, g) {
            Ok(resolved) => obj = resolved,
            Err(_) => return Obj::Null,
        }
    }
    obj
}

/// A dictionary key's value.
fn dict_get<'a>(pairs: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    pairs
        .iter()
        .find(|(candidate, _)| candidate.as_slice() == key)
        .map(|(_, value)| value)
}

/// Replace (or add) a dictionary key.
fn dict_set(pairs: &mut Vec<(Bytes, Obj)>, key: &[u8], value: Obj) {
    dict_remove(pairs, key);
    pairs.push((Bytes::copy_from_slice(key), value));
}

/// Remove a dictionary key when present.
fn dict_remove(pairs: &mut Vec<(Bytes, Obj)>, key: &[u8]) {
    pairs.retain(|(candidate, _)| candidate.as_slice() != key);
}

/// A PDF text string: ASCII stays literal, anything else becomes UTF-16BE
/// with a byte-order mark so the reader does not have to guess.
fn text_value(text: &str) -> Obj {
    if text.is_ascii() {
        return Obj::String(Bytes::copy_from_slice(text.as_bytes()));
    }
    let mut bytes = vec![0xfe, 0xff];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Obj::String(Bytes::copy_from_slice(&bytes))
}

/// Decode a text string: UTF-16BE when it carries the BOM, Latin-1 otherwise
/// (the ASCII range of PDFDocEncoding, which every name this tool matches
/// uses).
fn decode_text(obj: &Obj) -> Option<String> {
    match obj {
        Obj::String(bytes) => Some(decode_pdf_bytes(bytes)),
        Obj::Name(bytes) => std::str::from_utf8(bytes.as_slice())
            .ok()
            .map(str::to_string),
        _ => None,
    }
}

/// Decode a PDF string's bytes.
fn decode_pdf_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| match (pair.first(), pair.get(1)) {
                (Some(high), Some(low)) => u16::from_be_bytes([*high, *low]),
                _ => 0xfffd,
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

/// An integer object.
fn as_i64(obj: &Obj) -> Option<i64> {
    match obj {
        Obj::Int(value) => Some(*value),
        _ => None,
    }
}

/// A name object's bytes.
fn as_name(obj: &Obj) -> Option<Bytes> {
    match obj {
        Obj::Name(bytes) => Some(bytes.clone()),
        _ => None,
    }
}

/// A `skip_serializing_if` predicate for booleans.
fn is_false(value: &bool) -> bool {
    !*value
}

// --- Field appearance synthesis (SL-5.FORM.02) ---------------------------------
//
// A `/NeedAppearances` form shows nothing in a viewer that does not rebuild
// appearances, and flattening such a form bakes an empty box. Synthesising the
// appearance ourselves — a Form XObject that draws the field's value per its
// `/DA`, placed in the widget's `/AP /N` — makes the value visible everywhere
// and gives the flattener a real mark to bake. This is the ISO 32000-1
// §12.7.3.3 variable-text appearance: it serves text fields and choice fields
// (combo boxes and list boxes) alike, both of which draw a text string inside
// a `/Tx BMC … EMC` sequence. Buttons already carry their own appearance
// states and are out of this slice.

/// A read-only view of a copied object's dictionary.
fn object_dict(objects: &[(u32, Obj)], num: u32) -> Option<&Vec<(Bytes, Obj)>> {
    objects
        .iter()
        .find(|(candidate, _)| *candidate == num)
        .and_then(|(_, value)| match value {
            Obj::Dict(pairs) => Some(pairs),
            _ => None,
        })
}

/// A number object as `f64`.
fn num_f64(obj: &Obj) -> Option<f64> {
    match obj {
        Obj::Int(v) => Some(*v as f64),
        Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(i32::from(*scale))),
        _ => None,
    }
}

/// A whole number serialises as `/Int`, anything else as a 3-digit `/Real`, so
/// a box with integer corners stays integer in the output.
fn num_obj(v: f64) -> Obj {
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        #[allow(clippy::cast_possible_truncation)] // integral and bounded by the guard
        let n = v as i64;
        Obj::Int(n)
    } else {
        let scaled = (v * 1000.0).round();
        let clamped = if scaled >= i64::MAX as f64 {
            i64::MAX
        } else if scaled <= i64::MIN as f64 {
            i64::MIN
        } else {
            #[allow(clippy::cast_possible_truncation)] // clamped into i64 range above
            let n = scaled as i64;
            n
        };
        Obj::Real {
            scaled: clamped,
            scale: 3,
        }
    }
}

/// `(x0, y0, x1, y1)` from a widget's `/Rect`, when present and numeric.
fn read_rect(pairs: &[(Bytes, Obj)]) -> Option<(f64, f64, f64, f64)> {
    let Obj::Array(a) = dict_get(pairs, b"Rect")? else {
        return None;
    };
    let [x0, y0, x1, y1] = a.as_slice() else {
        return None;
    };
    Some((num_f64(x0)?, num_f64(y0)?, num_f64(x1)?, num_f64(y1)?))
}

/// Format a coordinate without trailing zeros.
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        #[allow(clippy::cast_possible_truncation)] // integral and bounded by the guard
        let n = v as i64;
        n.to_string()
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// The font name, size and colour operator from a `/DA` string, e.g.
/// `/Helv 12 Tf 0 g` -> (`Helv`, 12.0, `0 g`). Size 0 means auto-size. Missing
/// pieces fall back to Helvetica / auto / black.
fn parse_default_appearance(da: &[u8]) -> (Vec<u8>, f64, Vec<u8>) {
    let text = String::from_utf8_lossy(da);
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut font = b"Helv".to_vec();
    let mut size = 0.0;
    let mut color = b"0 g".to_vec();
    if let Some(tf) = tokens.iter().position(|t| *t == "Tf") {
        let name_tok = tf.checked_sub(2).and_then(|i| tokens.get(i));
        let size_tok = tf.checked_sub(1).and_then(|i| tokens.get(i));
        if let Some(name) = name_tok.and_then(|t| t.strip_prefix('/')) {
            font = name.as_bytes().to_vec();
        }
        if let Some(parsed) = size_tok.and_then(|t| t.parse::<f64>().ok()) {
            size = parsed;
        }
        if let Some(c) = tokens.get(tf.saturating_add(1)..).and_then(color_op) {
            color = c;
        } else if let Some(c) = tf
            .checked_sub(2)
            .and_then(|end| tokens.get(..end))
            .and_then(color_op)
        {
            color = c;
        }
    }
    (font, size, color)
}

/// The last colour-setting operator (`g`/`rg`/`k`) with its operands.
fn color_op(tokens: &[&str]) -> Option<Vec<u8>> {
    for (i, t) in tokens.iter().enumerate() {
        let operands = match *t {
            "g" => 1usize,
            "rg" => 3,
            "k" => 4,
            _ => continue,
        };
        if let Some(start) = i.checked_sub(operands) {
            if let Some(slice) = tokens.get(start..=i) {
                return Some(slice.join(" ").into_bytes());
            }
        }
    }
    None
}

/// Escape one line for a PDF literal string, mapped to the single-byte range a
/// standard-14 font draws (WinAnsi/Latin-1 overlap); anything wider becomes `?`.
fn escape_pdf_literal(line: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in line.chars() {
        // A code point that fits in one byte draws in a standard-14 font; wider
        // code points (U+0100 and up) have no single-byte glyph, so they become
        // `?`. `u8::try_from` does both the range check and the conversion.
        let byte = u8::try_from(u32::from(ch)).unwrap_or(b'?');
        match byte {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(byte);
            }
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            _ => out.push(byte),
        }
    }
    out
}

/// The WinAnsi glyph name assigned to a single byte (0x00–0xFF) by the Annex D
/// table in [`selis_font::tables`]. [`escape_pdf_literal`] writes the byte
/// verbatim into the appearance string, so this is the name whose AFM advance
/// measures what is actually drawn — for ASCII *and* high bytes. Digits are
/// therefore `zero`..`nine` (not `"7"`), `'` is `quotesingle` (the WinAnsi
/// assignment at 0x27), and 0xE9 is `eacute`.
///
/// # Budget
///
/// One constant-table index and a string compare; no allocation.
///
/// # Malformed Input
///
/// Unassigned codes (`.notdef`) return `None`, so the caller keeps the honest
/// 0.5 em fallback rather than claiming a width for a glyph that does not
/// exist.
fn winansi_glyph_name(byte: u8) -> Option<&'static str> {
    let name = *selis_font::tables::WinAnsiEncoding.get(usize::from(byte))?;
    (name != ".notdef").then_some(name)
}

/// The width of `line` in points at `size`, used to centre / right-quadd it.
/// For a standard-14 face, every single-byte glyph is measured with the exact
/// AFM advance from [`selis_font::standard14`] (the same table the renderer
/// uses): the byte's WinAnsi glyph name from [`winansi_glyph_name`] is the key,
/// because that is the glyph [`escape_pdf_literal`] draws. A character outside
/// one byte draws as `?`, whose AFM width is measured too. Embedded /
/// non-standard faces keep the honest 0.5 em heuristic rather than claiming an
/// exact metric we lack.
///
/// # Budget
///
/// One pass over the label's characters, bounded by the field-value limit;
/// no heap allocation.
///
/// # Malformed Input
///
/// A line with no resolvable glyph uses the AFM `question` width (matching
/// [`escape_pdf_literal`] for out-of-range characters) or the honest 0.5 em
/// fallback (for an unassigned byte) so alignment never claims a metric the
/// face does not define.
fn text_width(line: &str, size: f64, std_font: Option<&str>) -> f64 {
    let approx = || line.chars().count() as f64 * size * 0.5;
    let Some(name) = std_font.filter(|name| selis_font::standard14::is_standard(name)) else {
        return approx();
    };
    let question = f64::from(selis_font::standard14::width(name, "question").unwrap_or(500));
    let mut total = 0.0;
    for ch in line.chars() {
        let Ok(byte) = u8::try_from(u32::from(ch)) else {
            total += question; // escape_pdf_literal draws '?'
            continue;
        };
        // One source of truth for every byte: the WinAnsi glyph name of what
        // escape_pdf_literal draws. Previously ASCII letters/digits were keyed
        // by the character itself, so digits fell back to 500/1000 em because
        // the AFM names them "zero".."nine", and ASCII 0x27 was measured as
        // quoteright instead of the WinAnsi quotesingle.
        let advance =
            winansi_glyph_name(byte).and_then(|glyph| selis_font::standard14::width(name, glyph));
        total += f64::from(advance.unwrap_or(500));
    }
    total / 1000.0 * size
}

/// Build a `/Tx` variable-text appearance content stream: one `BT…ET` that
/// draws each of `lines` in a `width`×`height` box, honouring quadding. A text
/// field passes one line (split on newlines when multiline); a multi-select
/// choice passes one line per selected display label, stacked from the top.
/// Shared by text and choice fields: ISO 32000-1 §12.7.3.3 draws both inside
/// `/Tx BMC … EMC`.
///
/// # Budget
///
/// The stream is proportional to the number of lines, bounded by the field
/// value limit and the copied widget graph; no other budget charge.
///
/// # Malformed Input
///
/// A degenerate box is handled by the caller, which does not write an
/// appearance for it. More lines than fit are squeezed to the available lead
/// and clipped by the form `/BBox` rather than overflowing the page.
#[allow(clippy::too_many_arguments)]
fn text_appearance_content(
    lines: &[&str],
    font: &[u8],
    size: f64,
    color: &[u8],
    width: f64,
    height: f64,
    quadding: i64,
    multiline: bool,
    std_font: Option<&str>,
) -> Vec<u8> {
    const PAD: f64 = 2.0;
    let font_size = if size > 0.0 {
        size
    } else if lines.len() > 1 || multiline {
        (height / (lines.len().max(1) as f64 * 1.3)).clamp(4.0, 12.0)
    } else {
        (height - 2.0 * PAD).clamp(4.0, 12.0)
    };
    let font_name = String::from_utf8_lossy(font);
    let color_str = String::from_utf8_lossy(color);

    let mut out = Vec::new();
    out.extend_from_slice(b"/Tx BMC\nq\nBT\n");
    out.extend_from_slice(
        format!("{color_str}\n/{font_name} {} Tf\n", fmt_num(font_size)).as_bytes(),
    );

    let leading = if lines.len() > 1 {
        let available = (height - 2.0 * PAD - font_size).max(0.0);
        (font_size * 1.15)
            .min(available / (lines.len().saturating_sub(1) as f64))
            .max(font_size * 0.4)
    } else {
        font_size * 1.15
    };
    let first_y = if multiline || lines.len() > 1 {
        (height - PAD - font_size).max(0.0)
    } else {
        ((height - font_size) / 2.0).max(PAD) + font_size * 0.2
    };
    let mut prev_x = 0.0;
    let mut prev_y = 0.0;
    for (i, line) in lines.iter().enumerate() {
        let text_w = text_width(line, font_size, std_font);
        let x = match quadding {
            1 => ((width - text_w) / 2.0).max(PAD),
            2 => (width - PAD - text_w).max(PAD),
            _ => PAD,
        };
        let y = first_y - (i as f64) * leading;
        out.extend_from_slice(
            format!("{} {} Td\n", fmt_num(x - prev_x), fmt_num(y - prev_y)).as_bytes(),
        );
        out.push(b'(');
        out.extend_from_slice(&escape_pdf_literal(line));
        out.extend_from_slice(b") Tj\n");
        prev_x = x;
        prev_y = y;
    }
    out.extend_from_slice(b"ET\nQ\nEMC\n");
    out
}

/// The form's default `/DA` and its `/DR /Font` name→object map, from the copied
/// `/AcroForm` (its refs are already remapped). A font value is kept as-is: an
/// indirect reference or a direct font dictionary are both valid resource
/// values, and either can be used for the appearance's font selection and
/// metrics.
///
/// # Budget
///
/// One scan of the copied form dictionary; allocations are bounded by the
/// number of entries in its `/DR /Font` map.
///
/// # Malformed Input
///
/// A missing or non-dictionary `/AcroForm`, `/DA` or `/DR /Font` yields the
/// built-in `/DA` and an empty font map, so the caller either falls back or
/// keeps `/NeedAppearances` rather than guessing a font.
fn form_defaults(
    objects: &[(u32, Obj)],
    remap: &HashMap<u32, u32>,
    root_num: u32,
    acroform_ref: Option<Ref>,
) -> (Vec<u8>, Vec<(Bytes, Obj)>) {
    let acro = match acroform_ref {
        Some(r) => remap
            .get(&r.num)
            .copied()
            .and_then(|n| object_dict(objects, n)),
        None => object_dict(objects, root_num)
            .and_then(|cat| dict_get(cat, b"AcroForm"))
            .and_then(|v| match v {
                Obj::Dict(d) => Some(d),
                _ => None,
            }),
    };
    let Some(acro) = acro else {
        return (b"/Helv 0 Tf 0 g".to_vec(), Vec::new());
    };
    let da = match dict_get(acro, b"DA") {
        Some(Obj::String(s)) => s.as_slice().to_vec(),
        _ => b"/Helv 0 Tf 0 g".to_vec(),
    };
    let mut fonts = Vec::new();
    if let Some(Obj::Dict(dr)) = dict_get(acro, b"DR") {
        if let Some(Obj::Dict(font_dict)) = dict_get(dr, b"Font") {
            for (name, value) in font_dict {
                if matches!(value, Obj::Ref(_) | Obj::Dict(_)) {
                    fonts.push((name.clone(), value.clone()));
                }
            }
        }
    }
    (da, fonts)
}

/// The `/DR /Font` name→object map from one dictionary, when present. Both
/// indirect references and direct font dictionaries are kept: ISO 32000-1
/// allows either as a resource value, and a direct dictionary already has its
/// internal refs remapped by the copy step.
///
/// # Budget
///
/// One scan of the dictionary; allocations are bounded by the number of fonts
/// in the resource map.
///
/// # Malformed Input
///
/// A `/DR` that is not a dictionary, a `/Font` that is not a dictionary, or a
/// font value that is neither an indirect reference nor a dictionary
/// contributes nothing, so the caller falls back to the next font map.
fn dr_fonts_of(dict: &[(Bytes, Obj)]) -> Vec<(Bytes, Obj)> {
    let Some(Obj::Dict(dr)) = dict_get(dict, b"DR") else {
        return Vec::new();
    };
    let Some(Obj::Dict(fonts)) = dict_get(dr, b"Font") else {
        return Vec::new();
    };
    fonts
        .iter()
        .filter(|(_, value)| matches!(value, Obj::Ref(_) | Obj::Dict(_)))
        .cloned()
        .collect()
}

/// The font map for one field: its own or a widget's `/DR /Font` when present,
/// otherwise the form-level map. Field and widget refs are already remapped.
///
/// # Budget
///
/// One scan of the field dictionary and each widget dictionary; allocations
/// are bounded by the number of fonts in the resource map.
///
/// # Malformed Input
///
/// A `/DR` that is not a dictionary, or a font map whose values are neither
/// indirect references nor direct dictionaries, is ignored so the caller can
/// fall back to the form map.
fn field_dr_fonts(
    objects: &[(u32, Obj)],
    field_num: u32,
    widget_nums: &[u32],
    form_fonts: &[(Bytes, Obj)],
) -> Vec<(Bytes, Obj)> {
    if let Some(field) = object_dict(objects, field_num) {
        let own = dr_fonts_of(field);
        if !own.is_empty() {
            return own;
        }
    }
    for widget in widget_nums {
        if let Some(dict) = object_dict(objects, *widget) {
            let own = dr_fonts_of(dict);
            if !own.is_empty() {
                return own;
            }
        }
    }
    form_fonts.to_vec()
}

/// The `/BaseFont` name of a font resource, used to select standard-14 metrics.
/// The resource is either an indirect reference (resolved in the copied graph)
/// or a direct font dictionary.
///
/// # Budget
///
/// One dictionary read and one name decode; no retained allocation beyond the
/// returned name.
///
/// # Malformed Input
///
/// A font that is neither a reference to a dictionary nor a dictionary itself,
/// or that has no name `/BaseFont`, yields `None` and the caller keeps the
/// honest width fallback.
fn font_base_name(objects: &[(u32, Obj)], font: &Obj) -> Option<String> {
    let dict = match font {
        Obj::Ref(reference) => object_dict(objects, reference.num),
        Obj::Dict(pairs) => Some(pairs),
        _ => None,
    };
    dict?
        .iter()
        .find(|(key, _)| key.as_slice() == b"BaseFont")
        .and_then(|(_, value)| as_name(value))
        .map(|name| String::from_utf8_lossy(name.as_slice()).into_owned())
}

/// Synthesize `/AP /N` appearance streams for one field's widgets, wiring each
/// widget's `/AP /N` to a new Form XObject that draws every line in `labels`
/// (one line for text and single-select choices, one per selection for a
/// multi-select choice). Shared body for text and choice fields. Returns true
/// when at least one widget got an appearance (false when the field has no
/// resolvable font, no widget, or a degenerate box — in which case the caller
/// falls back to `/NeedAppearances`).
///
/// # Budget
///
/// No new budget charges; the widgets and their geometry were copied earlier.
/// Allocations are bounded by the number of widgets and by the label count,
/// both bounded by the copied object graph and the field-value limit.
///
/// # Malformed Input
///
/// A field-level `/DR` that cannot resolve the `/DA` font falls back to the
/// form map; if neither resolves it, this returns false rather than claiming an
/// appearance. Widgets with a missing/non-numeric `/Rect` or a degenerate box
/// are skipped.
fn synthesize_variable_appearances(
    objects: &mut Vec<(u32, Obj)>,
    remap: &HashMap<u32, u32>,
    next_num: &mut u32,
    record: &FieldRecord,
    labels: &[&str],
    default_da: &[u8],
    dr_fonts: &[(Bytes, Obj)],
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
    let (font, size, color) = parse_default_appearance(&da);

    // Widget geometries first, so every read finishes before the pushes/mutates.
    let mut jobs: Vec<(u32, f64, f64)> = Vec::new();
    for widget in &record.widgets {
        let Some(wref) = widget.obj_ref else { continue };
        let Some(wnum) = remap.get(&wref.num).copied() else {
            continue;
        };
        if let Some((x0, y0, x1, y1)) = object_dict(objects, wnum).and_then(|d| read_rect(d)) {
            jobs.push((wnum, (x1 - x0).abs(), (y1 - y0).abs()));
        }
    }

    // A widget/field-level /DR overrides the form map; its font is the one the
    // /DA name resolves to for this field's appearance.
    let widget_nums: Vec<u32> = jobs.iter().map(|job| job.0).collect();
    let fonts = field_dr_fonts(objects, field_num, &widget_nums, dr_fonts);
    let Some((_, font_obj)) = fonts
        .iter()
        .find(|(name, _)| name.as_slice() == font.as_slice())
    else {
        return false;
    };
    let std_font = font_base_name(objects, font_obj);

    let flags = object_dict(objects, field_num)
        .and_then(|d| dict_get(d, b"Ff"))
        .and_then(as_i64)
        .unwrap_or(0);
    let multiline = flags & 0x1000 != 0; // /Ff bit 13 (multiline)
    let quadding = object_dict(objects, field_num)
        .and_then(|d| dict_get(d, b"Q"))
        .and_then(as_i64)
        .unwrap_or(0);
    // A multiline text field splits its single value on newlines; a
    // multi-select choice carries one already-separated label per selection.
    let lines: Vec<&str> = if multiline && labels.len() == 1 {
        match labels.first() {
            Some(first) => first.split(['\n', '\r']).collect(),
            None => Vec::new(),
        }
    } else {
        labels.to_vec()
    };

    let mut any = false;
    for (wnum, width, height) in jobs {
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        let content = text_appearance_content(
            &lines,
            &font,
            size,
            &color,
            width,
            height,
            quadding,
            multiline,
            std_font.as_deref(),
        );
        let xnum = *next_num;
        *next_num = next_num.saturating_add(1);
        let stream = Obj::Stream {
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
                (
                    Bytes::copy_from_slice(b"Resources"),
                    Obj::Dict(vec![(
                        Bytes::copy_from_slice(b"Font"),
                        // Keep the resource value in its original form: a
                        // reference stays a reference, a direct dictionary is
                        // copied inline (its internal refs are already remapped).
                        Obj::Dict(vec![(Bytes::copy_from_slice(&font), font_obj.clone())]),
                    )]),
                ),
                (
                    Bytes::copy_from_slice(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                ),
            ],
            data: Bytes::copy_from_slice(&content),
        };
        objects.push((xnum, stream));
        if let Some(wd) = object_dict_mut(objects, wnum) {
            dict_set(
                wd,
                b"AP",
                Obj::Dict(vec![(
                    Bytes::copy_from_slice(b"N"),
                    Obj::Ref(Ref::new(xnum, 0)),
                )]),
            );
            any = true;
        }
    }
    any
}

/// Synthesize appearances for one text field: the value itself is the text the
/// widget draws.
///
/// # Budget
///
/// No new budget charges are made here; the widgets and their geometry were
/// copied earlier. Allocations are bounded by the number of widgets on the
/// field, which is bounded by the number of objects in the copied graph.
///
/// # Malformed Input
///
/// A widget with a missing or non-numeric `/Rect`, a field with no `/DA` font
/// resolvable in the form's `/DR`, or a degenerate box is skipped. When no
/// widget can be drawn the function returns false and the caller keeps
/// `/NeedAppearances`; no placeholder appearance is written.
fn synthesize_text_appearances(
    objects: &mut Vec<(u32, Obj)>,
    remap: &HashMap<u32, u32>,
    next_num: &mut u32,
    record: &FieldRecord,
    value: &str,
    default_da: &[u8],
    dr_fonts: &[(Bytes, Obj)],
) -> bool {
    synthesize_variable_appearances(
        objects,
        remap,
        next_num,
        record,
        &[value],
        default_da,
        dr_fonts,
    )
}

/// Synthesize appearances for one choice field (combo box or list box).
/// A non-edit choice draws the selected option's **display label** from
/// `/Opt`, while the fill stores the export value in `/V` and the option index
/// in `/I`; an editable combo whose typed value matches no option draws that
/// value itself. A multi-select choice draws every selected label as its own
/// stacked line, with all export values in `/V` and matched indices in `/I`; an
/// editable typed value that matched no option is drawn as itself and has no
/// `/I` index.
///
/// # Budget
///
/// No new budget charges; allocations are bounded by the selection count,
/// which the caller bounded by the value limit, and by the number of widgets.
///
/// # Malformed Input
///
/// An option index that is absent, outside `/Opt`, or `None` (a typed value
/// with no option) falls back to the raw value rather than drawing a guessed
/// label. Missing fonts, widgets or boxes make this return false so the caller
/// keeps `/NeedAppearances` instead of claiming an appearance it did not write.
fn synthesize_choice_appearances(
    objects: &mut Vec<(u32, Obj)>,
    remap: &HashMap<u32, u32>,
    next_num: &mut u32,
    record: &FieldRecord,
    plan: &Plan,
    default_da: &[u8],
    dr_fonts: &[(Bytes, Obj)],
) -> bool {
    match plan {
        Plan::Choice { value, index } => synthesize_variable_appearances(
            objects,
            remap,
            next_num,
            record,
            &[index
                .and_then(|i| record.option_labels.get(i))
                .map_or(value.as_str(), String::as_str)],
            default_da,
            dr_fonts,
        ),
        Plan::ChoiceMulti { values, indices } => {
            let labels: Vec<&str> = indices
                .iter()
                .zip(values)
                .map(|(index, value)| {
                    index
                        .and_then(|i| record.option_labels.get(i))
                        .map_or(value.as_str(), String::as_str)
                })
                .collect();
            synthesize_variable_appearances(
                objects, remap, next_num, record, &labels, default_da, dr_fonts,
            )
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::type_complexity
    )]

    use super::*;

    /// A minimal one-page PDF from raw object bodies, with a correct xref.
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

    /// One page carrying a text field, a checkbox, a choice, a two-widget
    /// radio group, a read-only text field, a signature field, and a push
    /// button.
    fn form_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R 7 0 R 9 0 R 12 0 R 13 0 R 14 0 R 16 0 R 17 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (
                5,
                b"<< /Fields [6 0 R 7 0 R 9 0 R 11 0 R 14 0 R 16 0 R 17 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 15 0 R >> >> >>",
            ),
            (
                6,
                b"<< /FT /Tx /T (Name) /V (Old) /Subtype /Widget /Rect [10 10 200 30] /P 3 0 R /DA (/Helv 12 Tf 0 g) >>",
            ),
            (
                7,
                b"<< /FT /Btn /T (Agree) /V /Off /Subtype /Widget /Rect [10 40 30 60] /P 3 0 R /AP << /N << /Off 8 0 R /Yes 8 0 R >> >> >>",
            ),
            (
                8,
                b"<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream",
            ),
            (
                9,
                b"<< /FT /Ch /T (Color) /V (Red) /Opt [(Red) (Green) (Blue)] /Subtype /Widget /Rect [10 70 120 90] /P 3 0 R >>",
            ),
            (
                10,
                b"<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream",
            ),
            (
                11,
                b"<< /FT /Btn /Ff 32768 /T (Size) /V /Small /Kids [12 0 R 13 0 R] >>",
            ),
            (
                12,
                b"<< /Subtype /Widget /Parent 11 0 R /Rect [10 100 30 120] /P 3 0 R /AP << /N << /Off 10 0 R /Small 10 0 R >> >> >>",
            ),
            (
                13,
                b"<< /Subtype /Widget /Parent 11 0 R /Rect [40 100 60 120] /P 3 0 R /AP << /N << /Off 10 0 R /Large 10 0 R >> >> >>",
            ),
            (
                14,
                b"<< /FT /Tx /T (Frozen) /Ff 1 /V (fixed) /Subtype /Widget /Rect [10 130 120 150] /P 3 0 R >>",
            ),
            (
                15,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
            ),
            (
                16,
                b"<< /FT /Sig /T (Sign) /Subtype /Widget /Rect [160 130 260 150] /P 3 0 R >>",
            ),
            (
                17,
                b"<< /FT /Btn /Ff 65536 /T (Reset) /Subtype /Widget /Rect [10 170 80 190] /P 3 0 R >>",
            ),
        ];
        build_pdf(objects)
    }

    /// A one-page form with a single-select combo, a single-select list box,
    /// an editable combo, a list box with a separate widget kid, and a
    /// multi-select list box. `/Opt` entries are `[export, label]` pairs so the
    /// drawn text can differ from the stored `/V`.
    fn choice_form_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R 7 0 R 8 0 R 11 0 R 12 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (
                5,
                b"<< /Fields [6 0 R 7 0 R 8 0 R 9 0 R 12 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 10 0 R >> >> >>",
            ),
            (
                6,
                b"<< /FT /Ch /Ff 131072 /T (Color) /V (r) /Opt [[(r) (Red)] [(g) (Green)] [(b) (Blue)]] /Q 1 /Subtype /Widget /Rect [10 70 120 90] /P 3 0 R >>",
            ),
            (
                7,
                b"<< /FT /Ch /T (Size) /V (s) /Opt [[(s) (Small)] [(m) (Medium)] [(l) (Large)]] /Subtype /Widget /Rect [10 100 120 130] /P 3 0 R >>",
            ),
            (
                8,
                b"<< /FT /Ch /Ff 393216 /T (Custom) /V (One) /Opt [(One) (Two)] /Subtype /Widget /Rect [10 140 120 160] /P 3 0 R >>",
            ),
            (
                9,
                b"<< /FT /Ch /T (Country) /V (us) /Opt [[(us) (United States)] [(ca) (Canada)]] /Kids [11 0 R] >>",
            ),
            (10, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (
                11,
                b"<< /Type /Annot /Subtype /Widget /Parent 9 0 R /Rect [10 170 220 190] /P 3 0 R >>",
            ),
            (
                12,
                b"<< /FT /Ch /Ff 2097152 /T (Multi) /V (x) /Opt [(x) (y)] /Subtype /Widget /Rect [10 200 120 220] /P 3 0 R >>",
            ),
        ];
        build_pdf(objects)
    }

    /// A one-field choice form whose `/AcroForm` has no `/DR` font map, so
    /// synthesis must refuse to claim an appearance and keep `/NeedAppearances`.
    fn choice_no_font_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (5, b"<< /Fields [6 0 R] /DA (/Helv 12 Tf 0 g) >>"),
            (
                6,
                b"<< /FT /Ch /Ff 131072 /T (Color) /V (r) /Opt [(r) (g)] /Subtype /Widget /Rect [10 70 120 90] /P 3 0 R >>",
            ),
        ];
        build_pdf(objects)
    }

    /// Choice fields for font-metric and font-resolution checks. All four draw
    /// `WAVE` at 12 pt, right-quadded in a 110 pt box. `Std` uses the form
    /// `/DR` (Helvetica); `Local` and `WidgetLocal` override it on the field
    /// and widget with Courier; `Embedded` points at a non-standard face.
    fn metrics_dr_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R 7 0 R 11 0 R 13 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (
                5,
                b"<< /Fields [6 0 R 7 0 R 11 0 R 12 0 R] /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 8 0 R >> >> >>",
            ),
            (
                6,
                b"<< /FT /Ch /T (Std) /V (w) /Opt [[(w) (WAVE)]] /Q 2 /DA (/Helv 12 Tf 0 g) /Subtype /Widget /Rect [10 70 120 90] /P 3 0 R >>",
            ),
            (
                7,
                b"<< /FT /Ch /T (Local) /V (w) /Opt [[(w) (WAVE)]] /Q 2 /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 9 0 R >> >> /Subtype /Widget /Rect [10 100 120 120] /P 3 0 R >>",
            ),
            (8, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            (9, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
            (10, b"<< /Type /Font /Subtype /Type1 /BaseFont /CustomFace >>"),
            (
                11,
                b"<< /FT /Ch /T (Embedded) /V (w) /Opt [[(w) (WAVE)]] /Q 2 /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 10 0 R >> >> /Subtype /Widget /Rect [10 130 120 150] /P 3 0 R >>",
            ),
            (
                12,
                b"<< /FT /Ch /T (WidgetLocal) /V (w) /Opt [[(w) (WAVE)]] /Q 2 /DA (/Helv 12 Tf 0 g) /Kids [13 0 R] >>",
            ),
            (
                13,
                b"<< /Type /Annot /Subtype /Widget /Parent 12 0 R /Rect [10 160 120 180] /P 3 0 R /DR << /Font << /Helv 9 0 R >> >> >>",
            ),
        ];
        build_pdf(objects)
    }

    /// A one-field form with an editable multi-select list box. `/Opt` has one
    /// `[export, label]` pair so the matched label differs from the export, and
    /// one plain option. The Edit flag (bit 18) allows a typed value alongside
    /// the matched selections.
    fn editable_multi_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (
                5,
                b"<< /Fields [6 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>",
            ),
            // 2097152 (MultiSelect) + 262144 (Edit).
            (
                6,
                b"<< /FT /Ch /Ff 2359296 /T (Tags) /V (a) /Opt [[(a) (Alpha)] (b)] /Subtype /Widget /Rect [10 70 130 130] /P 3 0 R >>",
            ),
            (7, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ];
        build_pdf(objects)
    }

    /// Two right-quadded fields whose fonts are **direct dictionaries** rather
    /// than references. `FormDict` uses the form-level `/DR`, while `FieldDict`
    /// overrides it with a field-level `/DR`. Both draw `WAVE` at 12 pt in a
    /// 110 pt box so the exact AFM widths give different x origins.
    fn inline_dict_font_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R 7 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (
                5,
                b"<< /Fields [6 0 R 7 0 R] /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>",
            ),
            (
                6,
                b"<< /FT /Ch /T (FormDict) /V (w) /Opt [[(w) (WAVE)]] /Q 2 /DA (/Helv 12 Tf 0 g) /Subtype /Widget /Rect [10 70 120 90] /P 3 0 R >>",
            ),
            (
                7,
                b"<< /FT /Ch /T (FieldDict) /V (w) /Opt [[(w) (WAVE)]] /Q 2 /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv << /Type /Font /Subtype /Type1 /BaseFont /Courier >> >> >> /Subtype /Widget /Rect [10 100 120 120] /P 3 0 R >>",
            ),
        ];
        build_pdf(objects)
    }

    /// A right-quadded choice whose display label carries a high WinAnsi byte:
    /// `W\xe9` ("Wé"). The byte is written verbatim into the appearance string,
    /// so the exact Helvetica width of WinAnsi `eacute` must drive the x origin.
    fn winansi_metrics_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (
                5,
                b"<< /Fields [6 0 R] /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 7 0 R >> >> >>",
            ),
            (
                6,
                b"<< /FT /Ch /T (HiByte) /V (w) /Opt [[(w) (W\xe9)]] /Q 2 /DA (/Helv 12 Tf 0 g) /Subtype /Widget /Rect [10 70 120 90] /P 3 0 R >>",
            ),
            (7, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ];
        build_pdf(objects)
    }

    /// Digit-rich and apostrophe-bearing labels for glyph-name metric checks.
    /// Both draw in Helvetica at 12 pt inside a 110 pt box: `Digits` is centred
    /// (`/Q 1`) with "777", `Apos` is right-quadded (`/Q 2`) with "IT'S". Their
    /// x origins must come from the AFM widths of `seven` and `quotesingle`,
    /// the glyph names the appearance string actually draws.
    fn glyph_name_metrics_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R 7 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (
                5,
                b"<< /Fields [6 0 R 7 0 R] /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 8 0 R >> >> >>",
            ),
            (
                6,
                b"<< /FT /Ch /T (Digits) /V (w) /Opt [[(w) (777)]] /Q 1 /DA (/Helv 12 Tf 0 g) /Subtype /Widget /Rect [10 70 120 90] /P 3 0 R >>",
            ),
            (
                7,
                b"<< /FT /Ch /T (Apos) /V (w) /Opt [[(w) (IT'S)]] /Q 2 /DA (/Helv 12 Tf 0 g) /Subtype /Widget /Rect [10 100 120 120] /P 3 0 R >>",
            ),
            (8, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ];
        build_pdf(objects)
    }

    /// The form fixture with an XFA packet attached to `/AcroForm`.
    fn xfa_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (5, b"<< /Fields [6 0 R] /XFA 7 0 R >>"),
            (
                6,
                b"<< /FT /Tx /T (Name) /Subtype /Widget /Rect [10 10 100 30] /P 3 0 R >>",
            ),
            (7, b"<< /Length 6 >>\nstream\n<x/>\n\nendstream"),
        ];
        build_pdf(objects)
    }

    /// The form fixture under an (unresolvable) encryption declaration: the
    /// `/Encrypt` entry is enough for the tool to refuse without touching a
    /// cipher.
    fn encrypted_fixture() -> Vec<u8> {
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R /Annots [6 0 R] >>",
            ),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            (5, b"<< /Fields [6 0 R] >>"),
            (
                6,
                b"<< /FT /Tx /T (Name) /Subtype /Widget /Rect [10 10 100 30] /P 3 0 R >>",
            ),
            (
                17,
                b"<< /Type /Encrypt /Filter /Standard /V 4 /Length 128 /R 4 >>",
            ),
        ];
        // Only the trailer differs; rewrite the last object's index by
        // writing the trailer with an /Encrypt entry.
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let max = 17u32;
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
                "trailer\n<< /Size {} /Root 1 0 R /Encrypt 17 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
                max + 1
            )
            .as_bytes(),
        );
        out
    }

    fn test_budget() -> Budget {
        Budget::unlimited()
    }

    fn report_of(fixture: &[u8]) -> FormFieldsReport {
        let budget = test_budget();
        let mut g = budget.guard();
        read_form_fields(fixture, &budget, &mut g).expect("read fields")
    }

    fn edits(pairs: &[(&str, &str)]) -> FillFormsOptions {
        FillFormsOptions {
            fields: pairs
                .iter()
                .map(|(name, value)| FieldEdit {
                    name: (*name).to_string(),
                    value: (*value).to_string(),
                    values: Vec::new(),
                })
                .collect(),
            ..FillFormsOptions::default()
        }
    }

    fn fill_of(fixture: &[u8], pairs: &[(&str, &str)]) -> (Vec<u8>, FillInfo) {
        let budget = test_budget();
        let mut g = budget.guard();
        fill_form(fixture, &edits(pairs), &budget, &mut g).expect("fill")
    }

    fn fill_synth(fixture: &[u8], pairs: &[(&str, &str)]) -> (Vec<u8>, FillInfo) {
        let budget = test_budget();
        let mut g = budget.guard();
        let mut opts = edits(pairs);
        opts.synthesize_appearances = true;
        fill_form(fixture, &opts, &budget, &mut g).expect("fill with synthesis")
    }

    #[test]
    fn lists_fields_with_kinds_values_and_states() {
        let report = report_of(&form_fixture());
        assert_eq!(report.status, "ok");
        assert_eq!(report.pages, Some(1));
        assert!(report.has_signature);
        let names: Vec<&str> = report
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Name", "Agree", "Color", "Size", "Frozen", "Sign", "Reset"]
        );
        let find = |name: &str| {
            report
                .fields
                .iter()
                .find(|field| field.name == name)
                .expect("field")
        };
        assert_eq!(find("Name").kind, FieldKind::Text);
        assert_eq!(find("Name").value.as_deref(), Some("Old"));
        assert_eq!(find("Name").page, Some(1));
        assert_eq!(find("Agree").kind, FieldKind::Checkbox);
        assert_eq!(find("Agree").value.as_deref(), Some("Off"));
        assert_eq!(find("Agree").states, ["Yes"]);
        assert_eq!(find("Color").kind, FieldKind::Choice);
        assert_eq!(find("Color").value.as_deref(), Some("Red"));
        assert_eq!(find("Color").options, ["Red", "Green", "Blue"]);
        assert_eq!(find("Size").kind, FieldKind::Radio);
        assert_eq!(find("Size").value.as_deref(), Some("Small"));
        assert_eq!(find("Size").states, ["Small", "Large"]);
        assert!(find("Frozen").read_only);
        assert_eq!(find("Sign").kind, FieldKind::Signature);
        assert_eq!(find("Reset").kind, FieldKind::PushButton);
    }

    #[test]
    fn fill_writes_and_reads_every_value_back() {
        let (bytes, info) = fill_of(
            &form_fixture(),
            &[
                ("Name", "Ada Lovelace"),
                ("Agree", "Yes"),
                ("Color", "Green"),
                ("Size", "Large"),
            ],
        );
        assert_eq!(info.filled, 4);
        assert_eq!(info.total, 7);
        assert!(info.has_signature);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("(Ada Lovelace)"), "text value written");
        assert!(text.contains("/V /Large"), "radio value written");
        assert!(text.contains("/AS /Large"), "selected widget state");
        assert!(text.contains("/AS /Off"), "unselected widget state");
        assert!(text.contains("/NeedAppearances true"), "appearances asked");

        let read_back = report_of(&bytes);
        let find = |name: &str| {
            read_back
                .fields
                .iter()
                .find(|field| field.name == name)
                .expect("field")
        };
        assert_eq!(find("Name").value.as_deref(), Some("Ada Lovelace"));
        assert_eq!(find("Agree").value.as_deref(), Some("Yes"));
        assert_eq!(find("Color").value.as_deref(), Some("Green"));
        assert_eq!(find("Size").value.as_deref(), Some("Large"));
    }

    #[test]
    fn unchecking_writes_the_off_state() {
        let (bytes, _) = fill_of(&form_fixture(), &[("Agree", "false"), ("Size", "off")]);
        let read_back = report_of(&bytes);
        let find = |name: &str| {
            read_back
                .fields
                .iter()
                .find(|field| field.name == name)
                .expect("field")
        };
        assert_eq!(find("Agree").value.as_deref(), Some("Off"));
        assert_eq!(find("Size").value.as_deref(), Some("Off"));
    }

    #[test]
    fn unicode_values_round_trip_as_utf16() {
        let (bytes, _) = fill_of(&form_fixture(), &[("Name", "Zoë – 東京")]);
        assert!(
            bytes.windows(2).any(|pair| pair == [0xfe, 0xff]),
            "non-ASCII text is written as UTF-16BE with a BOM"
        );
        let read_back = report_of(&bytes);
        assert_eq!(
            read_back
                .fields
                .iter()
                .find(|field| field.name == "Name")
                .and_then(|field| field.value.as_deref()),
            Some("Zoë – 東京")
        );
    }

    #[test]
    fn refusals_name_the_reason() {
        let fixture = form_fixture();
        let cases: &[(&[(&str, &str)], &str)] = &[
            (&[("Nope", "x")], "no form field"),
            (&[("Frozen", "x")], "read-only"),
            (&[("Sign", "x")], "signature field"),
            (&[("Reset", "x")], "push button"),
            (&[("Color", "Mauve")], "no option"),
            (&[("Agree", "Maybe")], "no state"),
            (&[("Name", "a"), ("Name", "b")], "named twice"),
        ];
        for (pairs, needle) in cases {
            let budget = test_budget();
            let mut g = budget.guard();
            let error = fill_form(&fixture, &edits(pairs), &budget, &mut g).expect_err("refused");
            assert!(
                error.to_string().contains(needle),
                "error {error:?} did not mention {needle:?}"
            );
        }
    }

    #[test]
    fn xfa_and_empty_edits_are_refused() {
        let fixture_xfa = xfa_fixture();
        let budget = test_budget();
        let mut g = budget.guard();
        let error = fill_form(&fixture_xfa, &edits(&[("Name", "x")]), &budget, &mut g)
            .expect_err("xfa refused");
        assert_eq!(error, FormError::Xfa);

        let fixture = form_fixture();
        let budget = test_budget();
        let mut g = budget.guard();
        let error = fill_form(&fixture, &FillFormsOptions::default(), &budget, &mut g)
            .expect_err("empty refused");
        assert_eq!(
            error,
            FormError::Invalid("no form fields to fill; name at least one field".to_string())
        );
    }

    #[test]
    fn an_encrypted_document_is_refused() {
        let fixture = encrypted_fixture();
        let budget = test_budget();
        let mut g = budget.guard();
        let error = read_form_fields(&fixture, &budget, &mut g).expect_err("encrypted refused");
        assert_eq!(error, FormError::Encrypted);
        let budget = test_budget();
        let mut g = budget.guard();
        let error = fill_form(&fixture, &edits(&[("Name", "x")]), &budget, &mut g)
            .expect_err("fill refused");
        assert_eq!(error, FormError::Encrypted);
    }

    #[test]
    fn a_second_fill_replaces_the_first_value() {
        let first = fill_of(&form_fixture(), &[("Name", "First")]).0;
        let (bytes, _) = fill_of(&first, &[("Name", "Second")]);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("(Second)"));
        assert!(!text.contains("(First)"), "the replaced value is gone");
    }

    /// Resolve the output's `/AcroForm` dictionary.
    fn acroform_of(bytes: &[u8]) -> Vec<(Bytes, Obj)> {
        let budget = test_budget();
        let mut g = budget.guard();
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("parse");
        let root = doc.revisions().last().unwrap().root.unwrap();
        let Obj::Dict(cat) = resolve_ref(bytes, &doc, root, &budget, &mut g).unwrap() else {
            panic!("catalog is not a dict");
        };
        match dict_get(&cat, b"AcroForm")
            .expect("catalog has /AcroForm")
            .clone()
        {
            Obj::Dict(d) => d,
            Obj::Ref(r) => match resolve_ref(bytes, &doc, r, &budget, &mut g).unwrap() {
                Obj::Dict(d) => d,
                _ => panic!("/AcroForm is not a dict"),
            },
            _ => panic!("/AcroForm is neither a dict nor a reference"),
        }
    }

    /// The `(dict, data)` of a field's `/AP /N` appearance stream in the output.
    fn appearance_of(bytes: &[u8], field_name: &str) -> (Vec<(Bytes, Obj)>, Vec<u8>) {
        let budget = test_budget();
        let mut g = budget.guard();
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("parse");
        let acro = acroform_of(bytes);
        let Some(Obj::Array(fields)) = dict_get(&acro, b"Fields") else {
            panic!("/AcroForm has no /Fields");
        };
        for field in fields {
            let Obj::Ref(fr) = field else { continue };
            let Obj::Dict(fd) = resolve_ref(bytes, &doc, *fr, &budget, &mut g).unwrap() else {
                continue;
            };
            if dict_get(&fd, b"T").and_then(decode_text).as_deref() != Some(field_name) {
                continue;
            }
            let apd = match dict_get(&fd, b"AP").expect("the field carries /AP").clone() {
                Obj::Dict(d) => d,
                Obj::Ref(r) => match resolve_ref(bytes, &doc, r, &budget, &mut g).unwrap() {
                    Obj::Dict(d) => d,
                    _ => panic!("/AP is not a dict"),
                },
                _ => panic!("/AP is neither a dict nor a reference"),
            };
            let Obj::Ref(nr) = dict_get(&apd, b"N").expect("/AP carries /N").clone() else {
                panic!("/AP /N is not a reference to a stream");
            };
            let Obj::Stream { dict, data } = resolve_ref(bytes, &doc, nr, &budget, &mut g).unwrap()
            else {
                panic!("/AP /N does not resolve to a stream");
            };
            return (dict, data.as_slice().to_vec());
        }
        panic!("no field named {field_name}");
    }

    /// Resolve a named field's dictionary from the output bytes.
    fn field_dict_of(bytes: &[u8], field_name: &str) -> Vec<(Bytes, Obj)> {
        let budget = test_budget();
        let mut g = budget.guard();
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("parse");
        let acro = acroform_of(bytes);
        let Some(Obj::Array(fields)) = dict_get(&acro, b"Fields") else {
            panic!("/AcroForm has no /Fields");
        };
        for field in fields {
            let Obj::Ref(fr) = field else { continue };
            let Obj::Dict(fd) = resolve_ref(bytes, &doc, *fr, &budget, &mut g).unwrap() else {
                continue;
            };
            if dict_get(&fd, b"T").and_then(decode_text).as_deref() == Some(field_name) {
                return fd;
            }
        }
        panic!("no field named {field_name}");
    }

    /// The `(dict, data)` of every `/AP /N` stream on a named field's widgets,
    /// read back from the output bytes. A merged field is its own widget; an
    /// unmerged field is walked through `/Kids`.
    fn widget_appearances_of(bytes: &[u8], field_name: &str) -> Vec<(Vec<(Bytes, Obj)>, Vec<u8>)> {
        let budget = test_budget();
        let mut g = budget.guard();
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("parse");
        let fd = field_dict_of(bytes, field_name);
        let mut widgets: Vec<Vec<(Bytes, Obj)>> = Vec::new();
        if let Some(Obj::Array(kids)) = dict_get(&fd, b"Kids") {
            for kid in kids {
                let Obj::Ref(kr) = kid else { continue };
                if let Obj::Dict(kd) = resolve_ref(bytes, &doc, *kr, &budget, &mut g).unwrap() {
                    widgets.push(kd);
                }
            }
        } else {
            widgets.push(fd);
        }
        let mut out = Vec::new();
        for wd in widgets {
            let Some(ap) = dict_get(&wd, b"AP") else {
                continue;
            };
            let ap = match ap {
                Obj::Dict(d) => d.clone(),
                Obj::Ref(r) => match resolve_ref(bytes, &doc, *r, &budget, &mut g).unwrap() {
                    Obj::Dict(d) => d,
                    _ => continue,
                },
                _ => continue,
            };
            let Some(Obj::Ref(nr)) = dict_get(&ap, b"N") else {
                continue;
            };
            if let Obj::Stream { dict, data } =
                resolve_ref(bytes, &doc, *nr, &budget, &mut g).unwrap()
            {
                assert_eq!(
                    dict_get(&dict, b"Subtype")
                        .and_then(as_name)
                        .as_ref()
                        .map(Bytes::as_slice),
                    Some(&b"Form"[..]),
                    "the widget's /AP /N is a Form XObject"
                );
                assert!(
                    dict_get(&dict, b"BBox").is_some(),
                    "the appearance has a /BBox"
                );
                out.push((dict, data.as_slice().to_vec()));
            }
        }
        out
    }

    /// The `(x, y)` of the first `Td` in an appearance content stream.
    fn first_td(content: &str) -> (f64, f64) {
        for line in content.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_suffix(" Td") {
                let mut nums = rest.split_whitespace();
                if let (Some(x), Some(y)) = (
                    nums.next().and_then(|n| n.parse::<f64>().ok()),
                    nums.next().and_then(|n| n.parse::<f64>().ok()),
                ) {
                    return (x, y);
                }
            }
        }
        panic!("appearance draws no text: {content}");
    }

    /// The `/BaseFont` of the font the appearance stream's `/Resources /Font`
    /// maps `/Helv` to, read back from the output bytes.
    fn appearance_font_base(bytes: &[u8], field_name: &str) -> Option<String> {
        let budget = test_budget();
        let mut g = budget.guard();
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).unwrap_or(0);
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("parse");
        let (dict, _) = widget_appearances_of(bytes, field_name)
            .into_iter()
            .next()?;
        let Obj::Dict(resources) = dict_get(&dict, b"Resources")? else {
            return None;
        };
        let Obj::Dict(fonts) = dict_get(resources, b"Font")? else {
            return None;
        };
        let (_, value) = fonts.iter().find(|(name, _)| name.as_slice() == b"Helv")?;
        let font: Vec<(Bytes, Obj)> = match value {
            Obj::Ref(reference) => {
                match resolve_ref(bytes, &doc, *reference, &budget, &mut g).ok()? {
                    Obj::Dict(d) => d,
                    _ => return None,
                }
            }
            Obj::Dict(d) => d.clone(),
            _ => return None,
        };
        dict_get(&font, b"BaseFont")
            .and_then(as_name)
            .map(|name| String::from_utf8_lossy(name.as_slice()).into_owned())
    }

    /// True when the `/Helv` value in an appearance's `/Resources /Font` is a
    /// direct dictionary (rather than a reference) read back from the bytes.
    fn appearance_font_is_inline_dict(bytes: &[u8], field_name: &str) -> bool {
        let (dict, _) = widget_appearances_of(bytes, field_name)
            .into_iter()
            .next()
            .expect("appearance");
        let Some(Obj::Dict(resources)) = dict_get(&dict, b"Resources") else {
            return false;
        };
        let Some(Obj::Dict(fonts)) = dict_get(resources, b"Font") else {
            return false;
        };
        fonts
            .iter()
            .any(|(name, value)| name.as_slice() == b"Helv" && matches!(value, Obj::Dict(_)))
    }

    #[test]
    fn synthesizes_a_text_field_appearance_stream() {
        let budget = test_budget();
        let mut g = budget.guard();
        let mut opts = edits(&[("Name", "Ada Lovelace")]);
        opts.synthesize_appearances = true;
        let (bytes, info) = fill_form(&form_fixture(), &opts, &budget, &mut g).expect("fill");
        assert_eq!(info.filled, 1);

        // The value still round-trips.
        let report = report_of(&bytes);
        let name = report
            .fields
            .iter()
            .find(|f| f.name == "Name")
            .expect("Name field");
        assert_eq!(name.value.as_deref(), Some("Ada Lovelace"));

        // The widget now carries a Form XObject appearance that draws the value.
        let (dict, data) = appearance_of(&bytes, "Name");
        let content = String::from_utf8_lossy(&data);
        assert!(
            content.contains("/Tx BMC"),
            "marked-content text: {content}"
        );
        assert!(
            content.contains(" Tf"),
            "selects the DA font/size: {content}"
        );
        assert!(
            content.contains("(Ada Lovelace) Tj"),
            "draws the value: {content}"
        );
        assert_eq!(
            dict_get(&dict, b"Subtype")
                .and_then(as_name)
                .as_ref()
                .map(Bytes::as_slice),
            Some(&b"Form"[..]),
            "a Form XObject",
        );
        let Some(Obj::Array(bbox)) = dict_get(&dict, b"BBox") else {
            panic!("the appearance has no /BBox");
        };
        assert_eq!(bbox.len(), 4);
        assert_eq!(as_i64(&bbox[2]), Some(190), "BBox width = Rect width");
        assert_eq!(as_i64(&bbox[3]), Some(20), "BBox height = Rect height");

        // A text-only synthesized fill does not also force a viewer rebuild.
        let acro = acroform_of(&bytes);
        assert_ne!(
            dict_get(&acro, b"NeedAppearances"),
            Some(&Obj::Bool(true)),
            "synthesized appearances should not force /NeedAppearances",
        );
    }

    /// Read back the choice fixtures: every filled combo/list box must carry a
    /// real `/AP /N` Form XObject drawing the selected option's display text,
    /// while `/V` and `/I` keep the export value and index.
    #[test]
    fn synthesizes_choice_and_listbox_appearance_streams() {
        let (bytes, info) = fill_synth(
            &choice_form_fixture(),
            &[
                ("Color", "b"),      // export value -> label "Blue"
                ("Size", "Large"),   // label -> export value "l"
                ("Custom", "typed"), // editable combo, value not an option
                ("Country", "ca"),   // unmerged list box -> label "Canada"
            ],
        );
        assert_eq!(info.filled, 4);

        // Values still round-trip through the existing read-back gate.
        let report = report_of(&bytes);
        let find = |name: &str| {
            report
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("field {name}"))
        };
        assert_eq!(find("Color").value.as_deref(), Some("b"));
        assert_eq!(find("Size").value.as_deref(), Some("l"));
        assert_eq!(find("Custom").value.as_deref(), Some("typed"));
        assert_eq!(find("Country").value.as_deref(), Some("ca"));

        // /V holds the export value; /I the selected index. The editable
        // combo's typed value matched no option, so it carries no /I.
        let color = field_dict_of(&bytes, "Color");
        assert_eq!(
            dict_get(&color, b"V").and_then(decode_text).as_deref(),
            Some("b")
        );
        assert_eq!(
            dict_get(&color, b"I"),
            Some(&Obj::Array(vec![Obj::Int(2)])),
            "the selected /Opt index"
        );
        let size = field_dict_of(&bytes, "Size");
        assert_eq!(
            dict_get(&size, b"V").and_then(decode_text).as_deref(),
            Some("l")
        );
        assert_eq!(dict_get(&size, b"I"), Some(&Obj::Array(vec![Obj::Int(2)])));
        let custom = field_dict_of(&bytes, "Custom");
        assert_eq!(
            dict_get(&custom, b"V").and_then(decode_text).as_deref(),
            Some("typed")
        );
        assert_eq!(
            dict_get(&custom, b"I"),
            None,
            "editable value has no /Opt index"
        );
        let country = field_dict_of(&bytes, "Country");
        assert_eq!(
            dict_get(&country, b"V").and_then(decode_text).as_deref(),
            Some("ca")
        );
        assert_eq!(
            dict_get(&country, b"I"),
            Some(&Obj::Array(vec![Obj::Int(1)]))
        );

        // The widget appearances are Form XObjects whose content draws the
        // label, per ISO 32000-1 §12.7.3.3 variable text.
        let cases: &[(&str, &str)] = &[
            ("Color", "(Blue) Tj"),
            ("Size", "(Large) Tj"),
            ("Custom", "(typed) Tj"),
            ("Country", "(Canada) Tj"),
        ];
        for (name, drawn) in cases {
            let appearances = widget_appearances_of(&bytes, name);
            assert_eq!(appearances.len(), 1, "one appearance for {name}");
            let content = String::from_utf8_lossy(&appearances[0].1);
            assert!(
                content.contains("/Tx BMC"),
                "variable text for {name}: {content}"
            );
            assert!(content.contains(" Tf"), "DA font for {name}: {content}");
            assert!(
                content.contains(drawn),
                "draws {drawn} for {name}: {content}"
            );
        }

        // Everything filled was synthesized, so no viewer rebuild is needed.
        let acro = acroform_of(&bytes);
        assert_ne!(dict_get(&acro, b"NeedAppearances"), Some(&Obj::Bool(true)));
    }

    /// A multi-select list box now draws every selected `/Opt` display label as
    /// a stacked line, and `/V` / `/I` carry all selections. Both the API's
    /// `values` list and the single-value form of the edit are accepted.
    #[test]
    fn multi_select_choice_draws_every_selected_label() {
        let fixture = choice_form_fixture();
        let budget = test_budget();
        let mut g = budget.guard();
        // The fixture's `/Opt` is [(x) (y)], so export == label here; two
        // distinct labels let us prove both were drawn.
        let opts = FillFormsOptions {
            fields: vec![FieldEdit {
                name: "Multi".to_string(),
                value: "x".to_string(),
                values: vec!["y".to_string()],
            }],
            synthesize_appearances: true,
        };
        let (bytes, info) = fill_form(&fixture, &opts, &budget, &mut g).expect("fill");
        assert_eq!(info.filled, 1);

        // The value list round-trips from the re-opened bytes.
        let report = report_of(&bytes);
        let multi = report
            .fields
            .iter()
            .find(|f| f.name == "Multi")
            .expect("field");
        assert!(multi.multi_select);
        assert_eq!(multi.values, ["x", "y"]);
        assert_eq!(
            multi.value, None,
            "a multi-valued choice has no single value"
        );

        // /V and /I carry every selection, in order.
        let field = field_dict_of(&bytes, "Multi");
        assert_eq!(
            dict_get(&field, b"V"),
            Some(&Obj::Array(vec![text_value("x"), text_value("y")]))
        );
        assert_eq!(
            dict_get(&field, b"I"),
            Some(&Obj::Array(vec![Obj::Int(0), Obj::Int(1)]))
        );

        // One Form XObject draws both labels, on separate stacked lines.
        let appearances = widget_appearances_of(&bytes, "Multi");
        assert_eq!(appearances.len(), 1);
        let content = String::from_utf8_lossy(&appearances[0].1);
        assert!(content.contains("/Tx BMC"), "variable text: {content}");
        assert!(
            content.contains("(x) Tj") && content.contains("(y) Tj"),
            "draws both selected labels: {content}"
        );
        let ys: Vec<f64> = content
            .lines()
            .filter(|line| line.trim_end().ends_with(" Td"))
            .filter_map(|line| {
                line.trim_end()
                    .trim_end_matches(" Td")
                    .split_whitespace()
                    .nth(1)
                    .and_then(|n| n.parse::<f64>().ok())
            })
            .collect();
        assert_eq!(ys.len(), 2, "one Td per selected label: {content}");
        assert!(ys[0] > ys[1], "labels stack downward: {ys:?}");

        let acro = acroform_of(&bytes);
        assert_ne!(dict_get(&acro, b"NeedAppearances"), Some(&Obj::Bool(true)));
    }

    /// A single value still works on a multi-select field (the common case),
    /// and genuinely impossible inputs are still refused with a typed error.
    #[test]
    fn multi_select_single_value_and_impossible_edits() {
        let fixture = choice_form_fixture();
        let (bytes, _) = fill_synth(&fixture, &[("Multi", "y")]);
        let field = field_dict_of(&bytes, "Multi");
        assert_eq!(
            dict_get(&field, b"V"),
            Some(&Obj::Array(vec![text_value("y")]))
        );
        assert_eq!(dict_get(&field, b"I"), Some(&Obj::Array(vec![Obj::Int(1)])));

        let cases: Vec<(Vec<FieldEdit>, &str)> = vec![
            (
                vec![FieldEdit {
                    name: "Multi".to_string(),
                    value: "x".to_string(),
                    values: vec!["not-an-option".to_string()],
                }],
                "has no option",
            ),
            (
                vec![FieldEdit {
                    name: "Color".to_string(), // single-select combo
                    value: "r".to_string(),
                    values: vec!["g".to_string()],
                }],
                "takes a single value",
            ),
        ];
        let text_budget = test_budget();
        let mut text_g = text_budget.guard();
        let text_error = fill_form(
            &form_fixture(),
            &FillFormsOptions {
                fields: vec![FieldEdit {
                    name: "Name".to_string(),
                    value: "a".to_string(),
                    values: vec!["b".to_string()],
                }],
                synthesize_appearances: true,
            },
            &text_budget,
            &mut text_g,
        )
        .expect_err("text field refuses multiple values");
        assert!(
            matches!(&text_error, FormError::Refused(detail) if detail.contains("takes a single value")),
            "error {text_error:?}"
        );
        for (fields, needle) in cases {
            let budget = test_budget();
            let mut g = budget.guard();
            let error = fill_form(
                &fixture,
                &FillFormsOptions {
                    fields,
                    synthesize_appearances: true,
                },
                &budget,
                &mut g,
            )
            .expect_err("refused");
            assert!(
                matches!(&error, FormError::Refused(detail) if detail.contains(needle)),
                "error {error:?} did not mention {needle:?}"
            );
        }
    }

    /// Quadding uses the exact AFM advance of a standard-14 face, not the old
    /// 0.5 em guess. `WAVE` in Helvetica is 2945/1000 em wide: at 12 pt in a
    /// 110 pt right-quadded box the baseline starts at 110 - 2 - 35.34 = 72.66.
    #[test]
    fn standard14_metrics_drive_exact_quadding() {
        let (bytes, _) = fill_synth(&metrics_dr_fixture(), &[("Std", "w")]);
        let appearances = widget_appearances_of(&bytes, "Std");
        let content = String::from_utf8_lossy(&appearances[0].1);
        let (x, _) = first_td(&content);
        assert!(
            (x - 72.66).abs() < 0.01,
            "exact Helvetica width, not the 84 pt 0.5-em guess: x={x}, {content}"
        );
        assert_eq!(
            appearance_font_base(&bytes, "Std").as_deref(),
            Some("Helvetica")
        );
    }

    /// A field-level or widget-level `/DR` overrides the form map. Both override
    /// fields use Courier (600/1000 em uniform): `WAVE` is 28.8 pt, so the
    /// right-quadded origin is 110 - 2 - 28.8 = 79.2, and the appearance's
    /// resource points at the Courier object. An embedded / non-standard face
    /// keeps the honest 0.5 em fallback (84 pt) rather than a fake exact metric.
    #[test]
    fn per_field_and_widget_dr_override_the_form_font() {
        let (bytes, _) = fill_synth(
            &metrics_dr_fixture(),
            &[("Local", "w"), ("WidgetLocal", "w"), ("Embedded", "w")],
        );
        for name in ["Local", "WidgetLocal"] {
            let appearances = widget_appearances_of(&bytes, name);
            let content = String::from_utf8_lossy(&appearances[0].1);
            let (x, _) = first_td(&content);
            assert!(
                (x - 79.2).abs() < 0.01,
                "Courier metrics for {name}: x={x}, {content}"
            );
            assert_eq!(
                appearance_font_base(&bytes, name).as_deref(),
                Some("Courier"),
                "{name} uses its own /DR font"
            );
        }
        let appearances = widget_appearances_of(&bytes, "Embedded");
        let content = String::from_utf8_lossy(&appearances[0].1);
        let (x, _) = first_td(&content);
        assert!(
            (x - 84.0).abs() < 0.01,
            "non-standard face keeps the honest fallback: x={x}, {content}"
        );
        assert_eq!(
            appearance_font_base(&bytes, "Embedded").as_deref(),
            Some("CustomFace")
        );
    }

    #[test]
    fn choice_synthesis_keeps_need_appearances_without_a_font() {
        let (bytes, info) = fill_synth(&choice_no_font_fixture(), &[("Color", "g")]);
        assert_eq!(info.filled, 1);
        let report = report_of(&bytes);
        assert_eq!(
            report.fields[0].value.as_deref(),
            Some("g"),
            "the value still round-trips"
        );
        assert_eq!(
            widget_appearances_of(&bytes, "Color").len(),
            0,
            "no appearance was claimed without a /DR font"
        );
        let acro = acroform_of(&bytes);
        assert_eq!(
            dict_get(&acro, b"NeedAppearances"),
            Some(&Obj::Bool(true)),
            "the viewer is still asked to rebuild"
        );
    }

    #[test]
    fn mixed_text_and_choice_fill_synthesizes_both_appearances() {
        let (bytes, _) = fill_synth(&form_fixture(), &[("Name", "Ada"), ("Color", "Green")]);
        for (name, drawn) in [("Name", "(Ada) Tj"), ("Color", "(Green) Tj")] {
            let appearances = widget_appearances_of(&bytes, name);
            assert_eq!(appearances.len(), 1, "one appearance for {name}");
            let content = String::from_utf8_lossy(&appearances[0].1);
            assert!(
                content.contains(drawn),
                "draws {drawn} for {name}: {content}"
            );
        }
        let acro = acroform_of(&bytes);
        assert_ne!(dict_get(&acro, b"NeedAppearances"), Some(&Obj::Bool(true)));
    }

    /// An editable multi-select accepts a typed value that is not an option:
    /// the value is stored in the `/V` array with no `/I` index, and the
    /// appearance draws the typed text as its own stacked label.
    #[test]
    fn editable_multi_select_draws_unmatched_typed_value() {
        let fixture = editable_multi_fixture();
        let budget = test_budget();
        let mut g = budget.guard();
        let opts = FillFormsOptions {
            fields: vec![FieldEdit {
                name: "Tags".to_string(),
                value: "a".to_string(), // matched export -> "Alpha"
                values: vec!["custom".to_string()], // typed, matches no option
            }],
            synthesize_appearances: true,
        };
        let (bytes, info) = fill_form(&fixture, &opts, &budget, &mut g).expect("fill");
        assert_eq!(info.filled, 1);

        // The re-opened bytes report both selections as a multi-value array.
        let tags = report_of(&bytes)
            .fields
            .into_iter()
            .find(|field| field.name == "Tags")
            .expect("Tags field");
        assert!(tags.multi_select);
        assert!(tags.editable);
        assert_eq!(tags.values, ["a", "custom"]);
        assert_eq!(
            tags.value, None,
            "a multi-valued choice has no single value"
        );

        // /V holds both export values; /I holds only the matched index.
        let field = field_dict_of(&bytes, "Tags");
        assert_eq!(
            dict_get(&field, b"V"),
            Some(&Obj::Array(vec![text_value("a"), text_value("custom")]))
        );
        assert_eq!(
            dict_get(&field, b"I"),
            Some(&Obj::Array(vec![Obj::Int(0)])),
            "the unmatched typed value has no /I index"
        );

        // One appearance draws the matched label and the typed value as two
        // stacked lines.
        let appearances = widget_appearances_of(&bytes, "Tags");
        assert_eq!(appearances.len(), 1);
        let content = String::from_utf8_lossy(&appearances[0].1);
        assert!(content.contains("/Tx BMC"), "variable text: {content}");
        assert!(content.contains("(Alpha) Tj"), "matched label: {content}");
        assert!(content.contains("(custom) Tj"), "typed value: {content}");
        let ys: Vec<f64> = content
            .lines()
            .filter(|line| line.trim_end().ends_with(" Td"))
            .filter_map(|line| {
                line.trim_end()
                    .trim_end_matches(" Td")
                    .split_whitespace()
                    .nth(1)
                    .and_then(|n| n.parse::<f64>().ok())
            })
            .collect();
        assert_eq!(ys.len(), 2, "one Td per value: {content}");
        assert!(ys[0] > ys[1], "labels stack downwards: {ys:?}");

        let acro = acroform_of(&bytes);
        assert_ne!(dict_get(&acro, b"NeedAppearances"), Some(&Obj::Bool(true)));
    }

    /// Direct font dictionaries in `/DR /Font` resolve at both the form and
    /// field level: the resource is carried inline into the appearance, and the
    /// `/BaseFont` drives the same exact AFM metrics a reference would.
    #[test]
    fn inline_dictionary_fonts_resolve_from_form_and_field_dr() {
        let (bytes, _) = fill_synth(
            &inline_dict_font_fixture(),
            &[("FormDict", "w"), ("FieldDict", "w")],
        );

        // Form-level inline dictionary: Helvetica metrics, inline resource.
        let form_data = widget_appearances_of(&bytes, "FormDict")
            .into_iter()
            .next()
            .expect("one appearance")
            .1;
        let content = String::from_utf8_lossy(&form_data);
        let (x, _) = first_td(&content);
        assert!(
            (x - 72.66).abs() < 0.01,
            "Helvetica inline dictionary metrics: x={x}, {content}"
        );
        assert_eq!(
            appearance_font_base(&bytes, "FormDict").as_deref(),
            Some("Helvetica")
        );
        assert!(
            appearance_font_is_inline_dict(&bytes, "FormDict"),
            "the form's direct font dictionary is carried into the /AP resource"
        );

        // Field-level inline dictionary overrides the form: Courier metrics.
        let field_data = widget_appearances_of(&bytes, "FieldDict")
            .into_iter()
            .next()
            .expect("one appearance")
            .1;
        let content = String::from_utf8_lossy(&field_data);
        let (x, _) = first_td(&content);
        assert!(
            (x - 79.2).abs() < 0.01,
            "Courier inline dictionary metrics: x={x}, {content}"
        );
        assert_eq!(
            appearance_font_base(&bytes, "FieldDict").as_deref(),
            Some("Courier")
        );
        assert!(
            appearance_font_is_inline_dict(&bytes, "FieldDict"),
            "the field's direct font dictionary is carried into the /AP resource"
        );
    }

    /// A high WinAnsi byte is written verbatim to the appearance string, so its
    /// AFM advance (not the 0.5 em heuristic) must position a right-quadded
    /// label. The expected x is computed from the font crate's own AFM widths.
    #[test]
    fn winansi_high_byte_metrics_drive_exact_quadding() {
        let (bytes, _) = fill_synth(&winansi_metrics_fixture(), &[("HiByte", "w")]);
        let (dict, data) = widget_appearances_of(&bytes, "HiByte")
            .into_iter()
            .next()
            .expect("one appearance");
        assert!(data.contains(&0xe9), "the high byte is drawn verbatim");

        let content = String::from_utf8_lossy(&data);
        let (x, _) = first_td(&content);
        let upper_w = f64::from(selis_font::standard14::width("Helvetica", "W").expect("W width"));
        let eacute =
            f64::from(selis_font::standard14::width("Helvetica", "eacute").expect("eacute width"));
        let text_width = (upper_w + eacute) / 1000.0 * 12.0;
        let expected = 110.0 - 2.0 - text_width;
        assert!(
            (x - expected).abs() < 0.01,
            "exact WinAnsi width, not the 0.5 em guess: x={x}, expected {expected}, {content}"
        );
        assert!(
            (expected - 96.0).abs() > 1.0,
            "the 0.5 em guess must differ from the exact metric"
        );
        assert_eq!(
            appearance_font_base(&bytes, "HiByte").as_deref(),
            Some("Helvetica")
        );
        assert_eq!(
            dict_get(&dict, b"Subtype")
                .and_then(as_name)
                .as_ref()
                .map(Bytes::as_slice),
            Some(&b"Form"[..])
        );
    }

    /// Unassigned WinAnsi codes return no glyph name, so the width fallback is
    /// honest rather than a fabricated metric.
    #[test]
    fn winansi_unassigned_byte_has_no_glyph_name() {
        assert_eq!(winansi_glyph_name(0xe9), Some("eacute"));
        assert_eq!(winansi_glyph_name(0x80), Some("Euro"));
        assert_eq!(winansi_glyph_name(0x81), None, ".notdef has no glyph");
    }

    /// Annex D.4 assignments: 0x27 is quotesingle, 0x92 is quoteright, 0xE9 is
    /// eacute, and 0x19 is unassigned (not quoteright). The old ASCII lookup
    /// used quoteright for 0x27 and keyed digits by the character itself —
    /// neither name is a key in the AFM tables.
    #[test]
    fn winansi_apostrophe_and_high_byte_glyph_names() {
        assert_eq!(winansi_glyph_name(0x27), Some("quotesingle"));
        assert_eq!(winansi_glyph_name(0x92), Some("quoteright"));
        assert_eq!(winansi_glyph_name(0xe9), Some("eacute"));
        assert_eq!(
            winansi_glyph_name(0x19),
            None,
            "0x19 is unassigned in WinAnsi"
        );
        assert_eq!(selis_font::standard14::width("Helvetica", "7"), None);
        assert_eq!(selis_font::standard14::width("Helvetica", "'"), None);
    }

    /// A digit-rich label is measured by the AFM widths of `zero`..`nine`, not
    /// the 500/1000 em fallback the old per-character lookup produced. "777"
    /// is three Helvetica `seven` glyphs (556 each): at 12 pt in a 110 pt box a
    /// centred label starts at (110 - 3*556/1000*12)/2 = 44.992. The expected
    /// value is computed from the font crate's AFM table itself.
    #[test]
    fn afm_digit_widths_drive_exact_quadding() {
        let (bytes, _) = fill_synth(&glyph_name_metrics_fixture(), &[("Digits", "w")]);
        let apps = widget_appearances_of(&bytes, "Digits");
        let content = String::from_utf8_lossy(&apps[0].1);
        assert!(apps[0].1.contains(&b'7'), "the digits are drawn verbatim");
        let (x, _) = first_td(&content);

        let seven = f64::from(
            selis_font::standard14::width("Helvetica", "seven").expect("seven AFM width"),
        );
        let text_w = 3.0 * seven / 1000.0 * 12.0;
        let expected = (110.0 - text_w) / 2.0;
        assert!(
            (x - expected).abs() < 0.01,
            "exact AFM digit widths, not the 500 fallback: x={x}, expected {expected}, {content}"
        );
        // The wrong key has no AFM entry, which is why the old code silently
        // used 500 and put the label at 46.0 instead.
        assert_eq!(selis_font::standard14::width("Helvetica", "7"), None);
        assert!((expected - 46.0).abs() > 0.5, "the fallback must differ");
    }

    /// An apostrophe is drawn as byte 0x27, i.e. WinAnsi `quotesingle` (191 in
    /// Helvetica), not `quoteright` (222). The right-quadded "IT'S" origin must
    /// come from the former; the expected x is computed from the AFM table.
    #[test]
    fn afm_apostrophe_width_drives_exact_quadding() {
        let (bytes, _) = fill_synth(&glyph_name_metrics_fixture(), &[("Apos", "w")]);
        let apps = widget_appearances_of(&bytes, "Apos");
        let content = String::from_utf8_lossy(&apps[0].1);
        assert!(
            apps[0].1.contains(&b'\''),
            "the apostrophe byte is drawn verbatim"
        );
        let (x, _) = first_td(&content);

        let aw = |g: &str| f64::from(selis_font::standard14::width("Helvetica", g).expect(g));
        let text_w = (aw("I") + aw("T") + aw("quotesingle") + aw("S")) / 1000.0 * 12.0;
        let expected = 110.0 - 2.0 - text_w;
        assert!(
            (x - expected).abs() < 0.01,
            "quotesingle AFM width, not quoteright: x={x}, expected {expected}, {content}"
        );
        let wrong_w = (aw("I") + aw("T") + aw("quoteright") + aw("S")) / 1000.0 * 12.0;
        assert!(
            (expected - (110.0 - 2.0 - wrong_w)).abs() > 0.1,
            "quoteright would have given a measurably different origin"
        );
    }
}
