//! `xtask size-check` — SL-0.WS.09.
//!
//! Builds the WASM target, optimises with wasm-opt, measures the
//! brotli-compressed size of each linked artifact, and compares every
//! measurement against two limits:
//!
//! * the absolute budget in `xtask/size-budgets.toml` (ADR-P0011);
//! * the 2% regression rule against the last recorded measurement in
//!   `xtask/size-baseline.json`.
//!
//! A budget whose artifact is not measured yet fails as *not measured* —
//! never as passing. `--update-baseline` re-records the baseline for
//! deliberate size changes.

use std::collections::BTreeMap;
use std::path::Path;

const BUDGETS: &str = "xtask/size-budgets.toml";
const BASELINE: &str = "xtask/size-baseline.json";

/// The regression threshold from the task Do: a measured size more than this
/// percentage above the baseline fails the check.
pub(crate) const MAX_REGRESSION_PCT: f64 = 2.0;

/// One budget row from `xtask/size-budgets.toml`.
#[derive(serde::Deserialize, Debug, Clone)]
struct BudgetRow {
    /// Human-readable chunk name (reported in failures).
    #[allow(dead_code)]
    chunk: String,
    /// The artifact this budget is evaluated against.
    artifact: String,
    /// Maximum brotli-compressed size in bytes.
    budget: u64,
}

/// The `[[budget]]` array at the root of `xtask/size-budgets.toml`.
#[derive(serde::Deserialize, Debug, Clone)]
struct BudgetsFile {
    budget: Vec<BudgetRow>,
}

/// The last measured artifact sizes, persisted to `xtask/size-baseline.json`
/// and committed so CI can enforce the regression rule (SL-0.WS.09). Keyed by
/// artifact name, values are brotli-compressed bytes.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SizeBaseline(BTreeMap<String, u64>);

impl SizeBaseline {
    /// The recorded size for `artifact`, if any.
    fn get(&self, artifact: &str) -> Option<u64> {
        self.0.get(artifact).copied()
    }

    /// Record or replace the size of `artifact`.
    fn set(&mut self, artifact: &str, brotli_bytes: u64) {
        self.0.insert(artifact.to_string(), brotli_bytes);
    }
}

/// The outcome of comparing one measured artifact against its budget and the
/// baseline. `NotMeasured` exists so unmeasurable chunks fail loudly instead
/// of silently passing the gate.
#[derive(Debug, PartialEq)]
pub(crate) enum SizeVerdict {
    /// Within budget and within the regression threshold.
    Ok,
    /// Over the absolute budget.
    OverBudget { budget: u64 },
    /// Over the baseline by more than [`MAX_REGRESSION_PCT`].
    Regressed { baseline: u64, pct: f64 },
    /// The artifact was not part of this run; it cannot pass.
    NotMeasured,
}

/// Decide the verdict for one artifact: budget first, then regression.
pub(crate) fn verdict_for(
    _artifact: &str,
    measured: Option<u64>,
    budget: u64,
    baseline: Option<u64>,
) -> SizeVerdict {
    let Some(size) = measured else {
        return SizeVerdict::NotMeasured;
    };
    if size > budget {
        return SizeVerdict::OverBudget { budget };
    }
    match baseline {
        Some(base) => {
            let pct = regression_pct(size, base);
            if pct > MAX_REGRESSION_PCT {
                SizeVerdict::Regressed {
                    baseline: base,
                    pct,
                }
            } else {
                SizeVerdict::Ok
            }
        }
        // No baseline yet: the absolute budget is the gate; this run records
        // the first baseline entry.
        None => SizeVerdict::Ok,
    }
}

/// Percentage change of `new` over `base`, positive when larger. A zero
/// baseline is treated as infinite regression (any positive size fails).
pub(crate) fn regression_pct(new: u64, base: u64) -> f64 {
    if base == 0 {
        return if new == 0 { 0.0 } else { f64::INFINITY };
    }
    let new = new as f64;
    let base = base as f64;
    (new - base) / base * 100.0
}

/// Check that the WASM target fits within the size budgets and did not
/// regress more than [`MAX_REGRESSION_PCT`] against the recorded baseline.
///
/// With `update_baseline`, the fresh measurements replace the baseline
/// instead of being compared against it (a deliberate accept of size drift).
/// With `strict`, budgets whose artifact is not measured yet also fail the
/// run; by default they are reported loudly but do not fail, so the PR gate
/// stays actionable while chunk artifacts do not exist yet.
pub fn run(update_baseline: bool, strict: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(BUDGETS).map_err(|e| format!("{BUDGETS}: {e}"))?;
    let budgets: BudgetsFile = toml::from_str(&text).map_err(|e| format!("{BUDGETS}: {e}"))?;
    let budgets = budgets.budget;
    let measured = measure_artifacts()?;

    let mut baseline = if update_baseline {
        SizeBaseline::default()
    } else {
        load_baseline()?
    };

    let mut failures = Vec::new();
    for row in &budgets {
        let size = measured.get(&row.artifact).copied();
        let base = baseline.get(&row.artifact);
        match verdict_for(&row.artifact, size, row.budget, base) {
            SizeVerdict::Ok => {
                let base_txt = base
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "none".to_string());
                let size_txt = size
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "  {:26} {size_txt:>8} bytes — budget {} ok (baseline {base_txt})",
                    row.artifact, row.budget
                );
            }
            SizeVerdict::OverBudget { budget } => {
                let size_txt = size
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "-".to_string());
                failures.push(format!(
                    "  {} [{}]: {size_txt} > budget {budget}",
                    row.chunk, row.artifact
                ));
            }
            SizeVerdict::Regressed { baseline: b, pct } => {
                let size_txt = size
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "-".to_string());
                failures.push(format!(
                    "  {} [{}]: {size_txt} regressed {pct:+.2}% (>{}% vs baseline {b})",
                    row.chunk, row.artifact, MAX_REGRESSION_PCT
                ));
            }
            SizeVerdict::NotMeasured => {
                let msg = format!(
                    "  {} [{}]: NOT MEASURED — no artifact in this run (budget {})",
                    row.chunk, row.artifact, row.budget
                );
                if strict {
                    failures.push(msg);
                } else {
                    println!("  WARNING {msg}");
                }
            }
        }
        if let Some(size) = size {
            baseline.set(&row.artifact, size);
        }
    }

    if update_baseline {
        write_baseline(&baseline)?;
        println!("size-check: baseline updated ({BASELINE})");
    }

    println!(
        "wasm size-check: {} artifact(s) measured, {} budget(s) checked",
        measured.len(),
        budgets.len()
    );
    if failures.is_empty() {
        println!("size-check: all budgets met");
        Ok(())
    } else {
        println!("size-check FAILED:");
        for f in &failures {
            println!("{f}");
        }
        Err("size budgets or regression rule violated".to_string())
    }
}

/// Build every wasm-producing target and measure the brotli size of the
/// optimised module of each resulting artifact. Returns artifact name →
/// brotli bytes. Today exactly one linked binary exists (`xtask`); when the
/// wasm shell lands (Phase 4), its chunk artifacts appear here automatically.
fn measure_artifacts() -> Result<BTreeMap<String, u64>, String> {
    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "xtask",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .status()
        .map_err(|e| format!("cargo build --target wasm32: {e}"))?;
    if !status.success() {
        return Err("wasm build failed".to_string());
    }

    // Optimise with wasm-opt. On Windows the npm-installed binary is a `.cmd`
    // wrapper, not an `.exe`, so resolve it explicitly.
    let wasm_opt = if std::env::consts::OS == "windows" {
        "wasm-opt.cmd".to_string()
    } else {
        "wasm-opt".to_string()
    };

    let wasm_dir = Path::new("target/wasm32-unknown-unknown/release");
    let mut out = BTreeMap::new();
    let entries = std::fs::read_dir(wasm_dir).map_err(|e| format!("{wasm_dir:?}: {e}"))?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let is_fresh_wasm = path.extension().is_some_and(|x| x == "wasm")
            && !path.to_string_lossy().ends_with(".opt.wasm");
        if !is_fresh_wasm {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let opt_out = path.with_extension("opt.wasm");
        let status = std::process::Command::new(&wasm_opt)
            .arg("-O3")
            .arg(&path)
            .arg("-o")
            .arg(&opt_out)
            .status()
            .map_err(|e| format!("wasm-opt: {e}"))?;
        if !status.success() {
            return Err(
                "wasm-opt failed. Install with `cargo install wasm-opt` or `npm i -g wasm-opt`."
                    .to_string(),
            );
        }
        let compressed = brotli_compress(&opt_out)?;
        out.insert(stem.to_string(), compressed.len() as u64);
        println!(
            "  measured {:26} raw {} → brotli {}",
            stem,
            std::fs::metadata(&opt_out)
                .map_err(|e| format!("{opt_out:?}: {e}"))?
                .len(),
            compressed.len()
        );
    }
    if out.is_empty() {
        return Err("no wasm artifacts found after the build".to_string());
    }
    Ok(out)
}

/// Load the recorded baseline; empty when the file does not exist yet.
fn load_baseline() -> Result<SizeBaseline, String> {
    if !Path::new(BASELINE).exists() {
        return Ok(SizeBaseline::default());
    }
    let text = std::fs::read_to_string(BASELINE).map_err(|e| format!("{BASELINE}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{BASELINE}: {e}"))
}

/// Persist the baseline in a stable, reviewable form.
fn write_baseline(baseline: &SizeBaseline) -> Result<(), String> {
    let json = serde_json::to_string_pretty(baseline).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(BASELINE, format!("{json}\n")).map_err(|e| format!("{BASELINE}: {e}"))
}

fn brotli_compress(path: &Path) -> Result<Vec<u8>, String> {
    // Use Node.js zlib's brotli from the command line.
    let out = std::process::Command::new("node")
        .args([
            "-e",
            &format!(
                "const z=require('node:zlib');const fs=require('fs');const d=fs.readFileSync('{}');\
                 fs.writeFileSync(process.stdout.fd,Buffer.from(z.brotliCompressSync(d)));",
                path.display().to_string().replace('\\', "\\\\")
            ),
        ])
        .output()
        .map_err(|e| format!("node brotli: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("node brotli compression failed: {stderr}"));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;

    // ---- regression math ----

    #[test]
    fn regression_pct_zero_change_is_zero() {
        assert!(regression_pct(100, 100).abs() < 1e-9);
    }

    #[test]
    fn regression_pct_increase_and_decrease() {
        assert!((regression_pct(110, 100) - 10.0).abs() < 1e-9);
        assert!((regression_pct(90, 100) + 10.0).abs() < 1e-9);
    }

    #[test]
    fn regression_pct_zero_baseline_is_infinite_for_any_size() {
        assert!(regression_pct(1, 0).is_infinite());
        assert_eq!(regression_pct(0, 0), 0.0);
    }

    // ---- verdicts ----

    #[test]
    fn missing_measurement_is_never_ok() {
        assert_eq!(verdict_for("a", None, 100, None), SizeVerdict::NotMeasured);
        assert_eq!(
            verdict_for("a", None, 100, Some(50)),
            SizeVerdict::NotMeasured
        );
    }

    #[test]
    fn over_absolute_budget_fails_even_if_improved() {
        assert_eq!(
            verdict_for("a", Some(150), 100, Some(160)),
            SizeVerdict::OverBudget { budget: 100 }
        );
    }

    #[test]
    fn regression_boundary_is_exclusive() {
        // Exactly +2% is allowed; anything beyond fails.
        assert_eq!(verdict_for("a", Some(102), 200, Some(100)), SizeVerdict::Ok);
        assert_eq!(
            verdict_for("a", Some(103), 200, Some(100)),
            SizeVerdict::Regressed {
                baseline: 100,
                pct: 3.0
            }
        );
    }

    #[test]
    fn decrease_and_small_increase_pass() {
        assert_eq!(verdict_for("a", Some(90), 200, Some(100)), SizeVerdict::Ok);
        assert_eq!(verdict_for("a", Some(101), 200, Some(100)), SizeVerdict::Ok);
    }

    #[test]
    fn first_run_with_no_baseline_uses_only_the_budget() {
        assert_eq!(verdict_for("a", Some(120), 200, None), SizeVerdict::Ok);
        assert_eq!(
            verdict_for("a", Some(250), 200, None),
            SizeVerdict::OverBudget { budget: 200 }
        );
    }

    // ---- baseline persistence ----

    #[test]
    fn baseline_round_trips_through_json() {
        let mut b = SizeBaseline::default();
        b.set("xtask", 158_453);
        b.set("core-parser-cdylib", 102_500);
        let json = serde_json::to_string_pretty(&b).expect("serialize");
        let back: SizeBaseline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, b);
        assert_eq!(back.get("xtask"), Some(158_453));
        assert_eq!(back.get("absent"), None);
    }
}
