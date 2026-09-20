//! Full-corpus differential text/font sweep (SL-3.CONF.01).
//!
//! Extracts page 1 of every corpus PDF with selis and with the named text
//! oracles, compares the normalised text (the [`crate::text_norm`]
//! normaliser N1–N6, similarity bands 0.99/0.95/0.75), records which fonts
//! each file uses (embedded / external-substitution-candidate / Type3, from
//! the oracle font inventory plus the fonts selis's own spans name), and
//! hashes selis's page-1 renders at every `--dpi` plus the normalised selis
//! text — the CORP.03 golden records, written to `golden.jsonl` for
//! `corpus expect-merge`.
//!
//! The same worker-pool / resume / include-exclude shape as the render sweep
//! (`xtask/src/sweep.rs`), but text-comparison lives here, in its own module,
//! and every shared helper is only *reused* from `sweep.rs` (visibility-only
//! reuse, no behaviour change there).
//!
//! Scope and honesty notes, for the G3 readout:
//! * page 1 per file (the CONF.01 scope); text extraction is
//!   DPI-independent, so `--dpi` only selects the golden-render resolutions,
//! * locally the available text oracle is MuPDF (`mutool draw -F txt`); the
//!   PDFium/pdf.js text legs run in the scheduled CI `render-conf` job
//!   through the pinned GHCR images (the drivers' `--text` modes), because
//!   Docker is not available on the dev host,
//! * every extract and render is the CLI's own per-file
//!   `Budget::profile(Surface::Viewer)` path, and additionally wall-clock
//!   bounded: a file that fails, exceeds the timeout, or extracts a monster
//!   page is a *typed outcome*, never a crash,
//! * font classes are the oracle's inventory truth (subset tag ⇒ embedded,
//!   `Type3` ⇒ Type3, anything else ⇒ external, i.e. selis substitutes or
//!   falls back — the heuristic is documented on
//!   [`crate::oracle::parse_mutool_fonts`], not hidden).
//!
//! Artifacts (`verdicts.jsonl` + `golden.jsonl` + `text-report.json`) go to
//! `--out` — never into the repo: sweep outputs are measurements, not
//! source. Only `corpus expect-merge` writes records into the repo.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::oracle::{self, FontClass, OracleFont};
use crate::sweep::{
    self, areas_for as sweep_areas_for, fail_text, file_weight, path_str, resolve_selis,
    run_with_timeout, sample_files, SideFail,
};
use crate::text_norm;

/// Similarity bands: `match` / `diff<5` / `diff<25` / `diff>=25`, the text
/// analog of the render sweep's 0.5/5/25% pixel bands.
const TEXT_MATCH_SIM: f64 = 0.99;
const TEXT_DIFF5_SIM: f64 = 0.95;
const TEXT_DIFF25_SIM: f64 = 0.75;

/// G3 per-file bar: ≥98% normalised extraction similarity (the Phase 3 gate
/// is "≥98% edit-distance agreement with PDFium on the extraction corpus").
const G3_MIN_SIM: f64 = 0.98;

/// Cap on a captured text-oracle stdout (the font inventory). Extraction
/// outputs go to files, never through a pipe, so only this small capture
/// needs a bound.
const MAX_FONTS_CAPTURE_BYTES: u64 = 4_000_000;

/// How many distinct font names a verdict carries per side (metadata-sized
/// records; the full inventory stays in the oracle output, not the verdict).
const MAX_FONTS_PER_VERDICT: usize = 32;

/// Signatures that count as "both sides extracted comparably".
const COMPARABLE: &[&str] = &[
    "match",
    "diff<5",
    "diff<25",
    "diff>=25",
    "empty_selis",
    "empty_oracle",
];

/// One (file, tool) text-comparison outcome, appended to `verdicts.jsonl`.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct TextVerdict {
    id: String,
    tool: String,
    sig: String,
    /// Normalised extraction similarity, when both sides extracted text.
    sim: Option<f64>,
    /// Normalised character counts per side (extraction volume, not quality).
    selis_chars: Option<u64>,
    oracle_chars: Option<u64>,
    /// The similarity ran over truncated inputs (text_norm cap).
    truncated: bool,
    /// Either side's normalised text is RTL-heavy (visual-vs-logical order
    /// risk; triaged separately per the normaliser docs).
    rtl: bool,
    /// Font resource names selis's page-1 spans name (`extract --format
    /// json`), bounded.
    selis_fonts: Vec<String>,
    /// The oracle's page-1 font inventory, bounded.
    oracle_fonts: Vec<OracleFont>,
    /// Any oracle font is external or Type3: divergence here is a
    /// substitution-driven candidate first, a recovery bug second.
    subst_candidate: bool,
    /// Typed-error or truncation detail; bounded, metadata only.
    detail: Option<String>,
}

/// The CORP.03 golden baseline of one file, appended to `golden.jsonl`:
/// selis's own page-1 renders hashed per DPI plus the normalised selis
/// text hash. `corpus expect-merge` records these into
/// `corpus/expect/<id>.toml`; `corpus verify --golden` re-checks them.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct GoldenRecord {
    id: String,
    /// Successful selis renders, DPI ("72"/"150"/"300") → sha256 of the PPM
    /// bytes the CLI wrote.
    render: BTreeMap<String, String>,
    /// Failed selis renders, DPI → typed error.
    render_err: BTreeMap<String, String>,
    /// sha256 of the normalised selis page-1 text (UTF-8), when extracted.
    text: Option<String>,
    /// Normalised selis character count.
    text_chars: u64,
    /// Why there is no text hash, when the extraction failed (as opposed to
    /// succeeding with no text, e.g. an image-only page).
    text_err: Option<String>,
}

/// Normalised text with its character count.
pub(crate) struct NormText {
    pub(crate) text: String,
    pub(crate) chars: u64,
}

// ── Signatures (SL-0.ORACLE.05: group by root cause, not by file) ──────────

/// The disagreement signature of one (file, tool) text outcome.
///
/// Precedence mirrors the render sweep: total failure first, then
/// empty-vs-text (one side found no text at all — a different root cause
/// from garbled text), then the similarity bands.
fn classify(
    selis: Result<&NormText, &SideFail>,
    oracle: Result<&NormText, &SideFail>,
    sim: Option<f64>,
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
        (Err(s), Ok(_)) => match s {
            SideFail::Timeout => ("selis_timeout", None),
            SideFail::Rejected(_) => ("selis_rejects", Some(fail_text(s))),
        },
        (Ok(_), Err(o)) => match o {
            SideFail::Timeout => ("oracle_timeout", None),
            SideFail::Rejected(_) => ("oracle_rejects", Some(fail_text(o))),
        },
        (Ok(s), Ok(o)) => {
            if s.chars == 0 && o.chars == 0 {
                return ("empty_both", None);
            }
            if s.chars == 0 {
                return ("empty_selis", None);
            }
            if o.chars == 0 {
                return ("empty_oracle", None);
            }
            let sim = sim.unwrap_or(0.0);
            if sim >= TEXT_MATCH_SIM {
                ("match", None)
            } else if sim >= TEXT_DIFF5_SIM {
                ("diff<5", None)
            } else if sim >= TEXT_DIFF25_SIM {
                ("diff<25", None)
            } else {
                ("diff>=25", None)
            }
        }
    }
}

/// Pairwise (oracle-vs-oracle calibration) classification: the same bands,
/// with side-agnostic failure names — which oracle failed goes in the
/// detail, so pairs aggregate.
fn classify_pair(
    a: Result<&NormText, &SideFail>,
    b: Result<&NormText, &SideFail>,
    sim: Option<f64>,
) -> (&'static str, Option<String>) {
    match (a, b) {
        (Err(x), Err(y)) => (
            "reject",
            Some(format!("a: {} | b: {}", fail_text(x), fail_text(y))),
        ),
        (Err(x), _) => match x {
            SideFail::Timeout => ("timeout", Some("a: wall-clock timeout".to_string())),
            SideFail::Rejected(_) => ("reject", Some(format!("a: {}", fail_text(x)))),
        },
        (_, Err(y)) => match y {
            SideFail::Timeout => ("timeout", Some("b: wall-clock timeout".to_string())),
            SideFail::Rejected(_) => ("reject", Some(format!("b: {}", fail_text(y)))),
        },
        (Ok(x), Ok(y)) => {
            if x.chars == 0 && y.chars == 0 {
                return ("empty_both", None);
            }
            if x.chars == 0 || y.chars == 0 {
                return ("empty", None);
            }
            let sim = sim.unwrap_or(0.0);
            if sim >= TEXT_MATCH_SIM {
                ("match", None)
            } else if sim >= TEXT_DIFF5_SIM {
                ("diff<5", None)
            } else if sim >= TEXT_DIFF25_SIM {
                ("diff<25", None)
            } else {
                ("diff>=25", None)
            }
        }
    }
}

// ── Process execution ──────────────────────────────────────────────────────

/// Run `program args...` with stdout redirected to `out_file` under a
/// wall-clock budget (extraction outputs bypass the pipe, so a monster page
/// cannot deadlock a full pipe buffer — it just fills disk in the worker
/// tmp dir, which the worker removes).
pub(crate) fn run_to_file(
    program: &Path,
    args: &[String],
    out_file: &Path,
    timeout: Duration,
) -> Result<(), SideFail> {
    let out = std::fs::File::create(out_file)
        .map_err(|e| SideFail::Rejected(format!("{}: {e}", out_file.display())))?;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::from(out))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SideFail::Rejected(format!("spawn failed: {e}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    if out_file.exists() {
                        return Ok(());
                    }
                    return Err(SideFail::Rejected("no output produced".to_string()));
                }
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read as _;
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

/// Run `program args...` capturing a *small* stdout (the font inventory),
/// refusing outputs above [`MAX_FONTS_CAPTURE_BYTES`].
fn run_capture_small(
    program: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<Vec<u8>, SideFail> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SideFail::Rejected(format!("spawn failed: {e}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                if let Some(pipe) = child.stdout.take() {
                    use std::io::Read as _;
                    // Bounded: the font inventory is small; a runaway
                    // stdout is a typed failure, not an allocation.
                    let mut limited = pipe.take(MAX_FONTS_CAPTURE_BYTES + 1);
                    let _ = limited.read_to_end(&mut stdout);
                }
                if u64::try_from(stdout.len()).unwrap_or(u64::MAX) > MAX_FONTS_CAPTURE_BYTES {
                    return Err(SideFail::Rejected("font inventory too large".to_string()));
                }
                if status.success() {
                    return Ok(stdout);
                }
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read as _;
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

/// sha256 hex of bytes (the CORP.03 golden hash function).
pub(crate) fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

/// Lowercase hex without dependencies beyond `sha2` (already vendored via
/// `selis-crypto`, so no new supply-chain surface).
fn hex_encode(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for &b in digest {
        out.push(HEX[usize::from(b >> 4)] as char);
        out.push(HEX[usize::from(b & 0x0f)] as char);
    }
    out
}

// ── The sweep itself ────────────────────────────────────────────────────────

/// Run the text/font sweep: `verdicts.jsonl` + `golden.jsonl` +
/// `text-report.json` into `cfg.out`. Resume is whole-file: a file counts
/// as done only when its golden record and every tool/pair verdict exist
/// (pair-only runs are one-shot and refuse `--resume`).
pub fn run(cfg: sweep::SweepConfig) -> Result<(), String> {
    std::fs::create_dir_all(&cfg.out).map_err(|e| format!("{}: {e}", cfg.out.display()))?;
    // Pair-only calibration (`--only-pair A+B`, the SL-0.ORACLE.04 baseline
    // mode) runs the named oracle legs and nothing else: no selis, no golden
    // renders, no font inventories. The rows it appends are exactly the
    // pair-verdict rows `--calibrate` emits — same schema, same metric.
    let pair_only = !cfg.only_pairs.is_empty();
    let (leg_tools, pair_labels) = if pair_only {
        pair_plan(&cfg)?
    } else {
        (cfg.tools.clone(), BTreeSet::new())
    };
    if pair_only && cfg.resume {
        return Err(
            "--only-pair does not support --resume (it is a one-shot calibration run)".to_string(),
        );
    }
    // Calibration legs still extract with selis (selis-vs-each-tool verdicts
    // are always emitted); oracle pairs are added on top. Pair-only
    // calibration never touches the engine.
    let pinned: Option<(PathBuf, String)> = if pair_only {
        None
    } else {
        let resolved = resolve_selis(cfg.selis.as_deref())?;
        // Pin a private copy of the selis binary into the output dir and drive
        // the whole sweep from the copy: the shared target dir is rebuilt by
        // other agents mid-sweep, and golden hashes are meaningless unless every
        // file was rendered/extracted by the identical binary. The copy's sha256
        // goes into the report as the baseline provenance.
        let selis_bin = cfg.out.join("selis-pinned.exe");
        if cfg.resume && selis_bin.exists() {
            // A resumed run keeps its original binary: re-pinning mid-sweep
            // would mix baselines inside one artifact set. Refuse a *different*
            // binary loudly instead of silently mixing.
            let pinned = sha256_hex_bytes(
                &std::fs::read(&selis_bin).map_err(|e| format!("{}: {e}", selis_bin.display()))?,
            );
            let fresh = sha256_hex_bytes(
                &std::fs::read(&resolved).map_err(|e| format!("{}: {e}", resolved.display()))?,
            );
            if pinned != fresh {
                return Err(format!(
                    "{} pins selis {pinned}, but --selis now resolves to {fresh}: \
                     delete the out dir for a new baseline",
                    cfg.out.display()
                ));
            }
        } else {
            std::fs::copy(&resolved, &selis_bin)
                .map_err(|e| format!("pin {}: {e}", selis_bin.display()))?;
        }
        let selis_digest = sha256_hex_bytes(
            &std::fs::read(&selis_bin).map_err(|e| format!("{}: {e}", selis_bin.display()))?,
        );
        Some((selis_bin, selis_digest))
    };
    let (selis_bin, selis_digest) = pinned.unwrap_or_else(|| (PathBuf::new(), String::new()));
    if cfg.calibrate && cfg.tools.len() < 2 {
        return Err(
            "calibration needs at least two text oracles (e.g. --tool mutool --tool pdfium)"
                .to_string(),
        );
    }
    let files = sample_files(cfg.sample, &cfg.include, &cfg.exclude)?;
    if files.is_empty() {
        return Err("no corpus PDFs found under corpus/pdfs".to_string());
    }
    let jobs = cfg.jobs.max(1);
    for w in 0..jobs {
        let tmp = cfg.out.join(format!("tmp-w{w}"));
        std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    }

    let verdict_path = cfg.out.join("verdicts.jsonl");
    let golden_path = cfg.out.join("golden.jsonl");
    let done = if cfg.resume {
        load_done_files(&verdict_path, &golden_path, &cfg.tools, cfg.calibrate)?
    } else {
        HashSet::new()
    };
    let verdict_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&verdict_path)
        .map_err(|e| format!("{}: {e}", verdict_path.display()))?;
    let golden_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&golden_path)
        .map_err(|e| format!("{}: {e}", golden_path.display()))?;
    let verdict_writer = Mutex::new(std::io::BufWriter::new(verdict_file));
    let golden_writer = Mutex::new(std::io::BufWriter::new(golden_file));
    let next = AtomicUsize::new(0);
    let done = Mutex::new(done);
    let dpis = cfg.dpis.clone();
    let tools = leg_tools.clone();
    let timeout = cfg.timeout;
    let out_dir = cfg.out.clone();
    let calibrate = cfg.calibrate;
    let pair_labels_ref = &pair_labels;

    std::thread::scope(|scope| {
        for w in 0..jobs {
            let tmp = out_dir.join(format!("tmp-w{w}"));
            let next_ref = &next;
            let files_ref = &files;
            let verdict_ref = &verdict_writer;
            let golden_ref = &golden_writer;
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
                    let id = oracle::corpus_id(file);
                    if done_ref.lock().is_ok_and(|mut d| !d.insert(id.clone())) {
                        continue;
                    }
                    let mut oracle_texts: Vec<(String, Result<NormText, SideFail>)> = Vec::new();
                    for tool in tools_ref {
                        oracle_texts
                            .push((tool.clone(), oracle_extract_text(tool, file, &tmp, timeout)));
                    }
                    if pair_only {
                        // Selected pairs only — the rows have the identical
                        // shape of a `--calibrate` pair verdict.
                        let verdicts: Vec<TextVerdict> = build_pair_verdicts(&id, &oracle_texts)
                            .into_iter()
                            .filter(|v| pair_labels_ref.contains(&v.tool))
                            .collect();
                        if let Ok(mut wr) = verdict_ref.lock() {
                            for v in &verdicts {
                                let _ = serde_json::to_writer(&mut *wr, v);
                                let _ = wr.write_all(b"\n");
                            }
                            let _ = wr.flush();
                        }
                        continue;
                    }
                    let (golden, selis_text) =
                        sweep_file_golden(selis_ref, dpis_ref, file, &tmp, timeout);
                    if let Ok(mut wr) = golden_ref.lock() {
                        let _ = serde_json::to_writer(&mut *wr, &golden);
                        let _ = wr.write_all(b"\n");
                        let _ = wr.flush();
                    }
                    let selis_fonts = selis_span_fonts(selis_ref, file, &tmp, timeout);
                    let mut verdicts = build_verdicts(
                        &id,
                        selis_text.as_ref(),
                        &oracle_texts,
                        &selis_fonts,
                        file,
                        timeout,
                    );
                    if calibrate {
                        verdicts.extend(build_pair_verdicts(&id, &oracle_texts));
                    }
                    if let Ok(mut wr) = verdict_ref.lock() {
                        for v in &verdicts {
                            let _ = serde_json::to_writer(&mut *wr, v);
                            let _ = wr.write_all(b"\n");
                        }
                        let _ = wr.flush();
                    }
                }
                let _ = std::fs::remove_dir_all(&tmp);
            });
        }
    });

    let verdicts = load_verdicts(&verdict_path)?;
    let goldens = load_goldens(&golden_path)?;
    let report_dpis: Vec<u32> = if pair_only {
        Vec::new()
    } else {
        cfg.dpis.clone()
    };
    let report = build_report(
        &selis_bin,
        &selis_digest,
        files.len(),
        &verdicts,
        &goldens,
        &leg_tools,
        &report_dpis,
        cfg.calibrate,
        pair_only,
    );
    let report_path = cfg.out.join("text-report.json");
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&report_path, json).map_err(|e| format!("{}: {e}", report_path.display()))?;
    print_report(&report);
    // SL-3.CONF.06: same fail-loud rule as the render sweep — the report is
    // written first so the artifacts survive, then a 0-comparable leg fails
    // the step instead of reporting green on an unmeasured leg.
    check_comparable_or_fail(&report, &verdicts)?;
    Ok(())
}

/// SL-3.CONF.06 — fail a text sweep that measured nothing.
///
/// Mirrors `sweep::check_comparable_or_fail`: any tool (or `a↔b` pair label)
/// with 0 comparable outcomes fails the `render-conf` text step, quoting the
/// first few typed details. `empty_both` pages are agreement on blank pages,
/// not failures, but they are not comparable either — a sample that is 100%
/// blank still measures nothing at the G3 bar, so it fails the same way.
fn check_comparable_or_fail(report: &TextReport, verdicts: &[TextVerdict]) -> Result<(), String> {
    let mut failures: Vec<String> = Vec::new();
    for (tool, tr) in &report.per_tool {
        if tr.comparable == 0 {
            let total = verdicts.iter().filter(|v| &v.tool == tool).count();
            let mut examples: Vec<String> = verdicts
                .iter()
                .filter(|v| &v.tool == tool)
                .filter_map(|v| v.detail.clone())
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .take(3)
                .collect();
            if examples.is_empty() {
                examples.push("(no typed detail recorded)".to_string());
            }
            let quoted = examples
                .iter()
                .map(|e| format!("`{e}`"))
                .collect::<Vec<_>>()
                .join(", ");
            failures.push(format!(
                "text leg `{tool}` produced 0 comparable pages of {total} outcome(s) — \
                 refusing green on an unmeasured leg (SL-3.CONF.06). e.g. {quoted}"
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Validate `--only-pair A+B` specs: the union of tools whose legs run, and
/// both label spellings of every requested pair (pair labels follow the
/// leg order, which the user does not control).
fn pair_plan(cfg: &sweep::SweepConfig) -> Result<(Vec<String>, BTreeSet<String>), String> {
    let mut legs: BTreeSet<String> = BTreeSet::new();
    let mut labels: BTreeSet<String> = BTreeSet::new();
    for (a, b) in &cfg.only_pairs {
        if a == b {
            return Err(format!(
                "--only-pair {a}+{b}: a pair needs two different tools"
            ));
        }
        for t in [a, b] {
            if !matches!(t.as_str(), "mutool" | "mupdf" | "pdfium" | "pdfjs") {
                return Err(format!(
                    "unknown text oracle tool `{t}` (mutool, pdfium, pdfjs)"
                ));
            }
            legs.insert(t.clone());
        }
        labels.insert(format!("{a}↔{b}"));
        labels.insert(format!("{b}↔{a}"));
    }
    Ok((legs.into_iter().collect(), labels))
}

/// The selis side of one file: page-1 renders hashed per DPI (golden) plus
/// the normalised page-1 text. Both the golden hash and the compared text
/// come from the same extraction, so they cannot silently diverge.
fn sweep_file_golden(
    selis_bin: &Path,
    dpis: &[u32],
    file: &Path,
    tmp: &Path,
    timeout: Duration,
) -> (GoldenRecord, Result<NormText, SideFail>) {
    let id = oracle::corpus_id(file);
    let mut render = BTreeMap::new();
    let mut render_err = BTreeMap::new();
    for dpi in dpis {
        let out = tmp.join(format!("golden-{dpi}.ppm"));
        let _ = std::fs::remove_file(&out);
        let outcome = render_selis(selis_bin, file, *dpi, &out, timeout)
            .and_then(|()| hash_file(&out).map_err(SideFail::Rejected));
        match outcome {
            Ok(hex) => {
                render.insert(dpi.to_string(), hex);
            }
            Err(fail) => {
                render_err.insert(dpi.to_string(), fail_text(&fail));
            }
        }
        let _ = std::fs::remove_file(&out);
    }
    let text_out = tmp.join("golden.txt");
    let _ = std::fs::remove_file(&text_out);
    let extracted = selis_extract_raw(selis_bin, file, &text_out, timeout);
    let _ = std::fs::remove_file(&text_out);
    // Empty-but-successful extraction (an image-only page) is not a failure:
    // there is no text to hash and nothing to compare — the verdict side
    // records it as `empty_*` via the zero character count.
    let (text, text_chars, text_err, verdict_side) = match extracted {
        Err(fail) => (None, 0, Some(fail_text(&fail)), Err(fail)),
        Ok(bytes) => match normalise_bytes(&bytes) {
            None => (
                None,
                0,
                None,
                Ok(NormText {
                    text: String::new(),
                    chars: 0,
                }),
            ),
            Some(norm) => (
                Some(sha256_hex_bytes(norm.text.as_bytes())),
                norm.chars,
                None,
                Ok(norm),
            ),
        },
    };
    (
        GoldenRecord {
            id,
            render,
            render_err,
            text,
            text_chars,
            text_err,
        },
        verdict_side,
    )
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

/// sha256 of a file's bytes (the golden render hash: hash of exactly what
/// the CLI wrote, so `verify --golden` reproduces it byte-for-byte).
fn hash_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(sha256_hex_bytes(&bytes))
}

/// Raw page-1 text bytes from `selis extract --format text`.
fn selis_extract_raw(
    selis_bin: &Path,
    file: &Path,
    out: &Path,
    timeout: Duration,
) -> Result<Vec<u8>, SideFail> {
    let args = vec![
        "extract".to_string(),
        "--format".to_string(),
        "text".to_string(),
        "--page".to_string(),
        "0".to_string(),
        path_str(file),
    ];
    run_to_file(selis_bin, &args, out, timeout)?;
    std::fs::read(out).map_err(|e| SideFail::Rejected(format!("{}: {e}", out.display())))
}

/// Lossy-decode raw extractor bytes (MuPDF can emit non-UTF-8 bytes from
/// broken encodings — mojibake stays visible, it just must not panic) and
/// normalise. `None` is an empty normalisation (blank page), not a failure.
fn normalise_bytes(bytes: &[u8]) -> Option<NormText> {
    let text = text_norm::normalize(&String::from_utf8_lossy(bytes));
    if text.is_empty() {
        return None;
    }
    Some(NormText {
        chars: u64::try_from(text.chars().count()).unwrap_or(u64::MAX),
        text,
    })
}

/// Font resource names selis's page-1 spans name (`extract --format json`).
/// An empty list on success means the page names no fonts (image-only page),
/// not a failure — the verdict path never depends on this leg.
fn selis_span_fonts(selis_bin: &Path, file: &Path, tmp: &Path, timeout: Duration) -> Vec<String> {
    let out = tmp.join("selis.json");
    let _ = std::fs::remove_file(&out);
    let args = vec![
        "extract".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--page".to_string(),
        "0".to_string(),
        path_str(file),
    ];
    let bytes = match run_to_file(selis_bin, &args, &out, timeout)
        .and_then(|()| std::fs::read(&out).map_err(|e| SideFail::Rejected(e.to_string())))
    {
        Ok(b) => b,
        Err(_) => {
            let _ = std::fs::remove_file(&out);
            return Vec::new();
        }
    };
    let _ = std::fs::remove_file(&out);
    span_font_names(&bytes)
}

/// `lines[].spans[].font` from `selis-extract/1` JSON, deduped, bounded.
fn span_font_names(json_bytes: &[u8]) -> Vec<String> {
    let parsed: serde_json::Value = match serde_json::from_slice(json_bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(lines) = parsed.get("lines").and_then(|l| l.as_array()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for line in lines {
        let Some(spans) = line.get("spans").and_then(|s| s.as_array()) else {
            continue;
        };
        for span in spans {
            if let Some(name) = span.get("font").and_then(|f| f.as_str()) {
                if !name.is_empty() && !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                    if names.len() >= MAX_FONTS_PER_VERDICT {
                        return names;
                    }
                }
            }
        }
    }
    names
}

/// Page-1 text from a text oracle: extract to a file, normalise, count.
/// An empty normalisation is a blank page, not a failure (mirrors the selis
/// side: the verdict records it as `empty_*` via the zero character count).
///
/// The tool's own stdout goes to a side log, never to the text output:
/// the leg plans write the extraction themselves (mutool `-o`, the pdfium
/// and pdf.js drivers `--text`), and their progress banners ("… driver:
/// extracted page 1 …") would otherwise land byte-wise in the middle of
/// the extracted text. (The render legs use `Stdio::null()` for the same
/// reason; the selis side of this module legitimately captures stdout,
/// because `selis extract` writes its text there.)
pub(crate) fn oracle_extract_text(
    tool: &str,
    file: &Path,
    tmp: &Path,
    timeout: Duration,
) -> Result<NormText, SideFail> {
    let out = tmp.join("oracle.txt");
    let log = tmp.join("oracle-stdout.log");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&log);
    let (program, args) = oracle::plan_oracle_text(tool, file, &out).map_err(SideFail::Rejected)?;
    run_to_file(&program, &args, &log, timeout).map_err(|fail| match fail {
        // mutool narrates progress (`page <file> 1`) to stderr ahead of the
        // real error; the verdict keeps the error, not the narration.
        SideFail::Rejected(stderr) => SideFail::Rejected(strip_oracle_progress(&stderr)),
        SideFail::Timeout => SideFail::Timeout,
    })?;
    if !out.exists() {
        let _ = std::fs::remove_file(&log);
        return Err(SideFail::Rejected("no output produced".to_string()));
    }
    let bytes =
        std::fs::read(&out).map_err(|e| SideFail::Rejected(format!("{}: {e}", out.display())))?;
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&log);
    Ok(normalise_bytes(&bytes).unwrap_or(NormText {
        text: String::new(),
        chars: 0,
    }))
}

/// Drop an oracle's stderr progress preamble (`page <file> <n>`, which
/// mutool prints before the actual error) so the verdict detail names the
/// failure. Lines that do not match the progress shape are kept verbatim —
/// an unrecognised preamble is data, not noise.
fn strip_oracle_progress(stderr: &str) -> String {
    let mut lines = stderr.lines();
    let first = lines.next().unwrap_or("");
    let is_progress = first.starts_with("page ")
        && first
            .rsplit_once(' ')
            .is_some_and(|(_, n)| n.trim().parse::<u32>().is_ok());
    if is_progress {
        lines.collect::<Vec<_>>().join("\n")
    } else {
        stderr.to_string()
    }
}

/// The oracle's page-1 font inventory (mutool leg only; other tools have no
/// font-inventory leg and report `None` — a documented gap, not a silent
/// empty list).
fn oracle_fonts(tool: &str, file: &Path, timeout: Duration) -> Option<Vec<OracleFont>> {
    let (program, args) = oracle::plan_oracle_fonts(tool, file).ok()?;
    let bytes = run_capture_small(&program, &args, timeout).ok()?;
    let mut fonts = oracle::parse_mutool_fonts(&String::from_utf8_lossy(&bytes), 1);
    fonts.truncate(MAX_FONTS_PER_VERDICT);
    Some(fonts)
}

/// selis-vs-each-tool verdicts for one file. The font legs (two more
/// processes) run only for comparable text outcomes, where
/// substitution-driven divergence is the triage question; rejections carry
/// no font data.
fn build_verdicts(
    id: &str,
    selis: Result<&NormText, &SideFail>,
    oracle_texts: &[(String, Result<NormText, SideFail>)],
    selis_fonts: &[String],
    file: &Path,
    timeout: Duration,
) -> Vec<TextVerdict> {
    let mut out = Vec::new();
    for (tool, otext) in oracle_texts {
        // No similarity when either side is empty: emptiness is its own
        // signature (`empty_*`), and a 1.0 over two blank pages would lie
        // in the CDF. Scoring goes through the shared policy
        // (`text_norm::capped_similarity`) — the same normalised distance
        // `oracle compare-text` reports.
        let sim = match (&selis, otext) {
            (Ok(s), Ok(o)) if s.chars > 0 && o.chars > 0 => {
                Some(text_norm::capped_similarity(&s.text, &o.text).0)
            }
            _ => None,
        };
        let (sig, mut detail) = classify(selis, otext.as_ref(), sim);
        let (selis_chars, oracle_chars) = match (&selis, otext) {
            (Ok(s), Ok(o)) => (Some(s.chars), Some(o.chars)),
            (Ok(s), Err(_)) => (Some(s.chars), None),
            (Err(_), Ok(o)) => (None, Some(o.chars)),
            _ => (None, None),
        };
        let truncated = match (&selis, otext) {
            (Ok(s), Ok(o)) => text_norm::would_truncate(&s.text, &o.text),
            _ => false,
        };
        if truncated {
            detail = Some("similarity over truncated inputs".to_string());
        }
        let rtl = match (&selis, otext) {
            (Ok(s), Ok(o)) => text_norm::rtl_heavy(&s.text) || text_norm::rtl_heavy(&o.text),
            (Ok(s), Err(_)) => text_norm::rtl_heavy(&s.text),
            (Err(_), Ok(o)) => text_norm::rtl_heavy(&o.text),
            _ => false,
        };
        let (oracle_fonts, subst_candidate) = if COMPARABLE.contains(&sig) {
            match oracle_fonts(tool, file, timeout) {
                Some(fonts) => {
                    let subst = fonts
                        .iter()
                        .any(|f| f.class == FontClass::External || f.class == FontClass::Type3);
                    (fonts, subst)
                }
                None => (Vec::new(), false),
            }
        } else {
            (Vec::new(), false)
        };
        out.push(TextVerdict {
            id: id.to_string(),
            tool: tool.clone(),
            sig: sig.to_string(),
            sim,
            selis_chars,
            oracle_chars,
            truncated,
            rtl,
            selis_fonts: selis_fonts.to_vec(),
            oracle_fonts,
            subst_candidate,
            detail,
        });
    }
    out
}

/// Oracle-vs-oracle pair verdicts (calibration): every unordered tool pair
/// under the identical metric, text-only (no font legs on pairs).
fn build_pair_verdicts(
    id: &str,
    oracle_texts: &[(String, Result<NormText, SideFail>)],
) -> Vec<TextVerdict> {
    let mut out = Vec::new();
    for x in 0..oracle_texts.len() {
        for y in (x + 1)..oracle_texts.len() {
            let (aname, atext) = &oracle_texts[x];
            let (bname, btext) = &oracle_texts[y];
            let sim = match (atext, btext) {
                (Ok(a), Ok(b)) if a.chars > 0 && b.chars > 0 => {
                    Some(text_norm::capped_similarity(&a.text, &b.text).0)
                }
                _ => None,
            };
            let (sig, detail) = classify_pair(atext.as_ref(), btext.as_ref(), sim);
            let (achars, bchars) = match (atext, btext) {
                (Ok(a), Ok(b)) => (Some(a.chars), Some(b.chars)),
                (Ok(a), Err(_)) => (Some(a.chars), None),
                (Err(_), Ok(b)) => (None, Some(b.chars)),
                _ => (None, None),
            };
            out.push(TextVerdict {
                id: id.to_string(),
                tool: format!("{aname}↔{bname}"),
                sig: sig.to_string(),
                sim,
                selis_chars: achars,
                oracle_chars: bchars,
                truncated: false,
                rtl: false,
                selis_fonts: Vec::new(),
                oracle_fonts: Vec::new(),
                subst_candidate: false,
                detail,
            });
        }
    }
    out
}

/// File ids fully recorded by a previous run (for `--resume`): the golden
/// record plus every tool verdict, plus every pair verdict in calibration
/// mode. Anything less and the file is re-swept whole.
fn load_done_files(
    verdict_path: &Path,
    golden_path: &Path,
    tools: &[String],
    calibrate: bool,
) -> Result<HashSet<String>, String> {
    let mut verdict_keys: HashSet<String> = HashSet::new();
    for v in load_verdicts(verdict_path)? {
        verdict_keys.insert(format!("{}|{}", v.id, v.tool));
    }
    let mut done = HashSet::new();
    for g in load_goldens(golden_path)? {
        let mut complete = true;
        for tool in tools {
            if !verdict_keys.contains(&format!("{}|{tool}", g.id)) {
                complete = false;
                break;
            }
        }
        if complete && calibrate {
            for x in 0..tools.len() {
                for y in (x + 1)..tools.len() {
                    if !verdict_keys.contains(&format!("{}|{}↔{}", g.id, tools[x], tools[y])) {
                        complete = false;
                        break;
                    }
                }
            }
        }
        if complete {
            done.insert(g.id);
        }
    }
    Ok(done)
}

fn load_verdicts(path: &Path) -> Result<Vec<TextVerdict>, String> {
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

fn load_goldens(path: &Path) -> Result<Vec<GoldenRecord>, String> {
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
struct TextReport {
    task: &'static str,
    scope: &'static str,
    selis: String,
    /// sha256 of the pinned selis binary that produced every golden hash
    /// (baselines are meaningless without the producer identity).
    selis_digest: String,
    tools: Vec<String>,
    dpis: Vec<u32>,
    files: usize,
    normaliser: &'static str,
    /// This report tallies `--calibrate`-shaped pair rows only (the
    /// `--only-pair` calibration mode; the selis rows are absent).
    pair_only: bool,
    bands_sim: [f64; 3],
    per_tool: BTreeMap<String, TextToolReport>,
    golden: GoldenSummary,
}

#[derive(Serialize, Default)]
struct TextToolReport {
    comparable: u64,
    outcomes: BTreeMap<String, u64>,
    /// Distribution of (1 − similarity) × 100 over comparable outcomes with
    /// a similarity — the text CDF, shaped like the render sweep's diff-pct
    /// CDF so the two readouts compare directly.
    diff_stats: BTreeMap<String, DiffStatsView>,
    /// Per-band file counts (≥0.99 / ≥0.98 / ≥0.95 / ≥0.75 similarity).
    bands: BTreeMap<String, u64>,
    /// Agreement at the G3 bar (≥0.98) by corpus source.
    sources: BTreeMap<String, SourceAgreement>,
    /// Agreement at the G3 bar by oracle font class (substitution analysis).
    font_classes: BTreeMap<String, FontClassAgreement>,
    clusters: Vec<TextCluster>,
    g3: G3Readout,
}

#[derive(Serialize, Default)]
struct DiffStatsView {
    n: u64,
    /// Arithmetic mean of the sample — the single scalar the oracle-vs-oracle
    /// baseline tables quote (the CDF percentiles stay the shape readout).
    mean: Option<f64>,
    p50: Option<f64>,
    p75: Option<f64>,
    p90: Option<f64>,
    p95: Option<f64>,
    p99: Option<f64>,
    max: Option<f64>,
}

/// Nearest-rank percentile of a sorted sample (mirrors the render sweep's
/// CDF so the two readouts compare directly).
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil();
    let idx = rank.max(1.0) as usize - 1;
    sorted.get(idx.min(sorted.len() - 1)).copied()
}

fn diff_stats(values: &mut [f64]) -> DiffStatsView {
    values.sort_by(|a, b| a.total_cmp(b));
    let round2 = |v: f64| (v * 100.0).round() / 100.0;
    let mean = if values.is_empty() {
        None
    } else {
        let sum: f64 = values.iter().sum();
        Some(round2(sum / values.len() as f64))
    };
    DiffStatsView {
        n: u64::try_from(values.len()).unwrap_or(u64::MAX),
        mean,
        p50: percentile(values, 50.0).map(round2),
        p75: percentile(values, 75.0).map(round2),
        p90: percentile(values, 90.0).map(round2),
        p95: percentile(values, 95.0).map(round2),
        p99: percentile(values, 99.0).map(round2),
        max: values.last().copied().map(round2),
    }
}

#[derive(Serialize, Default)]
struct SourceAgreement {
    comparable: u64,
    within_g3: u64,
}

#[derive(Serialize, Default)]
struct FontClassAgreement {
    files: u64,
    within_g3: u64,
}

#[derive(Serialize)]
struct TextCluster {
    signature: String,
    outcomes: u64,
    files: u64,
    weight: u64,
    areas: Vec<&'static str>,
    examples: Vec<String>,
}

#[derive(Serialize, Default)]
struct G3Readout {
    criterion: String,
    oracle: String,
    comparable: u64,
    within_g3: u64,
    agreement_pct: f64,
    met: bool,
}

#[derive(Serialize, Default)]
struct GoldenSummary {
    files: usize,
    full_render_text: u64,
    partial: u64,
    none: u64,
}

/// Aggregate verdicts + goldens into the report.
#[allow(clippy::too_many_arguments)]
fn build_report(
    selis_bin: &Path,
    selis_digest: &str,
    files: usize,
    verdicts: &[TextVerdict],
    goldens: &[GoldenRecord],
    tools: &[String],
    dpis: &[u32],
    calibrate: bool,
    pair_only: bool,
) -> TextReport {
    let mut per_tool: BTreeMap<String, TextToolReport> = BTreeMap::new();
    let mut diffs_by_tool: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for v in verdicts {
        let tr = per_tool.entry(v.tool.clone()).or_default();
        *tr.outcomes.entry(v.sig.clone()).or_default() += 1;
        if !COMPARABLE.contains(&v.sig.as_str()) {
            continue;
        }
        tr.comparable += 1;
        if let Some(sim) = v.sim {
            diffs_by_tool
                .entry(v.tool.clone())
                .or_default()
                .push((1.0 - sim) * 100.0);
            for (band, min) in [
                ("ge99", TEXT_MATCH_SIM),
                ("ge98", G3_MIN_SIM),
                ("ge95", TEXT_DIFF5_SIM),
                ("ge75", TEXT_DIFF25_SIM),
            ] {
                if sim >= min {
                    *tr.bands.entry(band.to_string()).or_default() += 1;
                }
            }
            let within = sim >= G3_MIN_SIM;
            let sa = tr.sources.entry(source_of(&v.id)).or_default();
            sa.comparable += 1;
            if within {
                sa.within_g3 += 1;
            }
            let class = font_class_key(v);
            let fa = tr.font_classes.entry(class).or_default();
            fa.files += 1;
            if within {
                fa.within_g3 += 1;
            }
        }
    }
    for (tool, mut values) in diffs_by_tool {
        if let Some(tr) = per_tool.get_mut(&tool) {
            tr.diff_stats
                .insert("all".to_string(), diff_stats(&mut values));
        }
    }
    let mut report = TextReport {
        task: if calibrate || pair_only {
            "SL-3.CONF.01-calibration"
        } else {
            "SL-3.CONF.01"
        },
        scope: if pair_only {
            "page 1 text of each corpus file, selected oracle-vs-oracle pairs \
             only (normaliser N1-N6) — the SL-0.ORACLE.04 text calibration mode"
        } else {
            "page 1 text of each corpus file, selis vs oracle extraction \
             (normaliser N1-N6), plus selis golden renders per DPI"
        },
        selis: selis_bin.display().to_string(),
        selis_digest: selis_digest.to_string(),
        tools: tools.to_vec(),
        dpis: dpis.to_vec(),
        files,
        normaliser: "N1-N6 (xtask/src/text_norm.rs)",
        pair_only,
        bands_sim: [TEXT_MATCH_SIM, TEXT_DIFF5_SIM, TEXT_DIFF25_SIM],
        per_tool,
        golden: golden_summary(goldens, dpis),
    };
    let is_calib = calibrate || pair_only;
    for (tool, tr) in report.per_tool.iter_mut() {
        tr.clusters = ranked_clusters(tool, verdicts);
        let within = tr.bands.get("ge98").copied().unwrap_or(0);
        let agreement = if tr.comparable > 0 {
            within as f64 / tr.comparable as f64 * 100.0
        } else {
            0.0
        };
        let criterion = if is_calib && tool.contains('↔') {
            "pairwise oracle agreement under the CONF.01 text metric — the G3 \
             98% bar applies to selis, not to the oracles; read the CDF instead"
                .to_string()
        } else {
            format!(
                ">= {G3_MIN_SIM} normalised extraction similarity on >= 95% of \
                 the extraction corpus (G3 gate)"
            )
        };
        tr.g3 = G3Readout {
            criterion,
            oracle: oracle_identity(tool),
            comparable: tr.comparable,
            within_g3: within,
            agreement_pct: (agreement * 100.0).round() / 100.0,
            met: !is_calib && tr.comparable > 0 && agreement >= 95.0,
        };
    }
    report
}

/// The oracle font-class key of a verdict for the substitution analysis:
/// files whose oracle inventory holds a Type3 font, an external
/// (substitution-candidate) font, only embedded fonts, or no inventory.
fn font_class_key(v: &TextVerdict) -> String {
    if v.oracle_fonts.is_empty() {
        return "unknown".to_string();
    }
    if v.oracle_fonts.iter().any(|f| f.class == FontClass::Type3) {
        return "has_type3".to_string();
    }
    if v.subst_candidate {
        return "has_external".to_string();
    }
    "all_embedded".to_string()
}

fn golden_summary(goldens: &[GoldenRecord], dpis: &[u32]) -> GoldenSummary {
    let mut summary = GoldenSummary {
        files: goldens.len(),
        ..Default::default()
    };
    for g in goldens {
        let full_render = dpis.iter().all(|d| g.render.contains_key(&d.to_string()));
        let has_text = g.text.is_some();
        if full_render && has_text {
            summary.full_render_text += 1;
        } else if g.render.is_empty() && g.text.is_none() {
            summary.none += 1;
        } else {
            summary.partial += 1;
        }
    }
    summary
}

/// Ranked clusters for one tool: signature → files affected × corpus weight
/// (wild/govdocs ×3, SL-0.ORACLE.05 step 3), then signature for stability.
fn ranked_clusters(tool: &str, verdicts: &[TextVerdict]) -> Vec<TextCluster> {
    let mut sig_ids: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    let mut sig_count: BTreeMap<String, u64> = BTreeMap::new();
    for v in verdicts.iter().filter(|v| v.tool == tool) {
        sig_ids
            .entry(v.sig.clone())
            .or_default()
            .insert(v.id.clone());
        *sig_count.entry(v.sig.clone()).or_default() += 1;
    }
    let mut clusters: Vec<TextCluster> = sig_ids
        .into_iter()
        .map(|(sig, ids)| {
            let detail_example = verdicts
                .iter()
                .find(|v| v.tool == tool && v.sig == sig)
                .and_then(|v| v.detail.clone());
            let areas: Vec<&'static str> = match sig.as_str() {
                "match" | "both_reject" | "empty_both" => vec![],
                "oracle_rejects" | "oracle_timeout" | "reject" | "timeout" => vec![],
                "selis_rejects" | "selis_timeout" => {
                    sweep_areas_for(&sig, detail_example.as_deref())
                }
                _ => vec!["text"],
            };
            TextCluster {
                outcomes: sig_count.get(&sig).copied().unwrap_or(0),
                files: u64::try_from(ids.len()).unwrap_or(u64::MAX),
                weight: ids
                    .iter()
                    .map(|id| u64::try_from(file_weight(id)).unwrap_or(u64::MAX))
                    .sum(),
                areas,
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

/// The corpus source of a file id: its first path segment (`flat` when the
/// id has none, matching the extraction layouts of SL-0.CORP.01).
fn source_of(id: &str) -> String {
    match id.split_once('/') {
        Some((src, _)) => src.to_string(),
        None => "flat".to_string(),
    }
}

/// The comparable identity of a text oracle tool.
fn oracle_identity(tool: &str) -> String {
    if tool.contains('↔') {
        return format!("pairwise text comparison ({tool})");
    }
    match tool {
        "mutool" | "mupdf" => "MuPDF mutool draw -F txt (local install; pinned container: \
                               xtask/oracles.toml [tool.mupdf])"
            .to_string(),
        "pdfium" => "PDFium FPDFText (pinned container ghcr.io/wertcore/selis-pdf/oracle-pdfium)"
            .to_string(),
        "pdfjs" => "pdf.js getTextContent (pinned container \
                    ghcr.io/wertcore/selis-pdf/oracle-pdfjs)"
            .to_string(),
        other => other.to_string(),
    }
}

/// The ranked triage view, straight to stdout (the ORACLE.05 cluster list).
fn print_report(report: &TextReport) {
    if report.pair_only {
        println!(
            "text pair sweep: {} corpus files, oracle-vs-oracle pairs \
             (tools {:?}), no selis/golden legs",
            report.files, report.tools
        );
    } else {
        println!(
            "text sweep: {} corpus files, golden DPIs {:?}, tools {:?}",
            report.files, report.dpis, report.tools
        );
        println!("  selis: {} (sha256:{})", report.selis, report.selis_digest);
        println!(
            "  golden: {} full render+text, {} partial, {} none (of {} files)",
            report.golden.full_render_text,
            report.golden.partial,
            report.golden.none,
            report.golden.files,
        );
    }
    for (tool, tr) in &report.per_tool {
        println!(
            "  oracle {tool}: comparable={} (G3: {}/{} = {:.2}% >= 0.98 — met: {})",
            tr.comparable, tr.g3.within_g3, tr.g3.comparable, tr.g3.agreement_pct, tr.g3.met,
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
        if let Some(stats) = tr.diff_stats.get("all") {
            println!(
                "    (1-sim)*100 CDF: n={} mean={:?} p50={:?} p75={:?} p90={:?} p95={:?} p99={:?} max={:?}",
                stats.n, stats.mean, stats.p50, stats.p75, stats.p90, stats.p95, stats.p99,
                stats.max
            );
        }
        for (class, fa) in &tr.font_classes {
            println!(
                "    fonts {class}: {}/{} files within G3",
                fa.within_g3, fa.files
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> NormText {
        let text = text_norm::normalize(s);
        let chars = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
        NormText { text, chars }
    }

    #[test]
    fn classify_bands_match_the_g3_bar() {
        let a = norm("the quick brown fox jumps over the lazy dog");
        let b = norm("the quick brown fox jumps over the lazy dog");
        let (sig, _) = classify(Ok(&a), Ok(&b), Some(1.0));
        assert_eq!(sig, "match");
        let (sig, _) = classify(Ok(&a), Ok(&b), Some(0.96));
        assert_eq!(sig, "diff<5");
        let (sig, _) = classify(Ok(&a), Ok(&b), Some(0.80));
        assert_eq!(sig, "diff<25");
        let (sig, _) = classify(Ok(&a), Ok(&b), Some(0.10));
        assert_eq!(sig, "diff>=25");
    }

    #[test]
    fn classify_puts_empty_before_bands() {
        let full = norm("some text here");
        let empty = norm("   ");
        let (sig, _) = classify(Ok(&empty), Ok(&full), Some(0.0));
        assert_eq!(sig, "empty_selis");
        let (sig, _) = classify(Ok(&full), Ok(&empty), Some(0.0));
        assert_eq!(sig, "empty_oracle");
        let (sig, _) = classify(Ok(&empty), Ok(&empty), Some(1.0));
        assert_eq!(sig, "empty_both");
    }

    #[test]
    fn classify_reports_timeouts_as_their_own_signatures() {
        let full = norm("some text here");
        let timeout = SideFail::Timeout;
        let rejected = SideFail::Rejected("boom".to_string());
        let (sig, _) = classify(Err(&timeout), Ok(&full), None);
        assert_eq!(sig, "selis_timeout");
        let (sig, _) = classify(Ok(&full), Err(&timeout), None);
        assert_eq!(sig, "oracle_timeout");
        let (sig, _) = classify(Err(&rejected), Err(&rejected), None);
        assert_eq!(sig, "both_reject");
    }

    #[test]
    fn span_font_names_read_selis_extract_json() {
        let json = br#"{"schema": "selis-extract/1", "lines": [
          {"bbox": [0, 0, 1, 1], "spans": [
            {"text": "hi", "font": "F1", "size": 12.00, "bbox": [0, 0, 1, 1]},
            {"text": "yo", "font": "F1", "size": 12.00, "bbox": [0, 0, 1, 1]},
            {"text": "!", "font": "F2", "size": 10.00, "bbox": [0, 0, 1, 1]}
          ]}
        ]}"#;
        assert_eq!(
            span_font_names(json),
            vec!["F1".to_string(), "F2".to_string()]
        );
        assert!(span_font_names(b"not json").is_empty());
        assert!(span_font_names(b"{}").is_empty());
    }

    #[test]
    fn golden_hash_is_sha256_hex() {
        // The empty-string SHA-256 test vector pins the hash function.
        assert_eq!(
            sha256_hex_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(hex_encode(&[0xabu8, 0x00, 0xff]), "ab00ff");
    }

    #[test]
    fn font_class_key_separates_substitution_candidates() {
        let embedded = OracleFont {
            name: "ABCDEF+Arial".to_string(),
            ftype: "TrueType".to_string(),
            class: FontClass::Embedded,
        };
        let external = OracleFont {
            name: "Helvetica".to_string(),
            ftype: "Type1".to_string(),
            class: FontClass::External,
        };
        let type3 = OracleFont {
            name: String::new(),
            ftype: "Type3".to_string(),
            class: FontClass::Type3,
        };
        let verdict = |fonts: Vec<OracleFont>, subst: bool| TextVerdict {
            id: "x".to_string(),
            tool: "mutool".to_string(),
            sig: "match".to_string(),
            sim: Some(1.0),
            selis_chars: Some(1),
            oracle_chars: Some(1),
            truncated: false,
            rtl: false,
            selis_fonts: Vec::new(),
            oracle_fonts: fonts,
            subst_candidate: subst,
            detail: None,
        };
        assert_eq!(
            font_class_key(&verdict(vec![embedded], false)),
            "all_embedded"
        );
        assert_eq!(
            font_class_key(&verdict(vec![external], true)),
            "has_external"
        );
        assert_eq!(font_class_key(&verdict(vec![type3], true)), "has_type3");
        assert_eq!(font_class_key(&verdict(Vec::new(), false)), "unknown");
    }

    #[test]
    fn classify_pair_mirrors_the_text_bands() {
        let a = norm("the quick brown fox");
        let b = norm("the quick brown fox");
        let (sig, _) = classify_pair(Ok(&a), Ok(&b), Some(1.0));
        assert_eq!(sig, "match");
        let fail = SideFail::Rejected("cannot authenticate".to_string());
        let (sig, _) = classify_pair(Err(&fail), Err(&fail), None);
        assert_eq!(sig, "reject");
        let empty = norm("");
        let (sig, _) = classify_pair(Ok(&a), Ok(&empty), Some(0.0));
        assert_eq!(sig, "empty");
    }

    #[test]
    fn oracle_progress_preamble_is_stripped_but_errors_survive() {
        assert_eq!(
            strip_oracle_progress(
                "page C:\\x\\y.pdf 1\nerror: cannot authenticate password: no password supplied"
            ),
            "error: cannot authenticate password: no password supplied"
        );
        // No preamble: verbatim.
        assert_eq!(
            strip_oracle_progress("warning: trying to repair broken xref"),
            "warning: trying to repair broken xref"
        );
        // A first line that merely starts with "page" but is not the
        // progress shape is data, not noise.
        assert_eq!(
            strip_oracle_progress("page tree is broken"),
            "page tree is broken"
        );
    }

    /// SL-3.CONF.06 DoD: a synthetic all-`oracle_rejects` text leg turns the
    /// job red instead of reporting green on an unmeasured leg.
    #[test]
    fn zero_comparable_text_leg_fails_with_typed_details() {
        let verdict = |id: &str| TextVerdict {
            id: id.to_string(),
            tool: "pdfium".to_string(),
            sig: "oracle_rejects".to_string(),
            sim: None,
            selis_chars: Some(10),
            oracle_chars: None,
            truncated: false,
            rtl: false,
            selis_fonts: Vec::new(),
            oracle_fonts: Vec::new(),
            subst_candidate: false,
            detail: Some("pdfium: no [tool.pdfium] pin recorded".to_string()),
        };
        let verdicts = vec![verdict("a"), verdict("b")];
        let report = build_report(
            Path::new("selis"),
            "deadbeef",
            2,
            &verdicts,
            &[],
            &["pdfium".to_string()],
            &[72, 150, 300],
            false,
            false,
        );
        assert_eq!(report.per_tool["pdfium"].comparable, 0);
        let err = check_comparable_or_fail(&report, &verdicts).expect_err("0 comparable must fail");
        assert!(err.contains("pdfium"), "names the leg: {err}");
        assert!(err.contains("0 comparable"), "states the count: {err}");
        assert!(err.contains("no [tool.pdfium] pin"), "quotes detail: {err}");
    }

    #[test]
    fn nonzero_comparable_text_leg_stays_green() {
        let verdicts = vec![TextVerdict {
            id: "a".to_string(),
            tool: "pdfium".to_string(),
            sig: "match".to_string(),
            sim: Some(1.0),
            selis_chars: Some(10),
            oracle_chars: Some(10),
            truncated: false,
            rtl: false,
            selis_fonts: Vec::new(),
            oracle_fonts: Vec::new(),
            subst_candidate: false,
            detail: None,
        }];
        let report = build_report(
            Path::new("selis"),
            "deadbeef",
            1,
            &verdicts,
            &[],
            &["pdfium".to_string()],
            &[72, 150, 300],
            false,
            false,
        );
        assert_eq!(report.per_tool["pdfium"].comparable, 1);
        check_comparable_or_fail(&report, &verdicts).expect("measured leg stays green");
    }
}
