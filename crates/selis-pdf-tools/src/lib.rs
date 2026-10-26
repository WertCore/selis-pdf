//! The document tools, byte-oriented and shared by every binding.
//!
//! This crate is the home of the operations that used to live only in the
//! CLI: merge, split, page selection/order/rotation, metadata, and redaction.
//! The algorithms are unchanged — including the full-document writer's
//! structure reconciliation and the structural verification gate — but their
//! input/output boundary is now a backend:
//!
//! * [`native-fs`](https://docs.rs/selis-pdf-tools) (default) reads and
//!   commits real files exactly as the CLI did, preserving chunked progress
//!   and Ctrl-C cancellation;
//! * with default features off, paths address an in-memory table. That is the
//!   backend the browser tools chunk uses, so document bytes never need a
//!   filesystem and never leave the page.
//!
//! The browser binding writes inputs with [`insert_input`], invokes one of the
//! operation wrappers, and reads the verified output with [`take_output`]. A
//! failed operation writes nothing and returns a typed [`ToolError`].

#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

mod io;
mod memory;
#[cfg(feature = "native-fs")]
mod progress;
pub mod runtime;
mod tools;
mod verify_report;
pub mod write_gate;

use selis_sandbox::Clock;

pub use memory::{clear, insert_input, take_output, MemorySession};
pub use verify_report::Verification;

/// A tool operation failure.
///
/// The message is already user-facing and contains no document bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

impl From<selis_error::Error> for ToolError {
    fn from(e: selis_error::Error) -> Self {
        ToolError(e.to_string())
    }
}

/// The result of one tool operation.
pub type ToolResult<T> = Result<T, ToolError>;

// The moved CLI implementation keeps its historical names internally. `use`
// (not a type alias) so the tuple-struct constructor stays in the value
// namespace for the many `CliError(...)` call sites.
pub(crate) use ToolError as CliError;
pub(crate) type CliResult<T> = ToolResult<T>;

pub(crate) use io::read_file;

/// The clock the CLI supplied as its binding boundary. `selis-sandbox`
/// returns the real monotonic clock where one is sanctioned and a frozen
/// clock on `wasm32`, so the same call is safe in both backends.
pub(crate) fn shell_clock() -> impl Clock {
    selis_sandbox::shell_clock()
}

/// Warn on stderr when a full rewrite will invalidate a digital signature.
///
/// The helper never fails the operation it guards: a missing or damaged
/// signature structure simply produces no warning.
pub fn warn_if_digital_signature(
    src: &[u8],
    doc: &selis_pdf_cos::Doc,
    budget: &selis_sandbox::Budget,
    g: &mut selis_sandbox::BudgetGuard<'_>,
    path: &str,
) {
    tools::warn_if_digital_signature(src, doc, budget, g, path);
}

/// Merge two or more PDFs into `output`, preserving the reconciled document
/// structures (outlines, named destinations, page labels, forms, layers,
/// attachments) the CLI merge carries.
///
/// # Errors
///
/// [`ToolError`] when an input cannot be read, the document is malformed, or
/// the output fails structural verification. No output is committed on error.
pub fn merge(inputs: &[String], output: &str) -> ToolResult<Verification> {
    tools::merge(inputs, output)
}

/// Extract the inclusive, zero-based page range `first..=last` into `output`.
///
/// # Errors
///
/// As [`merge`].
pub fn split(path: &str, first: usize, last: usize, output: &str) -> ToolResult<Verification> {
    tools::split(path, first, last, output)
}

/// Set the standard document information fields (`Title`, `Author`, ...).
///
/// # Errors
///
/// As [`merge`].
pub fn set_metadata(path: &str, fields: &[(&str, &str)], output: &str) -> ToolResult<Verification> {
    tools::set_metadata(path, fields, output)
}

/// Apply rectangle redactions to every page.
///
/// # Errors
///
/// As [`merge`].
pub fn redact(
    path: &str,
    rects: &[(f64, f64, f64, f64)],
    output: &str,
) -> ToolResult<Verification> {
    tools::redact(path, rects, output)
}

/// Rotate all pages, or the page selection `pages`, by `angle` degrees.
///
/// `pages` uses the CLI's inclusive page-selection syntax (`1-3,7`).
///
/// # Errors
///
/// As [`merge`].
pub fn rotate(
    path: &str,
    angle: i64,
    pages: Option<&str>,
    output: &str,
) -> ToolResult<Verification> {
    tools::rotate(path, angle, pages, output)
}

/// Delete the selected pages.
///
/// # Errors
///
/// As [`merge`].
pub fn delete(path: &str, pages: &str, output: &str) -> ToolResult<Verification> {
    tools::delete(path, pages, output)
}

/// Rewrite the document with pages in `order` (the CLI's selection syntax).
///
/// # Errors
///
/// As [`merge`].
pub fn reorder(path: &str, order: &str, output: &str) -> ToolResult<Verification> {
    tools::reorder(path, order, output)
}
