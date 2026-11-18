//! Crash-safe save commit for incremental edits (SL-5.EDIT.04).
//!
//! [`crate::incremental::save_incremental`] returns bytes and never touches the
//! filesystem. This module is the safe commit of those bytes: it verifies the
//! output **before** anything reaches disk, then delegates the durable swap to
//! [`selis_io::commit_atomic`] (temp + fsync + atomic rename + directory fsync).
//!
//! Verification is deliberately stricter than "it parses": the candidate must
//! keep the original bytes as a verbatim prefix (EDIT.03 I3), end at a `%%EOF`
//! (no torn tail), append exactly one complete revision when it differs from
//! the original, parse under both the strict and resilient readers, and keep
//! the page count. A candidate that fails any of these produces **no file** and
//! a typed error; the original is left byte-identical and the caller's in-memory
//! session is untouched.
//!
//! The commit's fault hook is exposed as [`SaveOptions::fault`] so the nightly
//! kill-test can deterministically abort at 500 points. Production callers use
//! [`save_atomic`], which never sets it.
//!
//! # Budget
//!
//! Every parse and survey used for verification is charged to the caller's
//! [`Budget`] / [`BudgetGuard`], and a `tick()` checks the deadline and cancel
//! token immediately before the commit. The file I/O itself does no document
//! derived allocation and reads no clock (see `selis_io::commit`).
//!
//! # Malformed Input
//!
//! A candidate that is not a valid one-revision extension of the original is
//! refused with `PREFIX_VIOLATION` or `VERIFY_FAILED` before any write. I/O and
//! injected-fault failures are `WRITE_FAILED`. In all cases the destination is
//! unchanged and no temp file is left behind.

use std::path::Path;

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

pub use selis_io::commit::{CommitFault as SaveFault, CommitReceipt};

/// Options for [`save_atomic_with`]. `Default` is the production configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveOptions {
    /// Deterministic kill-test hook. `None` in production.
    pub fault: Option<SaveFault>,
}

/// The structural checks a candidate passed before it was allowed to replace
/// the destination. Returned so the caller can report *why* the save succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveVerification {
    /// Page count observed in the candidate.
    pub pages: u64,
    /// Number of revisions in the candidate.
    pub revisions: usize,
    /// Whether the original bytes are a verbatim prefix (EDIT.03 I3).
    pub prefix_preserved: bool,
    /// Bytes appended past the original (0 for a no-op save).
    pub appended_bytes: u64,
}

/// What a successful commit produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveReport {
    /// Bytes written to the destination.
    pub bytes: u64,
    /// Whether the containing directory was fsynced (POSIX: yes; Windows: no,
    /// the swap used `MOVEFILE_WRITE_THROUGH`).
    pub directory_fsynced: bool,
}

/// Verify a candidate output against the original before it may be committed.
///
/// This is the verification-before-replace gate from
/// `23-EDIT-MODEL-SPEC.md §7` step 3-4, specialised for an incremental save:
/// I3 prefix, a complete `%%EOF` tail, exactly one appended revision, strict
/// parse, resilient parse agreement and page-count preservation.
///
/// # Errors
///
/// `PREFIX_VIOLATION` when the candidate is not a byte-for-byte extension of
/// `original`; `VERIFY_FAILED` (detail naming the discrepancy) for every other
/// refusal. Parse/survey budget and cancellation errors propagate unchanged.
///
/// # Budget
///
/// Charges each parse, resilient parse and survey to `g`.
///
/// # Malformed Input
///
/// See the module docs. No filesystem access occurs in this function.
pub fn verify_before_commit(
    original: &[u8],
    candidate: &[u8],
    expected_pages: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<SaveVerification> {
    // I3: the original revision is never rewritten (EDIT.03's load-bearing
    // invariant). Use .get so a short candidate is a clean refusal, not a slice
    // that could panic.
    let prefix_preserved = candidate.get(..original.len()) == Some(original);
    if !prefix_preserved {
        return Err(err!(
            Code::PrefixViolation,
            during = "save",
            detail = "candidate is not a byte-identical extension of the original"
        ));
    }
    // A torn tail must be rejected here, not left for the reader to discard:
    // find_startxref would otherwise find the ORIGINAL startxref and "verify"
    // a file that is really original bytes plus a half-written revision.
    if !ends_with_eof(candidate) {
        return Err(err!(
            Code::VerifyFailed,
            during = "save",
            detail = "candidate does not end with %%EOF (torn or incomplete output)"
        ));
    }

    let original_start = selis_pdf_cos::xref::find_startxref(original, 4096).ok_or_else(|| {
        err!(
            Code::VerifyFailed,
            during = "save",
            detail = "original has no startxref"
        )
    })?;
    let candidate_start =
        selis_pdf_cos::xref::find_startxref(candidate, 4096).ok_or_else(|| {
            err!(
                Code::VerifyFailed,
                during = "save",
                detail = "candidate has no startxref"
            )
        })?;
    let original_doc = selis_pdf_cos::parse_revisions(original, original_start, budget, g)?;
    let doc = selis_pdf_cos::parse_revisions(candidate, candidate_start, budget, g)?;

    // "Prefix of intended changes": a save either changes nothing, or appends
    // exactly one complete revision whose xref points at the previous tail.
    let appended = candidate.len().saturating_sub(original.len());
    if appended == 0 {
        if candidate != original {
            return Err(err!(
                Code::VerifyFailed,
                during = "save",
                detail = "same-length candidate differs from the original"
            ));
        }
    } else if candidate_start <= original_start || doc.len() != original_doc.len().saturating_add(1)
    {
        return Err(err!(
            Code::VerifyFailed,
            during = "save",
            detail = "candidate did not append exactly one complete revision"
        ));
    }

    // The resilient reader must see the same revision count: a torn newest
    // revision would be rolled back and yield fewer.
    let resilient =
        selis_pdf_cos::parse_revisions_resilient(candidate, candidate_start, budget, g)?
            .ok_or_else(|| {
                err!(
                    Code::VerifyFailed,
                    during = "save",
                    detail = "resilient parse found no revision"
                )
            })?;
    if resilient.len() != doc.len() {
        return Err(err!(
            Code::VerifyFailed,
            during = "save",
            detail = "resilient parse discarded the newest revision"
        ));
    }

    let observed = selis_pdf_cos::verify::survey(candidate, budget, g)?;
    if observed.pages != expected_pages {
        return Err(err!(
            Code::VerifyFailed,
            during = "save",
            detail = format!(
                "page count changed: {} -> {}",
                expected_pages, observed.pages
            )
        ));
    }

    Ok(SaveVerification {
        pages: observed.pages,
        revisions: doc.len(),
        prefix_preserved,
        appended_bytes: u64::try_from(appended).unwrap_or(u64::MAX),
    })
}

/// Survey `original` for the page count a save must preserve.
///
/// # Errors
///
/// Propagates parse/survey errors from [`selis_pdf_cos::verify::survey`].
///
/// # Budget
///
/// Charges the survey to `g`.
///
/// # Malformed Input
///
/// An unparseable original is a typed error; no file is touched.
pub fn expected_pages(original: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<u64> {
    Ok(selis_pdf_cos::verify::survey(original, budget, g)?.pages)
}

/// Verify and atomically commit `candidate` to `path`, with the kill-test hook.
///
/// The original bytes are read from `original` for the prefix and page-count
/// checks; the destination file is only replaced after every check passes.
///
/// # Errors
///
/// `PREFIX_VIOLATION` / `VERIFY_FAILED` from [`verify_before_commit`],
/// `WRITE_FAILED` from the atomic commit (including every injected fault), or a
/// propagated budget/cancellation error. The destination is untouched on every
/// error path.
///
/// # Budget
///
/// See the module docs: verification parses and the pre-commit `tick()`.
///
/// # Malformed Input
///
/// See the module docs. A refused candidate produces no file.
pub fn save_atomic_with(
    path: &Path,
    original: &[u8],
    candidate: &[u8],
    options: &SaveOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<SaveReport> {
    let pages = expected_pages(original, budget, g)?;
    let _verification = verify_before_commit(original, candidate, pages, budget, g)?;
    g.tick()?; // caller's clock / cancel convention, checked before any I/O
    let receipt = selis_io::commit_atomic(
        path,
        candidate,
        &selis_io::CommitOptions {
            fault: options.fault,
        },
    )?;
    Ok(SaveReport {
        bytes: receipt.bytes,
        directory_fsynced: receipt.directory_fsynced,
    })
}

/// Production entry point: verified, crash-safe save with no fault injection.
///
/// # Errors
///
/// As [`save_atomic_with`] with default (no-fault) options.
///
/// # Budget
///
/// As [`save_atomic_with`].
///
/// # Malformed Input
///
/// As [`save_atomic_with`].
pub fn save_atomic(
    path: &Path,
    original: &[u8],
    candidate: &[u8],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<SaveReport> {
    save_atomic_with(
        path,
        original,
        candidate,
        &SaveOptions::default(),
        budget,
        g,
    )
}

/// True when the last non-whitespace bytes are the `%%EOF` marker. A truncated
/// incremental update fails this even though its original `startxref` still
/// parses — which is exactly the torn-tail case verification must catch.
fn ends_with_eof(bytes: &[u8]) -> bool {
    let mut end = bytes.len();
    while end > 0
        && matches!(
            bytes.get(end.saturating_sub(1)),
            Some(b' ' | b'\n' | b'\r' | b'\t' | 0)
        )
    {
        end = end.saturating_sub(1);
    }
    let start = end.saturating_sub(5);
    bytes.get(start..end) == Some(b"%%EOF".as_slice())
}
