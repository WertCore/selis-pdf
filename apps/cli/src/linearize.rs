//! The `selis linearize` command.
//!
//! A thin CLI wrapper over [`selis_pdf_tools::linearize`] (the "fast web view"
//! rewrite): read the input file, write the first-page objects first with a
//! real `/Linearized` dictionary, and print the measured report and verdict.
//! The tool op itself prints the one-line summary and verification line, so
//! this wrapper only forwards the verification object for `--json`.

use crate::verify_report::Verification;
use crate::{CliError, CliResult};

/// Linearize `path`, write `output`, and return the verified result.
///
/// The engine prints its own stderr line (sizes, object count, `/L` and `/N`)
/// and verification verdict; `--json` adds the machine-readable object on
/// stdout. The output is structurally re-opened before the file is committed.
///
/// # Errors
///
/// A typed tool error when the input cannot be read, is encrypted (unlock is a
/// separate operation), has no usable page tree, or the rewritten output fails
/// re-opening / verification. No output is committed on failure.
///
/// # Budget
///
/// Delegates to [`selis_pdf_tools::linearize`], which runs under
/// `Budget::unlimited()` with a guard ticking cancellation and wall time. This
/// wrapper adds no parsing of its own.
///
/// # Malformed Input
///
/// The tool op parses and refuses an encrypted document or a broken page tree
/// with a typed error and writes nothing; malformed input never reaches this
/// wrapper as a committed file.
pub(crate) fn run(path: &str, output: &str) -> CliResult<Verification> {
    let (_report, verification) =
        selis_pdf_tools::linearize(path, output).map_err(CliError::from)?;
    Ok(verification)
}
