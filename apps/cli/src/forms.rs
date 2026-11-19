//! The `selis form-fields` and `selis fill-forms` commands (Phase 4 — fill
//! forms, SL-5.JS.02 consent gate).
//!
//! `form-fields` prints the AcroForm as JSON: every field's fully qualified
//! name, kind, current value, choice options / button states, and flags. This
//! is the read half: it never writes a byte of the input.
//!
//! `fill-forms` takes repeatable `--set NAME=VALUE` pairs and writes a filled
//! PDF. Every filled value is re-read from the produced bytes before the file
//! is committed (see `selis_pdf_tools::fill_forms`); a value that does not
//! round-trip leaves no file behind.
//!
//! # Active content is off by default (ADR-P0020)
//!
//! A form carrying `/JS`, `/AA` or `/CO` is never evaluated silently. The CLI
//! has no interactive prompt, so it refuses with a typed error that names how
//! to grant one-time consent: `--allow-js` or `SELIS_ALLOW_JS=1`. Only after
//! that explicit grant does the form-fill path call
//! [`selis_pdf_js::JsSandbox::run_authorized`], which re-checks the consent
//! gate before the interpreter is touched. A script can never auto-run.

use selis_pdf_form::{FormField, ScriptManifest};
use selis_pdf_js::{EventKind, Field, JsBudget, JsRequest, JsSandbox};
use selis_policy::{
    ApprovedOrigins, ConsentPolicy, ConsentScope, DocumentIdentity, ScriptAuthorization,
};

use crate::{CliError, CliResult};

/// A form carries scripts but the reader has not granted consent.
///
/// The error is typed at the CLI boundary as a `CliError` (which is what every
/// command returns), and its text names both ways to grant one-time consent.
/// This is deliberately *before* any interpreter work: the native shell never
/// opens QuickJS for a document that has not been approved.
fn consent_required(path: &str) -> CliError {
    CliError(format!(
        "{path}: this form contains JavaScript actions (/JS, /AA, /CO). \
         Selis never runs form scripts without consent. Grant one-time consent \
         with `--allow-js` (or set `SELIS_ALLOW_JS=1`) to evaluate its /CO \
         calculations; no file was written."
    ))
}

/// `selis form-fields <path> [--output file]`
///
/// # Errors
///
/// [`CliError`] when the file cannot be read, is encrypted, or the report
/// cannot be encoded.
pub(crate) fn list(path: &str, output: Option<&str>) -> CliResult<()> {
    let report = selis_pdf_tools::form_fields(path).map_err(CliError::from)?;
    let json = report
        .to_json()
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    match output {
        Some(file) => super::write_file(file, &json),
        None => {
            println!("{}", String::from_utf8_lossy(&json));
            Ok(())
        }
    }
}

/// Evaluate a document's `/CO` calculation actions inside the budgeted native
/// sandbox, gated by reader consent (SL-5.JS.02/03).
///
/// `allow_js` is the CLI's one-time consent grant. When false the gate holds no
/// grant and every [`JsSandbox::run_authorized`] call returns
/// `JS_CONSENT_REQUIRED` before touching the interpreter — defence in depth
/// behind [`consent_required`]. When true the grant is recorded for exactly
/// this document id, so a second document never inherits it.
///
/// Returns the computed `(field, value)` pairs in `/CO` order.
///
/// # Budget
///
/// Every run uses [`JsBudget::conservative`] (ticks, heap, stack, script size,
/// field and string limits). The manifest's scanner is itself bounded in
/// `selis-pdf-form`; this function adds at most one run per discovered action
/// and never loops on document-controlled data.
///
/// # Malformed Input
///
/// The scanner reports only bounded script sources; over-long sources are left
/// to the sandbox's typed `JS_SCRIPT_REJECTED`. No script text or document byte
/// is echoed into an error (ADR-P0017).
///
/// # Errors
///
/// `JS_CONSENT_REQUIRED` without consent, or any typed sandbox error (API
/// refusal, budget exhausted, runtime error). No script text is echoed.
fn evaluate_calculations(
    manifest: &ScriptManifest,
    fields: &[FormField],
    allow_js: bool,
    doc_id: &str,
) -> CliResult<Vec<(String, String)>> {
    if manifest.calculations.is_empty() {
        return Ok(Vec::new());
    }

    // The gate is rebuilt per call with an empty admin list. Only the reader's
    // explicit `allow_js` grant can open it; no origin is ever pre-approved by
    // the CLI. Consent is keyed on this document id alone (per-document scope).
    let origins = ApprovedOrigins::new();
    let doc = DocumentIdentity::new(doc_id.to_string(), "local");
    let mut consent = ConsentPolicy::new();
    if allow_js {
        consent.grant(&doc, ConsentScope::Session);
    }
    let gate = ScriptAuthorization::new(&consent, &origins);

    // Defense in depth: the gate denies before any interpreter work even if a
    // future caller forgets the CLI's earlier `consent_required` refusal. The
    // interactive shell never reaches this point without `allow_js`.
    gate.authorize(&doc)
        .map_err(|e| CliError(format!("form script refused: {e}")))?;

    let mut sandbox = JsSandbox::new().map_err(|e| CliError(format!("js runtime: {e}")))?;
    let budget = JsBudget::conservative();

    let mut values: Vec<(String, String)> = fields
        .iter()
        .map(|f| (f.name.clone(), f.value.clone().unwrap_or_default()))
        .collect();

    let mut computed: Vec<(String, String)> = Vec::new();
    for calc in &manifest.calculations {
        let request = JsRequest {
            script: calc.script.clone(),
            event: EventKind::Calculate,
            target: calc.field.clone(),
            fields: values
                .iter()
                .map(|(name, value)| Field {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
        };
        let outcome = sandbox
            .run_authorized(request, budget, &gate, &doc)
            .map_err(|e| CliError(format!("form script refused: {e}")))?;

        // Apply the sandbox's read-back updates and the calculated event value
        // to the working field table, in /CO order, before the next action.
        if !calc.field.is_empty() {
            if let Some(event_value) = outcome.event_value {
                set_value(&mut values, &calc.field, event_value.clone());
                computed.push((calc.field.clone(), event_value));
            }
        }
        for update in outcome.updates {
            set_value(&mut values, &update.name, update.value.clone());
            computed.push((update.name, update.value));
        }
    }
    Ok(computed)
}

/// Replace (or append) one field's value in the working table.
fn set_value(values: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(slot) = values.iter_mut().find(|(n, _)| n == name) {
        slot.1 = value;
    } else {
        values.push((name.to_string(), value));
    }
}

/// `selis fill-forms <path> --output <file> --set NAME=VALUE ... [--allow-js]`
///
/// # Errors
///
/// [`CliError`] when a `--set` pair is malformed, when a form with scripts is
/// filled without consent, or when the fill is refused for any of the typed
/// reasons `selis_pdf_tools::fill_forms` reports.
pub(crate) fn fill(
    path: &str,
    sets: &[String],
    output: &str,
    allow_js: bool,
) -> CliResult<crate::verify_report::Verification> {
    let mut fields = Vec::with_capacity(sets.len());
    for item in sets {
        let (name, value) = item.split_once('=').ok_or_else(|| {
            CliError(format!(
                "--set expects NAME=VALUE, got {item:?}; everything after the first = is the value"
            ))
        })?;
        let name = name.trim();
        if name.is_empty() {
            return Err(CliError(format!(
                "--set has an empty field name in {item:?}"
            )));
        }
        fields.push(selis_pdf_tools::FieldEdit {
            name: name.to_string(),
            value: value.to_string(),
            values: Vec::new(),
        });
    }

    // Read the field list first: it carries the typed script manifest the
    // consent decision is made on (no PDF text is scraped or interpreted here).
    let report = selis_pdf_tools::form_fields(path).map_err(CliError::from)?;
    if report.has_scripts && !allow_js {
        return Err(consent_required(path));
    }

    if report.has_scripts {
        let manifest = ScriptManifest {
            has_scripts: report.has_scripts,
            calculations: report.calculations.clone(),
        };
        // Per-document identity: a grant for this file never applies to another.
        let doc_id = format!("file:{path}");
        let computed = evaluate_calculations(&manifest, &report.fields, allow_js, &doc_id)?;
        for (name, value) in computed {
            // Merge a calculated value into the fill. An existing --set wins
            // as the field's source of truth only when the script did not
            // compute that field; a computed value overrides the raw set.
            if let Some(edit) = fields.iter_mut().find(|edit| edit.name == name) {
                edit.value = value;
                edit.values.clear();
            } else if report.fields.iter().any(|field| field.name == name) {
                fields.push(selis_pdf_tools::FieldEdit {
                    name,
                    value,
                    values: Vec::new(),
                });
            }
        }
    }

    let options = selis_pdf_tools::FillFormsOptions {
        fields,
        ..Default::default()
    };
    selis_pdf_tools::fill_forms(path, &options, output).map_err(CliError::from)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing
    )]

    use super::*;
    use selis_error::Code;
    use selis_pdf_form::CalcScript;

    fn manifest_with(script: &str) -> ScriptManifest {
        ScriptManifest {
            has_scripts: true,
            calculations: vec![CalcScript {
                field: "Total".to_string(),
                script: script.to_string(),
            }],
        }
    }

    fn form_fields() -> Vec<FormField> {
        // The sandbox only reads `name` and `value` from these.
        serde_json::from_str(
            r#"[{"name":"Subtotal","kind":"text","value":"100"},
                {"name":"Total","kind":"text","value":"0"}]"#,
        )
        .expect("form fields")
    }

    #[test]
    fn a_form_with_scripts_refuses_without_consent() {
        let err = consent_required("scripted.pdf");
        let message = err.0;
        assert!(message.contains("--allow-js"), "{message}");
        assert!(message.contains("SELIS_ALLOW_JS=1"), "{message}");
        assert!(message.contains("no file was written"), "{message}");
    }

    #[test]
    fn default_deny_is_enforced_by_run_authorized() {
        // Even bypassing the CLI's early refusal, the gate denies: no script
        // runs and no interpreter is touched (the error is consent-required).
        let err = evaluate_calculations(
            &manifest_with("event.value = 'RAN';"),
            &form_fields(),
            false,
            "file:scripted.pdf",
        )
        .expect_err("default deny");
        assert!(err.0.contains("consent"), "{}", err.0);
        assert!(
            err.0
                .contains("no reader consent and no admin origin approval"),
            "{}",
            err.0
        );
    }

    #[test]
    fn with_consent_the_calculation_runs_and_computes_a_value() {
        let computed = evaluate_calculations(
            &manifest_with(
                "var sub = Number(this.getField('Subtotal').value); \
                 event.value = String(sub * 2);",
            ),
            &form_fields(),
            true,
            "file:scripted.pdf",
        )
        .expect("consented run");
        assert!(computed.contains(&("Total".to_string(), "200".to_string())));
    }

    #[test]
    fn consent_is_scoped_to_one_document() {
        // A grant for doc A appears in exactly one ConsentPolicy; a fresh gate
        // for doc B holds no grant and denies. (The CLI rebuilds both per call.)
        let a = DocumentIdentity::new("file:a.pdf", "local");
        let b = DocumentIdentity::new("file:b.pdf", "local");
        let mut consent = ConsentPolicy::new();
        consent.grant(&a, ConsentScope::Session);
        let origins = ApprovedOrigins::new();
        assert!(ScriptAuthorization::new(&consent, &origins)
            .authorize(&a)
            .is_ok());
        assert_eq!(
            ScriptAuthorization::new(&consent, &origins)
                .authorize(&b)
                .unwrap_err()
                .code(),
            Code::JsConsentRequired
        );
    }
}
