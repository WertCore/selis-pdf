//! The `selis batch` tool (SL-1A.TOOL.11): run a tool across a file set with
//! per-file isolation and a machine-readable report.
//!
//! One bad document must never kill the batch: every file is processed
//! independently, failures are collected, and `report.json` in the output
//! directory records the outcome, sizes, and error message per file. v1
//! supports `compress` and `protect`; the harness extends to the other tools.

use std::time::Instant;

use crate::compress;
use crate::failure::{self, Failure};
use crate::protect::PasswordOptions;
use crate::verify_report::Verification;
use crate::write_file;
use crate::{CliError, CliResult};

/// Which budget tripped, with the measured usage — the batch report carries
/// the same honest fields the single-file CLI prints (SL-1A.UI.06).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct BudgetExceeded {
    /// `alloc` | `objects` | `depth` | `wall` | `pixels`.
    pub resource: &'static str,
    /// The budget's limit, when the error detail carried it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// The running total that would have been needed, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested: Option<u64>,
    /// What to do about it (the same remedy text the CLI prints).
    pub remedy: &'static str,
}

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
    /// The budget that tripped, when the failure was budget exhaustion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetExceeded>,
    /// The verification display (SL-1A.UI.02), when the file succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
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

/// The per-tool options a batch run carries alongside the input list.
#[derive(Debug, Clone, Default)]
pub(crate) struct BatchOptions {
    /// Protect: the user password (empty = none; the owner password
    /// defaults to it per the single-file tool's rule).
    pub user_password: Option<String>,
    /// Protect: the owner password.
    pub owner_password: Option<String>,
    /// Protect: the permission grant spec (`None` = grant all four exposed).
    pub permissions: Option<String>,
}

/// Run `selis batch compress` over `inputs`, writing results into `outdir`.
///
/// # Errors
///
/// `IO_WRITE_FAILED` when the output directory or report cannot be created.
/// Per-file failures do **not** return an error — they are recorded in the
/// report and the batch continues (SL-1A.TOOL.11).
pub(crate) fn batch_compress(inputs: &[String], outdir: &str) -> CliResult<()> {
    run_batch("compress", inputs, outdir, &BatchOptions::default())
}

/// Run `selis batch protect` over `inputs`, encrypting each file with the
/// shared credentials and writing results into `outdir`.
///
/// A per-file failure — an already-encrypted input (typed
/// `ALREADY_ENCRYPTED`), a damaged document, an I/O error — is a report
/// entry, never a batch abort (SL-1A.TOOL.11).
///
/// # Errors
///
/// `IO_WRITE_FAILED` when the output directory or report cannot be created.
/// A missing/empty credential set is refused up front (the whole batch would
/// fail identically per file, so reporting it once is honest); everything
/// else is per-file.
pub(crate) fn batch_protect(
    inputs: &[String],
    outdir: &str,
    passwords: &PasswordOptions,
    permissions: Option<&str>,
) -> CliResult<()> {
    let options = BatchOptions {
        user_password: passwords.user.clone(),
        owner_password: passwords.owner.clone(),
        permissions: permissions.map(String::from),
    };
    run_batch("protect", inputs, outdir, &options)
}

/// The shared batch harness: run one tool per input file in isolation,
/// collect `FileReport`s, and write `report.json`.
///
/// # Errors
///
/// `IO_WRITE_FAILED` when the output directory or report cannot be created.
/// Per-file failures do not propagate.
fn run_batch(
    tool: &'static str,
    inputs: &[String],
    outdir: &str,
    options: &BatchOptions,
) -> CliResult<()> {
    if inputs.is_empty() {
        return Err(CliError("batch needs at least one input file".to_string()));
    }
    if tool == "protect" {
        // Refuse an empty credential set once, up front: every file would
        // fail identically, and the report would be noise.
        let (user, owner) = protect_passwords(options);
        if user.is_empty() && owner.is_empty() {
            return Err(CliError(
                "batch protect needs credentials: --user-password / --owner-password \
                 or SELIS_USER_PASSWORD / SELIS_OWNER_PASSWORD"
                    .to_string(),
            ));
        }
    }
    std::fs::create_dir_all(outdir)
        .map_err(|e| CliError(format!("cannot create {outdir}: {e}")))?;

    let mut files = Vec::with_capacity(inputs.len());
    for input in inputs {
        files.push(run_one(tool, input, outdir, options));
    }
    let ok = files.iter().filter(|f| f.status == "ok").count();
    let failed = files.len().saturating_sub(ok);
    let report = BatchReport {
        tool: tool.to_string(),
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
            Some(err) => {
                // SL-1A.UI.06: budget failures get the honest message, not a
                // bare error line.
                let failure = failure::classify(err);
                if matches!(failure, Failure::Budget { .. }) {
                    eprintln!("{}", failure::message(err, &failure));
                } else {
                    eprintln!("  FAIL {} — {err}", f.input);
                }
            }
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

/// The resolved protect credential pair for a batch run (argument, else
/// environment; the owner defaults to the user, mirroring the single-file
/// tool). Never logged.
fn protect_passwords(options: &BatchOptions) -> (String, String) {
    let user = options
        .user_password
        .clone()
        .or_else(|| std::env::var("SELIS_USER_PASSWORD").ok())
        .unwrap_or_default();
    let owner = options
        .owner_password
        .clone()
        .or_else(|| std::env::var("SELIS_OWNER_PASSWORD").ok())
        .unwrap_or_else(|| user.clone());
    (user, owner)
}

/// Process one input file in isolation; a panic-free typed failure is a
/// report entry, not a batch abort.
fn run_one(tool: &str, input: &str, outdir: &str, options: &BatchOptions) -> FileReport {
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

    // Per-file isolation: run the tool and catch any failure. SL-0.ERR.03: the
    // panic trampoline converts a bug into the typed INTERNAL_PANIC error —
    // code path + panic site, no document bytes (ADR-P0017) — instead of the
    // untyped "internal panic" string a bare catch_unwind produced.
    let op = tool.to_string();
    let input_owned = input.to_string();
    let out_owned = out_path.clone();
    let options_owned = options.clone();
    let outcome: Result<(u64, Verification), CliError> =
        selis_sandbox::trampoline::catch("cli-batch-file", move || match op.as_str() {
            "compress" => {
                compress::optimise_file(&input_owned, &out_owned).map(|(report, verification)| {
                    (
                        u64::try_from(report.out_bytes).unwrap_or(u64::MAX),
                        verification,
                    )
                })
            }
            "protect" => {
                let passwords = PasswordOptions {
                    user: options_owned.user_password.clone(),
                    owner: options_owned.owner_password.clone(),
                };
                crate::protect::protect_file(
                    &input_owned,
                    &out_owned,
                    &passwords,
                    options_owned.permissions.as_deref(),
                    true,
                )
                .map(|verification| {
                    let out_bytes = std::fs::metadata(&out_owned).map(|m| m.len()).unwrap_or(0);
                    (out_bytes, verification)
                })
            }
            other => Err(CliError(format!("unknown batch tool `{other}`"))),
        });
    let elapsed = started.elapsed().as_millis();
    match outcome {
        Ok((out_bytes, verification)) => FileReport {
            input: input.to_string(),
            output: Some(out_path),
            status: "ok".to_string(),
            error: None,
            budget: None,
            verification: Some(verification),
            in_bytes,
            out_bytes: Some(out_bytes),
            millis: elapsed,
        },
        Err(e) => {
            // SL-1A.UI.06: a budget exhaustion carries its measured usage in
            // the report, the same fields the single-file CLI prints.
            let budget = match failure::classify(&e.to_string()) {
                Failure::Budget {
                    resource,
                    limit,
                    requested,
                } => Some(BudgetExceeded {
                    resource,
                    limit,
                    requested,
                    remedy: failure::remedy(resource),
                }),
                _ => None,
            };
            FileReport {
                input: input.to_string(),
                output: None,
                status: "failed".to_string(),
                error: Some(e.to_string()),
                budget,
                verification: None,
                in_bytes,
                out_bytes: None,
                millis: elapsed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A budget exhaustion serialises into the report with the same fields
    /// the single-file CLI prints: which budget, the measured usage, and the
    /// remedy (SL-1A.UI.06).
    #[test]
    fn budget_exceeded_serialises_with_usage_and_remedy() {
        let raw = "big.pdf: [E4002] This document nests content too deeply. \
                   (during cos-parse) (detail depth limit=64 requested=65)";
        let budget = match failure::classify(raw) {
            Failure::Budget {
                resource,
                limit,
                requested,
            } => BudgetExceeded {
                resource,
                limit,
                requested,
                remedy: failure::remedy(resource),
            },
            other => panic!("expected budget, got {other:?}"),
        };
        let json = serde_json::to_value(&budget).unwrap();
        assert_eq!(json["resource"], "depth");
        assert_eq!(json["limit"], 64);
        assert_eq!(json["requested"], 65);
        assert!(
            json["remedy"]
                .as_str()
                .unwrap()
                .contains("split it into smaller files"),
            "{json}"
        );
    }

    /// A non-budget failure carries no budget object at all.
    #[test]
    fn non_budget_failures_carry_no_budget_object() {
        let report = FileReport {
            input: "a.pdf".into(),
            output: None,
            status: "failed".into(),
            error: Some("a.pdf: cannot open: [E1000] broken".into()),
            budget: None,
            verification: None,
            in_bytes: 10,
            out_bytes: None,
            millis: 1,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("budget").is_none(), "{json}");
        assert!(json.get("verification").is_none(), "{json}");
    }
}
