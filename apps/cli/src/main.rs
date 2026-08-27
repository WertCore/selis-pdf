//! The `selis` command-line interface (SL-1.COS.10, SL-1.COS.11).
//!
//! Phase 1 ships the structural inspector the qpdf oracle compares against:
//! `selis inspect --json <file>` dumps revisions, xref entries, and
//! deviations. Everything else comes with the phases that own it.

#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

use std::collections::BTreeMap;

use clap::{Parser, Subcommand};
use selis_error::{Code, Result};

mod batch;
mod check;
mod compress;
mod convert;
mod extract;
mod img2pdf;
mod inspect;
mod render;
mod search;
mod tools;
mod topdf;

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
    /// Extract a page's content (text|json|md|html|image|embedded).
    Extract {
        /// The PDF file to extract from.
        path: String,
        /// The first page (0-based; default 0).
        #[arg(long, default_value_t = 0)]
        page: usize,
        /// The last page (0-based; default: the first page).
        #[arg(long)]
        last: Option<usize>,
        /// The output format: text|json|md|html|image|embedded.
        #[arg(long, default_value = "text")]
        format: String,
        /// The output directory (for format=embedded, writes the files).
        #[arg(long)]
        output: Option<String>,
    },
    /// Convert a page range to PPM images in an output directory.
    Convert {
        /// The PDF file to convert.
        path: String,
        /// The output directory.
        output: String,
        /// The first page (0-based; default 0).
        #[arg(long, default_value_t = 0)]
        first: usize,
        /// The last page (0-based; default: the last page).
        #[arg(long)]
        last: Option<usize>,
    },
    /// Search a page's text for a query.
    Search {
        /// The PDF file to search.
        path: String,
        /// The query string.
        query: String,
        /// The page number (0-based; default 0).
        #[arg(long, default_value_t = 0)]
        page: usize,
    },
    /// Evaluate the conformance rules for a PDF.
    Check {
        /// The PDF file to check.
        path: String,
        /// The conformance profile: readable|ua.
        #[arg(long, default_value = "readable")]
        profile: String,
    },
    /// Merge several PDFs into one.
    Merge {
        /// The input PDFs.
        #[arg(required = true)]
        inputs: Vec<String>,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
    },
    /// Extract a page range into a new PDF.
    Split {
        /// The input PDF.
        path: String,
        /// The first page (0-based).
        #[arg(long, default_value_t = 0)]
        first: usize,
        /// The last page (0-based; default: the last page).
        #[arg(long)]
        last: Option<usize>,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
    },
    /// Set metadata fields and rewrite a PDF.
    SetMetadata {
        /// The input PDF.
        path: String,
        /// A `Key=Value` metadata field (repeatable).
        #[arg(long = "field")]
        fields: Vec<String>,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
    },
    /// Redact regions of a PDF (cover + strip text).
    Redact {
        /// The input PDF.
        path: String,
        /// A `x,y,w,h` rectangle (repeatable).
        #[arg(long = "rect", required = true)]
        rects: Vec<String>,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
    },
    /// Rotate pages of a PDF (adds to any existing /Rotate).
    Rotate {
        /// The input PDF.
        path: String,
        /// The rotation angle: 90, 180 or 270 (clockwise degrees).
        #[arg(short, long)]
        angle: i64,
        /// The pages to rotate (`0,2,5-7`; 0-based); default: all pages.
        #[arg(long)]
        pages: Option<String>,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
    },
    /// Delete pages from a PDF.
    Delete {
        /// The input PDF.
        path: String,
        /// The pages to delete (`0,2,5-7`; 0-based).
        #[arg(long, required = true)]
        pages: String,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
    },
    /// Reorder the pages of a PDF.
    Reorder {
        /// The input PDF.
        path: String,
        /// The new page order, every page exactly once (`2,0,1` or `3-5,0`).
        #[arg(long, required = true)]
        order: String,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
    },
    /// Convert images (JPEG/PNG) to a PDF, one page per image.
    #[command(name = "img2pdf")]
    Img2Pdf {
        /// The input images.
        #[arg(required = true)]
        inputs: Vec<String>,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// The page size: fit (page = image + margin), letter or a4.
        #[arg(long, default_value = "fit")]
        page_size: String,
        /// The page margin in points (default 36).
        #[arg(long, default_value_t = 36.0)]
        margin: f64,
    },
    /// Convert a Markdown or HTML file to PDF (documented subset).
    Topdf {
        /// The input file (.md/.markdown or .html/.htm).
        input: String,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
        /// The input format: auto, md or html.
        #[arg(long, default_value = "auto")]
        format: String,
        /// The page size: letter or a4.
        #[arg(long, default_value = "letter")]
        page_size: String,
        /// The document title recorded in the /Info dictionary.
        #[arg(long)]
        title: Option<String>,
    },
    /// Compress / optimise a PDF (lossless: no pixel changes).
    Compress {
        /// The input PDF.
        path: String,
        /// The output PDF.
        #[arg(short, long)]
        output: String,
    },
    /// Run a tool over many files with per-file isolation + a JSON report.
    Batch {
        /// The tool to run across the set: `compress`.
        tool: String,
        /// The input files.
        #[arg(required = true)]
        inputs: Vec<String>,
        /// The output directory (results + report.json).
        #[arg(long)]
        outdir: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Inspect { path, json } => inspect::run(&path, json),
        Command::Render { path, page, output } => render::run(&path, page, &output),
        Command::Extract {
            path,
            page,
            last,
            format,
            output,
        } => extract::run(&path, page, last, &format, output.as_deref()),
        Command::Convert {
            path,
            output,
            first,
            last,
        } => convert::run(&path, &output, first, last),
        Command::Search { path, query, page } => search::run(&path, &query, page),
        Command::Check { path, profile } => check::run(&path, &profile),
        Command::Merge { inputs, output } => tools::merge(&inputs, &output),
        Command::Split {
            path,
            first,
            last,
            output,
        } => tools::split(&path, first, last.unwrap_or(usize::MAX), &output),
        Command::SetMetadata {
            path,
            fields,
            output,
        } => {
            let parsed: Vec<(&str, &str)> =
                fields.iter().filter_map(|f| f.split_once('=')).collect();
            tools::set_metadata(&path, &parsed, &output)
        }
        Command::Redact {
            path,
            rects,
            output,
        } => {
            let parsed: Vec<(f64, f64, f64, f64)> = rects
                .iter()
                .filter_map(|r| {
                    let parts: Vec<&str> = r.split(',').collect();
                    if parts.len() == 4 {
                        Some((
                            parts.get(0)?.trim().parse().unwrap_or(0.0),
                            parts.get(1)?.trim().parse().unwrap_or(0.0),
                            parts.get(2)?.trim().parse().unwrap_or(0.0),
                            parts.get(3)?.trim().parse().unwrap_or(0.0),
                        ))
                    } else {
                        None
                    }
                })
                .collect();
            tools::redact(&path, &parsed, &output)
        }
        Command::Rotate {
            path,
            angle,
            pages,
            output,
        } => tools::rotate(&path, angle, pages.as_deref(), &output),
        Command::Delete {
            path,
            pages,
            output,
        } => tools::delete(&path, &pages, &output),
        Command::Reorder {
            path,
            order,
            output,
        } => tools::reorder(&path, &order, &output),
        Command::Img2Pdf {
            inputs,
            output,
            page_size,
            margin,
        } => match img2pdf::PageFit::parse(&page_size) {
            Ok(fit) => img2pdf::img2pdf(&inputs, &output, &fit, margin),
            Err(e) => Err(e),
        },
        Command::Topdf {
            input,
            output,
            format,
            page_size,
            title,
        } => topdf::topdf(&input, &output, &format, &page_size, title.as_deref()),
        Command::Compress { path, output } => compress::run(&path, &output),
        Command::Batch {
            tool,
            inputs,
            outdir,
        } => match tool.as_str() {
            "compress" => batch::batch_compress(&inputs, &outdir),
            other => Err(CliError(format!(
                "unknown batch tool `{other}` (supported: compress)"
            ))),
        },
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
    /// Embedded-file inventory (name, size, key).
    attachments: Vec<InspectAttachment>,
    /// The tagged structure tree, when present.
    structure: Option<InspectStructure>,
    /// Document metadata (Info dictionary and XMP fields).
    metadata: Option<BTreeMap<String, String>>,
}

#[derive(serde::Serialize)]
struct InspectAttachment {
    /// The display name.
    name: String,
    /// The declared size in bytes.
    size: i64,
    /// The name-tree key.
    key: String,
}

#[derive(serde::Serialize)]
struct InspectStructure {
    /// The number of structure elements.
    elements: usize,
    /// The structure types (e.g. P, H1, Table), in walk order.
    types: Vec<String>,
    /// The marked-content ids in reading order.
    mcid_order: Vec<u32>,
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

/// Write `bytes` to `path`.
///
/// # Errors
///
/// A CLI error with the I/O detail when the file cannot be written.
pub(crate) fn write_file(path: &str, bytes: &[u8]) -> CliResult<()> {
    std::fs::write(path, bytes).map_err(|e| CliError(format!("cannot write {path}: {e}")))
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
