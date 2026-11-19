//! The value types crossing the sandbox boundary.
//!
//! Only plain data crosses: a script's text, field name/value pairs, an event
//! descriptor, and the resulting values. No host handles, no URLs, no document
//! bytes (ADR-P0017 / ADR-P0020).

/// One form field visible to a script through `this.getField(name)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The fully-qualified field name as the document declares it.
    pub name: String,
    /// The field's current value, as text.
    pub value: String,
}

/// Which Acrobat event a script is handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A calculation script; the script assigns `event.value`.
    Calculate,
    /// A validation script; the script may set `event.rc = false`.
    Validate,
    /// A format script; the script assigns a formatted `event.value`.
    Format,
}

impl EventKind {
    /// The lowercase `event.type` string exposed inside the sandbox.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::Calculate => "calculate",
            EventKind::Validate => "validate",
            EventKind::Format => "format",
        }
    }
}

/// A request to run one script for one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsRequest {
    /// The JavaScript source, taken verbatim from the document.
    pub script: String,
    /// The event being handled.
    pub event: EventKind,
    /// The name of the field the event targets (`event.targetName`).
    pub target: String,
    /// The fields the script is allowed to read and write.
    pub fields: Vec<Field>,
}

/// A field whose value a script changed through `this.getField(name).value`.
///
/// The caller decides whether to apply these; the sandbox never writes a
/// document (ADR-P0020).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldUpdate {
    /// The field's name.
    pub name: String,
    /// The value the script assigned.
    pub value: String,
}

/// What a contained run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsOutcome {
    /// `event.value` after the script ran, when it was set.
    pub event_value: Option<String>,
    /// `event.rc` after the script ran (`true` unless a validate script rejected).
    pub event_rc: bool,
    /// Field values the script changed, relative to the request.
    pub updates: Vec<FieldUpdate>,
    /// Measured budget usage for diagnostics.
    pub usage: crate::budget::JsUsage,
}
