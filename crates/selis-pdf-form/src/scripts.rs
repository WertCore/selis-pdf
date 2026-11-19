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

use super::{decode_text, dict_get, open_doc, resolve_deep, FormError};
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

/// One calculation action discovered in the form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalcScript {
    /// The fully qualified field name this `/AA /C /JS` action calculates.
    pub field: String,
    /// The JavaScript source of the action (at most [`MAX_SCRIPT_BYTES`]).
    pub script: String,
}

/// The read-only script manifest of a document.
///
/// `has_scripts` is the typed boolean a shell uses to decide whether to show a
/// consent prompt. `calculations` is the ordered list a shell may evaluate
/// *after* consent has been granted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptManifest {
    /// True when any `/JS`, `/AA` or `/CO` key was found.
    pub has_scripts: bool,
    /// Calculation actions in `/CO` order (best effort; see module docs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calculations: Vec<CalcScript>,
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

    if let Some(Obj::Array(fields)) = dict_get(pairs, b"Fields") {
        for item in fields {
            walk_scripts(src, &doc, item, &order, budget, g, 0, &mut manifest)?;
        }
    }
    Ok(manifest)
}

/// Recursively walk a field (and its kids) for `/JS` and `/AA /C /JS`.
#[allow(clippy::too_many_arguments)]
fn walk_scripts(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    item: &Obj,
    order: &[String],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
    depth: usize,
    out: &mut ScriptManifest,
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

    let name = dict_get(pairs, b"T")
        .and_then(|t| decode_text(&resolve_deep(src, doc, t.clone(), 0, budget, g)))
        .unwrap_or_default();

    // A direct /JS on the field, or a calculate action under /AA /C.
    let mut script: Option<String> = None;
    if let Some(js) = dict_get(pairs, b"JS") {
        script = decode_text(&resolve_deep(src, doc, js.clone(), 0, budget, g));
    }
    if script.is_none() {
        if let Some(Obj::Dict(aa)) = dict_get(pairs, b"AA") {
            out.has_scripts = true;
            if let Some(Obj::Dict(calc)) = dict_get(aa, b"C") {
                if let Some(js) = dict_get(calc, b"JS") {
                    script = decode_text(&resolve_deep(src, doc, js.clone(), 0, budget, g));
                }
            }
        }
    }
    if let Some(source) = script {
        out.has_scripts = true;
        if out.calculations.len() < MAX_CALCULATIONS && source.len() <= MAX_SCRIPT_BYTES {
            out.calculations.push(CalcScript {
                field: if name.is_empty() {
                    order.first().cloned().unwrap_or_default()
                } else {
                    name.clone()
                },
                script: source,
            });
        }
    }

    if let Some(Obj::Array(kids)) = dict_get(pairs, b"Kids") {
        for kid in kids {
            walk_scripts(
                src,
                doc,
                kid,
                order,
                budget,
                g,
                depth.saturating_add(1),
                out,
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
}
