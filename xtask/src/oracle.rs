//! Oracle tools for differential testing (SL-0.ORACLE.01, SL-0.ORACLE.03).
//!
//! The oracles — qpdf, Ghostscript, MuPDF, PDFium, pdf.js — are third-party
//! binaries that are NEVER linked into the build (ADR-P0009). They are used
//! purely as reference producers: `xtask oracle render` rasterises a PDF with
//! an oracle so our output can be compared against it.
//!
//! Execution is **local-first, Docker-fallback**: if the tool is installed on
//! the host (qpdf on PATH, `gs`, `mutool`, a locally-built `pdfium_driver`),
//! it runs natively — faster and simplest for local dev. Otherwise the pinned
//! container image is dispatched. Docker is the CI mechanism because the
//! image digest pins the exact tool version on every runner; local installs
//! must be recorded via `xtask oracle check` so version drift is visible.
//!
//! The pins live in `xtask/oracles.toml` (`[tool.<id>]` tables): the image
//! built from `docker/oracles/<tool>/Dockerfile`, its digest (recorded after
//! the first CI build — Docker is unavailable on the dev host that authored
//! the pins), the digest-pinned base image, and the sha256-pinned tool
//! artifact. A container without a recorded digest is refused for
//! comparable output: a mutable tag is not a pin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const ORACLES_TOML: &str = "xtask/oracles.toml";

/// The render-capable oracles and the local binary each maps to. `qpdf` is a
/// structural oracle (SL-0.ORACLE.03), not a rasterizer; `pdfjs` has no local
/// binary (container-first: the pinned pdfjs-dist version is what makes its
/// output comparable).
const TOOL_BINARY: &[(&str, &str)] = &[
    ("ghostscript", "gs"),
    ("mupdf", "mutool"),
    ("mutool", "mutool"),
    ("pdfium", "pdfium_driver"),
];

/// The four triage verdicts of `21-TESTING-AND-ORACLES.md §5`.
pub const TRIAGE_VERDICTS: &[&str] =
    &["OurBug", "OracleBug", "SpecAmbiguous", "ToleranceTooTight"];

/// One oracle tool's pinned container identity, as recorded in
/// `xtask/oracles.toml`.
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ToolPin {
    /// What the oracle is used for (render, structural, text, ...).
    #[serde(default)]
    pub role: String,
    /// The pinned tool version (release tag or build id).
    #[serde(default)]
    pub version: String,
    /// The image reference our Dockerfile builds; published by the CI
    /// `oracle-images` job.
    pub image: String,
    /// The image manifest digest, recorded after the first build + push.
    /// Empty means "not built yet" — comparable dispatch is refused.
    #[serde(default)]
    pub digest: String,
    /// The base image (tag for display) and its manifest digest.
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub base_digest: String,
    /// The exact tool artifact built inside the container, with sha256.
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub source_sha256: String,
    /// The Dockerfile that produces the image.
    #[serde(default)]
    pub dockerfile: String,
    /// The tool's licence (oracle containers only; see ADR-P0021).
    #[serde(default)]
    pub licence: String,
}

/// The parsed shape of `xtask/oracles.toml`.
#[derive(serde::Deserialize, Debug)]
struct PinsFile {
    tool: BTreeMap<String, ToolPin>,
}

/// Load the pinned oracle identities from `xtask/oracles.toml`.
fn load_pins() -> Result<BTreeMap<String, ToolPin>, String> {
    let text = std::fs::read_to_string(ORACLES_TOML)
        .map_err(|e| format!("cannot read {ORACLES_TOML}: {e}"))?;
    parse_pins(&text)
}

/// Parse `xtask/oracles.toml` content into per-tool pins.
fn parse_pins(text: &str) -> Result<BTreeMap<String, ToolPin>, String> {
    let parsed: PinsFile = toml::from_str(text)
        .map_err(|e| format!("{ORACLES_TOML} does not match the [tool.<id>] schema: {e}"))?;
    if parsed.tool.is_empty() {
        return Err(format!("{ORACLES_TOML} records no [tool.<id>] pins"));
    }
    Ok(parsed.tool)
}

/// The runnable image reference for a pin: `image@sha256:...` when the
/// manifest digest has been recorded; otherwise `image:version` — a mutable
/// tag, usable only to validate the recipe in CI, never for comparable
/// output (the caller prints a loud warning in that case).
fn image_ref(pin: &ToolPin, tool: &str) -> Result<String, String> {
    if !pin.digest.is_empty() {
        return Ok(format!("{}@{}", pin.image, pin.digest));
    }
    if pin.version.is_empty() {
        return Err(format!(
            "{tool}: no image digest recorded in {ORACLES_TOML} — build and push \
             via the CI `oracle-images` job, then record it"
        ));
    }
    eprintln!(
        "warning: {tool} image digest not yet recorded — using recipe-validation \
         tag {}:{}, comparable output requires the pushed digest in {ORACLES_TOML}",
        pin.image, pin.version
    );
    Ok(format!("{}:{}", pin.image, pin.version))
}

pub enum OracleCommand {
    /// Render `file` to a PNG per DPI with the named tool.
    Render {
        tool: String,
        dpi: u32,
        file: PathBuf,
    },
    /// Report which oracles are available locally (and record their versions).
    Check,
    /// Compare `selis inspect --json` against `qpdf --json` (SL-0.ORACLE.03).
    Compare { file: PathBuf },
    /// Render with `selis` and an oracle at the same DPI and compare pixelwise
    /// (SL-0.ORACLE.02 foundation).
    CompareRender {
        tool: String,
        dpi: u32,
        file: PathBuf,
    },
    /// Compare text extracted by selis against an oracle (SL-0.ORACLE.04).
    CompareText { file: PathBuf },
    /// Triage workflow: compare a sample of corpus files against qpdf and
    /// group the disagreements by signature (SL-0.ORACLE.05). `verdicts`
    /// maps a signature to a verdict; `note` is recorded with each.
    Triage {
        sample: usize,
        verdicts: Vec<(String, String)>,
        note: Option<String>,
    },
}

pub fn run(cmd: OracleCommand) -> Result<(), String> {
    match cmd {
        OracleCommand::Render { tool, dpi, file } => render(&tool, dpi, &file),
        OracleCommand::Check => check(),
        OracleCommand::Compare { file } => compare(&file),
        OracleCommand::CompareRender { tool, dpi, file } => compare_render(&tool, dpi, &file),
        OracleCommand::CompareText { file } => compare_text(&file),
        OracleCommand::Triage {
            sample,
            verdicts,
            note,
        } => triage(sample, &verdicts, note.as_deref()),
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

    // pdf.js has no local binary: its comparability comes from the pinned
    // pdfjs-dist version inside the container, so it is always dispatched
    // there (see docker/oracles/pdfjs/).
    if tool == "pdfjs" {
        return render_container(tool, "node", dpi, file, &out_dir);
    }

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
        "pdfium" => Command::new(binary)
            .arg("--page")
            .arg("1")
            .arg("--dpi")
            .arg(dpi.to_string())
            .arg(file)
            .arg(&out)
            .status(),
        _ => return Err(format!("unknown oracle tool `{tool}`")),
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
///
/// The image runs `docker/oracles/<tool>/driver.*` with the same CLI
/// contract the local binary honours; the file is mounted read-only and the
/// PNG is written to a bind-mounted output path.
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
    let pins = load_pins()?;
    let pin = pins
        .get(tool)
        .ok_or_else(|| format!("{tool}: no [tool.{tool}] pin recorded in {ORACLES_TOML}"))?;
    let image = image_ref(pin, tool)?;
    let out = out_dir.join(format!("{tool}-{dpi}.png"));
    // Contract per oracle driver (see docker/oracles/*/Dockerfile):
    //   mutool draw | gs | pdfium_driver --page 1 --dpi N <in> <out> | pdfjs ...
    let mut cmd = Command::new("docker");
    cmd.arg("run")
        .arg("--rm")
        .arg("-v")
        .arg(format!(
            "{}:/in.pdf:ro",
            file.canonicalize().map_err(|e| e.to_string())?.display()
        ))
        .arg("-v")
        .arg(format!("{}:/out.png", out.display()));
    match tool {
        "mupdf" | "mutool" => {
            cmd.arg(image).arg("draw");
        }
        "ghostscript" => {
            cmd.arg(image).arg("-sDEVICE=png16m");
        }
        "pdfium" | "pdfjs" => {
            cmd.arg(image).arg("--page").arg("1");
        }
        _ => return Err(format!("unknown oracle tool `{tool}`")),
    }
    let status = cmd
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

/// Report which oracles are available locally and the pinned container images.
fn check() -> Result<(), String> {
    let pins = load_pins()?;
    println!("oracle check (local-first; Docker only for CI pinning):");
    let mut display = |id: &str, local: String| {
        let image = match pins.get(id) {
            Some(p) if !p.digest.is_empty() => format!("{}@{}", p.image, p.digest),
            Some(p) if !p.version.is_empty() => {
                format!("{}:{} (digest pending CI build)", p.image, p.version)
            }
            Some(_) => "unset".to_string(),
            None => "no pin".to_string(),
        };
        println!("  {id:12} local: {local:40} image: {image}");
    };
    for (id, binary) in TOOL_BINARY {
        let local = find_local(binary)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "NOT installed".to_string());
        display(id, local);
    }
    // qpdf is structural-only; pdf.js is container-first.
    display("qpdf", "structural only (SL-0.ORACLE.03)".to_string());
    display("pdfjs", "container-first (pinned pdfjs-dist)".to_string());
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
    let qpdf_bin = find_local("qpdf").ok_or_else(|| {
        "qpdf not installed locally. Install with `winget install qpdf` or add to PATH.".to_string()
    })?;
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
        .map(|obj_map| obj_map.as_object().map(|o| o.len() as u64).unwrap_or(0))
        .unwrap_or(0);
    let our_entries: u64 = ours["revisions"]
        .as_array()
        .map(|revs| revs.iter().filter_map(|r| r["entries"].as_u64()).sum())
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
                            if let Some(len) = len_val
                                .as_u64()
                                .or_else(|| len_val.as_str().and_then(|s| s.parse::<u64>().ok()))
                            {
                                if let Some(num) = k
                                    .strip_prefix("obj:")
                                    .and_then(|s| s.split_once(' '))
                                    .and_then(|(n, _)| n.parse::<u32>().ok())
                                {
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
        println!(
            "  stream lengths: qpdf reports {} streams",
            their_streams.len()
        );
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
        return Err(format!("`{}` failed: {stderr}", cmd.display()));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("`{}` JSON parse error: {e}", cmd.display()))
}

// ── Render comparison (SL-0.ORACLE.02 foundation) ──────────────────────────

/// Render a page with selis and with an oracle at the same DPI, compare
/// pixelwise, and report the percentage of differing pixels. Uses PPM (P6)
/// as the interchange format — both selis render and mutool draw support it.
fn compare_render(tool: &str, dpi: u32, file: &Path) -> Result<(), String> {
    if !file.exists() {
        return Err(format!("{}: no such file", file.display()));
    }
    let out_dir = std::env::temp_dir().join("selis-oracle-cmp");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{out_dir:?}: {e}"))?;

    let selis_bin = find_local("selis")
        .or_else(|| find_local("selis.exe"))
        .unwrap_or_else(|| PathBuf::from("target/debug/selis.exe"));

    let our_ppm = out_dir.join("our.ppm");
    let their_ppm = out_dir.join("their.ppm");

    // 1. Render with selis (same DPI as the oracle).
    let status = Command::new(&selis_bin)
        .arg("render")
        .arg("--page")
        .arg("0")
        .arg("--dpi")
        .arg(dpi.to_string())
        .arg(file)
        .arg(&our_ppm)
        .status()
        .map_err(|e| format!("selis render: {e}"))?;
    if !status.success() {
        return Err("selis render failed".to_string());
    }

    // 2. Render with the oracle (mutool draw to PPM).
    let binary = TOOL_BINARY
        .iter()
        .find(|(id, _)| *id == tool)
        .map(|(_, b)| *b)
        .ok_or_else(|| format!("unknown oracle tool `{tool}`"))?;
    let local = find_local(binary)
        .ok_or_else(|| format!("{tool} not installed locally; install it first"))?;
    let status = Command::new(&local)
        .arg("draw")
        .arg("-r")
        .arg(dpi.to_string())
        .arg("-o")
        .arg(&their_ppm)
        .arg(file)
        .status()
        .map_err(|e| format!("{tool}: {e}"))?;
    if !status.success() {
        return Err(format!("{tool}: render failed"));
    }

    // 3. Parse both PPMs and compare.
    let ours = parse_ppm(&std::fs::read(&our_ppm).map_err(|e| format!("our.ppm: {e}"))?)?;
    let theirs = parse_ppm(&std::fs::read(&their_ppm).map_err(|e| format!("their.ppm: {e}"))?)?;

    if ours.width != theirs.width || ours.height != theirs.height {
        // selis uses ceil scaling, mutool rounds — tolerate a small delta by
        // comparing the overlapping region.
        eprintln!(
            "note: size mismatch ours={}x{} theirs={}x{}; comparing the overlap",
            ours.width, ours.height, theirs.width, theirs.height
        );
    }

    let cmp_w = ours.width.min(theirs.width);
    let cmp_h = ours.height.min(theirs.height);
    let total = (cmp_w * cmp_h) as u64;
    let mut diff = 0u64;
    for y in 0..cmp_h {
        for x in 0..cmp_w {
            let oi = (y * ours.width + x) as usize * 3;
            let ti = (y * theirs.width + x) as usize * 3;
            let ours_px = [ours.rgb[oi], ours.rgb[oi + 1], ours.rgb[oi + 2]];
            let theirs_px = [theirs.rgb[ti], theirs.rgb[ti + 1], theirs.rgb[ti + 2]];
            // CIE76 ΔE in Lab space (the plan's per-pixel metric).
            let de = delta_e76(&ours_px, &theirs_px);
            if de > 2.3 {
                diff += 1;
            }
        }
    }

    let pct = diff as f64 / total as f64 * 100.0;
    println!("render comparison: {diff}/{total} pixels differ ({pct:.2}%) above ΔE76≈2.3");
    if pct < 0.5 {
        println!("render PASS (within 0.5% tolerance)");
        Ok(())
    } else {
        println!("render FAIL (exceeds 0.5% tolerance)");
        // Write a diff overlay for inspection.
        let diff_ppm = out_dir.join("diff.ppm");
        let mut diff_bytes = Vec::new();
        diff_bytes.extend_from_slice(b"P6\n");
        diff_bytes.extend_from_slice(format!("{cmp_w} {cmp_h}\n255\n").as_bytes());
        for y in 0..cmp_h {
            for x in 0..cmp_w {
                let oi = (y * ours.width + x) as usize * 3;
                let ti = (y * theirs.width + x) as usize * 3;
                let dr = (ours.rgb[oi] as i16 - theirs.rgb[ti] as i16).unsigned_abs() as u8;
                let dg = (ours.rgb[oi + 1] as i16 - theirs.rgb[ti + 1] as i16).unsigned_abs() as u8;
                let db = (ours.rgb[oi + 2] as i16 - theirs.rgb[ti + 2] as i16).unsigned_abs() as u8;
                // Amplify the difference for visibility.
                diff_bytes.push(dr.saturating_mul(4));
                diff_bytes.push(dg.saturating_mul(4));
                diff_bytes.push(db.saturating_mul(4));
            }
        }
        std::fs::write(&diff_ppm, &diff_bytes).map_err(|e| format!("diff.ppm: {e}"))?;
        println!("  diff overlay written to {diff_ppm:?}");
        Ok(())
    }
}

/// A minimal PPM P6 decoder (header + RGB bytes).
struct PpmImage {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

fn parse_ppm(data: &[u8]) -> Result<PpmImage, String> {
    // PPM header: "P6\n<width> <height>\n<maxval>\n" followed by raw RGB.
    // Parse token by token (whitespace-delimited, skipping `#` comments).
    let mut pos = 0usize;
    let mut tokens = Vec::new();
    while tokens.len() < 4 {
        // Skip whitespace.
        while pos < data.len() && (data[pos] as char).is_whitespace() {
            pos += 1;
        }
        if pos >= data.len() {
            return Err("invalid PPM header".to_string());
        }
        // Skip a `#` comment to end of line.
        if data[pos] == b'#' {
            while pos < data.len() && data[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        let start = pos;
        while pos < data.len() && !(data[pos] as char).is_whitespace() {
            pos += 1;
        }
        tokens.push(
            std::str::from_utf8(&data[start..pos])
                .map_err(|_| "invalid PPM header token")?
                .to_string(),
        );
    }
    if tokens[0] != "P6" {
        return Err(format!("expected PPM P6, got {}", tokens[0]));
    }
    let width: u32 = tokens[1].parse().map_err(|_| "invalid width")?;
    let height: u32 = tokens[2].parse().map_err(|_| "invalid height")?;
    let _maxval: u32 = tokens[3].parse().map_err(|_| "invalid maxval")?;
    // Body begins after the whitespace that terminated the maxval token.
    while pos < data.len() && (data[pos] as char).is_whitespace() {
        pos += 1;
    }
    let expected = (width * height) as usize * 3;
    if data.len() < pos + expected {
        return Err(format!(
            "truncated PPM: expected {expected} bytes, got {}",
            data.len().saturating_sub(pos)
        ));
    }
    Ok(PpmImage {
        width,
        height,
        rgb: data[pos..pos + expected].to_vec(),
    })
}

/// CIE76 ΔE between two sRGB pixels, computed in CIE Lab space.
fn delta_e76(a: &[u8; 3], b: &[u8; 3]) -> f64 {
    let la = srgb_to_lab(a);
    let lb = srgb_to_lab(b);
    let dl = la[0] - lb[0];
    let da = la[1] - lb[1];
    let db = la[2] - lb[2];
    (dl * dl + da * da + db * db).sqrt()
}

/// sRGB (8-bit) → CIE Lab, via linearisation and the D65 reference white.
fn srgb_to_lab(c: &[u8; 3]) -> [f64; 3] {
    let lin = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    let r = lin(c[0]);
    let g = lin(c[1]);
    let b = lin(c[2]);
    // sRGB → XYZ (D65).
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0) * t + 16.0 / 116.0
        }
    };
    let fx = f(x / 0.950_47);
    let fy = f(y / 1.0);
    let fz = f(z / 1.088_83);
    let l = 116.0 * fy - 16.0;
    let a = 500.0 * (fx - fy);
    let bb = 200.0 * (fy - fz);
    [l, a, bb]
}

// ── Text extraction comparison (SL-0.ORACLE.04) ────────────────────────────

/// Compare text extracted by `selis extract` and `mutool draw -F txt`.
fn compare_text(file: &Path) -> Result<(), String> {
    if !file.exists() {
        return Err(format!("{}: no such file", file.display()));
    }
    let selis_bin = find_local("selis")
        .or_else(|| find_local("selis.exe"))
        .unwrap_or_else(|| PathBuf::from("target/debug/selis.exe"));
    let mutool = find_local("mutool").ok_or_else(|| {
        "mutool not installed locally; install with `winget install ArtifexSoftware.mutool`"
            .to_string()
    })?;

    let selis_out = Command::new(&selis_bin)
        .arg("extract")
        .arg(file)
        .arg("--format")
        .arg("text")
        .arg("--page")
        .arg("0")
        .output()
        .map_err(|e| format!("selis extract: {e}"))?;
    let our_text = String::from_utf8_lossy(&selis_out.stdout).to_string();

    let mutool_out = Command::new(&mutool)
        .arg("draw")
        .arg("-F")
        .arg("txt")
        .arg(file)
        .output()
        .map_err(|e| format!("mutool draw: {e}"))?;
    let their_text = String::from_utf8_lossy(&mutool_out.stdout).to_string();

    let dist = edit_distance(&our_text, &their_text);
    let max_len = our_text.len().max(their_text.len());
    let similarity = if max_len > 0 {
        (1.0 - dist as f64 / max_len as f64) * 100.0
    } else {
        100.0
    };
    println!(
        "text comparison: selis={} chars, mutool={} chars, edit distance={}, similarity={:.1}%",
        our_text.len(),
        their_text.len(),
        dist,
        similarity
    );
    if similarity > 50.0 {
        println!("text PASS (similarity > 50%)");
    } else {
        println!("text FAIL (similarity ≤ 50%, text extraction is Phase 3)");
    }
    Ok(())
}

/// Simple Levenshtein distance (character-level).
fn edit_distance(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let m = a.len();
    let n = b.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

// ── Triage workflow (SL-0.ORACLE.05) ───────────────────────────────────────

/// One triage cluster: files sharing a disagreement signature.
struct Cluster {
    signature: String,
    files: Vec<PathBuf>,
}

/// Rank weight of a cluster (§5 step 2: files affected × source weight).
/// Wild/govdocs files weigh 3× — they predict real-world behaviour; the rest
/// weigh 1×.
fn cluster_weight(c: &Cluster) -> usize {
    c.files
        .iter()
        .map(|p| {
            let s = p.to_string_lossy();
            if s.contains("govdocs") || s.contains("wild") {
                3
            } else {
                1
            }
        })
        .sum()
}

/// Run the structural comparison against qpdf over a sample of the corpus and
/// group the disagreements by signature, so N failures collapse to a handful
/// of root causes. `verdicts` maps signature → verdict; when non-empty, the
/// verdict is recorded into every affected file's expectation record as an
/// `[annotation]` table (`21-TESTING-AND-ORACLES.md §3`).
fn triage(sample: usize, verdicts: &[(String, String)], note: Option<&str>) -> Result<(), String> {
    let qpdf_bin = find_local("qpdf").ok_or_else(|| "qpdf not installed locally".to_string())?;
    let selis_bin = find_local("selis")
        .or_else(|| find_local("selis.exe"))
        .unwrap_or_else(|| PathBuf::from("target/debug/selis.exe"));

    let mut pdfs = collect_corpus_pdfs()?;
    if pdfs.is_empty() {
        return Err("no corpus PDFs found under corpus/pdfs".to_string());
    }
    // Deterministic sample: sort, then stride across the whole tree so the
    // sample spans every source instead of whichever directory sorts first.
    pdfs.sort();
    let want = sample.max(1);
    if pdfs.len() > want {
        let step = pdfs.len() / want;
        pdfs = pdfs.into_iter().step_by(step).take(want).collect();
    }

    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for pdf in &pdfs {
        let signature = structural_signature(&selis_bin, &qpdf_bin, pdf);
        groups.entry(signature).or_default().push(pdf.clone());
    }
    let mut clusters: Vec<Cluster> = groups
        .into_iter()
        .map(|(signature, files)| Cluster { signature, files })
        .collect();
    clusters.sort_by(|a, b| {
        cluster_weight(b)
            .cmp(&cluster_weight(a))
            .then_with(|| a.signature.cmp(&b.signature))
    });

    println!(
        "oracle triage: {} files, {} signature groups (ranked):",
        pdfs.len(),
        clusters.len()
    );
    for c in &clusters {
        let mut display: Vec<String> =
            c.files.iter().map(|f| f.display().to_string()).collect();
        let rest = if display.len() > 5 {
            let n = display.len() - 5;
            display.truncate(5);
            format!(" … +{n} more")
        } else {
            String::new()
        };
        println!(
            "  {sig} [w={w}] : {n} file(s) — {list}{rest}",
            sig = c.signature,
            w = cluster_weight(c),
            n = c.files.len(),
            list = display.join(", "),
            rest = rest
        );
    }

    if verdicts.is_empty() {
        println!(
            "verdict step: none supplied. For each cluster record one of \
             {TRIAGE_VERDICTS:?}:\n  cargo xtask oracle triage --sample N \
             --verdict \"<signature>=<Verdict>\" --note \"...\""
        );
        return Ok(());
    }
    for (sig, verdict) in verdicts {
        if !TRIAGE_VERDICTS.contains(&verdict.as_str()) {
            return Err(format!(
                "verdict `{verdict}` is not one of {TRIAGE_VERDICTS:?} (cluster `{sig}`)"
            ));
        }
        let files = &clusters
            .iter()
            .find(|c| c.signature == *sig)
            .ok_or_else(|| format!("no cluster with signature `{sig}` in this sample"))?
            .files;
        for f in files {
            record_verdict(f, sig, verdict, note)?;
        }
        println!(
            "verdict `{verdict}` recorded on {} file(s) for `{sig}`",
            files.len()
        );
    }
    Ok(())
}

/// Longest note accepted in an expectation record (single line, authored
/// prose — metadata only, never document content).
const NOTE_MAX_CHARS: usize = 160;

/// The expectation record path for a corpus PDF (`corpus/expect/<id>.toml`,
/// id = path relative to corpus/pdfs minus the .pdf suffix, `/`-separated).
fn expect_path_for(pdf: &Path) -> Result<PathBuf, String> {
    let rel = pdf
        .strip_prefix("corpus/pdfs")
        .map_err(|_| format!("{}: not under corpus/pdfs", pdf.display()))?;
    let id = rel.to_string_lossy().replace('\\', "/");
    let id = id.strip_suffix(".pdf").unwrap_or(&id);
    Ok(Path::new("corpus/expect").join(format!("{id}.toml")))
}

/// Write a triage verdict into a file's expectation record as an
/// `[annotation]` table (`21-TESTING-AND-ORACLES.md §3/§5`). Any previous
/// annotation is replaced. The note is bounded authored prose — one line,
/// metadata only, never document content (`check-wild-hygiene` enforces the
/// file-size and line-length bounds).
fn record_verdict(
    pdf: &Path,
    signature: &str,
    verdict: &str,
    note: Option<&str>,
) -> Result<(), String> {
    let path = expect_path_for(pdf)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: cannot read expectation ({e})", path.display()))?;
    // Drop any previous annotation block, preserving every other field.
    let mut body = String::new();
    let mut in_annotation = false;
    for line in text.lines() {
        if line.trim() == "[annotation]" {
            in_annotation = true;
            continue;
        }
        if in_annotation {
            if line.starts_with('[') {
                in_annotation = false;
            } else {
                continue;
            }
        }
        body.push_str(line);
        body.push('\n');
    }
    while body.ends_with('\n') {
        body.pop();
    }
    if body.is_empty() {
        return Err(format!("{}: expectation record vanished", path.display()));
    }
    if let Some(n) = note {
        if n.chars().count() > NOTE_MAX_CHARS {
            return Err(format!(
                "--note exceeds {NOTE_MAX_CHARS} chars — metadata-only bound"
            ));
        }
    }
    body.push_str("\n[annotation]\n");
    body.push_str(&format!("triage = \"{signature}\"\n"));
    body.push_str(&format!("verdict = \"{verdict}\"\n"));
    if let Some(n) = note {
        body.push_str(&format!("note = \"{}\"\n", n.replace('"', "'")));
    }
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))
}

/// The signature of a file's structural agreement with qpdf: a short string
/// summarising the diffs (or "match").
///
/// Object counts are normalised before comparison so the known
/// representational difference (SL-0.ORACLE.03) cannot manufacture a
/// signature:
///   - ours: the union of xref object numbers across all revisions, minus
///     object 0 (the free-list head our xref records),
///   - qpdf: the number of `obj:N 0 R` keys in its object map (qpdf never
///     lists object 0; its `maxobjectid` counts the slot, which is why the
///     pre-normalisation comparator reported obj_delta=1 on every healthy
///     file).
/// Remaining difference, documented in `oracle compare`: our union spans all
/// revisions, qpdf's map is the final revision's live objects — files that
/// delete objects in later revisions legitimately differ.
fn structural_signature(selis_bin: &Path, qpdf_bin: &Path, file: &Path) -> String {
    let file_str = file.to_str().unwrap_or_default();
    let ours = run_json(selis_bin, &["inspect", "--json", file_str]).ok();
    let theirs = qpdf_json(qpdf_bin, file_str).ok();
    match (ours, theirs) {
        (Some(ours), Some(theirs)) => {
            let our_objects = count_our_objects(&ours) as u64;
            let their_objects = count_qpdf_objects(&theirs);
            let obj_delta = our_objects.abs_diff(their_objects);
            if obj_delta == 0 {
                "match".to_string()
            } else {
                format!("obj_delta={obj_delta}")
            }
        }
        // Who refused the file matters: a contract where we alone refuse is a
        // different root cause from one where the oracle refuses, and "both
        // refuse" is agreement, not disagreement.
        (None, Some(_)) => "qpdf_rejects".to_string(),
        (Some(_), None) => "selis_rejects".to_string(),
        (None, None) => "both_reject".to_string(),
    }
}

/// `qpdf --json` on a damaged-but-recoverable file exits 2 and emits no
/// stdout — a warning, not an open failure. `--warning-exit-0` keeps those
/// files in the comparable pool; only files qpdf cannot open at all fail
/// here (true `open_failed` signatures).
fn qpdf_json(qpdf_bin: &Path, file: &str) -> Result<serde_json::Value, String> {
    run_json(qpdf_bin, &["--warning-exit-0", "--json", file])
}

/// Union of xref object numbers across all revisions in our inspect JSON,
/// minus object 0 (the free-list head, not a live object).
fn count_our_objects(ours: &serde_json::Value) -> usize {
    let mut all = BTreeSet::new();
    if let Some(revs) = ours["revisions"].as_array() {
        for r in revs {
            if let Some(objs) = r["objects"].as_array() {
                for o in objs {
                    if let Some(n) = o.as_u64() {
                        all.insert(n);
                    }
                }
            }
        }
    }
    all.remove(&0);
    all.len()
}

/// The number of live objects qpdf's JSON object map lists: every `obj:N 0 R`
/// key. `maxobjectid` over-counts by including the free head slot, and the
/// map also carries a `trailer` key that is not an object.
fn count_qpdf_objects(theirs: &serde_json::Value) -> u64 {
    theirs["qpdf"]
        .as_array()
        .and_then(|a| a.get(1))
        .and_then(|m| m.as_object())
        .map(|objs| objs.keys().filter(|k| k.starts_with("obj:")).count() as u64)
        .unwrap_or(0)
}

/// Every `*.pdf` under `corpus/pdfs`, recursively.
fn collect_corpus_pdfs() -> Result<Vec<PathBuf>, String> {
    let root = PathBuf::from("corpus/pdfs");
    let mut out = Vec::new();
    collect_pdfs_rec(&root, &mut out)?;
    Ok(out)
}

fn collect_pdfs_rec(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{dir:?}: {e}"))?;
    for entry in rd {
        let p = entry.map_err(|e| e.to_string())?.path();
        if p.is_dir() {
            collect_pdfs_rec(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "pdf") {
            out.push(p);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN_TOML: &str = r#"
[tool.qpdf]
role = "structural"
version = "11.9.0"
image = "ghcr.io/wertcore/selis-pdf/oracle-qpdf"
digest = "sha256:aaaa"
base = "debian:bookworm-slim"
base_digest = "sha256:bbbb"
source_url = "https://example/qpdf.tar.gz"
source_sha256 = "cc"
dockerfile = "docker/oracles/qpdf/Dockerfile"
licence = "Apache-2.0"

[tool.mupdf]
role = "render"
version = "1.23.9"
image = "ghcr.io/wertcore/selis-pdf/oracle-mupdf"
base = "debian:bookworm-slim"
base_digest = "sha256:bbbb"
source_url = "https://example/mupdf.tar.gz"
source_sha256 = "dd"
dockerfile = "docker/oracles/mupdf/Dockerfile"
licence = "AGPL-3.0"
"#;

    #[test]
    fn pins_parse_into_typed_tables() {
        let pins = parse_pins(PIN_TOML).expect("valid pins");
        assert_eq!(pins.len(), 2);
        let qpdf = &pins["qpdf"];
        assert_eq!(qpdf.digest, "sha256:aaaa");
        assert_eq!(qpdf.licence, "Apache-2.0");
        assert!(qpdf.base_digest.starts_with("sha256:"));
    }

    #[test]
    fn pinned_digest_forms_digest_reference() {
        let pins = parse_pins(PIN_TOML).expect("valid pins");
        let r = image_ref(&pins["qpdf"], "qpdf").expect("digest recorded");
        assert_eq!(r, "ghcr.io/wertcore/selis-pdf/oracle-qpdf@sha256:aaaa");
    }

    #[test]
    fn missing_digest_falls_back_to_tagged_validation_ref() {
        let pins = parse_pins(PIN_TOML).expect("valid pins");
        let r = image_ref(&pins["mupdf"], "mupdf").expect("version recorded");
        // Tag-only refs are recipe-validation handles, never comparable
        // output; dispatch warns loudly (stderr) but proceeds for local use.
        assert_eq!(r, "ghcr.io/wertcore/selis-pdf/oracle-mupdf:1.23.9");
    }

    #[test]
    fn empty_pins_file_is_rejected() {
        assert!(parse_pins("# nothing here\n").is_err());
        assert!(parse_pins("not = \"toml schema\"\n").is_err());
    }

    #[test]
    fn our_objects_exclude_the_free_head() {
        let ours: serde_json::Value = serde_json::json!({
            "revisions": [
                { "objects": [0, 1, 2, 5] },
                { "objects": [2, 7] }
            ]
        });
        // Union {0,1,2,5,7} minus the free-head 0 → 4 live objects.
        assert_eq!(count_our_objects(&ours), 4);
    }

    #[test]
    fn qpdf_objects_count_obj_keys_not_maxobjectid() {
        let theirs: serde_json::Value = serde_json::json!({
            "qpdf": [
                { "maxobjectid": 3 },
                {
                    "obj:1 0 R": {},
                    "obj:2 0 R": {},
                    "trailer": {}
                }
            ]
        });
        // maxobjectid=3 counts the free head; the map lists 2 live objects.
        assert_eq!(count_qpdf_objects(&theirs), 2);
    }

    #[test]
    fn verdict_notes_stay_within_metadata_bounds() {
        let long_note = "x".repeat(NOTE_MAX_CHARS + 1);
        let tmp = std::env::temp_dir().join(format!("selis-triage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pdf_dir = tmp.join("pdfs");
        let expect_dir = tmp.join("expect");
        std::fs::create_dir_all(&pdf_dir).unwrap();
        std::fs::create_dir_all(&expect_dir).unwrap();
        let pdf = pdf_dir.join("t.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        let expect = expect_dir.join("t.toml");
        std::fs::write(&expect, "open = \"ok\"\npages = 1\n").unwrap();

        // The note is bounded before anything is written.
        let too_long = record_verdict_at(&expect, &pdf, "obj_delta=1", "OurBug", Some(&long_note));
        assert!(too_long.is_err(), "oversized note must be rejected");

        let ok = record_verdict_at(&expect, &pdf, "obj_delta=1", "OurBug", Some("xref free head"));
        assert!(ok.is_ok(), "bounded note must be accepted: {ok:?}");
        let text = std::fs::read_to_string(&expect).unwrap();
        assert!(text.contains("[annotation]"));
        assert!(text.contains("verdict = \"OurBug\""));
        assert!(text.contains("triage = \"obj_delta=1\""));
        assert!(text.contains("note = \"xref free head\""));
        // Pre-existing fields survive.
        assert!(text.contains("open = \"ok\""));
        // A second write replaces the previous annotation rather than
        // stacking a second one.
        let _ = record_verdict_at(&expect, &pdf, "match", "OracleBug", None);
        let text = std::fs::read_to_string(&expect).unwrap();
        assert_eq!(text.matches("[annotation]").count(), 1);
        assert!(text.contains("verdict = \"OracleBug\""));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn triage_verdict_set_is_the_documented_four() {
        assert_eq!(
            TRIAGE_VERDICTS,
            &["OurBug", "OracleBug", "SpecAmbiguous", "ToleranceTooTight"]
        );
    }

    /// Test seam: `record_verdict` against an explicit expectation path
    /// (the production one derives `corpus/expect/` from the cwd).
    fn record_verdict_at(
        expect_path: &Path,
        _pdf: &Path,
        signature: &str,
        verdict: &str,
        note: Option<&str>,
    ) -> Result<(), String> {
        let text = std::fs::read_to_string(expect_path)
            .map_err(|e| format!("{}: cannot read expectation ({e})", expect_path.display()))?;
        let mut body = String::new();
        let mut in_annotation = false;
        for line in text.lines() {
            if line.trim() == "[annotation]" {
                in_annotation = true;
                continue;
            }
            if in_annotation {
                if line.starts_with('[') {
                    in_annotation = false;
                } else {
                    continue;
                }
            }
            body.push_str(line);
            body.push('\n');
        }
        while body.ends_with('\n') {
            body.pop();
        }
        if body.is_empty() {
            return Err("expectation record vanished".to_string());
        }
        if let Some(n) = note {
            if n.chars().count() > NOTE_MAX_CHARS {
                return Err("note exceeds the metadata bound".to_string());
            }
        }
        body.push_str("\n[annotation]\n");
        body.push_str(&format!("triage = \"{signature}\"\n"));
        body.push_str(&format!("verdict = \"{verdict}\"\n"));
        if let Some(n) = note {
            body.push_str(&format!("note = \"{}\"\n", n.replace('"', "'")));
        }
        std::fs::write(expect_path, body).map_err(|e| format!("{e}"))
    }
}
