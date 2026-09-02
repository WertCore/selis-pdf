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
mod fuzz;
mod layers;
mod oracle;
mod purity;
mod sbom;
mod size_check;
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
    /// WASM size budgets (SL-0.WS.09).
    SizeCheck,
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
    Bench,
    /// Conformance ladder report (SL-0.OPS.02).
    Conformance,
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
        Command::SizeCheck => size_check::run(),
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
            OracleSub::Compare { file } => oracle::run(oracle::OracleCommand::Compare { file }),
            OracleSub::CompareRender { tool, dpi, file } => {
                oracle::run(oracle::OracleCommand::CompareRender { tool, dpi, file })
            }
            OracleSub::CompareText { file } => {
                oracle::run(oracle::OracleCommand::CompareText { file })
            }
            OracleSub::Triage { sample } => oracle::run(oracle::OracleCommand::Triage { sample }),
        },
        Command::Fuzz => fuzz::check(),
        Command::Bench => bench::run(),
        Command::Conformance => conformance::report(),
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
