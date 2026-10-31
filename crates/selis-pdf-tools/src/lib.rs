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

pub mod attachments;
pub mod compress;
pub mod images;
mod io;
mod memory;
pub mod overlay;
#[cfg(feature = "native-fs")]
mod progress;
pub mod repair;
pub mod runtime;
mod tools;
mod verify_report;
pub mod write_gate;
mod zip;

use selis_sandbox::Clock;

pub use compress::{compress, compress_file, CompressionReport};
pub use images::{image_to_pdf, ImagePageSize, ImageToPdfOptions};
pub use memory::{clear, insert_input, take_output, MemorySession, write_output};
pub use verify_report::Verification;

/// The most PDF parts one split request may produce.
///
/// The browser page can display a bounded number of downloads, and the tools
/// protocol is sized for one in-memory result. A split that would exceed this
/// limit is refused before any part is written; choose a larger interval or a
/// range instead.
pub const MAX_SPLIT_OUTPUTS: usize = 64;

/// The most blank pages one insert request may add.
///
/// The cap keeps a single edit from producing an unbounded output; a document
/// that already needs more blank pages can be built in successive, verified
/// requests.
pub const MAX_BLANK_PAGES: usize = 100;

/// One PDF produced by a multi-output split.
#[derive(Debug, Clone)]
pub struct SplitPart {
    /// The inclusive first source page (zero-based).
    pub first: usize,
    /// The inclusive last source page (zero-based).
    pub last: usize,
    /// The backend key the verified bytes were written to.
    pub output: String,
    /// The verification display for this part.
    pub verification: Verification,
    /// True when this part alone exceeds a size target.
    ///
    /// A single page that cannot fit the requested target is still emitted
    /// rather than being dropped; callers surface this flag instead of
    /// pretending the output met the target. Multi-page parts never carry it.
    pub over_target: bool,
}

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

/// Interleave the pages of two or more PDFs into `output`.
///
/// The first output page comes from the first input, the second from the
/// second input, and so on, cycling through the inputs until each is
/// exhausted. Document-level structures are reconciled exactly as in
/// [`merge`], and interleaved page labels are materialised per page so one
/// input's numbering style cannot leak into another input's pages.
///
/// # Errors
///
/// As [`merge`].
pub fn mix(inputs: &[String], output: &str) -> ToolResult<Verification> {
    tools::mix(inputs, output)
}

/// Extract the inclusive, zero-based page range `first..=last` into `output`.
///
/// # Errors
///
/// As [`merge`].
pub fn split(path: &str, first: usize, last: usize, output: &str) -> ToolResult<Verification> {
    tools::split(path, first, last, output)
}

/// Split `path` into consecutive groups of `every` pages.
///
/// Outputs are written as `{output_prefix}0.pdf`, `{output_prefix}1.pdf`, and
/// so on; the returned [`SplitPart`] values name the source range and the
/// exact backend key for each one. At most [`MAX_SPLIT_OUTPUTS`] parts are
/// produced, and each part is written only after its own verification passes;
/// a failing part aborts the request.
///
/// # Errors
///
/// As [`split`], plus `every == 0` and a part count above the limit.
pub fn split_every(path: &str, every: usize, output_prefix: &str) -> ToolResult<Vec<SplitPart>> {
    tools::split_every(path, every, output_prefix)
}

/// Split `path` in half at the middle page.
///
/// A document with `n` pages produces a first part with `n / 2` pages and a
/// second part with the remainder. A one-page document is refused rather than
/// pretending it was split.
///
/// # Errors
///
/// As [`split_every`].
pub fn split_half(path: &str, output_prefix: &str) -> ToolResult<Vec<SplitPart>> {
    tools::split_half(path, output_prefix)
}

/// Split `path` into consecutive groups whose measured PDF output stays at or
/// below `target_bytes`.
///
/// The writer is the measurement: each candidate group is built and written
/// in memory, so the boundary reflects the actual output rather than a page
/// estimate. A single page larger than the target is emitted on its own and
/// its [`SplitPart::over_target`] flag is set; a multi-page group is never
/// allowed to exceed the target. The 64-part cap is enforced before any part
/// is committed.
///
/// # Errors
///
/// As [`split`], plus `target_bytes == 0` and a part count above
/// [`MAX_SPLIT_OUTPUTS`].
pub fn split_by_size(
    path: &str,
    target_bytes: u64,
    output_prefix: &str,
) -> ToolResult<Vec<SplitPart>> {
    tools::split_by_size(path, target_bytes, output_prefix)
}

/// Split `path` at the pages named by the document's top-level bookmarks.
///
/// Only the first outline level creates boundaries. Pages before the first
/// bookmark become a separate front-matter part; duplicate bookmark targets
/// are collapsed; targets are applied in page order. A document without a
/// readable outline, or whose bookmarks do not divide it into at least two
/// parts, is refused rather than silently producing a copy.
///
/// # Errors
///
/// As [`split`], plus absent/truncated outlines and a single resulting part.
pub fn split_by_bookmarks(path: &str, output_prefix: &str) -> ToolResult<Vec<SplitPart>> {
    tools::split_by_bookmarks(path, output_prefix)
}

/// Extract the pages in `selection` (the CLI's `0,2,5-7` syntax) into one
/// PDF. The selected pages keep document order.
///
/// # Errors
///
/// As [`split`].
pub fn extract_pages(path: &str, selection: &str, output: &str) -> ToolResult<Verification> {
    tools::extract_pages(path, selection, output)
}

/// Bundle flat files into a deterministic, store-method ZIP.
///
/// This is the multi-output download path: names are validated (no
/// separators, control characters, or `..` markers), every entry is copied
/// once, and an oversized archive fails instead of truncating.
///
/// # Errors
///
/// [`ToolError`] when `entries` is empty, a name is unsafe, or the archive
/// exceeds the ZIP format's 4 GiB/65535-entry limits.
pub fn bundle_zip(entries: &[(&str, &[u8])]) -> ToolResult<Vec<u8>> {
    zip::store_bundle(entries).map_err(ToolError)
}

/// Set the standard document information fields (`Title`, `Author`, ...).
///
/// # Errors
///
/// As [`merge`].
pub fn set_metadata(path: &str, fields: &[(&str, &str)], output: &str) -> ToolResult<Verification> {
    tools::set_metadata(path, fields, output)
}

/// Remove the `/Info` dictionary and the catalog's XMP `/Metadata` packet.
///
/// Visible and structural features survive: pages, annotations, form fields,
/// optional-content groups, outlines, page labels, embedded files, and the
/// tagged structure tree. Removing metadata invalidates PDF/A conformance and
/// any signature that covers the catalog, so a caller should say so rather
/// than presenting the result as a lossless edit.
///
/// # Errors
///
/// As [`merge`], plus an encrypted input.
pub fn strip_metadata(path: &str, output: &str) -> ToolResult<Verification> {
    tools::strip_metadata(path, output)
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

pub use tools::{
    CropMargins, FlipAxis, NupOptions, PageLabelOptions, PageLabelStyle, ResizeOptions,
};

/// Crop pages by visual margins in points.
///
/// The margins are measured in each page's displayed orientation (rotation
/// applied), so a `/Rotate 90` page removes the edge the reader sees as left,
/// top, right, or bottom. The new rectangle becomes both `/MediaBox` and
/// `/CropBox`; `/BleedBox`, `/TrimBox`, and `/ArtBox` outside it are dropped.
/// `pages` is `None` for all pages or a zero-based selection (`0,2,5-7`).
///
/// # Errors
///
/// As [`merge`], plus negative or non-finite margins, all-zero margins, a page
/// whose margins leave no area, or a page rotation that is not a multiple of
/// 90 degrees.
pub fn crop(
    path: &str,
    margins: CropMargins,
    pages: Option<&str>,
    output: &str,
) -> ToolResult<Verification> {
    tools::crop(path, margins, pages, output)
}

/// Mirror the painted content of pages horizontally or vertically.
///
/// The axis is relative to each page's displayed orientation. Page boxes,
/// rotation, resources, annotations, and form fields are carried unchanged;
/// annotations are not mirrored because the operation transforms painted
/// content only. `pages` is `None` for all pages or a zero-based selection.
///
/// # Errors
///
/// As [`merge`], plus a page rotation that is not a multiple of 90 degrees, or
/// a page `/Contents` entry that is neither a stream reference nor an array.
pub fn flip(
    path: &str,
    axis: FlipAxis,
    pages: Option<&str>,
    output: &str,
) -> ToolResult<Verification> {
    tools::flip(path, axis, pages, output)
}

/// Scale pages to a new page size in points.
///
/// The page size is applied in each page's displayed orientation: selected
/// pages get a fresh `/MediaBox` and `/CropBox` of exactly `options.width` x
/// `options.height` and a normalized `/Rotate 0`. With `options.fit` the
/// content keeps its aspect ratio and is centred; otherwise it fills both
/// dimensions. Annotations and form fields are carried unchanged, and
/// unselected pages are copied as they were.
///
/// # Errors
///
/// As [`merge`], plus a width or height outside 1..=14400 points, a page with
/// no displayed area, or a rotation that is not a multiple of 90 degrees.
pub fn resize(
    path: &str,
    options: ResizeOptions,
    pages: Option<&str>,
    output: &str,
) -> ToolResult<Verification> {
    tools::resize(path, options, pages, output)
}

/// Place several pages on each sheet (N-up).
///
/// `options.per_sheet` pages are scaled into a grid of cells on one sheet:
/// 2 (1 x 2), 4 (2 x 2), 6 (2 x 3), or 9 (3 x 3). Every sheet takes the
/// first page's displayed size, and each page keeps its aspect ratio and its
/// displayed orientation. Content is placed through Form XObjects, so the
/// source pages' own resources are preserved, and annotations, links, and
/// form fields are not painted on the sheets.
///
/// # Errors
///
/// As [`merge`], plus a `per_sheet` that is not 2, 4, 6, or 9, a page with no
/// displayed area, a rotation that is not a multiple of 90 degrees, or an
/// encrypted input.
pub fn nup(path: &str, options: NupOptions, output: &str) -> ToolResult<Verification> {
    tools::nup(path, options, output)
}

/// Rebuild a damaged PDF from the strongest recovery that succeeds.
///
/// The output is a clean rewrite of every recovered page through the same
/// writer and structural gate as the other tools. The verification object's
/// `repair_mode` says which strategy produced it (`strict`,
/// `rolled-back revision`, or `reconstructed index`), and the recovered page
/// count is asserted on the output, so a repair never silently drops pages.
/// Encrypted inputs are refused; remove the password with unlock first.
///
/// # Errors
///
/// As [`merge`], plus an input whose object table cannot be recovered at all,
/// a recovered document with no pages, or an encrypted input.
pub fn repair(path: &str, output: &str) -> ToolResult<Verification> {
    repair::repair(path, output)
}

pub use repair::RepairMode;

/// Set the page labels shown by readers.
///
/// `pages` is `None` for every page or a zero-based selection (`0,2,5-7`).
/// Selected pages are renumbered in document order from the options' start,
/// using its style and prefix. Unselected pages keep the label they already
/// carried, materialised from the input's `/PageLabels` tree or its default
/// decimal numbering. Every output page gets an explicit label entry.
///
/// # Errors
///
/// As [`merge`], plus an invalid start, prefix, or page selection, or an
/// encrypted input.
pub fn page_labels(
    path: &str,
    options: &PageLabelOptions,
    pages: Option<&str>,
    output: &str,
) -> ToolResult<Verification> {
    tools::page_labels(path, options, pages, output)
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

/// Duplicate each selected page immediately after its original.
///
/// `pages` uses the CLI's zero-based selection syntax (`0,2,5-7`). The
/// selection is deduplicated and applied in source order; content, resources,
/// and annotations are copied with each page. Outlines and named destinations
/// keep targeting the first copy, while page labels re-key to the new page
/// sequence.
///
/// # Errors
///
/// As [`merge`], plus an empty or malformed page selection.
pub fn duplicate_pages(path: &str, pages: &str, output: &str) -> ToolResult<Verification> {
    tools::duplicate_pages(path, pages, output)
}

/// Insert `count` blank pages at a page boundary.
///
/// `after` is the 1-based page after which the blanks go: `0` inserts before
/// the first page, and `None` appends. Each blank uses the template page's
/// media box and rotation (the page at `after - 1`, the first page at `0`, or
/// the last page when appending).
///
/// # Errors
///
/// As [`merge`], plus `count == 0`, a count above [`MAX_BLANK_PAGES`], a
/// boundary outside the document, or a document with no pages.
pub fn insert_blank_pages(
    path: &str,
    after: Option<usize>,
    count: usize,
    output: &str,
) -> ToolResult<Verification> {
    tools::insert_blank_pages(path, after, count, output)
}
