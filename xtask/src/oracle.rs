//! Oracle tools for differential testing (SL-0.ORACLE.01).
//!
//! The oracles — qpdf, Ghostscript, MuPDF, PDFium, pdf.js — are third-party
//! binaries that are NEVER linked into the build (ADR-P0009). They are used
//! purely as reference producers: `xtask oracle render` rasterises a PDF with
//! an oracle so our output can be compared against it.
//!
//! Execution is **local-first, Docker-fallback**: if the tool is installed on
//! the host (qpdf on PATH, `gs`, `mutool`), it runs natively — faster and
//! simplest for local dev. Otherwise the pinned container image is pulled and
//! run. Docker is recommended for CI because the image digest pins the exact
//! tool version on every runner; local installs must be recorded via
//! `xtask oracle check` so version drift is visible.

use std::path::{Path, PathBuf};
use std::process::Command;

const ORACLES_TOML: &str = "xtask/oracles.toml";

/// The render-capable oracles and the local binary each maps to. `qpdf` is a
/// structural oracle (SL-0.ORACLE.03), not a rasterizer, so it is absent here.
const TOOL_BINARY: &[(&str, &str)] = &[
    ("ghostscript", "gs"),
    ("mupdf", "mutool"),
    ("mutool", "mutool"),
    ("pdfium", "pdfium"),
];

pub enum OracleCommand {
    /// Render `file` to a PNG per DPI with the named tool.
    Render { tool: String, dpi: u32, file: PathBuf },
    /// Report which oracles are available locally (and record their versions).
    Check,
}

pub fn run(cmd: OracleCommand) -> Result<(), String> {
    match cmd {
        OracleCommand::Render { tool, dpi, file } => render(&tool, dpi, &file),
        OracleCommand::Check => check(),
    }
}

/// Render a PDF page to a PNG. Uses the local binary when present, else the
/// pinned container.
fn render(tool: &str, dpi: u32, file: &Path) -> Result<(), String> {
    if !file.exists() {
        return Err(format!("{}: no such file", file.display()));
    }
    let out_dir = std::env::temp_dir().join("selis-oracle-render");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{out_dir:?}: {e}"))?;

    let binary = TOOL_BINARY
        .iter()
        .find(|(id, _)| *id == tool)
        .map(|(_, b)| *b)
        .ok_or_else(|| format!("unknown oracle tool `{tool}`"))?;

    if let Some(local) = find_local(binary) {
        return render_local(tool, &local, dpi, file, &out_dir);
    }
    render_container(tool, binary, dpi, file, &out_dir)
}

/// The path to a local oracle binary, if installed on PATH.
fn find_local(binary: &str) -> Option<PathBuf> {
    // On Windows the probe must look for `<name>.exe`.
    let probe = if std::env::consts::OS == "windows" {
        format!("{binary}.exe")
    } else {
        binary.to_string()
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&probe))
        .find(|p| p.is_file())
}

/// Rasterise with the native binary.
fn render_local(tool: &str, binary: &Path, dpi: u32, file: &Path, out_dir: &Path) -> Result<(), String> {
    let out = out_dir.join(format!("{tool}-{dpi}.png"));
    let status = match tool {
        "ghostscript" => Command::new(binary)
            .arg("-sDEVICE=png16m")
            .arg("-r")
            .arg(dpi.to_string())
            .arg("-o")
            .arg(&out)
            .arg(file)
            .status(),
        "mupdf" | "mutool" => Command::new(binary)
            .arg("draw")
            .arg("-r")
            .arg(dpi.to_string())
            .arg("-o")
            .arg(&out)
            .arg(file)
            .status(),
        "pdfium" => {
            return Err(format!(
                "{tool}: pdfium needs the pdfium-render driver; not yet wired (TODO)"
            ));
        }
        _ => unreachable!(),
    }
    .map_err(|e| format!("cannot run {tool} ({binary:?}): {e}"))?;
    if status.success() && out.exists() {
        println!("{}: rendered {} -> {}", tool, file.display(), out.display());
        Ok(())
    } else {
        Err(format!("{tool}: render failed (exit {status})"))
    }
}

/// Rasterise with the pinned container image (see `xtask/oracles.toml`).
fn render_container(tool: &str, binary: &str, dpi: u32, file: &Path, out_dir: &Path) -> Result<(), String> {
    if find_local("docker").is_none() {
        return Err(format!(
            "{tool}: not installed locally and Docker is not available. \
             Install the tool (e.g. `choco install qpdf ghostscript mupdf`) or Docker."
        ));
    }
    let image = load_image(tool)?;
    let out = out_dir.join(format!("{tool}-{dpi}.png"));
    let status = Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("-v")
        .arg(format!(
            "{}:/in.pdf:ro",
            file.canonicalize().map_err(|e| e.to_string())?.display()
        ))
        .arg("-v")
        .arg(format!("{}:/out.png", out.display()))
        .arg(image)
        .arg(binary)
        .arg("--dpi")
        .arg(dpi.to_string())
        .arg("/in.pdf")
        .arg("/out.png")
        .status()
        .map_err(|e| format!("cannot run docker: {e}"))?;
    if status.success() && out.exists() {
        println!("{}: rendered {} -> {}", tool, file.display(), out.display());
        Ok(())
    } else {
        Err(format!("{tool}: container render failed (exit {status})"))
    }
}

/// The container image for a tool from `xtask/oracles.toml` (pinned digest).
fn load_image(tool: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(ORACLES_TOML)
        .map_err(|e| format!("cannot read {ORACLES_TOML}: {e}"))?;
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.trim().strip_prefix(&format!("{tool} =")) {
            let img = rest.trim().trim_matches('"').to_string();
            if img.is_empty() {
                return Err(format!("{tool}: no image recorded in {ORACLES_TOML}"));
            }
            return Ok(img);
        }
    }
    Err(format!("{tool}: no image recorded in {ORACLES_TOML}"))
}

/// Report which oracles are available locally and the pinned container images.
fn check() -> Result<(), String> {
    let text = std::fs::read_to_string(ORACLES_TOML)
        .map_err(|e| format!("cannot read {ORACLES_TOML}: {e}"))?;
    println!("oracle check (local-first; Docker only for CI pinning):");
    for (id, binary) in TOOL_BINARY {
        let local = find_local(binary)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "NOT installed".to_string());
        let image = text
            .lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{id} =")))
            .map(|s| s.trim().trim_matches('"').to_string())
            .unwrap_or_else(|| "unset".to_string());
        println!("  {id:12} local: {local:40} image: {image}");
    }
    Ok(())
}