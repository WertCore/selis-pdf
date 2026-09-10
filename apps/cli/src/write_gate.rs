//! SL-1A.WRITE.05 — structural verification for tool output — and the
//! SL-1A.UI.02/06 commit surface.
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
//! The commit is also where SL-1A.UI.06 lives: large outputs are written in
//! chunks, with byte progress on stderr and a cancellation check between
//! chunks, so Ctrl-C maps to a typed `CANCELLED` and no partial file — never
//! a torn destination.
//!
//! # Scope
//!
//! This is the pre-G2 weaker standard (`23-EDIT-MODEL-SPEC.md §7`): it must
//! NOT be reused for content-rewriting operations (text editing, redaction
//! content stripping). The full render + text verification is SL-1A.WRITE.08
//! (filed at G2).

#![allow(clippy::arithmetic_side_effects)]

use selis_pdf_cos::verify::{Expectations, Fault, Observed, Verdict};
use selis_sandbox::{Budget, BudgetGuard, CancelToken};

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
    /// The outline entry count must match the input's.
    pub outlines: bool,
    /// The embedded-file count must match the input's.
    pub embedded_files: bool,
}

/// Counts guaranteed preserved by the full-rewrite tools (split picks a
/// subset of pages, so its page count is asserted by the caller instead).
pub(crate) const PRESERVE_ALL: Preserved = Preserved {
    pages: true,
    annotations: true,
    fields: true,
    ocgs: true,
    outlines: true,
    embedded_files: true,
};

/// Counts preserved by a page-selection operation: everything except pages
/// (and outlines, whose items pruned with the pages that hosted their
/// destinations). Embedded files never reference pages and always carry
/// through.
pub(crate) const PRESERVE_EXCEPT_PAGES: Preserved = Preserved {
    pages: false,
    annotations: true,
    fields: true,
    ocgs: true,
    outlines: false,
    embedded_files: true,
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
        outlines: if preserved.outlines {
            Some(input.outlines)
        } else {
            None
        },
        embedded_files: if preserved.embedded_files {
            Some(input.embedded_files)
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
/// Returns the verification verdict (SL-1A.UI.02: the display built from it
/// is the visible proof of the pass).
///
/// # Errors
///
/// `VERIFY_FAILED` when the output fails structural verification;
/// `WRITE_FAILED` when the atomic commit fails; `CANCELLED` when the
/// operation's cancel token fired during the commit.
pub(crate) fn write_verified(
    bytes: &[u8],
    output: &str,
    expected: &Expectations,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Verdict> {
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

    commit(bytes, output, &g.cancel_token().clone())?;
    Ok(verdict)
}

/// Commit bytes to `output` atomically (temp + fsync + rename) without
/// verification. For verbatim copies (e.g. `unlock` of an unencrypted
/// document): the bytes ARE the original document, so structural
/// verification would re-litigate the user's file rather than our writer.
///
/// # Errors
///
/// `WRITE_FAILED` when the atomic commit fails; `CANCELLED` when the
/// operation's cancel token fired during the commit.
pub(crate) fn atomic_write(bytes: &[u8], output: &str, cancel: &CancelToken) -> CliResult<()> {
    commit(bytes, output, cancel)
}

/// The shared atomic commit with SL-1A.UI.06 progress + cancellation:
/// temp file, chunked append (cancellation checked between chunks, byte
/// progress on stderr for large outputs), fsync, rename. A cancellation
/// before the rename leaves the destination untouched and the temp file
/// removed (`FileSink`'s drop).
fn commit(bytes: &[u8], output: &str, cancel: &CancelToken) -> CliResult<()> {
    let sink = selis_io::FileSink::create(output)
        .map_err(|e| CliError(format!("cannot create output {output}: {e}")))?;
    let mut sink: Box<dyn selis_io::DocSink> = Box::new(sink);
    let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if total >= crate::progress::MIN_REPORTED {
        sink = Box::new(crate::progress::ProgressSink::new(
            sink,
            total,
            cancel.clone(),
            crate::progress::stderr_report(output),
        ));
    }
    sink.append(bytes)
        .map_err(|e| CliError(format!("cannot write {output}: {e}")))?;
    sink.finish()
        .map_err(|e| CliError(format!("cannot commit {output}: {e}")))?;
    Ok(())
}

/// Survey `src`, verify `bytes` against the full preservation set
/// (`PRESERVE_ALL`: pages, annotations, fields, OCGs, outline entries,
/// embedded files), commit atomically, and return the display. For the
/// full-rewrite tools whose object walk carries every surveyed structure
/// through (unlock, protect, clear-permissions).
///
/// # Errors
///
/// As [`write_verified`].
pub(crate) fn verify_all_preserved(
    bytes: &[u8],
    output: &str,
    src: &[u8],
    in_bytes: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<crate::verify_report::Verification> {
    let observed = selis_pdf_cos::verify::survey(src, budget, g)
        .map_err(|e| CliError(format!("input survey failed: {e}")))?;
    let expected = expectations_from(&observed, &PRESERVE_ALL);
    let verdict = write_verified(bytes, output, &expected, budget, g)?;
    Ok(crate::verify_report::Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        in_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    ))
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
) -> CliResult<Verdict> {
    write_verified(bytes, output, &Expectations::none(), budget, g)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    /// The expectation builders carry the extended preservation set.
    #[test]
    fn expectations_cover_the_extended_counts() {
        let observed = Observed {
            pages: 3,
            annotations: 2,
            fields: 1,
            ocgs: 4,
            outlines: 7,
            embedded_files: 5,
        };
        let all = expectations_from(&observed, &PRESERVE_ALL);
        assert_eq!(all.pages, Some(3));
        assert_eq!(all.annotations, Some(2));
        assert_eq!(all.fields, Some(1));
        assert_eq!(all.ocgs, Some(4));
        assert_eq!(all.outlines, Some(7));
        assert_eq!(all.embedded_files, Some(5));

        // A page selection keeps embedded files but prunes outlines.
        let selection = expectations_from(&observed, &PRESERVE_EXCEPT_PAGES);
        assert_eq!(selection.pages, None);
        assert_eq!(selection.annotations, Some(2));
        assert_eq!(selection.outlines, None);
        assert_eq!(selection.embedded_files, Some(5));
    }
}
