//! The `selis subset-fonts` command.
//!
//! A thin CLI wrapper over [`selis_pdf_tools::subset_fonts`]: read the input
//! file, replace the embedded TrueType programs with subsets that keep only
//! the glyphs the document's text actually shows, write the verified output,
//! and print the measured report. The engine refuses — without writing any
//! output — documents that are encrypted, that use no eligible embedded font,
//! or whose font shape it cannot subset.

use crate::verify_report::Verification;
use crate::{CliError, CliResult};

/// Subset the embedded fonts of `path`, write `output`, and print the report.
///
/// The one-line summary and the verification line go to stderr; `--json` adds
/// the machine-readable verification object on stdout. The tool op reopens the
/// output and structurally verifies it before the file is committed.
///
/// # Errors
///
/// A typed tool error when the input cannot be read, is malformed or
/// encrypted, has no eligible embedded TrueType font, uses a font shape this
/// pass cannot subset, or the rewrite fails verification. No output is
/// committed on failure.
///
/// # Budget
///
/// Delegates to [`selis_pdf_tools::subset_fonts`], which runs under
/// `Budget::unlimited()` with a guard that ticks cancellation and wall time.
/// This wrapper adds no parsing of its own.
///
/// # Malformed Input
///
/// Handled entirely by the tool op: a malformed document, an unsupported font
/// program, or a rewrite that would not shrink the file is refused loudly and
/// never written. The caller sees the typed reason, not a silent copy.
pub(crate) fn run(path: &str, output: &str) -> CliResult<Verification> {
    let (report, verification) =
        selis_pdf_tools::subset_fonts(path, output).map_err(CliError::from)?;
    eprintln!(
        "subset fonts in {path}: {in} -> {out} bytes, {fonts} font(s) subset, {glyphs} glyph(s) kept -> {output}",
        in = report.in_bytes,
        out = report.out_bytes,
        fonts = report.fonts_subset,
        glyphs = report.glyphs_kept,
    );
    verification.emit_line();
    Ok(verification)
}
