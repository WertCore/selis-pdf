//! SL-1.ROB.01 — wild-corpus open sweep.
//!
//! Opens every local corpus PDF under a fresh per-file Viewer [`Budget`],
//! classifies every non-open outcome by its typed error code (grouped by
//! `(code, during)` as a root-cause proxy), and writes a machine-readable
//! report:
//!
//! ```text
//! cargo test -p selis-cli --test rob01_baseline -- --ignored --nocapture
//! ```
//!
//! The report lands in `target/rob01-report.json` (wall clock, per-file budget
//! verdicts, outcome counts by code, and the residual file list). Every open
//! runs under the SL-0.ERR.03 panic trampoline: a parser bug becomes a typed
//! `INTERNAL_PANIC` outcome instead of killing the sweep, so the DoD's
//! "0 panics" is measured rather than assumed. Each file's open also runs on a
//! spawned thread so a hang cannot take the whole sweep with it; the file is
//! recorded as `hang` after the wait and the sweep continues.
//!
//! Full DoD note: the 10k wild fetch is disk-bound (SL-0.CORP.05) and is NOT
//! fetched here; this sweep covers the locally available corpus
//! (fetched seeds + veraPDF + Ghent + govdocs1 + synthetic).
//!
//! The corpus is fetched/extracted locally and never committed
//! (SL-0.LEGAL.04), so this test is `#[ignore]`d and skipped when
//! `corpus/pdfs` is empty. Expectations under `corpus/expect/` (SL-0.CORP.03)
//! are consulted when present: by exact relative id first, then — for corpora
//! whose extraction layout differs from the expectation layout (veraPDF) — by
//! unambiguous basename. A file whose outcome differs from its recorded
//! expectation is reported as drift, never silently accepted.
//!
//! The open-rate floor below ratchets upward as root causes are fixed; it
//! guards the campaign against silent regressions.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use selis_sandbox::{Budget, Surface};
use std::io::Write as _;
use std::sync::mpsc;
use std::time::Duration;

/// The minimum fraction of the local corpus that must open under the Viewer
/// budget. History of the wild/fetched flat corpus (977 files): ratcheted
/// 897 → 919 → 956 → 963 → 966 → 973 over the SL-1.ROB.01 campaign. The full
/// 10k wild sweep (SL-0.CORP.05) is pending the disk-bound fetch; this floor
/// tracks the local corpus until then.
const OPEN_RATE_FLOOR: f64 = 0.99;

/// Extra seconds a single file may exceed the Viewer budget's own wall limit
/// before the sweep declares it a hang. The engine checks its deadline at
/// every `tick`, so a healthy budget failure is well inside this slack.
const HANG_SLACK_SECS: u64 = 30;

/// One file's sweep outcome.
#[derive(serde::Serialize, Clone)]
struct FileOutcome {
    /// Corpus id (path relative to `corpus/pdfs`, no `.pdf`).
    id: String,
    /// `ok`, `err`, `hang`, or `read-error`.
    outcome: &'static str,
    /// The typed error code name, when the outcome is `err`.
    code: Option<String>,
    /// What the engine was doing, when recorded.
    during: Option<String>,
    /// File size in bytes (a structural fact, not content).
    bytes: u64,
    /// Milliseconds the open took (includes the read).
    millis: u128,
    /// Whether the open terminated within the Viewer wall budget plus the
    /// hang slack.
    within_budget_wall: bool,
    /// Whether the outcome differs from the recorded expectation.
    expectation_drift: bool,
    /// How the expectation was matched: `exact`, `basename`, or absent.
    expect_match: &'static str,
}

/// A recorded expectation: the expected `open` value and error code.
struct Expectation {
    open: String,
    code: Option<String>,
}

/// Load every `corpus/expect/**/*.toml` as `(relative id, record)`.
fn load_expectations(root: &std::path::Path) -> Vec<(String, Expectation)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "toml") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/")
                    .replace(".toml", "");
                let Some(text) = std::fs::read_to_string(&path).ok() else {
                    continue;
                };
                let Some((open, code)) = parse_expect(&text) else {
                    continue;
                };
                out.push((rel, Expectation { open, code }));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Parse one expectation record: the `open` value and optional error `code`.
fn parse_expect(text: &str) -> Option<(String, Option<String>)> {
    let mut open = None;
    let mut code = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("open = ") {
            open = Some(v.trim_matches('"').to_owned());
        } else if let Some(v) = line.strip_prefix("code = ") {
            code = Some(v.trim_matches('"').to_owned());
        }
    }
    open.map(|o| (o, code))
}

/// Normalise an expectation's code spelling to the registry name: the
/// generated enum's `Debug` is `CamelCase`, the registry's `name()` is
/// `SCREAMING_SNAKE`; both identify the same code.
fn expect_code_normalised(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            let prev = name.chars().nth(i - 1).unwrap_or(ch);
            if !prev.is_ascii_uppercase() {
                out.push('_');
            }
        }
        for up in ch.to_uppercase() {
            out.push(up);
        }
    }
    out
}

/// Recursively collect `*.pdf` files under `dir`.
fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|x| x == "pdf") {
            out.push(path);
        }
    }
}

fn corpus_dir() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("corpus");
    p.push("pdfs");
    p
}

/// The workspace root (`target/rob01-report.json` lives there).
fn workspace_root() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p
}

fn expect_dir() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("corpus");
    p.push("expect");
    p
}

/// Open one file on a spawned thread with its own Viewer budget and the panic
/// trampoline, so neither a panic nor a hang can poison the sweep.
///
/// The thread is deliberately leaked when it hangs: a hung `Session::open`
/// cannot be killed from inside the process, so the sweep abandons it and
/// keeps measuring. The budget's `bytes`/`objects`/`depth` limits bound what
/// the abandoned thread can hold.
fn open_one(path: &std::path::Path, root: &std::path::Path, wall: Duration) -> FileOutcome {
    let id = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .replace(".pdf", "");
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let started = std::time::Instant::now();
    let (tx, rx) = mpsc::channel();
    let path = path.to_owned();
    std::thread::Builder::new()
        .name(format!(
            "rob01-open:{}",
            id.rsplit('/').next().unwrap_or(&id)
        ))
        .spawn(move || {
            // A fresh per-file Budget: one document can never consume
            // another file's allowance (ADR-P0006).
            let budget = Budget::profile(Surface::Viewer);
            let Ok(src) = std::fs::read(&path) else {
                let _ = tx.send(("read-error", Some("IO_READ_FAILED".to_owned()), None));
                return;
            };
            let result = selis_sandbox::trampoline::catch("rob01-open", || {
                selis_pdf_engine::Session::open(src, &budget)
            });
            match result {
                Ok(_) => {
                    let _ = tx.send(("ok", None, None));
                }
                Err(e) => {
                    let _ = tx.send((
                        "err",
                        Some(e.code().name().to_owned()),
                        e.ctx().during.map(str::to_owned),
                    ));
                }
            }
        })
        .expect("spawn open worker");

    let (outcome, code, during): (&'static str, Option<String>, Option<String>) =
        match rx.recv_timeout(wall + Duration::from_secs(HANG_SLACK_SECS)) {
            Ok(v) => v,
            Err(_) => ("hang", None, None),
        };
    let millis = started.elapsed().as_millis();
    let within_budget_wall = millis <= (wall + Duration::from_secs(HANG_SLACK_SECS)).as_millis();
    FileOutcome {
        id,
        outcome,
        code,
        during,
        bytes,
        millis,
        within_budget_wall,
        expectation_drift: false,
        expect_match: "none",
    }
}

#[test]
#[ignore = "needs the extracted local corpus (see pdf-plan/10 SL-0.CORP.01-04)"]
fn local_corpus_open_sweep() {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect(&corpus_dir(), &mut files);
    files.sort();
    if files.is_empty() {
        eprintln!("corpus/pdfs is empty; skipping sweep (extract the corpora first)");
        return;
    }

    let wall = Duration::from_nanos(Budget::profile(Surface::Viewer).wall);
    let root = corpus_dir();
    let sweep_started = std::time::Instant::now();
    let mut outcomes: Vec<FileOutcome> = Vec::with_capacity(files.len());
    for (i, path) in files.iter().enumerate() {
        let o = open_one(path, &root, wall);
        if o.outcome != "ok" {
            eprintln!(
                "  [{}/{}] {} -> {} {:?} ({} ms)",
                i + 1,
                files.len(),
                o.id,
                o.outcome,
                o.code,
                o.millis
            );
        }
        outcomes.push(o);
    }
    let sweep_wall = sweep_started.elapsed();

    // ── expectations: exact relative id first, then unambiguous basename ────
    let expectations = load_expectations(&expect_dir());
    let by_id: std::collections::HashMap<&str, &Expectation> = expectations
        .iter()
        .map(|(id, e)| (id.as_str(), e))
        .collect();
    let mut by_basename: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::new();
    for (id, _) in &expectations {
        let base = id.rsplit('/').next().unwrap_or(id);
        by_basename.entry(base).or_default().push(id);
    }

    let mut drift = 0usize;
    let mut expect_exact = 0usize;
    let mut expect_basename = 0usize;
    let mut expect_ambiguous = 0usize;
    let mut drift_files: Vec<String> = Vec::new();
    for o in &mut outcomes {
        let exp: Option<(&str, &Expectation)> = if let Some(e) = by_id.get(o.id.as_str()) {
            expect_exact += 1;
            o.expect_match = "exact";
            Some((o.id.as_str(), *e))
        } else {
            let base = o.id.rsplit('/').next().unwrap_or(&o.id);
            let candidates: &[&str] = by_basename.get(base).map_or(&[], |v| v.as_slice());
            match candidates {
                // veraPDF expectations are recorded under the basename only;
                // the extraction layout nests them one level deeper.
                [only] => {
                    expect_basename += 1;
                    o.expect_match = "basename";
                    let id_str: &str = only;
                    by_id.get(id_str).map(|e| (id_str, *e))
                }
                [] => None,
                _ => {
                    expect_ambiguous += 1;
                    None
                }
            }
        };
        if let Some((_, e)) = exp {
            let actual_open = if o.outcome == "ok" { "ok" } else { "err" };
            // Expectations record the Debug name (`TrailerMissingRoot`); the
            // sweep reports the registry name (`TRAILER_MISSING_ROOT`).
            let exp_code = e.code.as_deref().map(expect_code_normalised);
            if actual_open != e.open
                || (e.open == "err" && o.code.as_deref().map(str::to_owned) != exp_code)
            {
                drift += 1;
                o.expectation_drift = true;
                let entry = format!(
                    "{}: expected {}/{} got {}/{}",
                    o.id,
                    e.open,
                    exp_code.as_deref().unwrap_or("-"),
                    o.outcome,
                    o.code.as_deref().unwrap_or("-")
                );
                eprintln!("  DRIFT {entry}");
                drift_files.push(entry);
            }
        }
    }

    // ── classify ────────────────────────────────────────────────────────────
    let total = outcomes.len();
    let opened = outcomes.iter().filter(|o| o.outcome == "ok").count();
    let errs = outcomes.iter().filter(|o| o.outcome == "err").count();
    let hangs: Vec<&FileOutcome> = outcomes.iter().filter(|o| o.outcome == "hang").collect();
    let read_errors = outcomes
        .iter()
        .filter(|o| o.outcome == "read-error")
        .count();
    let panics = outcomes
        .iter()
        .filter(|o| o.code.as_deref() == Some("INTERNAL_PANIC"))
        .count();
    let over_budget = outcomes.iter().filter(|o| !o.within_budget_wall).count();
    let budget_exhausted = outcomes
        .iter()
        .filter(|o| o.code.as_deref().is_some_and(|c| c.starts_with("BUDGET_")))
        .count();
    let rate = opened as f64 / total.max(1) as f64;

    // Root-cause groups: (code, during), as in the campaign reports.
    let mut by_root_cause: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for o in &outcomes {
        if o.outcome != "ok" {
            let key = format!(
                "{}|{}",
                o.code.clone().unwrap_or_else(|| o.outcome.to_owned()),
                o.during.clone().unwrap_or_default()
            );
            by_root_cause.entry(key).or_default().push(o.id.clone());
        }
    }

    // ── report ──────────────────────────────────────────────────────────────
    let report = serde_json::json!({
        "task": "SL-1.ROB.01",
        "scope": "local corpus; the 10k wild fetch is pending SL-0.CORP.05 (disk-bound)",
        "corpus_root": "corpus/pdfs",
        "surface": "viewer",
        "per_file_budget": true,
        "hang_slack_secs": HANG_SLACK_SECS,
        "wall_clock_ms": sweep_wall.as_millis(),
        "total_files": total,
        "expectation": {
            "matched_exact": expect_exact,
            "matched_basename": expect_basename,
            "ambiguous": expect_ambiguous,
            "drift": drift,
            "drift_files": drift_files,
        },
        "outcomes": {
            "ok": opened,
            "err": errs,
            "read_error": read_errors,
            "hang": hangs.len(),
        },
        "open_rate": rate,
        "open_rate_floor": OPEN_RATE_FLOOR,
        "panics": panics,
        "over_budget_wall": over_budget,
        "budget_exhausted": budget_exhausted,
        "by_root_cause": by_root_cause.iter()
            .map(|(k, v)| serde_json::json!({"key": k, "count": v.len(), "files": v}))
            .collect::<Vec<_>>(),
        "residual": outcomes.iter()
            .filter(|o| o.outcome != "ok")
            .map(|o| serde_json::json!({
                "id": o.id, "outcome": o.outcome, "code": o.code,
                "during": o.during, "bytes": o.bytes, "millis": o.millis,
                "expectation_drift": o.expectation_drift,
            }))
            .collect::<Vec<_>>(),
    });
    let mut report_path = workspace_root();
    report_path.push("target");
    std::fs::create_dir_all(&report_path).expect("create target dir");
    report_path.push("rob01-report.json");
    let mut f = std::fs::File::create(&report_path).expect("create report");
    f.write_all(serde_json::to_string_pretty(&report).unwrap().as_bytes())
        .expect("write report");

    println!(
        "SL-1.ROB.01 (local): {opened}/{total} opened ({:.1}%), {errs} typed errors, \
         {} hangs, {panics} panics, {drift} expectation drifts, {over_budget} over wall, \
         {budget_exhausted} budget-exhausted — sweep wall {}s — report: {}",
        rate * 100.0,
        hangs.len(),
        sweep_wall.as_secs(),
        report_path.display()
    );
    for (key, names) in &by_root_cause {
        println!("  {key} ({} files)", names.len());
    }

    // ── the gates ───────────────────────────────────────────────────────────
    assert!(
        hangs.is_empty(),
        "hangs must be investigated, never silently accepted: {:?}",
        hangs.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
    );
    assert_eq!(
        panics, 0,
        "0-panic goal violated: INTERNAL_PANIC outcomes are bugs (SL-0.ERR.03 converts \
         them, the campaign must fix them); see target/rob01-report.json residual"
    );
    assert!(
        rate >= OPEN_RATE_FLOOR,
        "open rate {rate:.3} fell below the {OPEN_RATE_FLOOR:.3} campaign floor"
    );
}
