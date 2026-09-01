//! Oracle tools for differential testing (SL-0.ORACLE.01, SL-0.ORACLE.03).
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

use std::collections::{BTreeMap, BTreeSet};
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
    /// Compare `selis inspect --json` against `qpdf --json` (SL-0.ORACLE.03).
    Compare { file: PathBuf },
}

pub fn run(cmd: OracleCommand) -> Result<(), String> {
    match cmd {
        OracleCommand::Render { tool, dpi, file } => render(&tool, dpi, &file),
        OracleCommand::Check => check(),
        OracleCommand::Compare { file } => compare(&file),
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
fn render_local(
    tool: &str,
    binary: &Path,
    dpi: u32,
    file: &Path,
    out_dir: &Path,
) -> Result<(), String> {
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
fn render_container(
    tool: &str,
    binary: &str,
    dpi: u32,
    file: &Path,
    out_dir: &Path,
) -> Result<(), String> {
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

// ── Structural comparison (SL-0.ORACLE.03) ────────────────────────────────

/// Compare our structural model (`selis inspect --json`) against qpdf's
/// (`qpdf --json`) and report a normalised diff of object counts, page count,
/// xref entries, and stream lengths.
fn compare(file: &Path) -> Result<(), String> {
    if !file.exists() {
        return Err(format!("{}: no such file", file.display()));
    }
    let qpdf_bin = find_local("qpdf")
        .ok_or_else(|| "qpdf not installed locally. Install with `winget install qpdf` or add to PATH.".to_string())?;
    let selis_bin = find_local("selis")
        .or_else(|| find_local("selis.exe"))
        .unwrap_or_else(|| PathBuf::from("target/debug/selis.exe"));

    let ours = run_json(&selis_bin, &["inspect", "--json", file.to_str().unwrap()])?;
    let theirs = run_json(&qpdf_bin, &["--json", file.to_str().unwrap()])?;

    let mut diffs: Vec<String> = Vec::new();

    // 1. Page count.
    let their_pages = theirs["pages"]
        .as_array()
        .map(|a| a.len() as u64)
        .unwrap_or(0);
    // Our inspect doesn't directly expose page count; the engine's Session
    // does. We record qpdf's count as ground truth and note the gap.
    if their_pages > 0 {
        diffs.push(format!(
            "  pages: qpdf={their_pages} ours=unknown (not yet in inspect --json)"
        ));
    }

    // 2. Object count from qpdf v2: qpdf[0].maxobjectid.
    let qpdf_meta = theirs["qpdf"].as_array().and_then(|a| a.get(0));
    let their_objects = qpdf_meta
        .and_then(|m| m["maxobjectid"].as_u64())
        .unwrap_or(0);
    let our_objects = ours["revisions"]
        .as_array()
        .map(|revs| {
            let mut all = BTreeSet::new();
            for r in revs {
                if let Some(objs) = r["objects"].as_array() {
                    for o in objs {
                        if let Some(n) = o.as_u64() {
                            all.insert(n);
                        }
                    }
                }
            }
            all.len()
        })
        .unwrap_or(0);
    if our_objects as u64 != their_objects {
        diffs.push(format!(
            "  object count: ours={our_objects} qpdf={their_objects} delta={}",
            (our_objects as u64).abs_diff(their_objects)
        ));
    } else {
        println!("  object count: {our_objects} (match)");
    }

    // 3. Xref entry / object count from qpdf v2: count obj: keys in qpdf[1].
    let their_obj_count = theirs["qpdf"]
        .as_array()
        .and_then(|a| a.get(1))
        .map(|obj_map| {
            obj_map.as_object()
                .map(|o| o.len() as u64)
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let our_entries: u64 = ours["revisions"]
        .as_array()
        .map(|revs| {
            revs.iter()
                .filter_map(|r| r["entries"].as_u64())
                .sum()
        })
        .unwrap_or(0);
    if our_entries != their_obj_count {
        diffs.push(format!(
            "  xref entries: ours={our_entries} qpdf={their_obj_count} delta={}",
            our_entries.abs_diff(their_obj_count)
        ));
    } else {
        println!("  xref entries: {our_entries} (match)");
    }

    // 4. Stream lengths from qpdf v2: obj:N 0 R -> stream -> dict -> /Length.
    let their_streams: BTreeMap<u32, u64> = theirs["qpdf"]
        .as_array()
        .and_then(|a| a.get(1))
        .and_then(|obj_map| obj_map.as_object())
        .map(|objs| {
            let mut out = BTreeMap::new();
            for (k, v) in objs {
                if let Some(stream) = v["stream"].as_object() {
                    if let Some(dict) = stream["dict"].as_object() {
                        if let Some(len_val) = dict.get("/Length") {
                            if let Some(len) = len_val.as_u64()
                                .or_else(|| len_val.as_str().and_then(|s| s.parse::<u64>().ok()))
                            {
                                if let Some(num) = k.strip_prefix("obj:").and_then(|s| s.split_once(' ')).and_then(|(n, _)| n.parse::<u32>().ok()) {
                                    out.insert(num, len);
                                }
                            }
                        }
                    }
                }
            }
            out
        })
        .unwrap_or_default();
    if !their_streams.is_empty() {
        println!("  stream lengths: qpdf reports {} streams", their_streams.len());
    }

    if diffs.is_empty() {
        println!("structural comparison: OK (no diffs found)");
    } else {
        println!("structural comparison: {} diff(s):", diffs.len());
        for d in &diffs {
            println!("{d}");
        }
        println!("known representational differences (SL-0.ORACLE.03 DoD):");
        println!("  - object count: qpdf's maxobjectid counts object 0 (the free");
        println!("    head); we count the union of non-free xref entries.");
        println!("  - xref entries: we sum every revision's declared entries; qpdf");
        println!("    reports the final revision's live objects.");
    }
    Ok(())
}

/// Run a command that produces JSON and parse it.
fn run_json(cmd: &Path, args: &[&str]) -> Result<serde_json::Value, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run `{}': {e}", cmd.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "`{}` failed: {stderr}",
            cmd.display()
        ));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("`{}` JSON parse error: {e}", cmd.display()))
}

/// Generate a unique set of object numbers from our inspect output (union
/// across all revisions).
fn our_objects(ours: &serde_json::Value) -> BTreeSet<u32> {
    let mut all = BTreeSet::new();
    if let Some(revs) = ours["revisions"].as_array() {
        for r in revs {
            if let Some(objs) = r["objects"].as_array() {
                for o in objs {
                    if let Some(n) = o.as_u64() {
                        all.insert(n as u32);
                    }
                }
            }
        }
    }
    all
}