//! The `selis batch` tool (SL-1A.TOOL.11): run a tool across a file set with
//! per-file isolation and a machine-readable report.
//!
//! One bad document must never kill the batch: every file is processed
//! independently, failures are collected, and `report.json` in the output
//! directory records the outcome, sizes, and error message per file. v1
//! supports `compress`; the harness extends to the other tools.

use std::time::Instant;

use crate::compress;
use crate::write_file;
use crate::{CliError, CliResult};

/// One file's outcome, serialised into `report.json`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct FileReport {
    /// The input path, as given.
    pub input: String,
    /// The output path, when the file succeeded.
    pub output: Option<String>,
    /// `ok` or the error that isolated this file.
    pub status: String,
    /// The error detail when `status != "ok"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Input size in bytes.
    pub in_bytes: u64,
    /// Output size in bytes, when produced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_bytes: Option<u64>,
    /// Processing wall time in milliseconds.
    pub millis: u128,
}

/// The batch report written to `<outdir>/report.json`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BatchReport {
    /// The tool applied across the set.
    pub tool: String,
    /// Total files attempted.
    pub total: usize,
    /// Files that succeeded.
    pub ok: usize,
    /// Files that failed (isolated).
    pub failed: usize,
    /// Per-file outcomes, in input order.
    pub files: Vec<FileReport>,
}

/// Run `selis batch compress` over `inputs`, writing results into `outdir`.
///
/// # Errors
///
/// `IO_WRITE_FAILED` when the output directory or report cannot be created.
/// Per-file failures do **not** return an error — they are recorded in the
/// report and the batch continues (SL-1A.TOOL.11).
pub(crate) fn batch_compress(inputs: &[String], outdir: &str) -> CliResult<()> {
    if inputs.is_empty() {
        return Err(CliError("batch needs at least one input file".to_string()));
    }
    std::fs::create_dir_all(outdir)
        .map_err(|e| CliError(format!("cannot create {outdir}: {e}")))?;

    let mut files = Vec::with_capacity(inputs.len());
    for input in inputs {
        files.push(run_one(input, outdir));
    }
    let ok = files.iter().filter(|f| f.status == "ok").count();
    let failed = files.len().saturating_sub(ok);
    let report = BatchReport {
        tool: "compress".to_string(),
        total: files.len(),
        ok,
        failed,
        files,
    };

    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| CliError(format!("cannot serialise report: {e}")))?;
    let report_path = std::path::Path::new(outdir).join("report.json");
    write_file(&report_path.to_string_lossy(), json.as_bytes())?;
    for f in &report.files {
        match &f.error {
            Some(err) => eprintln!("  FAIL {} — {err}", f.input),
            None => eprintln!(
                "  ok   {} ({} -> {} bytes)",
                f.input,
                f.in_bytes,
                f.out_bytes.unwrap_or(0)
            ),
        }
    }
    eprintln!(
        "batch: {} ok, {} failed of {} — report at {outdir}/report.json",
        ok, failed, report.total
    );
    Ok(())
}

/// Process one input file in isolation; a panic-free typed failure is a
/// report entry, not a batch abort.
fn run_one(input: &str, outdir: &str) -> FileReport {
    let started = Instant::now();
    let in_bytes = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    let stem = std::path::Path::new(input)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let out_path = format!(
        "{}",
        std::path::Path::new(outdir)
            .join(format!("{stem}.pdf"))
            .display()
    );

    // Per-file isolation: run the tool and catch any failure.
    let outcome = std::panic::catch_unwind(|| compress::optimise_file(input, &out_path));
    let elapsed = started.elapsed().as_millis();
    match outcome {
        Ok(Ok(report)) => FileReport {
            input: input.to_string(),
            output: Some(out_path),
            status: "ok".to_string(),
            error: None,
            in_bytes,
            out_bytes: Some(u64::try_from(report.out_bytes).unwrap_or(u64::MAX)),
            millis: elapsed,
        },
        Ok(Err(e)) => FileReport {
            input: input.to_string(),
            output: None,
            status: "failed".to_string(),
            error: Some(e.to_string()),
            in_bytes,
            out_bytes: None,
            millis: elapsed,
        },
        Err(_) => FileReport {
            input: input.to_string(),
            output: None,
            status: "failed".to_string(),
            error: Some("internal panic during processing".to_string()),
            in_bytes,
            out_bytes: None,
            millis: elapsed,
        },
    }
}
