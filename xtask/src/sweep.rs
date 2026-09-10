//! Full-corpus differential render sweep (SL-2.CONF.01).
//!
//! Renders page 1 of every corpus PDF with selis and with the named oracle
//! tools at 72/150/300 DPI, compares pixels with the SL-0.ORACLE.02
//! normalisation + tolerance machinery (PPM interchange, overlap region,
//! CIE76 dE per pixel, 2.3 threshold), and groups every outcome into a
//! disagreement *signature* so N failures collapse to a handful of root
//! causes (SL-0.ORACLE.05). The report feeds the G2 readout and
//! `conformance/areas.toml` promotion (SL-2.CONF.02).
//!
//! Scope and honesty notes, for the G2 readout:
//! * page 1 per file (the compare-render scope); the corpus, not the page
//!   count, is what the G2 threshold quantifies,
//! * locally the available oracle is MuPDF (`mutool`); the PDFium/pdf.js
//!   comparisons run in the scheduled CI `render-conf` job through the
//!   pinned GHCR images (`xtask/oracles.toml`) because Docker is not
//!   available on the dev host,
//! * every render is the CLI's own per-file `Budget::profile(Surface::Viewer)`
//!   path, and additionally wall-clock bounded: a file that fails to render,
//!   exceeds the timeout, or is too large to compare is a *typed outcome*
//!   (`selis_rejects`, `selis_timeout`, `oversized`, ...), never a crash.
//!
//! Artifacts (verdict JSONL + summary report) go to `--out` — never into the
//! repo: sweep outputs are measurements, not source.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::oracle;

/// Per-pixel dE76 threshold, identical to compare-render (SL-0.ORACLE.02).
const DELTA_E_TOLERANCE: f64 = 2.3;

/// G2 per-file threshold: <= 0.5% differing pixels (12-PHASE-2-render.md gate).
const G2_MAX_DIFF_PCT: f64 = 0.5;

/// A pixel-dimension cap for the comparison. A page above this many pixels
/// (e.g. an A0 Ghent poster at 300 DPI) is recorded as `oversized` instead of
/// being loaded — the harness must not allocate hundreds of MB per side, per
/// worker.
const MAX_COMPARE_PIXELS: u64 = 40_000_000;

/// The signatures that count as "both sides rendered comparably".
const COMPARABLE: &[&str] = &[
    "match",
    "diff<5",
    "diff<25",
    "diff>=25",
    "blank_selis",
    "blank_oracle",
    "size_skew",
];

/// Everything the sweep needs, resolved once.
pub struct SweepConfig {
    pub tools: Vec<String>,
    pub dpis: Vec<u32>,
    pub sample: Option<usize>,
    pub out: PathBuf,
    pub timeout: Duration,
    pub jobs: usize,
    pub selis: Option<PathBuf>,
    pub resume: bool,
}

/// One (file, tool, dpi) outcome, appended to `verdicts.jsonl`.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Verdict {
    id: String,
    tool: String,
    dpi: u32,
    signature: String,
    /// % of overlapping pixels above dE76 2.3, when both sides rendered.
    diff_pct: Option<f64>,
    /// selis render dimensions [w, h], when it rendered.
    ours: Option<[u32; 2]>,
    /// oracle render dimensions [w, h], when it rendered.
    theirs: Option<[u32; 2]>,
    /// Typed-error or size detail; bounded, metadata only (no document data).
    detail: Option<String>,
}

/// Why one side produced no comparable render.
#[derive(Debug, Clone)]
enum SideFail {
    /// The tool exited non-zero / produced no image; carries one bounded
    /// stderr line (typed error text, not document content).
    Rejected(String),
    /// The wall-clock budget for this render ran out.
    Timeout,
}

/// The comparison result for two rendered pages.
#[derive(Debug, Clone)]
struct Comparison {
    diff_pct: f64,
    selis_blank: bool,
    oracle_blank: bool,
}

/// The normalised image both sides are compared in: 8-bit RGB.
struct Rgb {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Rgb {
    fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

// ── Signatures (SL-0.ORACLE.05: group by root cause, not by file) ──────────

/// The disagreement signature of one (file, tool, dpi) outcome.
///
/// Precedence: total failure first (a file that does not render is a
/// different root cause from a file that renders wrongly), then geometry
/// skew, then blank-vs-painted, then the diff-magnitude buckets.
fn classify(
    selis: Result<&Rgb, &SideFail>,
    oracle: Result<&Rgb, &SideFail>,
    cmp: Option<&Comparison>,
) -> (&'static str, Option<String>) {
    match (selis, oracle) {
        (Err(s), Err(o)) => (
            "both_reject",
            Some(format!(
                "selis: {} | oracle: {}",
                fail_text(s),
                fail_text(o)
            )),
        ),
        (Err(s), Ok(_)) => ("selis_rejects", Some(fail_text(s))),
        (Ok(_), Err(o)) => ("oracle_rejects", Some(fail_text(o))),
        (Ok(_), Ok(_)) => {
            let cmp = cmp.expect("both sides rendered, comparison must exist");
            if size_skewed(selis, oracle) {
                return ("size_skew", None);
            }
            if cmp.selis_blank && !cmp.oracle_blank && cmp.diff_pct > G2_MAX_DIFF_PCT {
                return ("blank_selis", None);
            }
            if cmp.oracle_blank && !cmp.selis_blank && cmp.diff_pct > G2_MAX_DIFF_PCT {
                return ("blank_oracle", None);
            }
            if cmp.diff_pct <= G2_MAX_DIFF_PCT {
                ("match", None)
            } else if cmp.diff_pct < 5.0 {
                ("diff<5", None)
            } else if cmp.diff_pct < 25.0 {
                ("diff<25", None)
            } else {
                ("diff>=25", None)
            }
        }
    }
}

/// Bounded stderr line for a failure (typed error text only; the harness
/// truncates to keep records metadata-sized).
fn fail_text(f: &SideFail) -> String {
    match f {
        SideFail::Rejected(stderr) => stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.chars().take(120).collect())
            .unwrap_or_default(),
        SideFail::Timeout => "wall-clock timeout".to_string(),
    }
}

/// True when the two renders disagree on geometry by more than the +-1 px
/// ceil-vs-round artefact the overlap comparator tolerates.
fn size_skewed(selis: Result<&Rgb, &SideFail>, oracle: Result<&Rgb, &SideFail>) -> bool {
    let (Some(a), Some(b)) = (selis.ok(), oracle.ok()) else {
        return false;
    };
    if a.pixels() == 0 || b.pixels() == 0 {
        return true;
    }
    let (aw, ah, bw, bh) = (
        f64::from(a.width),
        f64::from(a.height),
        f64::from(b.width),
        f64::from(b.height),
    );
    (aw / bw < 0.99 || bw / aw < 0.99) || (ah / bh < 0.99 || bh / ah < 0.99)
}

/// The conformance area(s) a signature maps to, for the SL-2.CONF.02
/// promotion readout. Error codes map through the registry ranges of
/// 03-CONVENTIONS.md §3; the detail line carries `[Ennnn]`.
fn areas_for(signature: &str, detail: Option<&str>) -> Vec<&'static str> {
    match signature {
        "match" | "both_reject" | "oversized" => vec![],
        // Oracle-side failures are not our conformance claims.
        "oracle_rejects" | "oracle_timeout" => vec![],
        "diff<5" | "diff<25" | "diff>=25" | "blank_selis" | "blank_oracle" | "selis_timeout" => {
            vec!["render"]
        }
        "size_skew" => vec!["document", "render"],
        "selis_rejects" => detail
            .and_then(error_code)
            .map_or_else(Vec::new, areas_for_error_code),
        _ => vec![],
    }
}

/// The `[Ennnn]` typed-error id in a selis stderr line.
fn error_code(detail: &str) -> Option<u16> {
    let start = detail.find("[E")?;
    let digits: String = detail[start + 2..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Registry ranges (03-CONVENTIONS.md §3) → conformance areas.
fn areas_for_error_code(code: u16) -> Vec<&'static str> {
    match code {
        1000..=1499 => vec!["cos", "xref"],
        1500..=1799 => vec!["filters"],
        1800..=1999 => vec!["encryption"],
        2000..=2299 => vec!["text"],
        2300..=2599 => vec!["render"],
        2600..=2799 => vec!["annot"],
        4000..=4299 => vec!["render"],
        _ => vec![],
    }
}

// ── Comparison (compare-render machinery, reused) ──────────────────────────

/// Compare two RGB renders: overlap region, per-pixel dE76, byte-equal fast
/// path (identical bytes have dE 0, so the fast path is exact). Also reports
/// per-side blankness (>= 99.5% pure white) for the blank-vs-painted signature.
fn compare_images(ours: &Rgb, theirs: &Rgb) -> Comparison {
    let cmp_w = ours.width.min(theirs.width);
    let cmp_h = ours.height.min(theirs.height);
    let total = u64::from(cmp_w) * u64::from(cmp_h);
    let ours_stride = usize::try_from(ours.width).unwrap_or(0) * 3;
    let theirs_stride = usize::try_from(theirs.width).unwrap_or(0) * 3;
    let row_bytes = usize::try_from(cmp_w).unwrap_or(0) * 3;
    let rows = usize::try_from(cmp_h).unwrap_or(0);
    let cols = usize::try_from(cmp_w).unwrap_or(0);

    let mut diff = 0u64;
    let mut our_white = 0u64;
    let mut their_white = 0u64;
    let mut any_row_differs = false;
    for y in 0..rows {
        let Some(orow) = row_slice(&ours.data, y, ours_stride, row_bytes) else {
            break;
        };
        let Some(trow) = row_slice(&theirs.data, y, theirs_stride, row_bytes) else {
            break;
        };
        if orow == trow {
            continue;
        }
        any_row_differs = true;
        for x in 0..cols {
            let oi = x * 3;
            let (or_, og, ob) = (orow[oi], orow[oi + 1], orow[oi + 2]);
            let (tr_, tg, tb) = (trow[oi], trow[oi + 1], trow[oi + 2]);
            if or_ == 255 && og == 255 && ob == 255 {
                our_white += 1;
            }
            if tr_ == 255 && tg == 255 && tb == 255 {
                their_white += 1;
            }
            if or_ == tr_ && og == tg && ob == tb {
                continue;
            }
            let de = oracle::delta_e76(&[or_, og, ob], &[tr_, tg, tb]);
            if de > DELTA_E_TOLERANCE {
                diff += 1;
            }
        }
    }

    let pct = if total == 0 {
        100.0
    } else {
        diff as f64 / total as f64 * 100.0
    };
    let (selis_blank, oracle_blank) = if any_row_differs && total > 0 {
        let ratio = |w: u64| w as f64 / total as f64;
        (ratio(our_white) >= 0.995, ratio(their_white) >= 0.995)
    } else {
        (false, false)
    };
    Comparison {
        diff_pct: pct,
        selis_blank,
        oracle_blank,
    }
}

/// One row of an RGB image as a `cmp_w * 3` slice, or `None` when the stride
/// arithmetic would overflow (a truncated image compares as truncated rather
/// than panicking).
fn row_slice(data: &[u8], y: usize, stride: usize, row_bytes: usize) -> Option<&[u8]> {
    let start = y.checked_mul(stride)?;
    let end = start.checked_add(row_bytes)?;
    data.get(start..end)
}

// ── Render loading (with the oversized-canvas guard) ───────────────────────

/// Peek a render's dimensions from its header (PPM when `ppm`, else PNG),
/// without loading the payload.
fn peek_dimensions(path: &Path, ppm: bool) -> Result<(u32, u32), SideFail> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| SideFail::Rejected(format!("{}: {e}", path.display())))?;
    let mut head = [0u8; 64];
    let n = f
        .read(&mut head)
        .map_err(|e| SideFail::Rejected(format!("{}: {e}", path.display())))?;
    if ppm {
        oracle::ppm_dimensions(&head[..n])
    } else {
        crate::png::dimensions(&head[..n])
    }
    .map_err(|e| SideFail::Rejected(format!("{}: {e}", path.display())))
}

/// Refuse canvases above the comparison cap (typed outcome, no allocation).
fn check_cap(w: u32, h: u32, side: &str) -> Result<(), SideFail> {
    if u64::from(w) * u64::from(h) > MAX_COMPARE_PIXELS {
        return Err(SideFail::Rejected(format!("{side}: oversized {w}x{h}")));
    }
    Ok(())
}

/// Load a selis PPM render, refusing oversized canvases before allocation.
fn load_selis_ppm(path: &Path) -> Result<Rgb, SideFail> {
    let (w, h) = peek_dimensions(path, true)?;
    check_cap(w, h, "selis")?;
    let bytes =
        std::fs::read(path).map_err(|e| SideFail::Rejected(format!("{}: {e}", path.display())))?;
    let img =
        oracle::parse_ppm(&bytes).map_err(|e| SideFail::Rejected(format!("selis ppm: {e}")))?;
    Ok(Rgb {
        width: img.width,
        height: img.height,
        data: img.rgb,
    })
}

/// Load an oracle render (PPM for mutool, PNG for the pdfium/pdf.js/ghostscript
/// drivers), refusing oversized canvases before allocation.
fn load_oracle(path: &Path, tool: &str) -> Result<Rgb, SideFail> {
    let ppm = tool == "mutool" || tool == "mupdf";
    let (w, h) = peek_dimensions(path, ppm)?;
    check_cap(w, h, tool)?;
    let bytes =
        std::fs::read(path).map_err(|e| SideFail::Rejected(format!("{}: {e}", path.display())))?;
    if ppm {
        let img =
            oracle::parse_ppm(&bytes).map_err(|e| SideFail::Rejected(format!("{tool}: {e}")))?;
        Ok(Rgb {
            width: img.width,
            height: img.height,
            data: img.rgb,
        })
    } else {
        let img =
            crate::png::decode(&bytes).map_err(|e| SideFail::Rejected(format!("{tool}: {e}")))?;
        Ok(Rgb {
            width: img.width,
            height: img.height,
            data: img.rgb,
        })
    }
}

// ── Process execution (typed outcomes, never a crash) ──────────────────────

/// Run `program args...` with a wall-clock budget. Returns the captured
/// stderr (only its first non-empty line survives into the verdict) on a
/// typed failure.
fn run_with_timeout(program: &Path, args: &[String], timeout: Duration) -> Result<(), SideFail> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SideFail::Rejected(format!("spawn failed: {e}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return Err(SideFail::Rejected(stderr));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SideFail::Timeout);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(SideFail::Rejected(format!("wait failed: {e}"))),
        }
    }
}

/// Canonicalised absolute path text (spawned executables and container mounts
/// need it; falls back to the given path when canonicalisation fails).
fn path_str(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

// ── The sweep itself ────────────────────────────────────────────────────────

/// Run the sweep and write `verdicts.jsonl` + `sweep-report.json` into
/// `cfg.out`.
pub fn run(cfg: SweepConfig) -> Result<(), String> {
    std::fs::create_dir_all(&cfg.out).map_err(|e| format!("{}: {e}", cfg.out.display()))?;
    let selis_bin = resolve_selis(cfg.selis.as_deref())?;
    let files = sample_files(cfg.sample)?;
    if files.is_empty() {
        return Err("no corpus PDFs found under corpus/pdfs".to_string());
    }
    let jobs = cfg.jobs.max(1);
    for w in 0..jobs {
        let tmp = cfg.out.join(format!("tmp-w{w}"));
        std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    }

    let verdict_path = cfg.out.join("verdicts.jsonl");
    let done = if cfg.resume {
        load_done_keys(&verdict_path)?
    } else {
        HashSet::new()
    };
    let verdict_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&verdict_path)
        .map_err(|e| format!("{}: {e}", verdict_path.display()))?;
    let writer = Mutex::new(std::io::BufWriter::new(verdict_file));
    let next = AtomicUsize::new(0);
    let done = Mutex::new(done);
    let dpis = cfg.dpis.clone();
    let tools = cfg.tools.clone();
    let timeout = cfg.timeout;
    let out_dir = cfg.out.clone();

    std::thread::scope(|scope| {
        for w in 0..jobs {
            let tmp = out_dir.join(format!("tmp-w{w}"));
            let next_ref = &next;
            let files_ref = &files;
            let writer_ref = &writer;
            let done_ref = &done;
            let selis_ref = &selis_bin;
            let tools_ref = &tools;
            let dpis_ref = &dpis;
            scope.spawn(move || {
                loop {
                    let i = next_ref.fetch_add(1, Ordering::SeqCst);
                    if i >= files_ref.len() {
                        break;
                    }
                    let file = &files_ref[i];
                    for tool in tools_ref {
                        for dpi in dpis_ref {
                            let key = format!("{}|{tool}|{dpi}", oracle::corpus_id(file));
                            if done_ref.lock().is_ok_and(|mut d| !d.insert(key)) {
                                continue;
                            }
                            let verdict = sweep_one(selis_ref, tool, *dpi, file, &tmp, timeout);
                            if let Ok(mut wr) = writer_ref.lock() {
                                let _ = serde_json::to_writer(&mut *wr, &verdict);
                                let _ = wr.write_all(b"\n");
                                let _ = wr.flush();
                            }
                        }
                    }
                }
                let _ = std::fs::remove_dir_all(&tmp);
            });
        }
    });

    let verdicts = load_verdicts(&verdict_path)?;
    let report = build_report(&selis_bin, files.len(), &verdicts, &cfg.tools, &cfg.dpis)?;
    let report_path = cfg.out.join("sweep-report.json");
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&report_path, json).map_err(|e| format!("{}: {e}", report_path.display()))?;
    print_report(&report);
    Ok(())
}

/// Sweep one (file, tool, dpi): render both sides, compare, classify.
fn sweep_one(
    selis_bin: &Path,
    tool: &str,
    dpi: u32,
    file: &Path,
    tmp: &Path,
    timeout: Duration,
) -> Verdict {
    let mut verdict = Verdict {
        id: oracle::corpus_id(file),
        tool: tool.to_string(),
        dpi,
        signature: "match".to_string(),
        diff_pct: None,
        ours: None,
        theirs: None,
        detail: None,
    };

    let our_path = tmp.join("ours.ppm");
    let their_path = tmp.join(if tool == "mutool" || tool == "mupdf" {
        "theirs.ppm"
    } else {
        "theirs.png"
    });
    let _ = std::fs::remove_file(&our_path);
    let _ = std::fs::remove_file(&their_path);

    let ours = render_selis(selis_bin, file, dpi, &our_path, timeout)
        .and_then(|()| load_selis_ppm(&our_path));
    let theirs = render_oracle(tool, file, dpi, &their_path, timeout)
        .and_then(|()| load_oracle(&their_path, tool));

    verdict.ours = ours.as_ref().ok().map(|r| [r.width, r.height]);
    verdict.theirs = theirs.as_ref().ok().map(|r| [r.width, r.height]);

    let comparison = match (&ours, &theirs) {
        (Ok(o), Ok(t)) => Some(compare_images(o, t)),
        _ => None,
    };
    if let Some(c) = &comparison {
        verdict.diff_pct = Some((c.diff_pct * 100.0).round() / 100.0);
    }
    let (signature, detail) = classify(ours.as_ref(), theirs.as_ref(), comparison.as_ref());
    verdict.signature = signature.to_string();
    verdict.detail = detail;
    let _ = std::fs::remove_file(&our_path);
    let _ = std::fs::remove_file(&their_path);
    verdict
}

/// Render page 1 with the selis CLI (its own per-file Budget) to a PPM.
fn render_selis(
    selis_bin: &Path,
    file: &Path,
    dpi: u32,
    out: &Path,
    timeout: Duration,
) -> Result<(), SideFail> {
    let args = vec![
        "render".to_string(),
        "--page".to_string(),
        "0".to_string(),
        "--dpi".to_string(),
        dpi.to_string(),
        path_str(file),
        path_str(out),
    ];
    run_with_timeout(selis_bin, &args, timeout)
}

/// Render page 1 with an oracle via its planned command (local binary or the
/// pinned container), under the same wall-clock budget.
fn render_oracle(
    tool: &str,
    file: &Path,
    dpi: u32,
    out: &Path,
    timeout: Duration,
) -> Result<(), SideFail> {
    let (program, args) =
        oracle::plan_oracle_render(tool, dpi, file, out).map_err(SideFail::Rejected)?;
    run_with_timeout(&program, &args, timeout)?;
    if !out.exists() {
        return Err(SideFail::Rejected("no output produced".to_string()));
    }
    Ok(())
}

/// Resolve the selis binary: explicit flag, then the target dir (release,
/// debug), then the workspace target dir, then PATH.
fn resolve_selis(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        let canonical = p
            .canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))?;
        return Ok(unverbatim(&canonical));
    }
    let exe = if cfg!(windows) { "selis.exe" } else { "selis" };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        candidates.push(Path::new(&dir).join("release").join(exe));
        candidates.push(Path::new(&dir).join("debug").join(exe));
    }
    candidates.push(Path::new("target").join("release").join(exe));
    candidates.push(Path::new("target").join("debug").join(exe));
    for c in candidates {
        if c.is_file() {
            let canonical = c
                .canonicalize()
                .map_err(|e| format!("{}: {e}", c.display()))?;
            return Ok(unverbatim(&canonical));
        }
    }
    oracle::find_local("selis").ok_or_else(|| {
        "selis binary not found — build it (cargo build --release -p selis-cli) or pass --selis"
            .to_string()
    })
}

/// Display form without the Windows verbatim prefix; spawned children keep
/// the canonical form (long verapdf paths exceed MAX_PATH).
fn unverbatim(p: &Path) -> PathBuf {
    p.to_string_lossy()
        .strip_prefix(r"\\?\")
        .map_or_else(|| p.to_path_buf(), PathBuf::from)
}

/// The corpus file list, deterministically stride-sampled when `sample` is set.
fn sample_files(sample: Option<usize>) -> Result<Vec<PathBuf>, String> {
    let mut pdfs = oracle::collect_corpus_pdfs()?;
    pdfs.sort();
    if let Some(want) = sample {
        let want = want.max(1);
        if pdfs.len() > want {
            let step = pdfs.len() / want;
            pdfs = pdfs.into_iter().step_by(step).take(want).collect();
        }
    }
    Ok(pdfs)
}

/// Keys already recorded in a previous run (for `--resume`).
fn load_done_keys(path: &Path) -> Result<HashSet<String>, String> {
    Ok(load_verdicts(path)?
        .into_iter()
        .map(|v| format!("{}|{}|{}", v.id, v.tool, v.dpi))
        .collect())
}

fn load_verdicts(path: &Path) -> Result<Vec<Verdict>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    Ok(out)
}

// ── Report ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Report {
    task: &'static str,
    scope: &'static str,
    selis: String,
    tools: Vec<String>,
    dpis: Vec<u32>,
    files: usize,
    tolerance: serde_json::Value,
    per_tool: BTreeMap<String, ToolReport>,
}

#[derive(Serialize, Default)]
struct ToolReport {
    comparable: u64,
    per_dpi: BTreeMap<String, DpiReport>,
    /// Per-source corpus agreement at 150 DPI (the G2 DPI).
    sources_150: BTreeMap<String, SourceAgreement>,
    clusters: Vec<Cluster>,
    g2_150: G2Readout,
}

#[derive(Serialize, Default, Clone)]
struct DpiReport {
    outcomes: BTreeMap<String, u64>,
    comparable: u64,
    within_tolerance: u64,
}

#[derive(Serialize, Default)]
struct SourceAgreement {
    comparable: u64,
    within_tolerance: u64,
}

#[derive(Serialize)]
struct Cluster {
    signature: String,
    outcomes: u64,
    files: u64,
    weight: u64,
    areas: Vec<&'static str>,
    examples: Vec<String>,
}

#[derive(Serialize, Default)]
struct G2Readout {
    criterion: String,
    oracle: String,
    comparable: u64,
    within_tolerance: u64,
    agreement_pct: f64,
    met: bool,
}

/// Aggregate the verdict JSONL into the report (G2 readout at 150 DPI per
/// tool + ranked signature clusters).
fn build_report(
    selis_bin: &Path,
    files: usize,
    verdicts: &[Verdict],
    tools: &[String],
    dpis: &[u32],
) -> Result<Report, String> {
    let mut per_tool: BTreeMap<String, ToolReport> = BTreeMap::new();
    for v in verdicts {
        let tr = per_tool.entry(v.tool.clone()).or_default();
        let dr = tr.per_dpi.entry(v.dpi.to_string()).or_default();
        *dr.outcomes.entry(v.signature.clone()).or_default() += 1;
        if COMPARABLE.contains(&v.signature.as_str()) {
            tr.comparable += 1;
            dr.comparable += 1;
            if v.signature == "match" {
                dr.within_tolerance += 1;
            }
            if v.dpi == 150 {
                let sa = tr.sources_150.entry(source_of(&v.id)).or_default();
                sa.comparable += 1;
                if v.signature == "match" {
                    sa.within_tolerance += 1;
                }
            }
        }
    }
    let mut report = Report {
        task: "SL-2.CONF.01",
        scope: "page 1 of each corpus file, 72/150/300 DPI, selis vs oracle render",
        selis: selis_bin.display().to_string(),
        tools: tools.to_vec(),
        dpis: dpis.to_vec(),
        files,
        tolerance: serde_json::json!({
            "delta_e": DELTA_E_TOLERANCE,
            "max_differing_pixels_pct": G2_MAX_DIFF_PCT,
        }),
        per_tool,
    };
    for (tool, tr) in report.per_tool.iter_mut() {
        tr.clusters = ranked_clusters(tool, verdicts);

        // G2 readout at 150 DPI (the gate's DPI), against this oracle.
        let dr = tr.per_dpi.get("150").cloned().unwrap_or_default();
        let agreement = if dr.comparable > 0 {
            dr.within_tolerance as f64 / dr.comparable as f64 * 100.0
        } else {
            0.0
        };
        tr.g2_150 = G2Readout {
            criterion: format!(
                "<= {G2_MAX_DIFF_PCT}% differing pixels (dE76 > {DELTA_E_TOLERANCE}) on >= 95% of \
                 the render corpus at 150 DPI"
            ),
            oracle: oracle_identity(tool),
            comparable: dr.comparable,
            within_tolerance: dr.within_tolerance,
            agreement_pct: (agreement * 100.0).round() / 100.0,
            met: dr.comparable > 0 && agreement >= 95.0,
        };
    }
    Ok(report)
}

/// Ranked clusters for one tool: signature -> files affected x corpus weight
/// (wild/govdocs x3, SL-0.ORACLE.05 step 3), then signature for stability.
fn ranked_clusters(tool: &str, verdicts: &[Verdict]) -> Vec<Cluster> {
    let mut sig_ids: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    let mut sig_count: BTreeMap<String, u64> = BTreeMap::new();
    for v in verdicts.iter().filter(|v| v.tool == tool) {
        sig_ids
            .entry(v.signature.clone())
            .or_default()
            .insert(v.id.clone());
        *sig_count.entry(v.signature.clone()).or_default() += 1;
    }
    let mut clusters: Vec<Cluster> = sig_ids
        .into_iter()
        .map(|(sig, ids)| {
            let detail_example = verdicts
                .iter()
                .find(|v| v.tool == tool && v.signature == sig)
                .and_then(|v| v.detail.clone());
            Cluster {
                outcomes: sig_count.get(&sig).copied().unwrap_or(0),
                files: u64::try_from(ids.len()).unwrap_or(u64::MAX),
                weight: ids
                    .iter()
                    .map(|id| u64::try_from(file_weight(id)).unwrap_or(u64::MAX))
                    .sum(),
                areas: areas_for(&sig, detail_example.as_deref()),
                examples: ids.into_iter().take(3).collect(),
                signature: sig,
            }
        })
        .collect();
    clusters.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| a.signature.cmp(&b.signature))
    });
    clusters
}

/// The corpus source of a file id: its first path segment (`flat` when the id
/// has none, matching the extraction layouts of SL-0.CORP.01).
fn source_of(id: &str) -> String {
    match id.split_once('/') {
        Some((src, _)) => src.to_string(),
        None => "flat".to_string(),
    }
}

/// Corpus weight of one file id (SL-0.ORACLE.05 step 3: wild/govdocs x3).
fn file_weight(id: &str) -> usize {
    if id.contains("govdocs") || id.contains("wild") {
        3
    } else {
        1
    }
}

/// The comparable identity of an oracle tool (local install or pinned image).
fn oracle_identity(tool: &str) -> String {
    match tool {
        "mutool" | "mupdf" => {
            "MuPDF mutool (local install; pinned container: xtask/oracles.toml [tool.mupdf])"
                .to_string()
        }
        "pdfium" => {
            "PDFium (pinned container ghcr.io/wertcore/selis-pdf/oracle-pdfium, chromium-7961)"
                .to_string()
        }
        "pdfjs" => {
            "pdf.js (pinned container ghcr.io/wertcore/selis-pdf/oracle-pdfjs, 6.2.108)".to_string()
        }
        "ghostscript" => {
            "Ghostscript (pinned container ghcr.io/wertcore/selis-pdf/oracle-ghostscript, 9.56.1)"
                .to_string()
        }
        other => other.to_string(),
    }
}

/// The ranked triage view, straight to stdout (the ORACLE.05 cluster list).
fn print_report(report: &Report) {
    println!(
        "render sweep: {} corpus files, DPIs {:?}, tools {:?}",
        report.files, report.dpis, report.tools
    );
    for (tool, tr) in &report.per_tool {
        println!(
            "  oracle {tool}: comparable={} (G2@150: {}/{} = {:.2}% — met: {})",
            tr.comparable,
            tr.g2_150.within_tolerance,
            tr.g2_150.comparable,
            tr.g2_150.agreement_pct,
            tr.g2_150.met,
        );
        for c in &tr.clusters {
            println!(
                "    {} [w={}] : {} file(s), {} outcome(s), areas={:?} — e.g. {}",
                c.signature,
                c.weight,
                c.files,
                c.outcomes,
                c.areas,
                c.examples.join(", ")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_puts_total_failure_first() {
        let s = SideFail::Rejected("open failed".to_string());
        let o = SideFail::Rejected("cannot authenticate".to_string());
        let (sig, _) = classify(Err(&s), Err(&o), None);
        assert_eq!(sig, "both_reject");
        let blank = rgb_blank(10, 10);
        let (sig, _) = classify(Err(&s), Ok(&blank), None);
        assert_eq!(sig, "selis_rejects");
        let (sig, _) = classify(Ok(&blank), Err(&o), None);
        assert_eq!(sig, "oracle_rejects");
    }

    #[test]
    fn classify_buckets_diff_magnitudes() {
        let a = rgb_paint(10, 10);
        let b = rgb_paint(10, 10);
        for (pct, want) in [
            (0.4, "match"),
            (1.2, "diff<5"),
            (12.0, "diff<25"),
            (40.0, "diff>=25"),
        ] {
            let cmp = Comparison {
                diff_pct: pct,
                selis_blank: false,
                oracle_blank: false,
            };
            let (sig, _) = classify(Ok(&a), Ok(&b), Some(&cmp));
            assert_eq!(sig, want, "pct {pct}");
        }
    }

    #[test]
    fn classify_flags_one_sided_blank() {
        let painted = rgb_paint(10, 10);
        let blank = rgb_blank(10, 10);
        let cmp = Comparison {
            diff_pct: 90.0,
            selis_blank: true,
            oracle_blank: false,
        };
        let (sig, _) = classify(Ok(&blank), Ok(&painted), Some(&cmp));
        assert_eq!(sig, "blank_selis");
        let cmp = Comparison {
            diff_pct: 90.0,
            selis_blank: false,
            oracle_blank: true,
        };
        let (sig, _) = classify(Ok(&painted), Ok(&blank), Some(&cmp));
        assert_eq!(sig, "blank_oracle");
    }

    #[test]
    fn classify_flags_size_skew_over_one_percent() {
        let a = rgb_paint(100, 100);
        let b = rgb_paint(120, 100);
        let cmp = Comparison {
            diff_pct: 30.0,
            selis_blank: false,
            oracle_blank: false,
        };
        let (sig, _) = classify(Ok(&a), Ok(&b), Some(&cmp));
        assert_eq!(sig, "size_skew");
    }

    #[test]
    fn identical_pixels_agree_via_fast_path() {
        let a = rgb_paint(8, 8);
        let b = rgb_paint(8, 8);
        let c = compare_images(&a, &b);
        assert_eq!(c.diff_pct, 0.0);
        assert!(!c.selis_blank && !c.oracle_blank);
    }

    #[test]
    fn blank_versus_painted_differs_fully() {
        let a = rgb_blank(8, 8);
        let b = rgb_paint(8, 8);
        let c = compare_images(&a, &b);
        assert!(c.diff_pct > 90.0);
        assert!(c.selis_blank && !c.oracle_blank);
    }

    #[test]
    fn row_slice_tolerates_truncated_images() {
        let data = vec![1u8; 7];
        assert!(row_slice(&data, 0, 9, 9).is_none());
        assert!(row_slice(&data, usize::MAX, 3, 3).is_none());
    }

    #[test]
    fn error_codes_map_to_registry_areas() {
        assert_eq!(
            areas_for("selis_rejects", Some("[E1204] bad xref")),
            vec!["cos", "xref"]
        );
        assert_eq!(
            areas_for("selis_rejects", Some("[E1502] filter")),
            vec!["filters"]
        );
        assert_eq!(
            areas_for("selis_rejects", Some("[E1801] encrypted")),
            vec!["encryption"]
        );
        assert_eq!(
            areas_for("selis_rejects", Some("[E2301] content")),
            vec!["render"]
        );
        assert_eq!(
            areas_for("selis_rejects", Some("no code here")),
            Vec::<&str>::new()
        );
        assert_eq!(areas_for("diff>=25", None), vec!["render"]);
        assert_eq!(areas_for("match", None), Vec::<&str>::new());
        assert_eq!(areas_for("oracle_rejects", None), Vec::<&str>::new());
    }

    #[test]
    fn ppm_dimensions_peek_reads_the_header() {
        let mut ppm = b"P6\n7 5\n255\n".to_vec();
        ppm.extend_from_slice(&[0u8; 7 * 5 * 3]);
        assert_eq!(oracle::ppm_dimensions(&ppm), Ok((7, 5)));
        assert!(oracle::ppm_dimensions(b"junk").is_err());
    }

    #[test]
    fn corpus_ids_weight_wild_sources_up() {
        assert_eq!(file_weight("govdocs1/000009"), 3);
        assert_eq!(file_weight("wild/x"), 3);
        assert_eq!(file_weight("synthetic/basic-text"), 1);
        assert_eq!(source_of("verapdf/PDF_A-1b/file"), "verapdf");
        assert_eq!(source_of("160F-2019"), "flat");
    }

    // ── helpers ──

    /// An all-white image (the blank side).
    fn rgb_blank(w: u32, h: u32) -> Rgb {
        Rgb {
            width: w,
            height: h,
            data: vec![255; (w * h * 3) as usize],
        }
    }

    /// An image with a deterministic non-white pattern.
    fn rgb_paint(w: u32, h: u32) -> Rgb {
        let data = (0..(w * h * 3) as usize)
            .map(|i| u8::try_from((i * 37) % 251).unwrap_or(1))
            .collect();
        Rgb {
            width: w,
            height: h,
            data,
        }
    }
}
