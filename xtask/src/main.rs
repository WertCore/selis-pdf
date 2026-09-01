#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! `xtask` — all Selis automation, in Rust (ADR-P0002).
//!
//! Subcommands implemented in Phase 0: `check-layers`, `check-codes`, `lint`.
//! Every other subcommand is a stub that fails loudly rather than silently
//! succeeding (SL-0.WS.02 DoD): a stub that returns 0 is a lie, and a CI gate
//! that lies is worse than no gate.

mod checks;
mod codes;
mod conformance;
mod corpus;
mod fuzz;
mod layers;
mod oracle;
mod synthetic;
mod purity;
mod sbom;
mod unsafe_check;

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
    /// WASM size budgets (SL-0.WS.09).
    SizeCheck,
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
    Compare {
        file: std::path::PathBuf,
    },
    /// Render with selis and an oracle at the same DPI and compare (SL-0.ORACLE.02).
    CompareRender {
        #[arg(long)]
        tool: String,
        #[arg(long, default_value = "150")]
        dpi: u32,
        file: std::path::PathBuf,
    },
    /// Compare text extracted by selis and mutool (SL-0.ORACLE.04).
    CompareText {
        file: std::path::PathBuf,
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
        Command::SizeCheck => not_in_phase_0("size-check"),
        Command::Corpus(args) => match args.sub {
            CorpusSub::Fetch { tag } => corpus::run(corpus::CorpusCommand::Fetch(tag)),
            CorpusSub::List => corpus::run(corpus::CorpusCommand::List),
            CorpusSub::Stats => corpus::run(corpus::CorpusCommand::Stats),
            CorpusSub::ExpectGenerate => corpus::run(corpus::CorpusCommand::ExpectGenerate),
            CorpusSub::Verify => corpus::run(corpus::CorpusCommand::Verify),
            CorpusSub::SyntheticGenerate => synthetic::generate(),
        },
        Command::Oracle(args) => match args.sub {
            OracleSub::Render { tool, dpi, file } => oracle::run(oracle::OracleCommand::Render { tool, dpi, file }),
            OracleSub::Check => oracle::run(oracle::OracleCommand::Check),
            OracleSub::Compare { file } => oracle::run(oracle::OracleCommand::Compare { file }),
            OracleSub::CompareRender { tool, dpi, file } => oracle::run(oracle::OracleCommand::CompareRender { tool, dpi, file }),
            OracleSub::CompareText { file } => oracle::run(oracle::OracleCommand::CompareText { file }),
        },
        Command::Fuzz => fuzz::check(),
        Command::Bench => not_in_phase_0("bench"),
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
