//! The `selis` command-line interface (SL-1.COS.10, SL-1.COS.11).
//!
//! Phase 1 ships the structural inspector the qpdf oracle compares against:
//! `selis inspect --json <file>` dumps revisions, xref entries, and
//! deviations. Everything else comes with the phases that own it.

use clap::{Parser, Subcommand};
use selis_error::{Code, Result};

mod inspect;
mod render;

#[derive(Parser)]
#[command(
    name = "selis",
    about = "The Selis PDF engine",
    version,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Structural dump of a PDF (revisions, xref, deviations).
    Inspect {
        /// The PDF file to inspect.
        path: String,
        /// Emit JSON (the oracle format; the default).
        #[arg(long)]
        json: bool,
    },
    /// Render a page to a PPM image.
    Render {
        /// The PDF file to render.
        path: String,
        /// The page number (0-based; default 0).
        #[arg(long, default_value_t = 0)]
        page: usize,
        /// The output PPM file.
        output: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Inspect { path, json } => inspect::run(&path, json),
        Command::Render { path, page, output } => render::run(&path, page, &output),
    };
    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("selis: error: {e}");
            std::process::exit(1);
        }
    }
}

/// A readable error for the CLI.
#[derive(Debug)]
struct CliError(String);

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CliError {}

impl From<selis_error::Error> for CliError {
    fn from(e: selis_error::Error) -> Self {
        CliError(e.to_string())
    }
}

pub(crate) type CliResult<T> = std::result::Result<T, CliError>;

/// The structural JSON emitted by `inspect --json` (SL-1.COS.10).
///
/// This is the shape the qpdf oracle (SL-0.ORACLE.03) compares against: the
/// object model is what matters, not exact byte layout.
#[derive(serde::Serialize)]
struct InspectDoc {
    /// The PDF version header, when present.
    version: Option<String>,
    /// Each revision, oldest first.
    revisions: Vec<InspectRevision>,
    /// Deviations the parser tolerated.
    deviations: Vec<InspectDeviation>,
    /// The trailer /Root reference, when found.
    root: Option<String>,
    /// The trailer /Size, when found.
    size: Option<u64>,
    /// Whether the xref needed reconstruction.
    reconstructed: bool,
}

#[derive(serde::Serialize)]
struct InspectRevision {
    /// Byte range this revision occupies.
    byte_range: [u64; 2],
    /// Number of xref entries declared by this revision.
    entries: usize,
    /// The object numbers declared by this revision, sorted.
    objects: Vec<u32>,
}

#[derive(serde::Serialize)]
struct InspectDeviation {
    /// The deviation's short name (e.g. `double-sign`).
    name: &'static str,
    /// The byte offset it was recorded at.
    offset: u64,
}

/// Serialise a `Deviation` for `inspect --json`.
fn deviation_json(d: &selis_pdf_cos::Deviation) -> InspectDeviation {
    InspectDeviation {
        name: d.name(),
        offset: d.offset(),
    }
}

/// Format a `Ref` as `"N G"`.
fn ref_str(r: &selis_pdf_cos::Ref) -> String {
    format!("{} {}", r.num, r.gen)
}

/// Read a file's bytes for inspection.
///
/// # Errors
///
/// `IO_READ_FAILED` when the file cannot be read.
pub(crate) fn read_file(path: &str) -> CliResult<Vec<u8>> {
    std::fs::read(path)
        .map_err(|e| {
            let mut ctx = selis_error::Ctx::new();
            ctx.detail = Some(format!("cannot read {path}: {e}"));
            selis_error::Error::with(Code::IoReadFailed, ctx)
        })
        .map_err(CliError::from)
}

fn parse_budget() -> selis_sandbox::Budget {
    selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer)
}

fn err_unimplemented(what: &str) -> CliError {
    CliError(format!("{what}: not implemented in phase 1"))
}

/// The engine entry point `inspect` uses (placeholder until SL-1.DOC.01).
#[allow(dead_code)]
fn _engine_placeholder() -> CliResult<()> {
    Err(err_unimplemented("engine"))
}

/// A marker so `Result`/`Result` are used in the crate prelude.
fn _assert_result(_: Result<()>) {}
