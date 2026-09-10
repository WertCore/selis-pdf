#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! `xtask` — all Selis automation, in Rust (ADR-P0002).
//!
//! Subcommands implemented in Phase 0: `check-layers`, `check-codes`, `lint`.
//! Every other subcommand is a stub that fails loudly rather than silently
//! succeeding (SL-0.WS.02 DoD): a stub that returns 0 is a lie, and a CI gate
//! that lies is worse than no gate.

mod bench;
mod checks;
mod codes;
mod conformance;
mod corpus;
mod coverage;
mod fixtures;
mod fuzz;
mod layers;
mod oracle;
mod perf_check;
#[cfg(not(target_arch = "wasm32"))]
mod perf_wasm;
mod png;
mod purity;
mod render_perf;
mod render_set;
mod sbom;
mod size_check;
mod sweep;
mod synthetic;
mod unsafe_check;
mod wild;
mod wild_hygiene;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "Selis build automation",
    version,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the workspace.
    Build,
    /// Run the workspace test suite.
    Test,
    /// Format, lint, and run every mechanical gate.
    Lint,
    /// Enforce the layer model from xtask/layers.toml (SL-0.WS.03).
    CheckLayers,
    /// Enforce ADR-P0005/P0011/P0016: no fs/net/env/clock below L4 (SL-0.WS.04).
    CheckPurity,
    /// Every `unsafe` block has a `// SAFETY:` and sits on the allowlist (SL-0.WS.05).
    CheckUnsafe,
    /// Budget/Malformed-Input doc sections and loop ticks (SL-0.WS.05).
    CheckContracts,
    /// Registry consistency: duplicate ids, changed meanings, unregistered uses (SL-0.ERR.01).
    CheckCodes,
    /// No direct allocation with document-derived lengths outside selis-sandbox (SL-0.WS.05).
    CheckAlloc,
    /// Flags are owned, dated, and not past expiry (03-CONVENTIONS.md §11).
    CheckFlags,
    /// Wild-corpus policy gates: CI excludes wild sources, expectations carry no
    /// document content (06-CORPUS-POLICY.md §7, SL-0.CORP.05).
    CheckWildHygiene,
    /// WASM size budgets + 2% regression rule (SL-0.WS.09).
    SizeCheck {
        /// Record the fresh measurement as the new baseline instead of
        /// comparing against it.
        #[arg(long)]
        update_baseline: bool,
        /// Fail on budgets whose artifact is not measured yet (default:
        /// report loudly but pass).
        #[arg(long)]
        strict: bool,
    },
    /// Coverage floors per crate (SL-0.WS.08).
    Coverage,
    /// Mutation testing scoped to sandbox/edit/redact/sign (SL-0.WS.08).
    Mutate,
    /// Corpus fetch / stats / verify (SL-0.CORP.01-03).
    Corpus(CorpusArgs),
    /// Oracle tools and comparisons (SL-0.ORACLE.01). Local-first, Docker fallback.
    Oracle(OracleArgs),
    /// Fuzzing harness (SL-0.SEC.02).
    Fuzz,
    /// Criterion benchmarks (SL-0.PERF.01).
    Bench(BenchArgs),
    /// Performance budgets vs criterion (SL-0.PERF.02).
    PerfCheck {
        /// Fail on budgets whose harness does not exist yet (default:
        /// report loudly but pass).
        #[arg(long)]
        strict: bool,
    },
    /// Conformance ladder report (SL-0.OPS.02).
    Conformance,
    /// Measure render wall-time per page vs oracles on the benchmark set
    /// (SL-2.PERF.02); writes the record `perf-check` gates the ratio on.
    PerfRender {
        /// Benchmark set directory.
        #[arg(long, default_value = "bench/render-set")]
        set: std::path::PathBuf,
        /// Render resolution in DPI (default 72 = 1 pt per px; see
        /// `render_perf` for why 150 needs the device-matrix fix first).
        #[arg(long, default_value_t = 72)]
        dpi: u32,
        /// Timed repeats per page (median is recorded).
        #[arg(long, default_value_t = 5)]
        repeats: usize,
        /// Results JSON output.
        #[arg(long, default_value = "bench/render-results.json")]
        out: std::path::PathBuf,
        /// Comma-separated oracle ids to time (`mutool`, `pdfium`).
        #[arg(long, default_value = "mutool,pdfium")]
        oracles: String,
        /// Explicit `pdfium_driver` binary path (else PATH lookup).
        #[arg(long)]
        pdfium_driver: Option<std::path::PathBuf>,
        /// Explicit `mutool` binary path (else PATH lookup).
        #[arg(long)]
        mutool: Option<std::path::PathBuf>,
        /// Time the engine only (no oracle processes).
        #[arg(long)]
        skip_oracles: bool,
    },
    /// Compare WASM-vs-native render on the benchmark set via the embedded
    /// wasmtime driver (SL-2.PERF.03). Native-only (wasmtime has no wasm32
    /// target).
    #[cfg(not(target_arch = "wasm32"))]
    PerfWasm {
        /// Benchmark set directory.
        #[arg(long, default_value = "bench/render-set")]
        set: std::path::PathBuf,
        /// Timed repeats per page (median is recorded).
        #[arg(long, default_value_t = 5)]
        repeats: usize,
        /// Results JSON output.
        #[arg(long, default_value = "bench/wasm-results.json")]
        out: std::path::PathBuf,
    },
    /// Generate the deterministic render benchmark set (SL-2.PERF.02).
    RenderSet {
        /// Verify the committed fixtures against their generator instead of
        /// regenerating them.
        #[arg(long)]
        check: bool,
        /// Output directory for the produced PDFs.
        #[arg(long, default_value = "bench/render-set")]
        outdir: std::path::PathBuf,
    },
    /// Generate the tool-suite smoke fixtures and run every tool over them so
    /// the outputs can be oracle-checked (SL-1A.WRITE.05 CI slot).
    Fixtures {
        /// Output directory for the produced tool outputs.
        #[arg(long, default_value = "target/tool-outputs")]
        outdir: std::path::PathBuf,
    },
    /// CycloneDX SBOM (SL-0.WS.07).
    Sbom,
    /// Signing (G6).
    Sign,
    /// Package shells.
    Package,
    /// Release train.
    Release,
    /// Publish the three OSS crates (ADR-P0030).
    PublishOss,
}

#[derive(clap::Args)]
struct BenchArgs {
    /// Record the current results as the baseline.
    #[arg(long)]
    record_baseline: bool,
    /// Compare current results against the recorded baseline; fail on >2% regression.
    #[arg(long)]
    compare_baseline: bool,
}

#[derive(clap::Args)]
struct CorpusArgs {
    #[command(subcommand)]
    sub: CorpusSub,
}

#[derive(clap::Args)]
struct OracleArgs {
    #[command(subcommand)]
    sub: OracleSub,
}

#[derive(Subcommand)]
enum OracleSub {
    /// Render a PDF page to a PNG with the named oracle (local-first, Docker fallback).
    Render {
        #[arg(long)]
        tool: String,
        #[arg(long, default_value = "150")]
        dpi: u32,
        file: std::path::PathBuf,
    },
    /// Report which oracles are available locally and their pinned images.
    Check,
    /// Print the pinned container image ref for a tool (for CI pull/extract).
    ImageRef {
        /// The oracle id (`qpdf`, `mupdf`, `ghostscript`, `pdfium`, `pdfjs`).
        tool: String,
    },
    /// Run `qpdf --check` over one tool output (SL-1A.WRITE.05).
    CheckOutput { file: std::path::PathBuf },
    /// Run `qpdf --check` over every *.pdf in a directory (SL-1A.WRITE.05 CI slot).
    CheckOutputDir { dir: std::path::PathBuf },
    /// Compare `selis inspect --json` against `qpdf --json` (SL-0.ORACLE.03).
    Compare { file: std::path::PathBuf },
    /// Render with selis and an oracle at the same DPI and compare (SL-0.ORACLE.02).
    CompareRender {
        #[arg(long)]
        tool: String,
        #[arg(long, default_value = "150")]
        dpi: u32,
        file: std::path::PathBuf,
    },
    /// Compare text extracted by selis and mutool (SL-0.ORACLE.04).
    CompareText { file: std::path::PathBuf },
    /// Triage: run structural compare over a corpus sample and group disagreements (SL-0.ORACLE.05).
    Triage {
        #[arg(long, default_value = "100")]
        sample: usize,
        /// Record a verdict for a signature: `signature=Verdict` (repeatable).
        /// Verdict is one of OurBug | OracleBug | SpecAmbiguous |
        /// ToleranceTooTight; it is written into each affected file's
        /// expectation record as an `[annotation]` table.
        #[arg(long = "verdict", value_name = "SIGNATURE=VERDICT")]
        verdict: Vec<String>,
        /// Prose context recorded alongside every `--verdict` annotation.
        #[arg(long)]
        note: Option<String>,
        /// Remove stale annotations: expectation records whose `triage`
        /// equals this signature lose their `[annotation]` block (repeatable).
        /// Used when a comparator fix dissolves a cluster.
        #[arg(long = "clear", value_name = "SIGNATURE")]
        clear: Vec<String>,
    },
    /// Full-corpus differential render sweep (SL-2.CONF.01): render page 1 of
    /// every corpus PDF with selis and the named oracles at the given DPIs,
    /// compare with the compare-render machinery, and group outcomes by
    /// disagreement signature. Report + per-file verdicts go to `--out`.
    Sweep {
        /// Oracle tools to compare against (repeatable).
        #[arg(long = "tool", value_name = "TOOL", default_value = "mutool")]
        tool: Vec<String>,
        /// DPIs to sweep (comma-separated).
        #[arg(long, value_delimiter = ',', default_value = "72,150,300")]
        dpi: Vec<u32>,
        /// Deterministic stride sample instead of the whole corpus.
        #[arg(long)]
        sample: Option<usize>,
        /// Output directory for verdicts.jsonl + sweep-report.json.
        #[arg(long, default_value = "target/sweep")]
        out: std::path::PathBuf,
        /// Wall-clock budget per render (seconds).
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
        /// Parallel workers (default: one per CPU).
        #[arg(long)]
        jobs: Option<usize>,
        /// Explicit selis binary (default: the release target dir).
        #[arg(long)]
        selis: Option<std::path::PathBuf>,
        /// Skip (file, tool, dpi) outcomes already in verdicts.jsonl.
        #[arg(long, default_value_t = false)]
        resume: bool,
    },
}

#[derive(Subcommand)]
enum CorpusSub {
    /// Download manifests to the cache, verifying hashes (SL-0.CORP.01).
    Fetch {
        /// Only fetch entries carrying this tag (smoke, render, text, ...).
        #[arg(short, long)]
        tag: Option<String>,
    },
    /// List the configured corpora.
    List,
    /// Report total corpora and tag distribution.
    Stats,
    /// Write open-outcome expectations for every corpus PDF (SL-0.CORP.03).
    ExpectGenerate,
    /// Re-open every corpus PDF and diff against its expectation record.
    Verify,
    /// Generate the synthetic corpus (SL-0.CORP.04).
    SyntheticGenerate,
    /// Gated acquisition of wild corpus samples (SL-0.CORP.05).
    Wild(WildArgs),
}

#[derive(clap::Args)]
struct WildArgs {
    #[command(subcommand)]
    sub: WildSub,
}

#[derive(Subcommand)]
enum WildSub {
    /// Fetch SAFEDOCS zips into the wild cache (policy-gated; see
    /// pdf-plan/06-CORPUS-POLICY.md).
    Fetch {
        /// Number of ~1000-PDF SAFEDOCS zips to fetch.
        #[arg(long, default_value_t = 1)]
        zips: usize,
        /// First zip index (stratify samples across the corpus).
        #[arg(long, default_value_t = 0)]
        zip_start: usize,
        /// Assert you have read pdf-plan/06-CORPUS-POLICY.md.
        #[arg(long)]
        i_have_read_the_policy: bool,
    },
    /// List wild batches and their PDF counts.
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Build => run("cargo", &["build", "--workspace"]),
        Command::Test => run("cargo", &["test", "--workspace"]),
        Command::Lint => lint(),
        Command::CheckLayers => layers::check(),
        Command::CheckCodes => codes::check(),
        Command::CheckPurity => purity::check(),
        Command::CheckUnsafe => unsafe_check::check(),
        Command::CheckContracts => checks::check_contracts(),
        Command::CheckAlloc => checks::check_alloc(),
        Command::CheckFlags => not_in_phase_0("check-flags"),
        Command::CheckWildHygiene => wild_hygiene::check(),
        Command::SizeCheck {
            update_baseline,
            strict,
        } => size_check::run(update_baseline, strict),
        Command::Coverage => coverage::run(),
        Command::Mutate => coverage::mutate(),
        Command::Corpus(args) => match args.sub {
            CorpusSub::Fetch { tag } => corpus::run(corpus::CorpusCommand::Fetch(tag)),
            CorpusSub::List => corpus::run(corpus::CorpusCommand::List),
            CorpusSub::Stats => corpus::run(corpus::CorpusCommand::Stats),
            CorpusSub::ExpectGenerate => corpus::run(corpus::CorpusCommand::ExpectGenerate),
            CorpusSub::Verify => corpus::run(corpus::CorpusCommand::Verify),
            CorpusSub::SyntheticGenerate => synthetic::generate(),
            CorpusSub::Wild(args) => match args.sub {
                WildSub::Fetch {
                    zips,
                    zip_start,
                    i_have_read_the_policy,
                } => wild::run(wild::WildCommand::Fetch {
                    zips,
                    zip_start,
                    ack: i_have_read_the_policy,
                }),
                WildSub::Status => wild::run(wild::WildCommand::Status),
            },
        },
        Command::Oracle(args) => match args.sub {
            OracleSub::Render { tool, dpi, file } => {
                oracle::run(oracle::OracleCommand::Render { tool, dpi, file })
            }
            OracleSub::Check => oracle::run(oracle::OracleCommand::Check),
            OracleSub::ImageRef { tool } => oracle::run(oracle::OracleCommand::ImageRef { tool }),
            OracleSub::CheckOutput { file } => {
                oracle::run(oracle::OracleCommand::CheckOutput { file })
            }
            OracleSub::CheckOutputDir { dir } => {
                oracle::run(oracle::OracleCommand::CheckOutputDir { dir })
            }
            OracleSub::Compare { file } => oracle::run(oracle::OracleCommand::Compare { file }),
            OracleSub::CompareRender { tool, dpi, file } => {
                oracle::run(oracle::OracleCommand::CompareRender { tool, dpi, file })
            }
            OracleSub::CompareText { file } => {
                oracle::run(oracle::OracleCommand::CompareText { file })
            }
            OracleSub::Triage {
                sample,
                verdict,
                note,
                clear,
            } => parse_verdicts(&verdict).and_then(|verdicts| {
                oracle::run(oracle::OracleCommand::Triage {
                    sample,
                    verdicts,
                    note,
                    clear,
                })
            }),
            OracleSub::Sweep {
                tool,
                dpi,
                sample,
                out,
                timeout_secs,
                jobs,
                selis,
                resume,
            } => sweep::run(sweep::SweepConfig {
                tools: tool,
                dpis: dpi,
                sample,
                out,
                timeout: std::time::Duration::from_secs(timeout_secs),
                jobs: jobs
                    .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get())),
                selis,
                resume,
            }),
        },
        Command::Fuzz => fuzz::check(),
        Command::Bench(args) => bench::run(args.record_baseline, args.compare_baseline),
        Command::PerfCheck { strict } => perf_check::run(strict),
        Command::Conformance => conformance::report(),
        Command::RenderSet { check, outdir } => {
            if check {
                render_set::check(&outdir)
            } else {
                render_set::generate(&outdir)
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        Command::PerfWasm { set, repeats, out } => perf_wasm::run(&set, repeats, &out),
        Command::PerfRender {
            set,
            dpi,
            repeats,
            out,
            oracles,
            pdfium_driver,
            mutool,
            skip_oracles,
        } => {
            let tools = oracles
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            render_perf::run(
                &set,
                dpi,
                repeats,
                &out,
                &render_perf::OracleTools {
                    tools,
                    pdfium_driver,
                    mutool,
                },
                skip_oracles,
            )
        }
        Command::Fixtures { outdir } => fixtures::run(&outdir),
        Command::Sbom => sbom::sbom(),
        Command::Sign => not_in_phase_0("sign"),
        Command::Package => not_in_phase_0("package"),
        Command::Release => not_in_phase_0("release"),
        Command::PublishOss => not_in_phase_0("publish-oss"),
    };
    match result {
        Ok(()) => {
            println!("xtask: ok");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("xtask: error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn lint() -> Result<(), String> {
    run("cargo", &["fmt", "--all", "--", "--check"])?;
    run("cargo", &["clippy", "--workspace", "--all-targets"])?;
    layers::check()?;
    codes::check()?;
    purity::check()?;
    unsafe_check::check()?;
    checks::check_contracts()?;
    wild_hygiene::check()?;
    // SL-0.WS.07 — supply-chain gates.
    run("cargo", &["deny", "check"])?;
    run("cargo", &["vet"])?;
    sbom::sbom()?;
    Ok(())
}

/// Parse `--verdict signature=Verdict` pairs. The verdict value set is
/// enforced by the triage step; this only rejects malformed pairs early.
/// The pair separator is the first `=`, so signatures containing `=`
/// (`obj_delta=1`) must quote the whole argument and split at the LAST `=`.
fn parse_verdicts(raw: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for v in raw {
        let (sig, verdict) = v
            .rsplit_once('=')
            .ok_or_else(|| format!("--verdict `{v}` must be `signature=Verdict`"))?;
        let sig = sig.trim();
        let verdict = verdict.trim();
        if sig.is_empty() || verdict.is_empty() {
            return Err(format!("--verdict `{v}` must be `signature=Verdict`"));
        }
        out.push((sig.to_string(), verdict.to_string()));
    }
    Ok(out)
}

/// Run a subprocess, streaming its output, and fail with a typed message on a
/// non-zero exit.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .map_err(|e| format!("failed to spawn {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} exited with {status}", args.join(" ")))
    }
}

/// SL-0.WS.02 DoD: an unimplemented subcommand is an explicit error, never a
/// silent success.
fn not_in_phase_0(name: &str) -> Result<(), String> {
    Err(format!("`{name}` is not implemented in phase 0"))
}
