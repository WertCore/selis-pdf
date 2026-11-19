//! SL-5.JS.02 — detection of form scripts for the consent gate.
//!
//! A PDF may carry active content in three places this crate cares about:
//!
//! * `/JS` — a document- or field-level JavaScript action;
//! * `/AA` — an additional-actions dictionary, whose `/C` entry is a calculate
//!   action (`/CO` in the AcroForm names the evaluation order);
//! * `/CO` — the AcroForm calculation order.
//!
//! This module is deliberately **read-only and wasm-compatible**: it reports
//! what is present so a shell can decide whether to ask the reader for consent.
//! It never evaluates a script. Evaluation is the job of `selis-pdf-js` (native)
//! or the browser sandbox, both of which call a consent gate before running
//! anything (ADR-P0020). Detection here is bounded: a hostile document cannot
//! make the walk unbounded or smuggle an over-long script past the limit.
//!
//! ## Supported event subset (SL-5.FORM.03)
//!
//! Three field-level `/AA` actions are recognised, in the order a native shell
//! evaluates them after consent:
//!
//! 1. **calculate** — `/AA /C /JS`, sequenced by the AcroForm `/CO` array;
//! 2. **validate** — `/AA /V /JS`, run on each field with a candidate value;
//! 3. **format** — `/AA /F /JS`, run on each field after it validates.
//!
//! `/CO` entries may be field names or references to field dictionaries; both
//! are resolved to a fully-qualified name. Calculate actions whose field is in
//! `/CO` are emitted in that order; any calculate action *not* named in `/CO`
//! is appended in field-tree walk order rather than silently dropped (a
//! defensive best effort, since a conforming form lists them all). Validate and
//! format actions have no document-declared global order and are emitted in
//! field-tree walk order.
//!
//! Not claimed: field-level `/AA /K` (keystroke) actions, document-level `/JS`
//! execution, Acrobat's per-keystroke re-entrancy, and the exact moment Acrobat
//! re-runs calculations during editing. This is a commit-time sequence, not a
//! full replica of Acrobat's event engine.

use super::{decode_text, dict_get, join_name, open_doc, resolve_deep, FormError};
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};
use serde::{Deserialize, Serialize};

/// The longest script source this scanner will report. Longer strings are
/// counted as "has scripts" but their source is omitted: the runner will refuse
/// them with `JS_SCRIPT_REJECTED` against the sandbox budget anyway, and no
/// document-derived text is echoed into a diagnostic (ADR-P0017).
const MAX_SCRIPT_BYTES: usize = 64 * 1024;
/// A form cannot legitimately carry more calculation actions than this; the
/// rest are ignored after the flag is set.
const MAX_CALCULATIONS: usize = 256;
/// Walk depth guard for nested field trees.
const MAX_FIELD_DEPTH: usize = 32;

/// One calculate action discovered in the form (`/AA /C /JS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalcScript {
    /// The fully qualified field name this action calculates.
    pub field: String,
    /// The JavaScript source of the action (at most [`MAX_SCRIPT_BYTES`]).
    pub script: String,
}

/// One validate action discovered in the form (`/AA /V /JS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateScript {
    /// The fully qualified field name this action validates.
    pub field: String,
    /// The JavaScript source of the action (at most [`MAX_SCRIPT_BYTES`]).
    pub script: String,
}

/// One format action discovered in the form (`/AA /F /JS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatScript {
    /// The fully qualified field name this action formats.
    pub field: String,
    /// The JavaScript source of the action (at most [`MAX_SCRIPT_BYTES`]).
    pub script: String,
}

/// The read-only script manifest of a document (SL-5.FORM.03).
///
/// `has_scripts` is the typed boolean a shell uses to decide whether to show a
/// consent prompt. The three action lists are what a shell may evaluate *after*
/// consent has been granted, in the Acrobat subset order
/// **calculate (`/CO`) → validate (`/V`) → format (`/F`)**.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptManifest {
    /// True when any `/JS`, `/AA` or `/CO` key was found.
    pub has_scripts: bool,
    /// Calculate actions in `/CO` order (see the module docs for the ordering
    /// rule and its honest limits).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calculations: Vec<CalcScript>,
    /// Validate actions, in field-tree walk order (Acrobat defines no global
    /// order for validation; it runs on the field being committed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validations: Vec<ValidateScript>,
    /// Format actions, in field-tree walk order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<FormatScript>,
}

/// Scan a document for form-script markers and calculation actions.
///
/// # Budget
///
/// One document parse plus a bounded walk of the AcroForm field tree. Every
/// visited field ticks `budget`; the manifest never grows past
/// [`MAX_CALCULATIONS`] entries.
///
/// # Malformed Input
///
/// An encrypted document is refused. Unresolvable references and wrong-shaped
/// objects are skipped rather than guessed at; a document that cannot be opened
/// is a typed [`FormError::Malformed`].
///
/// # Errors
///
/// [`FormError::Encrypted`] for an encrypted document,
/// [`FormError::Malformed`]/[`FormError::Unresolvable`] for one this crate
/// cannot read.
pub fn scan_form_scripts(
    src: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<ScriptManifest, FormError> {
    let doc = open_doc(src, budget, g)?;
    let mut manifest = ScriptManifest::default();
    let Some(acro) = selis_pdf_cos::reconcile::catalog_entry(src, &doc, b"AcroForm", budget, g)
        .map_err(|e| FormError::Unresolvable(format!("cannot read /AcroForm: {e}")))?
    else {
        return Ok(manifest);
    };
    let acro = resolve_deep(src, &doc, acro, 0, budget, g);
    let Obj::Dict(pairs) = &acro else {
        return Ok(manifest);
    };

    if dict_get(pairs, b"JS").is_some() || dict_get(pairs, b"AA").is_some() {
        manifest.has_scripts = true;
    }

    // /CO is an array of field names (or refs to field dicts) in calculation
    // order. It is itself evidence of active content even when a field's /AA
    // action lives in an object the shallow walk below does not reach.
    let mut order: Vec<String> = Vec::new();
    if let Some(Obj::Array(items)) = dict_get(pairs, b"CO") {
        manifest.has_scripts = true;
        for item in items {
            let value = resolve_deep(src, &doc, item.clone(), 0, budget, g);
            if let Some(name) = decode_text(&value) {
                order.push(name);
            }
        }
    }

    // Collect per-field actions with fully-qualified names; ordering is applied
    // once the whole tree is known (calculate uses /CO, the others use walk
    // order).
    let mut found: Vec<(String, ScriptEvent, String)> = Vec::new();
    if let Some(Obj::Array(fields)) = dict_get(pairs, b"Fields") {
        for item in fields {
            walk_scripts(src, &doc, item, "", budget, g, 0, &mut manifest, &mut found)?;
        }
    }

    let mut calculates: Vec<(String, String)> = Vec::new();
    let mut validates: Vec<(String, String)> = Vec::new();
    let mut formats: Vec<(String, String)> = Vec::new();
    for (field, event, script) in found {
        let slot = match event {
            ScriptEvent::Calculate => &mut calculates,
            ScriptEvent::Validate => &mut validates,
            ScriptEvent::Format => &mut formats,
        };
        if slot.len() < MAX_CALCULATIONS {
            slot.push((field, script));
        }
    }
    // Emit calculate actions in /CO order, then append any that /CO omitted.
    let mut emitted = vec![false; calculates.len()];
    for name in &order {
        for (index, (field, _)) in calculates.iter().enumerate() {
            let already = emitted.get(index).copied().unwrap_or(true);
            if !already && field == name {
                let script = calculates
                    .get(index)
                    .map(|(_, source)| source.clone())
                    .unwrap_or_default();
                manifest.calculations.push(CalcScript {
                    field: field.clone(),
                    script,
                });
                if let Some(flag) = emitted.get_mut(index) {
                    *flag = true;
                }
            }
        }
    }
    for (index, (field, script)) in calculates.into_iter().enumerate() {
        if !emitted.get(index).copied().unwrap_or(true) {
            manifest.calculations.push(CalcScript { field, script });
        }
    }
    manifest.validations = validates
        .into_iter()
        .map(|(field, script)| ValidateScript { field, script })
        .collect();
    manifest.formats = formats
        .into_iter()
        .map(|(field, script)| FormatScript { field, script })
        .collect();
    Ok(manifest)
}

/// Which `/AA` action a discovered script belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptEvent {
    Calculate,
    Validate,
    Format,
}

/// Recursively walk a field (and its kids) for `/JS` and `/AA` actions.
#[allow(clippy::too_many_arguments)]
fn walk_scripts(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    item: &Obj,
    prefix: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    depth: usize,
    out: &mut ScriptManifest,
    found: &mut Vec<(String, ScriptEvent, String)>,
) -> Result<(), FormError> {
    if depth > MAX_FIELD_DEPTH {
        return Ok(());
    }
    g.tick()
        .map_err(|e| FormError::Limit(format!("script scan budget exhausted: {e}")))?;
    let value = resolve_deep(src, doc, item.clone(), 0, budget, g);
    let Obj::Dict(pairs) = &value else {
        return Ok(());
    };

    let own_name = dict_get(pairs, b"T")
        .and_then(|t| decode_text(&resolve_deep(src, doc, t.clone(), 0, budget, g)));
    let name = join_name(prefix, own_name.as_deref());

    // `/JS` directly on a field is evidence of active content but has no event
    // signature this module can sequence; it is reported as a marker only.
    if dict_get(pairs, b"JS").is_some() {
        out.has_scripts = true;
    }

    if let Some(Obj::Dict(aa)) = dict_get(pairs, b"AA") {
        out.has_scripts = true;
        for (key, event) in [
            (&b"C"[..], ScriptEvent::Calculate),
            (&b"V"[..], ScriptEvent::Validate),
            (&b"F"[..], ScriptEvent::Format),
        ] {
            if let Some(Obj::Dict(action)) = dict_get(aa, key) {
                if let Some(js) = dict_get(action, b"JS") {
                    if let Some(source) =
                        decode_text(&resolve_deep(src, doc, js.clone(), 0, budget, g))
                    {
                        if source.len() <= MAX_SCRIPT_BYTES {
                            found.push((name.clone(), event, source));
                        }
                    }
                }
            }
        }
    }

    if let Some(Obj::Array(kids)) = dict_get(pairs, b"Kids") {
        for kid in kids {
            walk_scripts(
                src,
                doc,
                kid,
                &name,
                budget,
                g,
                depth.saturating_add(1),
                out,
                found,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::panic
    )]

    use super::*;
    use selis_sandbox::Budget;

    /// A minimal one-page PDF from raw object bodies, with a correct xref.
    fn build_pdf(objects: &[(u32, &str)]) -> Vec<u8> {
        let max = objects.iter().map(|(num, _)| *num).max().unwrap_or(0);
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = vec![0u64; max as usize + 1];
        for (num, body) in objects {
            offsets[*num as usize] = out.len() as u64;
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body.as_bytes());
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

    fn scan(bytes: &[u8]) -> ScriptManifest {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        scan_form_scripts(bytes, &budget, &mut g).expect("scan")
    }

    /// A minimal one-page PDF with the supplied AcroForm body.
    fn pdf(acro_body: &str) -> Vec<u8> {
        let objects: Vec<(u32, &str)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, acro_body),
        ];
        build_pdf(&objects)
    }

    #[test]
    fn a_plain_form_has_no_scripts() {
        let bytes = pdf("<< /Fields [6 0 R] >>");
        let manifest = scan(&bytes);
        assert!(!manifest.has_scripts);
        assert!(manifest.calculations.is_empty());
    }

    #[test]
    fn a_document_level_js_action_is_detected() {
        let bytes = pdf("<< /Fields [] /JS (app.alert\\(1\\)) /AA << /WC 7 0 R >> >>");
        let manifest = scan(&bytes);
        assert!(manifest.has_scripts);
    }

    #[test]
    fn a_co_calculation_order_is_detected() {
        let bytes = pdf("<< /Fields [] /CO [6 0 R] >>");
        let manifest = scan(&bytes);
        assert!(manifest.has_scripts);
    }

    #[test]
    fn a_calculate_action_yields_the_field_and_source() {
        let objects: Vec<(u32, &str)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Fields [6 0 R] /CO [6 0 R] >>"),
            (
                6,
                "<< /FT /Tx /T (Total) /AA << /C << /JS (event.value = '42';) >> >> >>",
            ),
        ];
        let bytes = build_pdf(&objects);
        let manifest = scan(&bytes);
        assert!(manifest.has_scripts);
        assert_eq!(manifest.calculations.len(), 1);
        assert_eq!(manifest.calculations[0].field, "Total");
        assert_eq!(manifest.calculations[0].script, "event.value = '42';");
    }

    #[test]
    fn validate_and_format_actions_are_discovered() {
        let objects: Vec<(u32, &str)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Fields [6 0 R 7 0 R] >>"),
            (
                6,
                "<< /FT /Tx /T (Qty) /AA << /V << /JS (event.rc = Number(event.value) > 0;) >> /F << /JS (event.value = util.printf('%d', event.value);) >> >> >>",
            ),
            (
                7,
                "<< /FT /Tx /T (Group) /Kids [8 0 R] >>",
            ),
            (
                8,
                "<< /Subtype /Widget /Parent 7 0 R /AA << /V << /JS (event.rc = true;) >> >> >>",
            ),
        ];
        let bytes = build_pdf(&objects);
        let manifest = scan(&bytes);
        assert_eq!(manifest.validations.len(), 2);
        assert_eq!(manifest.validations[0].field, "Qty");
        assert_eq!(manifest.validations[1].field, "Group");
        assert_eq!(manifest.formats.len(), 1);
        assert_eq!(manifest.formats[0].field, "Qty");
        assert!(manifest.formats[0].script.contains("util.printf"));
    }

    #[test]
    fn calculations_are_emitted_in_co_order() {
        let objects: Vec<(u32, &str)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Fields [6 0 R 7 0 R] /CO [(B) (A)] >>"),
            (
                6,
                "<< /FT /Tx /T (A) /AA << /C << /JS (event.value = 'a';) >> >> >>",
            ),
            (
                7,
                "<< /FT /Tx /T (B) /AA << /C << /JS (event.value = 'b';) >> >> >>",
            ),
        ];
        let bytes = build_pdf(&objects);
        let manifest = scan(&bytes);
        let names: Vec<&str> = manifest
            .calculations
            .iter()
            .map(|c| c.field.as_str())
            .collect();
        assert_eq!(names, ["B", "A"]);
    }
}
