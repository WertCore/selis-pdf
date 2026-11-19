//! Commit-time Acrobat event sequencing (SL-5.FORM.03).
//!
//! A real form fires several field scripts for one logical commit. This module
//! runs the three field-level events this subset supports, in order:
//!
//! 1. **calculate** (`/AA /C`, ordered by `/CO`);
//! 2. **validate** (`/AA /V`) on every field with a candidate value;
//! 3. **format** (`/AA /F`) on every field that validated.
//!
//! Every script is evaluated through [`JsSandbox::run_authorized`], so the
//! **same consent gate** is checked before each one (and once up front). A
//! validate script that sets `event.rc = false` rejects the whole sequence with
//! a typed refusal naming its field; no value is committed.
//!
//! ## Honest limits
//!
//! This is a commit-time sequence, not a replica of Acrobat's interactive event
//! engine. Acrobat's keystroke (`/K`) events, per-keystroke re-entrancy, the
//! exact moment it recalculates dependencies, and its separate *display* value
//! are not modelled: a format script's `event.value` is written to the field's
//! stored `/V` because this engine keeps one value per field. Document-level
//! `/JS` execution is out of scope. See `pdf-plan/15-PHASE-5-edit.md` FORM.03.

use std::collections::BTreeMap;

use selis_error::{err, Code, Result};
use selis_policy::{DocumentIdentity, ScriptAuthorization};

use crate::budget::JsBudget;
use crate::sandbox::JsSandbox;
use crate::types::{EventKind, Field, FieldUpdate, JsRequest};

/// One field script to run as part of a commit, in execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormAction {
    /// The target field's fully-qualified name.
    pub field: String,
    /// The JavaScript source, taken verbatim from the document.
    pub script: String,
    /// Which event this action handles.
    pub event: EventKind,
}

/// A whole commit's worth of actions and the field values they start from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSequenceRequest {
    /// Actions in the order they must run: calculate, validate, then format.
    pub actions: Vec<FormAction>,
    /// The candidate field values (the fill plus the document's current ones).
    pub fields: Vec<Field>,
}

/// What a sequenced run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSequenceOutcome {
    /// The final field table, after all actions.
    pub values: Vec<Field>,
    /// Fields whose value changed from the request, for the caller to write back.
    pub computed: Vec<FieldUpdate>,
    /// The field a validate script refused (`event.rc = false`), if any.
    pub rejected_field: Option<String>,
}

impl JsSandbox {
    /// Run a whole calculate → validate → format sequence under one gate.
    ///
    /// Every action goes through [`JsSandbox::run_authorized`], so consent is
    /// re-checked before each script and no interpreter work happens without
    /// it. A validation refusal is a typed `JS_VALIDATE_REFUSED` naming the
    /// field; the returned values/computed lists are not produced in that case.
    ///
    /// # Budget
    ///
    /// One sandbox run per action, each with the supplied [`JsBudget`]. The
    /// number of actions is bounded by the caller's manifest scanner.
    ///
    /// # Errors
    ///
    /// `JS_CONSENT_REQUIRED` without consent, `JS_VALIDATE_REFUSED` when a
    /// validate script sets `event.rc = false`, or any sandbox error.
    // The shared registry error is used by every fallible API in this crate;
    // boxing it here would diverge from the surrounding surface for no gain.
    #[allow(clippy::result_large_err)]
    pub fn run_event_sequence(
        &mut self,
        request: EventSequenceRequest,
        budget: JsBudget,
        gate: &ScriptAuthorization<'_>,
        doc: &DocumentIdentity,
    ) -> Result<EventSequenceOutcome> {
        // Check the gate once up front: a denial must happen before any field
        // table is built or any interpreter work is done.
        gate.authorize(doc)?;

        let mut table: BTreeMap<String, String> = BTreeMap::new();
        for field in &request.fields {
            let _ = table.insert(field.name.clone(), field.value.clone());
        }
        let original = table.clone();

        for action in &request.actions {
            let fields: Vec<Field> = table
                .iter()
                .map(|(name, value)| Field {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect();
            let outcome = self.run_authorized(
                JsRequest {
                    script: action.script.clone(),
                    event: action.event,
                    target: action.field.clone(),
                    fields,
                },
                budget,
                gate,
                doc,
            )?;

            // A script may write other fields through `this.getField(name).value`.
            for update in outcome.updates {
                let _ = table.insert(update.name, update.value);
            }
            // Only an actual assignment to event.value changes the target; a
            // script that leaves it untouched keeps the field's current value.
            if outcome.event_value_set {
                if let Some(value) = outcome.event_value {
                    let _ = table.insert(action.field.clone(), value);
                }
            }
            if action.event == EventKind::Validate && !outcome.event_rc {
                return Err(err!(
                    Code::JsValidateRefused,
                    during = "js-validate",
                    detail = action.field.clone()
                ));
            }
        }

        let values: Vec<Field> = table
            .iter()
            .map(|(name, value)| Field {
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        let mut computed = Vec::new();
        for (name, before) in &original {
            if let Some(after) = table.get(name) {
                if after != before {
                    computed.push(FieldUpdate {
                        name: name.clone(),
                        value: after.clone(),
                    });
                }
            }
        }
        Ok(EventSequenceOutcome {
            values,
            computed,
            rejected_field: None,
        })
    }
}
