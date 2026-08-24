//! Structured logging for engineers.
//!
//! # The one rule
//!
//! **A log record never contains document content** (ADR-P0017). The [`Record`]
//! type makes that structural rather than aspirational: its message is a
//! `&'static str`, so a `format!` of a document-derived string cannot be placed
//! in one. Variable data goes in [`Record::fields`], whose values are a closed
//! enum of engine-controlled types.
//!
//! # No global state
//!
//! There is no static logger. A [`LogSink`] is passed down from the shell, because
//! a WASM build, a CLI build, and a fuzz harness want different sinks and no
//! engine crate may name one (ADR-P0005).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Fine-grained tracing. Compiled in, off by default, expensive when on.
    Trace,
    /// Diagnostic detail useful when investigating.
    Debug,
    /// Notable but expected.
    Info,
    /// Something was wrong and the engine recovered. A malformed document that
    /// we tolerated is a `Warn`, not an `Error`.
    Warn,
    /// An operation failed.
    Error,
}

impl Level {
    /// A short stable identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

impl Default for Level {
    /// `Warn` — the release default (01-ARCHITECTURE.md §11).
    fn default() -> Self {
        Level::Warn
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured field value.
///
/// Deliberately closed. There is no `Bytes` variant and no free-form `String`
/// variant that accepts document text: [`Value::Sanitised`] exists for the one
/// legitimate case and its constructor is the only way in.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A signed integer — an offset, a count, an object number.
    Int(i64),
    /// An unsigned integer.
    Uint(u64),
    /// A boolean.
    Bool(bool),
    /// An engine-controlled string constant.
    Static(&'static str),
    /// Document-derived text, already reduced to a safe form.
    Sanitised(String),
}

impl Value {
    /// Reduce document-derived text to a form safe to log.
    ///
    /// Keeps ASCII alphanumerics, `-`, `_` and `.`; replaces every other byte
    /// with `?`; truncates to 32 characters. This bounds a log line, prevents
    /// log injection via a crafted `/Name`, and keeps content out of telemetry.
    #[must_use]
    pub fn sanitised(raw: &[u8]) -> Self {
        let mut out = String::new();
        for &b in raw.iter().take(32) {
            let c = char::from(b);
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                out.push(c);
            } else {
                out.push('?');
            }
        }
        Value::Sanitised(out)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{v}"),
            Value::Uint(v) => write!(f, "{v}"),
            Value::Bool(v) => write!(f, "{v}"),
            Value::Static(v) => f.write_str(v),
            Value::Sanitised(v) => f.write_str(v),
        }
    }
}

/// One log record.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// Severity.
    pub level: Level,
    /// The emitting crate, e.g. `"selis-pdf-cos"`.
    pub target: &'static str,
    /// The message. `&'static str` by design: this is the type-level guarantee
    /// that a formatted document string cannot become a log message.
    pub message: &'static str,
    /// Structured fields.
    pub fields: Vec<(&'static str, Value)>,
}

impl Record {
    /// A record with no fields.
    #[must_use]
    pub fn new(level: Level, target: &'static str, message: &'static str) -> Self {
        Self {
            level,
            target,
            message,
            fields: Vec::new(),
        }
    }

    /// Attach a field.
    #[must_use]
    pub fn field(mut self, key: &'static str, value: Value) -> Self {
        self.fields.push((key, value));
        self
    }

    /// Render as a single line: `level target: message key=value ...`.
    #[must_use]
    pub fn to_line(&self) -> String {
        use core::fmt::Write as _;
        let mut s = String::new();
        let _ = write!(s, "{} {}: {}", self.level, self.target, self.message);
        for (k, v) in &self.fields {
            let _ = write!(s, " {k}={v}");
        }
        s
    }
}

/// Where records go. Supplied by the shell.
pub trait LogSink: Send + Sync {
    /// Emit a record. Must not block and must not fail.
    fn emit(&self, record: &Record);

    /// The minimum level this sink wants. Callers check before building a
    /// record, so a disabled level costs nothing.
    fn enabled(&self, level: Level) -> bool {
        level >= Level::Warn
    }
}

/// A sink that discards everything. The default, and the release default below
/// `warn`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl LogSink for NullSink {
    fn emit(&self, _record: &Record) {}
    fn enabled(&self, _level: Level) -> bool {
        false
    }
}

/// A sink that retains records in memory. For tests and for `selis diagnose`,
/// which produces a user-reviewable bundle rather than transmitting anything
/// (ADR-P0017).
///
/// A poisoned lock is treated as "no records", never as a panic: a logging sink
/// must not be able to take down an engine operation.
#[derive(Debug, Default)]
pub struct CaptureSink {
    records: std::sync::Mutex<Vec<Record>>,
    min: Level,
}

impl CaptureSink {
    /// A capture sink retaining records at or above `min`.
    #[must_use]
    pub const fn new(min: Level) -> Self {
        Self {
            records: std::sync::Mutex::new(Vec::new()),
            min,
        }
    }

    /// The records captured so far.
    #[must_use]
    pub fn records(&self) -> Vec<Record> {
        self.records
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// How many records were captured.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Whether nothing was captured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl LogSink for CaptureSink {
    fn emit(&self, record: &Record) {
        if let Ok(mut g) = self.records.lock() {
            g.push(record.clone());
        }
    }
    fn enabled(&self, level: Level) -> bool {
        level >= self.min
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_respects_the_level_filter() {
        let sink = CaptureSink::new(Level::Warn);
        assert!(!sink.enabled(Level::Debug));
        assert!(sink.enabled(Level::Error));
        sink.emit(&Record::new(Level::Warn, "selis-pdf-cos", "reconstructed index"));
        assert_eq!(sink.len(), 1);
    }

    #[test]
    fn null_sink_is_off() {
        let sink = NullSink;
        assert!(!sink.enabled(Level::Error));
    }

    /// ADR-P0017: a sanitised value cannot carry a newline, an escape sequence,
    /// or an unbounded amount of document text.
    #[test]
    fn sanitised_values_are_bounded_and_inert() {
        let v = Value::sanitised(b"\x1b[31m/Evil Name\nwith newline\xff");
        let s = alloc::format!("{v}");
        assert!(!s.contains('\n'));
        assert!(!s.contains('\x1b'));
        assert!(s.chars().count() <= 32);

        let long = Value::sanitised(&[b'a'; 10_000]);
        assert!(alloc::format!("{long}").chars().count() <= 32);
    }

    #[test]
    fn record_line_is_greppable() {
        let r = Record::new(Level::Warn, "selis-pdf-cos", "xref offset out of range")
            .field("offset", Value::Uint(4096))
            .field("object", Value::Uint(12));
        let line = r.to_line();
        assert_eq!(
            line,
            "warn selis-pdf-cos: xref offset out of range offset=4096 object=12"
        );
        assert!(!line.contains('\n'));
    }
}
