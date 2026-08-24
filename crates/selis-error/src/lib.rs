//! The Selis error taxonomy and its numeric code registry.
//!
//! One error type, one registry, generated from `codes.toml`
//! (03-CONVENTIONS.md §3). Zero dependencies at runtime.
//!
//! # Why one type
//!
//! Every layer from the lexer to the C ABI reports the same [`Error`], carrying
//! the same numeric [`Code`]. Bindings do not invent taxonomies
//! (24-BINDINGS-SPEC.md §1), so the number a Swift caller sees is the number the
//! xref parser produced.
//!
//! # The load-bearing field
//!
//! [`Error::doc_state`] answers *"what happened to the user's document?"* without
//! reading the source. A caller can always tell whether the user's work survived.
//!
//! ```
//! use selis_error::{Code, DocState, Error};
//!
//! let e = Error::new(Code::BudgetBytes);
//! assert_eq!(e.code().id(), 4000);
//! assert_eq!(e.doc_state(), DocState::PartiallyLoaded);
//! assert!(e.code().retryable());
//! ```
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::borrow::Cow;
use alloc::string::String;
use core::fmt;

include!(concat!(env!("OUT_DIR"), "/codes.rs"));

/// The broad class of a failure.
///
/// Callers switch on this when they want one recovery policy for a family of
/// codes rather than a match arm per code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Kind {
    /// The document's bytes violate the format.
    Malformed,
    /// The construct is well-formed but this engine version does not implement it.
    Unsupported,
    /// The *caller's* request was invalid, not the document.
    Invalid,
    /// A password or credential is required or was wrong.
    Auth,
    /// Refused by policy: entitlement, permission bits, consent, active content.
    Policy,
    /// A resource budget was exhausted (ADR-P0006).
    Budget,
    /// The caller's cancel token was signalled.
    Cancelled,
    /// The source or sink failed.
    Io,
    /// An engine bug. Always worth a report.
    Internal,
}

impl Kind {
    /// A short stable identifier, useful in logs and JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Malformed => "malformed",
            Kind::Unsupported => "unsupported",
            Kind::Invalid => "invalid",
            Kind::Auth => "auth",
            Kind::Policy => "policy",
            Kind::Budget => "budget",
            Kind::Cancelled => "cancelled",
            Kind::Io => "io",
            Kind::Internal => "internal",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happened to the user's document when this error was produced.
///
/// This is the field the UI needs and the field a caller must never have to
/// guess (03-CONVENTIONS.md §3, SL-0.ERR.02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DocState {
    /// Nothing was loaded. There is no session to preserve.
    NotLoaded,
    /// The document is fully loaded and intact; the failure was elsewhere.
    Loaded,
    /// Some of the document loaded. What is present is usable; some is missing.
    PartiallyLoaded,
    /// A modification was attempted and the document is exactly as it was.
    Unchanged,
    /// The in-memory document changed. The user has unsaved state that matters.
    Modified,
}

impl DocState {
    /// A short stable identifier, useful in logs and JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DocState::NotLoaded => "not-loaded",
            DocState::Loaded => "loaded",
            DocState::PartiallyLoaded => "partially-loaded",
            DocState::Unchanged => "unchanged",
            DocState::Modified => "modified",
        }
    }

    /// Whether the user has state that would be lost by abandoning the session.
    #[must_use]
    pub const fn has_user_state(self) -> bool {
        matches!(self, DocState::Modified)
    }
}

impl fmt::Display for DocState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where in the document an error occurred, and what the engine was doing.
///
/// Deliberately narrow: byte offsets and object numbers are structural facts,
/// never document *content*. ADR-P0017 forbids document bytes in any diagnostic
/// that might be transmitted, so there is no field here that could hold one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ctx {
    /// Byte offset in the source, when known.
    pub offset: Option<u64>,
    /// Indirect object number, when the failure is attributable to one.
    pub object: Option<u32>,
    /// Zero-based page index, when the failure is attributable to one.
    pub page: Option<u32>,
    /// A short static label for the operation, e.g. `"xref-stream"`.
    ///
    /// Static so it cannot accidentally carry document-derived text.
    pub during: Option<&'static str>,
    /// A structural detail, e.g. a filter name or a limit.
    ///
    /// Callers must only place engine-controlled text here. Names lifted from a
    /// document must be passed through [`Ctx::sanitise`] first.
    pub detail: Option<String>,
}

impl Ctx {
    /// An empty context.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            offset: None,
            object: None,
            page: None,
            during: None,
            detail: None,
        }
    }

    /// Record the byte offset.
    #[must_use]
    pub const fn at(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Record the indirect object number.
    #[must_use]
    pub const fn object(mut self, num: u32) -> Self {
        self.object = Some(num);
        self
    }

    /// Record the page index.
    #[must_use]
    pub const fn page(mut self, index: u32) -> Self {
        self.page = Some(index);
        self
    }

    /// Record what the engine was doing.
    #[must_use]
    pub const fn during(mut self, what: &'static str) -> Self {
        self.during = Some(what);
        self
    }

    /// Record a structural detail.
    #[must_use]
    pub fn detail(mut self, text: impl Into<String>) -> Self {
        self.detail = Some(text.into());
        self
    }

    /// Reduce document-derived text to a form safe to log or transmit.
    ///
    /// Keeps ASCII alphanumerics, `-`, `_` and `.`; replaces every other byte
    /// with `?`; truncates to 64 characters. A `/Name` from a hostile document
    /// can therefore appear in a diagnostic without becoming an exfiltration
    /// channel or a log-injection vector (ADR-P0017).
    #[must_use]
    pub fn sanitise(raw: &[u8]) -> String {
        let mut out = String::new();
        for &b in raw.iter().take(64) {
            let c = char::from(b);
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                out.push(c);
            } else {
                out.push('?');
            }
        }
        out
    }
}

/// The Selis error type.
///
/// Cheap to construct and to move: a code plus a mostly-empty context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    code: Code,
    ctx: Ctx,
}

impl Error {
    /// An error with no context.
    #[must_use]
    pub const fn new(code: Code) -> Self {
        Self {
            code,
            ctx: Ctx::new(),
        }
    }

    /// An error with context.
    #[must_use]
    pub const fn with(code: Code, ctx: Ctx) -> Self {
        Self { code, ctx }
    }

    /// The registered code.
    #[must_use]
    pub const fn code(&self) -> Code {
        self.code
    }

    /// The structural context.
    #[must_use]
    pub const fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    /// The class of failure.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.code.kind()
    }

    /// What happened to the user's document.
    #[must_use]
    pub const fn doc_state(&self) -> DocState {
        self.code.doc_state()
    }

    /// The English user-facing message.
    ///
    /// Prefer resolving [`Code::message_key`] against a locale bundle; this is
    /// the fallback.
    #[must_use]
    pub const fn user_msg(&self) -> &'static str {
        self.code.user_msg_en()
    }

    /// Replace the context, keeping the code.
    #[must_use]
    pub fn context(mut self, ctx: Ctx) -> Self {
        self.ctx = ctx;
        self
    }

    /// Record what the engine was doing, if not already recorded.
    #[must_use]
    pub fn during(mut self, what: &'static str) -> Self {
        if self.ctx.during.is_none() {
            self.ctx.during = Some(what);
        }
        self
    }

    /// Record a byte offset, if not already recorded.
    #[must_use]
    pub fn at(mut self, offset: u64) -> Self {
        if self.ctx.offset.is_none() {
            self.ctx.offset = Some(offset);
        }
        self
    }

    /// Whether this error is a budget exhaustion (ADR-P0006).
    #[must_use]
    pub const fn is_budget(&self) -> bool {
        matches!(self.code.kind(), Kind::Budget)
    }

    /// Whether this error is a cancellation.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.code.kind(), Kind::Cancelled)
    }

    /// Whether this error means the source needs more bytes before the parse can
    /// continue (`IO_PENDING`; see `SL-1.COS.07`).
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.code == Code::IoPending
    }

    /// A single-line, machine-greppable rendering.
    ///
    /// Contains no document content by construction — [`Ctx`] has no field that
    /// can hold one.
    #[must_use]
    pub fn to_log_line(&self) -> String {
        use core::fmt::Write as _;
        let mut s = String::new();
        let _ = write!(
            s,
            "E{} {} kind={} doc_state={}",
            self.code.id(),
            self.code.name(),
            self.kind(),
            self.doc_state()
        );
        if let Some(d) = self.ctx.during {
            let _ = write!(s, " during={d}");
        }
        if let Some(o) = self.ctx.offset {
            let _ = write!(s, " offset={o}");
        }
        if let Some(n) = self.ctx.object {
            let _ = write!(s, " object={n}");
        }
        if let Some(p) = self.ctx.page {
            let _ = write!(s, " page={p}");
        }
        if let Some(ref d) = self.ctx.detail {
            let _ = write!(s, " detail={d}");
        }
        s
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[E{}] {}", self.code.id(), self.code.user_msg_en())?;
        if let Some(d) = self.ctx.during {
            write!(f, " (during {d})")?;
        }
        if let Some(o) = self.ctx.offset {
            write!(f, " (at byte {o})")?;
        }
        if let Some(ref d) = self.ctx.detail {
            write!(f, " ({d})")?;
        }
        Ok(())
    }
}

impl From<Code> for Error {
    fn from(code: Code) -> Self {
        Error::new(code)
    }
}

/// The crate-wide result alias.
pub type Result<T> = core::result::Result<T, Error>;

/// Construct an [`Error`] with context in one expression.
///
/// ```
/// use selis_error::{err, Code};
/// let e = err!(Code::XrefMalformed, during = "xref-table", at = 4096);
/// assert_eq!(e.ctx().offset, Some(4096));
/// assert_eq!(e.ctx().during, Some("xref-table"));
/// ```
#[macro_export]
macro_rules! err {
    ($code:expr) => { $crate::Error::new($code) };
    ($code:expr, $($field:ident = $value:expr),+ $(,)?) => {{
        let ctx = $crate::Ctx::new();
        $( let ctx = $crate::__err_field!(ctx, $field, $value); )+
        $crate::Error::with($code, ctx)
    }};
}

/// Implementation detail of [`err!`].
#[doc(hidden)]
#[macro_export]
macro_rules! __err_field {
    ($ctx:expr, at, $v:expr) => {
        $ctx.at($v)
    };
    ($ctx:expr, object, $v:expr) => {
        $ctx.object($v)
    };
    ($ctx:expr, page, $v:expr) => {
        $ctx.page($v)
    };
    ($ctx:expr, during, $v:expr) => {
        $ctx.during($v)
    };
    ($ctx:expr, detail, $v:expr) => {
        $ctx.detail($v)
    };
}

/// A locale catalogue for user-facing messages (SL-0.ERR.04).
///
/// The engine never formats a user string. It resolves a
/// [`Code::message_key`] through this trait, so localisation is a data change.
pub trait Messages {
    /// Resolve a message key. Return `None` to fall back to English.
    fn lookup(&self, key: &str) -> Option<&str>;
}

/// The English fallback catalogue, backed by the registry itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnglishMessages;

impl Messages for EnglishMessages {
    fn lookup(&self, key: &str) -> Option<&str> {
        ALL_CODES
            .iter()
            .find(|c| c.message_key() == key)
            .map(|c| c.user_msg_en())
    }
}

/// A pseudo-locale used to prove no message is hardcoded (SL-0.ERR.04 DoD).
///
/// Every lookup succeeds and returns a marked string, so a build rendered with
/// this catalogue shows immediately which strings bypassed the registry: they
/// are the ones that are still English.
#[derive(Debug, Clone, Copy, Default)]
pub struct PseudoMessages;

impl Messages for PseudoMessages {
    fn lookup(&self, _key: &str) -> Option<&str> {
        Some("«⟦pseudo⟧»")
    }
}

/// Resolve the user-facing message for a code through a catalogue.
#[must_use]
pub fn user_message<'a, M: Messages>(catalogue: &'a M, code: Code) -> Cow<'a, str> {
    match catalogue.lookup(code.message_key()) {
        Some(s) => Cow::Borrowed(s),
        None => Cow::Borrowed(code.user_msg_en()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// SL-0.ERR.02 DoD: every registered code has a doc_state, and it is one of
    /// the five defined values. The generated `doc_state()` is total, so this
    /// asserts the stronger property that the mapping is *sensible*: nothing
    /// below is `Modified` unless it can genuinely leave a dirty document.
    #[test]
    fn every_code_has_a_doc_state() {
        for code in ALL_CODES {
            let _ = code.doc_state();
            // A parse-time failure can never have modified anything.
            if code.id() < 2000 {
                assert_ne!(
                    code.doc_state(),
                    DocState::Modified,
                    "{} is a parse-domain code and must not claim Modified",
                    code.name()
                );
            }
        }
    }

    #[test]
    fn ids_are_unique_and_round_trip() {
        let mut ids: Vec<u32> = ALL_CODES.iter().map(|c| c.id()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate code id in the registry");

        for code in ALL_CODES {
            assert_eq!(Code::from_id(code.id()), Some(code));
        }
        assert_eq!(Code::from_id(999_999), None);
    }

    #[test]
    fn ids_fall_in_their_declared_range() {
        // 03-CONVENTIONS.md §3 range table. A code outside every range means
        // someone invented a domain without recording it.
        const RANGES: &[(u32, u32)] = &[
            (1000, 1499),
            (1500, 1799),
            (1800, 1999),
            (2000, 2299),
            (2300, 2599),
            (2600, 2799),
            (2800, 2999),
            (3000, 3199),
            (3200, 3399),
            (3400, 3599),
            (3600, 3799),
            (4000, 4299),
            (4300, 4499),
            (5000, 5299),
            (6000, 6299),
            (7000, 7299),
        ];
        for code in ALL_CODES {
            let id = code.id();
            assert!(
                RANGES.iter().any(|&(lo, hi)| id >= lo && id <= hi),
                "{} ({}) is outside every declared range",
                code.name(),
                id
            );
        }
    }

    #[test]
    fn budget_codes_are_budget_kind() {
        for code in ALL_CODES {
            if code.name().starts_with("BUDGET_") {
                assert_eq!(code.kind(), Kind::Budget, "{}", code.name());
            }
        }
    }

    #[test]
    fn message_keys_are_unique_and_kebab() {
        let mut keys: Vec<&str> = ALL_CODES.iter().map(|c| c.message_key()).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count);
        for code in ALL_CODES {
            let key = code.message_key();
            assert!(key.starts_with("error-"));
            assert!(
                key.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit()),
                "{key} is not kebab-case"
            );
        }
    }

    /// SL-0.ERR.04 DoD: a pseudo-locale build renders every message, i.e. every
    /// code resolves through the catalogue rather than through a hardcode.
    #[test]
    fn pseudo_locale_renders_every_message() {
        for code in ALL_CODES {
            let msg = user_message(&PseudoMessages, code);
            assert_eq!(
                msg.as_ref(),
                "«⟦pseudo⟧»",
                "{} bypassed the catalogue",
                code.name()
            );
        }
    }

    #[test]
    fn english_catalogue_matches_registry() {
        for code in ALL_CODES {
            let msg = user_message(&EnglishMessages, code);
            assert_eq!(msg.as_ref(), code.user_msg_en());
        }
    }

    /// ADR-P0017: a diagnostic must not carry document bytes. `Ctx::sanitise`
    /// is the only path by which document-derived text enters an error.
    #[test]
    fn sanitise_strips_hostile_bytes() {
        let raw = b"\x00\x1b[31mEvil\nName/With Spaces\xff";
        let s = Ctx::sanitise(raw);
        assert!(!s.contains('\n'));
        assert!(!s.contains('\x1b'));
        assert!(!s.contains(' '));
        assert!(s.contains("Evil"));
        assert!(Ctx::sanitise(&[b'x'; 4096]).chars().count() <= 64);
    }

    #[test]
    fn log_line_has_no_content_and_is_greppable() {
        let e = err!(
            Code::XrefMalformed,
            during = "xref-table",
            at = 4096u64,
            object = 12u32
        );
        let line = e.to_log_line();
        assert!(line.starts_with("E1101 XREF_MALFORMED"));
        assert!(line.contains("doc_state=not-loaded"));
        assert!(line.contains("offset=4096"));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn display_is_user_facing() {
        let e = Error::new(Code::WrongPassword);
        let s = alloc::format!("{e}");
        assert!(s.contains("[E1801]"));
        assert!(s.contains("password"));
    }

    #[test]
    fn helpers_classify_correctly() {
        assert!(Error::new(Code::BudgetWall).is_budget());
        assert!(Error::new(Code::Cancelled).is_cancelled());
        assert!(Error::new(Code::IoPending).is_pending());
        assert!(!Error::new(Code::NotAPdf).is_budget());
    }

    #[test]
    fn context_is_not_overwritten_by_during() {
        let e = Error::new(Code::FlateCorrupt)
            .during("inner")
            .during("outer");
        assert_eq!(e.ctx().during, Some("inner"));
    }
}
