//! SL-1A.WRITE.05 — structural verification for tool output.
//!
//! Every tool writes through [`write_verified`]: the produced bytes are
//! verified structurally (`selis_pdf_cos::verify`) *before* they reach disk,
//! then committed atomically (temp + fsync + rename via
//! `selis_io::FileSink`). A failing verification produces no file at all —
//! the typed error names the fault, and the destination keeps whatever it
//! held before the call.
//!
//! Expectations are captured from the input with
//! [`selis_pdf_cos::verify::survey`] and asserted on the output where the
//! operation guarantees preservation (page count is operation-specific; the
//! per-tool wrappers below decide which counts must hold).
//!
//! # Scope
//!
//! This is the pre-G2 weaker standard (`23-EDIT-MODEL-SPEC.md §7`): it must
//! NOT be reused for content-rewriting operations (text editing, redaction
//! content stripping). The full render + text verification is SL-1A.WRITE.08
//! (filed at G2).

#![allow(clippy::arithmetic_side_effects)]

use selis_pdf_cos::verify::{Expectations, Fault, Observed};
use selis_sandbox::{Budget, BudgetGuard};

use crate::{CliError, CliResult};

/// Which counts an operation guarantees are preserved from input to output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Preserved {
    /// The page count must match the input's.
    pub pages: bool,
    /// The annotation count must match the input's.
    pub annotations: bool,
    /// The form-field count must match the input's.
    pub fields: bool,
    /// The optional-content group count must match the input's.
    pub ocgs: bool,
}

/// Counts guaranteed preserved by the full-rewrite tools (split picks a
/// subset of pages, so its page count is asserted by the caller instead).
pub(crate) const PRESERVE_ALL: Preserved = Preserved {
    pages: true,
    annotations: true,
    fields: true,
    ocgs: true,
};

/// Counts preserved by a page-selection operation: everything except pages.
pub(crate) const PRESERVE_EXCEPT_PAGES: Preserved = Preserved {
    pages: false,
    annotations: true,
    fields: true,
    ocgs: true,
};

/// Build the output expectations from the input's observed counts.
pub(crate) fn expectations_from(input: &Observed, preserved: &Preserved) -> Expectations {
    Expectations {
        pages: if preserved.pages {
            Some(input.pages)
        } else {
            None
        },
        annotations: if preserved.annotations {
            Some(input.annotations)
        } else {
            None
        },
        fields: if preserved.fields {
            Some(input.fields)
        } else {
            None
        },
        ocgs: if preserved.ocgs {
            Some(input.ocgs)
        } else {
            None
        },
    }
}

/// Verify writer output structurally and commit it atomically to `output`.
///
/// The bytes never touch the destination until verification passes and the
/// temp file is fsynced. On any failure the destination is untouched and a
/// typed error names the first fault.
///
/// # Errors
///
/// `VERIFY_FAILED` when the output fails structural verification;
/// `WRITE_FAILED` when the atomic commit fails.
pub(crate) fn write_verified(
    bytes: &[u8],
    output: &str,
    expected: &Expectations,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<()> {
    // Verification BEFORE anything reaches the destination.
    let verdict = selis_pdf_cos::verify::verify_structural(bytes, expected, budget, g)
        .map_err(|e| CliError(format!("verification failed to run: {e}")))?;
    if !verdict.ok {
        let first = verdict
            .faults
            .first()
            .map(|f: &Fault| f.to_string())
            .unwrap_or_else(|| "unknown fault".to_string());
        return Err(CliError(format!(
            "output failed structural verification (VERIFY_FAILED): {first}"
        )));
    }

    // Atomic commit: temp file in the destination directory, fsync, rename.
    let sink = selis_io::FileSink::create(output)
        .map_err(|e| CliError(format!("cannot create output {output}: {e}")))?;
    let mut sink: Box<dyn selis_io::DocSink> = Box::new(sink);
    sink.append(bytes)
        .map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    sink.finish()
        .map_err(|e| CliError(format!("cannot commit {output}: {e}")))?;
    Ok(())
}

/// Commit bytes to `output` atomically (temp + fsync + rename) without
/// verification. For verbatim copies (e.g. `unlock` of an unencrypted
/// document): the bytes ARE the original document, so structural
/// verification would re-litigate the user's file rather than our writer.
///
/// # Errors
///
/// `WRITE_FAILED` when the atomic commit fails.
pub(crate) fn atomic_write(bytes: &[u8], output: &str) -> CliResult<()> {
    let sink = selis_io::FileSink::create(output)
        .map_err(|e| CliError(format!("cannot create output {output}: {e}")))?;
    let mut sink: Box<dyn selis_io::DocSink> = Box::new(sink);
    sink.append(bytes)
        .map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    sink.finish()
        .map_err(|e| CliError(format!("cannot commit {output}: {e}")))?;
    Ok(())
}

/// Verify *generated* output (no input document to survey: `topdf`,
/// `img2pdf`) with no count expectations — the reference-resolution and
/// page-tree checks still run — and commit atomically.
///
/// # Errors
///
/// `VERIFY_FAILED` when the output fails structural verification;
/// `WRITE_FAILED` when the atomic commit fails.
pub(crate) fn write_generated(
    bytes: &[u8],
    output: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<()> {
    write_verified(bytes, output, &Expectations::none(), budget, g)
}
