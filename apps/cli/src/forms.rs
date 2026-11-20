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
//!
//! # Administrator pre-approval (SL-5.JS.03)
//!
//! An administrator may supply an explicit, scoped allow-list with
//! `--script-policy <FILE>` (or `SELIS_SCRIPT_POLICY=<FILE>`). Each line is
//! `origin <value>` or `id <value>`; a malformed file is a typed refusal and
//! enables nothing. The list may only narrow when a prompt/grant is needed for
//! an explicitly named origin or id — it is **never** a silent override of a
//! reader's `--deny-js` decline, which is checked first by the policy gate.

use selis_pdf_form::{FormField, ScriptManifest};
use selis_pdf_js::{EventKind, Field, JsBudget, JsSandbox};
use selis_policy::{
    ApprovedOrigins, ConsentPolicy, ConsentScope, DocumentIdentity, PolicyParseError,
    ScriptAuthorization, ScriptPolicy,
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
         calculations. An administrator may pre-approve this exact document id \
         with `--script-policy <FILE>`; that never overrides --deny-js; \
         no file was written."
    ))
}

/// Load the administrator's explicit scripting allow-list.
///
/// The engine crates never touch the filesystem; this is the shell boundary,
/// where a configured file path is read and handed to the typed parser as
/// text. A malformed file is a typed refusal (no entry from it is applied).
fn load_admin_policy(path: Option<&str>) -> CliResult<(ScriptPolicy, String)> {
    let path = path
        .map(str::to_string)
        .or_else(|| std::env::var("SELIS_SCRIPT_POLICY").ok());
    let Some(path) = path else {
        return Ok((ScriptPolicy::new(), "no policy supplied".to_string()));
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        CliError(format!(
            "cannot read administrator scripting policy {path}: {e}; no scripts were enabled"
        ))
    })?;
    let policy = parse_admin_policy(&text, &path)?;
    Ok((policy, path))
}

/// Parse administrator policy text into the typed allow-list.
fn parse_admin_policy(text: &str, source: &str) -> CliResult<ScriptPolicy> {
    ScriptPolicy::parse(text).map_err(|e: PolicyParseError| {
        CliError(format!(
            "administrator scripting policy {source} is malformed ({}, {}): {e}; no scripts were enabled",
            e.code().name(),
            e
        ))
    })
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

/// Evaluate a document's field scripts inside the budgeted native sandbox, in
/// the Acrobat subset order **calculate (`/CO`) → validate (`/V`) → format
/// (`/F`)**, gated by reader consent (SL-5.FORM.03 / SL-5.JS.02/03).
///
/// `allow_js` is the CLI's one-time consent grant. When false the gate holds no
/// grant and every sandbox run returns `JS_CONSENT_REQUIRED` before touching
/// the interpreter — defence in depth behind [`consent_required`]. When true
/// the grant is recorded for exactly this document id, so a second document
/// never inherits it. `deny_js` is the explicit reader refusal: it outranks both
/// the grant and the admin list. `origins` is the administrator's parsed
/// allow-list (empty by default).
///
/// Returns the computed `(field, value)` pairs; a validation refusal is a typed
/// error that names the refusing field and writes no file.
///
/// # Budget
///
/// Every action uses [`JsBudget::conservative`]. The manifest scanner is
/// bounded in `selis-pdf-form`; this adds one run per discovered action and
/// never loops on document-controlled data.
///
/// # Errors
///
/// `JS_CONSENT_REQUIRED` without consent, `JS_VALIDATE_REFUSED` naming the field
/// when validation rejects a value, or any typed sandbox error. No script text
/// is echoed into an error (ADR-P0017).
fn evaluate_scripts(
    manifest: &ScriptManifest,
    fields: &[FormField],
    allow_js: bool,
    deny_js: bool,
    origins: &ApprovedOrigins,
    doc_id: &str,
    policy_source: &str,
) -> CliResult<Vec<(String, String)>> {
    if manifest.calculations.is_empty()
        && manifest.validations.is_empty()
        && manifest.formats.is_empty()
    {
        return Ok(Vec::new());
    }

    // The gate is rebuilt per call. Consent/decline is keyed on this document
    // id alone (per-document scope); the admin list is the explicit, scoped
    // exception. A decline is recorded first and outranks every other grant.
    let doc = DocumentIdentity::new(doc_id.to_string(), "local");
    let mut consent = ConsentPolicy::new();
    if deny_js {
        consent.decline(&doc);
    } else if allow_js {
        consent.grant(&doc, ConsentScope::Session);
    }
    let gate = ScriptAuthorization::new(&consent, origins);

    // Defense in depth: the gate denies before any interpreter work even if a
    // future caller forgets the CLI's earlier refusal. The reader's decline is
    // checked first by the gate, so an admin entry cannot silently re-enable it.
    gate.authorize(&doc).map_err(|e| {
        CliError(format!(
            "form script refused ({}, {}): {e}",
            e.code().name(),
            e
        ))
    })?;

    // Audit line: when the admin allow-list (not an explicit grant) is what
    // opened the gate, say exactly which entry did it. This is printed to stderr
    // before any interpreter work; a reader decline is impossible here because
    // `authorize` refused it above.
    if !allow_js && origins.is_approved(&doc) {
        eprintln!(
            "selis: {doc_id}: scripting enabled by explicit administrator policy from \
             {policy_source} (a reader --deny-js decline always takes precedence)"
        );
    }

    // Build the actions in Acrobat subset order. Each list comes already
    // ordered from the typed manifest (calculations by /CO).
    let mut actions: Vec<selis_pdf_js::FormAction> = Vec::new();
    for calc in &manifest.calculations {
        actions.push(selis_pdf_js::FormAction {
            field: calc.field.clone(),
            script: calc.script.clone(),
            event: EventKind::Calculate,
        });
    }
    for validate in &manifest.validations {
        actions.push(selis_pdf_js::FormAction {
            field: validate.field.clone(),
            script: validate.script.clone(),
            event: EventKind::Validate,
        });
    }
    for format in &manifest.formats {
        actions.push(selis_pdf_js::FormAction {
            field: format.field.clone(),
            script: format.script.clone(),
            event: EventKind::Format,
        });
    }

    let values: Vec<Field> = fields
        .iter()
        .map(|f| Field {
            name: f.name.clone(),
            value: f.value.clone().unwrap_or_default(),
        })
        .collect();
    let request = selis_pdf_js::EventSequenceRequest {
        actions,
        fields: values,
    };
    let mut sandbox = JsSandbox::new().map_err(|e| CliError(format!("js runtime: {e}")))?;
    let outcome = sandbox
        .run_event_sequence(request, JsBudget::conservative(), &gate, &doc)
        .map_err(|e| {
            if e.code() == selis_error::Code::JsValidateRefused {
                CliError(format!(
                    "a validation script rejected its field: {e}; no file was written"
                ))
            } else {
                CliError(format!("form script refused: {e}"))
            }
        })?;
    Ok(outcome
        .computed
        .into_iter()
        .map(|update| (update.name, update.value))
        .collect())
}

/// `selis form-export <path> [--format json|csv] [--output file]`
///
/// Export a PDF's current field values as versioned JSON or CSV. Binary and
/// attachment-like fields (signatures, push buttons, NUL-bearing values) are
/// skipped with a typed note on stderr, never silently dropped.
///
/// # Errors
///
/// [`CliError`] when the file cannot be read or an unknown format is requested.
pub(crate) fn export(path: &str, format: &str, output: Option<&str>) -> CliResult<()> {
    use selis_pdf_form::formdata::{export_csv, export_json};

    let report = selis_pdf_tools::form_fields(path).map_err(CliError::from)?;
    let data = match format {
        "json" => export_json(&report),
        "csv" => export_csv(&report),
        other => {
            return Err(CliError(format!(
                "unknown form-data format {other:?}; use `json` or `csv`"
            )))
        }
    };
    for skipped in &data.skipped {
        eprintln!(
            "skipped field {:?}: {} ({:?})",
            skipped.name,
            skipped.reason.note(),
            skipped.reason
        );
    }
    match output {
        Some(file) => super::write_file(file, &data.bytes),
        None => {
            println!("{}", String::from_utf8_lossy(&data.bytes));
            Ok(())
        }
    }
}

/// `selis form-import <path> <data> --output <file> [--format json|csv|auto]`
///
/// Import field values from a JSON or CSV form-data file and fill the PDF with
/// them. The values travel through the same writer and read-back check as any
/// other fill, so a value that does not survive is refused.
///
/// # Errors
///
/// [`CliError`] when either file cannot be read, the data is not the requested
/// format, or a field refuses the edit.
pub(crate) fn import(
    path: &str,
    data_path: &str,
    format: &str,
    output: &str,
) -> CliResult<crate::verify_report::Verification> {
    use selis_pdf_form::formdata::{import_csv, import_json};

    let data = crate::read_file(data_path)?;
    let fields = match format {
        "json" => import_json(&data),
        "csv" => import_csv(&data),
        "auto" => {
            let trimmed = String::from_utf8_lossy(&data);
            if trimmed.trim_start().starts_with('{') {
                import_json(&data)
            } else {
                import_csv(&data)
            }
        }
        other => {
            return Err(CliError(format!(
                "unknown form-data format {other:?}; use `json`, `csv` or `auto`"
            )))
        }
    }
    .map_err(|e| CliError(format!("{data_path}: {e}")))?;
    let options = selis_pdf_tools::FillFormsOptions {
        fields,
        synthesize_appearances: true,
    };
    selis_pdf_tools::fill_forms(path, &options, output).map_err(CliError::from)
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
    deny_js: bool,
    script_policy: Option<&str>,
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

    // Load the administrator's explicit allow-list before any decision. A
    // malformed file is a typed refusal and enables nothing.
    let (policy, policy_source) = load_admin_policy(script_policy)?;
    let origins = policy.origins();

    // Read the field list first: it carries the typed script manifest the
    // consent decision is made on (no PDF text is scraped or interpreted here).
    let report = selis_pdf_tools::form_fields(path).map_err(CliError::from)?;
    // Per-document identity: a grant or decline for this file never applies to
    // another. The admin list can name this exact id or its (local) origin.
    let doc_id = format!("file:{path}");
    let doc = DocumentIdentity::new(doc_id.clone(), "local".to_string());

    if report.has_scripts {
        if deny_js {
            return Err(CliError(format!(
                "{path}: scripting was explicitly declined for this document (--deny-js); \
                 an administrator allow-list never overrides a reader decline. No file was written."
            )));
        }
        if !allow_js && !origins.is_approved(&doc) {
            return Err(consent_required(path));
        }
    }

    if report.has_scripts {
        let manifest = ScriptManifest {
            has_scripts: report.has_scripts,
            calculations: report.calculations.clone(),
            validations: report.validations.clone(),
            formats: report.formats.clone(),
        };
        // The candidate values the scripts see are the document's values with
        // this invocation's --set edits overlaid, so a validate action inspects
        // the value the reader actually asked to commit.
        let mut candidates = report.fields.clone();
        for edit in &fields {
            if let Some(field) = candidates.iter_mut().find(|f| f.name == edit.name) {
                field.value = Some(edit.value.clone());
            }
        }
        let computed = evaluate_scripts(
            &manifest,
            &candidates,
            allow_js,
            deny_js,
            origins,
            &doc_id,
            &policy_source,
        )?;
        for (name, value) in computed {
            // A computed value overrides the raw --set value for that field.
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

    fn no_origins() -> ApprovedOrigins {
        ApprovedOrigins::new()
    }

    fn manifest_with(script: &str) -> ScriptManifest {
        ScriptManifest {
            has_scripts: true,
            calculations: vec![CalcScript {
                field: "Total".to_string(),
                script: script.to_string(),
            }],
            ..ScriptManifest::default()
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
        let err = evaluate_scripts(
            &manifest_with("event.value = 'RAN';"),
            &form_fields(),
            false,
            false,
            &no_origins(),
            "file:scripted.pdf",
            "no policy supplied",
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
        let computed = evaluate_scripts(
            &manifest_with(
                "var sub = Number(this.getField('Subtotal').value); \
                 event.value = String(sub * 2);",
            ),
            &form_fields(),
            true,
            false,
            &no_origins(),
            "file:scripted.pdf",
            "no policy supplied",
        )
        .expect("consented run");
        assert!(computed.contains(&("Total".to_string(), "200".to_string())));
    }

    #[test]
    fn a_validate_refusal_is_typed_and_names_the_field() {
        use selis_pdf_form::ValidateScript;
        let manifest = ScriptManifest {
            has_scripts: true,
            validations: vec![ValidateScript {
                field: "Total".to_string(),
                script: "event.rc = false;".to_string(),
            }],
            ..ScriptManifest::default()
        };
        let err = evaluate_scripts(
            &manifest,
            &form_fields(),
            true,
            false,
            &no_origins(),
            "file:scripted.pdf",
            "no policy supplied",
        )
        .expect_err("validation refuses");
        assert!(err.0.contains("validation script rejected"), "{}", err.0);
        assert!(err.0.contains("Total"), "{}", err.0);
        assert!(err.0.contains("no file was written"), "{}", err.0);
    }

    #[test]
    fn actions_run_in_calculate_validate_format_order() {
        use selis_pdf_form::{FormatScript, ValidateScript};
        let manifest = ScriptManifest {
            has_scripts: true,
            calculations: vec![CalcScript {
                field: "Total".to_string(),
                script: "this.getField('Subtotal').value = 'C'; event.value = '10';".to_string(),
            }],
            validations: vec![ValidateScript {
                field: "Total".to_string(),
                script: "this.getField('Subtotal').value = this.getField('Subtotal').value + 'V';"
                    .to_string(),
            }],
            formats: vec![FormatScript {
                field: "Total".to_string(),
                script: "this.getField('Subtotal').value = this.getField('Subtotal').value + 'F';"
                    .to_string(),
            }],
        };
        let computed = evaluate_scripts(
            &manifest,
            &form_fields(),
            true,
            false,
            &no_origins(),
            "file:sequence.pdf",
            "no policy supplied",
        )
        .expect("consented sequence");
        assert!(computed.contains(&("Subtotal".to_string(), "CVF".to_string())));
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

    /// SL-5.JS.03: an admin allow-list permits exactly the named document id
    /// (or origin) and nothing else, even without `--allow-js`.
    #[test]
    fn admin_policy_permits_only_the_exact_scoped_document() {
        let manifest = manifest_with("event.value = 'ADMIN';");
        let policy =
            parse_admin_policy("id file:scripted.pdf\n", "policy.txt").expect("valid admin policy");
        let computed = evaluate_scripts(
            &manifest,
            &form_fields(),
            false,
            false,
            policy.origins(),
            "file:scripted.pdf",
            "policy.txt",
        )
        .expect("admin pre-approved run");
        assert!(computed.contains(&("Total".to_string(), "ADMIN".to_string())));

        // A different document id is not covered by the exact-id entry.
        let err = evaluate_scripts(
            &manifest,
            &form_fields(),
            false,
            false,
            policy.origins(),
            "file:other.pdf",
            "policy.txt",
        )
        .expect_err("other document not approved");
        assert!(err.0.contains(Code::JsConsentRequired.name()));
    }

    /// The load-bearing invariant: `--deny-js` (the CLI's "No") is absolute.
    #[test]
    fn a_reader_decline_is_never_overridden_by_admin_policy() {
        let manifest = manifest_with("event.value = 'RAN';");
        let policy = parse_admin_policy("origin local\nid file:scripted.pdf\n", "policy.txt")
            .expect("valid admin policy");
        let err = evaluate_scripts(
            &manifest,
            &form_fields(),
            true, // even a simultaneous --allow-js grant must lose
            true, // --deny-js
            policy.origins(),
            "file:scripted.pdf",
            "policy.txt",
        )
        .expect_err("decline wins");
        assert!(err.0.contains(Code::JsDeclined.name()), "{}", err.0);
    }

    /// A malformed admin policy is a typed refusal, not a skipped line that
    /// might accidentally approve a document.
    #[test]
    fn malformed_admin_policy_is_a_typed_refusal() {
        let err = parse_admin_policy("allow everything\n", "policy.txt")
            .expect_err("malformed policy refused");
        assert!(err.0.contains(Code::JsPolicyMalformed.name()), "{}", err.0);
        assert!(err.0.contains("policy.txt"));
        assert!(err.0.contains("no scripts were enabled"));
    }
}
