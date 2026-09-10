//! SL-1A.WRITE.06 — the debug-only save operation the kill-test harness
//! drives. Compiled in debug builds only (`#[cfg(debug_assertions)]`): a
//! hidden `selis __crash-save` subcommand that runs one tool operation while
//! the sink aborts the process at a deterministic point, configured through
//! the `SELIS_DEBUG_CRASH_AFTER_BYTES` / `SELIS_DEBUG_CRASH_PHASE` env vars
//! read by `selis_io::{FileSink, AppendFileSink}`.
//!
//! The kill-test harness (`tests/write06_kill_test.rs`) spawns this binary as
//! a child process per kill point, then asserts the on-disk result is always
//! either absent, the untouched original, or a complete valid document —
//! never a torn file. The operation paths exercised are:
//!
//! * `split` — full rewrite (temp + rename commit),
//! * `rotate` — full rewrite (the current WRITE.02-less tool path), plus the
//!   incremental-update append path via an in-place `AppendFileSink`,
//! * `compress` — full rewrite (temp + rename commit).

#![cfg(debug_assertions)]
#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use selis_sandbox::{Budget, BudgetGuard};

/// Which save path the debug op drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrashOp {
    /// Split (page extraction) — full rewrite, temp+rename commit.
    Split,
    /// Rotate one page — full rewrite, temp+rename commit.
    RotateRewrite,
    /// Rotate one page — incremental update appended in place (WRITE.02
    /// writer + `AppendFileSink`).
    RotateIncremental,
    /// Compress — full rewrite, temp+rename commit.
    Compress,
}

/// Run one operation with crash injection active. Never returns on an
/// injected crash (the sink aborts the process); on success prints a marker.
pub(crate) fn run_crash_save(op: CrashOp, input: &str, output: &str) -> crate::CliResult<()> {
    match op {
        CrashOp::Split => crate::tools::split(input, 0, 0, output).map(|_v| ()),
        CrashOp::RotateRewrite => crate::tools::rotate(input, 90, Some("0"), output).map(|_v| ()),
        CrashOp::RotateIncremental => {
            let budget = Budget::unlimited();
            let mut g = budget.guard();
            rotate_incremental_for_kill_test(input, output, &budget, &mut g)
        }
        CrashOp::Compress => crate::compress::optimise_file(input, output).map(|_pair| ()),
    }
}

/// Parse a `CrashOp` from the subcommand argument.
pub(crate) fn parse_crash_op(s: &str) -> Option<CrashOp> {
    match s {
        "split" => Some(CrashOp::Split),
        "rotate-rewrite" => Some(CrashOp::RotateRewrite),
        "rotate-incremental" => Some(CrashOp::RotateIncremental),
        "compress" => Some(CrashOp::Compress),
        _ => None,
    }
}

/// Rotate one page as an incremental update (WRITE.02 writer) appended
/// in place through an `AppendFileSink` — the SL-0.IO.05 incremental path
/// the kill test must prove. The original file's bytes are never touched;
/// the new revision is appended and fsynced by `finish()`.
fn rotate_incremental_for_kill_test(
    path: &str,
    output: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> crate::CliResult<()> {
    use selis_pdf_cos::xref;
    use selis_pdf_cos::Obj;

    let src = crate::read_file(path)?;
    let startxref = xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, budget, g)
        .map_err(|e| crate::CliError(format!("{path}: cannot open: {e}")))?;

    // The first page's current object: rebuild it with /Rotate 90. For the
    // kill test the page is located through the engine's page model.
    let session = selis_pdf_engine::Session::open(
        src.clone(),
        &Budget::profile(selis_sandbox::Surface::Viewer),
    )
    .map_err(|e| crate::CliError(format!("{path}: {e}")))?;
    let _ = session;

    // Find the first /Type /Page object in the newest revision.
    let rev = doc
        .revisions()
        .last()
        .ok_or_else(|| crate::CliError(format!("{path}: no revisions")))?;
    let mut page_num = None;
    let mut page_obj: Option<Obj> = None;
    for (num, entry) in &rev.entries {
        if let selis_pdf_cos::XrefEntry::InUse { offset, .. } = entry {
            let obj = selis_pdf_cos::resolve_object(&src, *offset, budget, g)
                .map_err(|e| crate::CliError(format!("{path}: {e}")))?;
            let is_page = matches!(&obj,
                Obj::Dict(pairs) if pairs.iter().any(|(k, v)| k.as_slice() == b"Type"
                    && matches!(v, Obj::Name(n) if n.as_slice() == b"Page")));
            if is_page {
                page_num = Some(*num);
                page_obj = Some(obj);
                break;
            }
        }
    }
    let (page_num, page_obj) = match (page_num, page_obj) {
        (Some(n), Some(o)) => (n, o),
        _ => return Err(crate::CliError(format!("{path}: no page object found"))),
    };

    // Add /Rotate 90 to the page dict (existing /Rotate increases).
    let rotated = match page_obj {
        Obj::Dict(pairs) => {
            let mut pairs = pairs;
            let existing = pairs
                .iter()
                .find(|(k, _)| k.as_slice() == b"Rotate")
                .and_then(|(_, v)| match v {
                    Obj::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0);
            let next = existing.saturating_add(90).rem_euclid(360);
            if let Some(slot) = pairs.iter_mut().find(|(k, _)| k.as_slice() == b"Rotate") {
                slot.1 = Obj::Int(next);
            } else {
                pairs.push((
                    selis_bytes::Bytes::copy_from_slice(b"Rotate"),
                    Obj::Int(next),
                ));
            }
            Obj::Dict(pairs)
        }
        _ => unreachable!("checked is_page above"),
    };

    // Original trailer entries carried forward (/Root, /ID when present).
    let mut trailer: Vec<(Vec<u8>, Obj)> = Vec::new();
    if let Some(root) = rev.root {
        trailer.push((b"Root".to_vec(), Obj::Ref(root)));
    }
    if let Some((_, id)) = rev.trailer.iter().find(|(k, _)| k.as_slice() == b"ID") {
        trailer.push((b"ID".to_vec(), id.clone()));
    }

    // The output IS the input path for an in-place append; the harness copies
    // the original to the output path first so the "untouched original"
    // assertion stays meaningful.
    let updated = selis_pdf_cos::doc_writer::write_incremental_update(
        &src,
        &[(page_num, rotated)],
        &trailer,
        budget,
        g,
    )
    .map_err(|e| crate::CliError(format!("{path}: incremental write: {e}")))?;

    // The appended part is everything after the original prefix. Verify
    // structurally, then append in place: write the original bytes (already
    // on disk if output == input; the harness copies), then append the tail.
    let verdict = selis_pdf_cos::verify::verify_structural(
        &updated,
        &selis_pdf_cos::verify::Expectations::none(),
        budget,
        g,
    )
    .map_err(|e| crate::CliError(format!("{path}: verification failed to run: {e}")))?;
    if !verdict.ok {
        return Err(crate::CliError(format!(
            "{path}: incremental output failed structural verification (VERIFY_FAILED)"
        )));
    }

    // In-place append: the destination already holds the original bytes
    // (copied there by the harness); append only the tail.
    let appended = updated.len().saturating_sub(src.len());
    let tail = updated
        .get(src.len()..)
        .ok_or_else(|| crate::CliError("append: tail shorter than prefix".to_string()))?;
    debug_assert_eq!(tail.len(), appended);
    if output != path {
        // First make sure the destination holds the original (the harness
        // normally does this; kept for standalone use).
        if std::fs::read(output).unwrap_or_default() != src {
            std::fs::write(output, &src)
                .map_err(|e| crate::CliError(format!("cannot seed {output}: {e}")))?;
        }
    }
    let sink = selis_io::AppendFileSink::open(output)
        .map_err(|e| crate::CliError(format!("cannot open {output} for append: {e}")))?;
    let mut sink: Box<dyn selis_io::DocSink> = Box::new(sink);
    sink.append(tail)
        .map_err(|e| crate::CliError(format!("cannot append to {output}: {e}")))?;
    sink.finish()
        .map_err(|e| crate::CliError(format!("cannot commit {output}: {e}")))?;
    Ok(())
}
