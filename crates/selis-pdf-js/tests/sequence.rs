#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! SL-5.FORM.03 DoD — the Acrobat commit-time event sequence.

use selis_error::Code;
use selis_pdf_js::{EventKind, EventSequenceRequest, Field, FormAction, JsBudget, JsSandbox};
use selis_policy::{
    ApprovedOrigins, ConsentPolicy, ConsentScope, DocumentIdentity, ScriptAuthorization,
};

fn gate(allow: bool) -> (ConsentPolicy, ApprovedOrigins, DocumentIdentity) {
    let doc = DocumentIdentity::new("file:sequence.pdf".to_string(), "local".to_string());
    let mut consent = ConsentPolicy::new();
    if allow {
        consent.grant(&doc, ConsentScope::Session);
    }
    (consent, ApprovedOrigins::new(), doc)
}

fn fields() -> Vec<Field> {
    vec![
        Field {
            name: "A".to_string(),
            value: "1".to_string(),
        },
        Field {
            name: "B".to_string(),
            value: "2".to_string(),
        },
        Field {
            name: "Log".to_string(),
            value: String::new(),
        },
    ]
}

#[test]
fn sequence_runs_calculate_then_validate_then_format_in_order() {
    let (consent, origins, doc) = gate(true);
    let authorization = ScriptAuthorization::new(&consent, &origins);
    let request = EventSequenceRequest {
        actions: vec![
            FormAction {
                field: "A".to_string(),
                script: "this.getField('Log').value = 'C'; event.value = '10';".to_string(),
                event: EventKind::Calculate,
            },
            FormAction {
                field: "B".to_string(),
                script: "this.getField('Log').value = this.getField('Log').value + 'V';"
                    .to_string(),
                event: EventKind::Validate,
            },
            FormAction {
                field: "A".to_string(),
                script: "this.getField('Log').value = this.getField('Log').value + 'F';"
                    .to_string(),
                event: EventKind::Format,
            },
        ],
        fields: fields(),
    };
    let mut sandbox = JsSandbox::new().expect("sandbox");
    let outcome = sandbox
        .run_event_sequence(request, JsBudget::conservative(), &authorization, &doc)
        .expect("consented sequence");
    let log = outcome.values.iter().find(|f| f.name == "Log").unwrap();
    assert_eq!(
        log.value, "CVF",
        "actions must run in /CO -> /V -> /F order"
    );
    let a = outcome.values.iter().find(|f| f.name == "A").unwrap();
    assert_eq!(a.value, "10", "calculate's event.value is committed");
    assert!(outcome.rejected_field.is_none());
    // B was never assigned event.value, so its value must not be cleared.
    let b = outcome.values.iter().find(|f| f.name == "B").unwrap();
    assert_eq!(b.value, "2");
}

#[test]
fn sequence_refuses_without_consent_before_running_any_script() {
    let (consent, origins, doc) = gate(false);
    let authorization = ScriptAuthorization::new(&consent, &origins);
    let request = EventSequenceRequest {
        actions: vec![FormAction {
            field: "A".to_string(),
            // An infinite loop: if the interpreter were touched at all we
            // would get JS_INSTRUCTION_BUDGET, not a consent refusal.
            script: "while(true){}".to_string(),
            event: EventKind::Calculate,
        }],
        fields: fields(),
    };
    let mut sandbox = JsSandbox::new().expect("sandbox");
    let err = sandbox
        .run_event_sequence(request, JsBudget::conservative(), &authorization, &doc)
        .expect_err("default deny");
    assert_eq!(err.code(), Code::JsConsentRequired);
}

#[test]
fn validate_rc_false_refuses_with_a_typed_error_naming_the_field() {
    let (consent, origins, doc) = gate(true);
    let authorization = ScriptAuthorization::new(&consent, &origins);
    let request = EventSequenceRequest {
        actions: vec![FormAction {
            field: "B".to_string(),
            script: "event.rc = false;".to_string(),
            event: EventKind::Validate,
        }],
        fields: fields(),
    };
    let mut sandbox = JsSandbox::new().expect("sandbox");
    let err = sandbox
        .run_event_sequence(request, JsBudget::conservative(), &authorization, &doc)
        .expect_err("validation refusal");
    assert_eq!(err.code(), Code::JsValidateRefused);
    assert!(err.to_string().contains('B'), "{}", err);
}

#[test]
fn format_value_is_written_back_to_the_field() {
    let (consent, origins, doc) = gate(true);
    let authorization = ScriptAuthorization::new(&consent, &origins);
    let request = EventSequenceRequest {
        actions: vec![FormAction {
            field: "A".to_string(),
            script: "event.value = util.printf('%.2f', Number(event.value));".to_string(),
            event: EventKind::Format,
        }],
        fields: fields(),
    };
    let mut sandbox = JsSandbox::new().expect("sandbox");
    let outcome = sandbox
        .run_event_sequence(request, JsBudget::conservative(), &authorization, &doc)
        .expect("format sequence");
    let a = outcome.values.iter().find(|f| f.name == "A").unwrap();
    assert_eq!(a.value, "1.00");
    assert!(outcome
        .computed
        .iter()
        .any(|u| u.name == "A" && u.value == "1.00"));
}
