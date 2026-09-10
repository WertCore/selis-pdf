//! `xtask perf-check` — SL-0.PERF.02.
//!
//! Loads the performance budgets from `xtask/perf-budgets.toml` (an encoding
//! of 03-CONVENTIONS.md §12) and evaluates every row:
//!
//! * `measuring` rows with `metric = "mean-ns"` run `xtask bench` and compare
//!   the measured criterion means against the absolute budget and — when
//!   `gate = "baseline-2"` — against the recorded `bench/baselines.json`
//!   mean (>5% regression fails, the 03-CONVENTIONS.md §9 release rule).
//! * `not-measurable` rows fail as `not implemented`. They never pass
//!   silently; the default run reports them as warnings so the nightly job
//!   stays actionable until the benchmarks land, and `--strict` turns them
//!   into hard failures.

use std::collections::BTreeMap;

const BUDGETS: &str = "xtask/perf-budgets.toml";
const BASELINE_FILE: &str = "bench/baselines.json";

/// Regression threshold for the perf gate (03-CONVENTIONS.md §9: "blocks
/// release on >5% regression"). Deliberately not the 2% from SL-0.PERF.01:
/// cross-run noise on unpinned hardware exceeds 2% for the O(n)-eviction
/// cache benchmarks, and the committed baseline was recorded on a different
/// machine than the CI runner. CI compares against a runner-fresh baseline
/// (see `.github/workflows/ci.yml`).
pub(crate) const MAX_REGRESSION_PCT: f64 = 5.0;

/// Absolute regression delta below which a difference is noise, in
/// nanoseconds. A percentage-only gate is meaningless for the nanosecond
/// kernel benches (5% of 15 ns is under the timer's resolution), so a row
/// regresses only when BOTH the percentage threshold and this floor are
/// exceeded.
///
/// Additionally the floor never scales below half the baseline: the
/// allocation-heavy cache benches swing ±40% *between identical invocations*
/// on unpinned hardware (measured), so a "regression" means the new mean is
/// ≥1.5× the baseline AND >5% over it. Tighter gating requires the pinned
/// reference machine (`bench/README.md` — not yet written, PERF.01 DoD gap).
pub(crate) const NOISE_FLOOR_NS: f64 = 500.0;

/// The regression floor for a baseline: the larger of the absolute noise
/// floor and half the baseline mean.
pub(crate) fn regression_floor_ns(baseline: f64) -> f64 {
    NOISE_FLOOR_NS.max(baseline * 0.5)
}

/// How a budget row's metric is expressed.
#[derive(serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Metric {
    /// Criterion mean wall time, nanoseconds.
    #[serde(rename = "mean-ns")]
    MeanNs,
    /// Unitless throughput ratio against PDFium.
    #[serde(rename = "ratio")]
    Ratio,
    /// A percentage.
    #[serde(rename = "percent")]
    Percent,
    /// Bytes of memory.
    #[serde(rename = "bytes")]
    Bytes,
    /// End-to-end latency, milliseconds.
    #[serde(rename = "duration-ms")]
    DurationMs,
}

/// Whether a row is enforced now or still awaiting its harness.
#[derive(serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Status {
    /// A benchmark exists; the budget is enforced.
    Measuring,
    /// No benchmark exists yet; must fail as `not implemented`.
    NotMeasurable,
}

/// The regression comparison applied to a measuring row.
#[derive(serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Fail when the current mean exceeds `bench/baselines.json` by more
    /// than [`MAX_REGRESSION_PCT`].
    #[serde(rename = "baseline-2")]
    Baseline2,
    /// Absolute budget only.
    #[serde(rename = "none")]
    None,
}

/// One row of `xtask/perf-budgets.toml`.
#[derive(serde::Deserialize, Debug, Clone)]
pub(crate) struct PerfBudget {
    /// Human-readable path description (03-CONVENTIONS.md §12 wording).
    pub path: String,
    /// The §12 group the row belongs to.
    #[allow(dead_code)]
    pub section: String,
    /// The metric the budget applies to.
    pub metric: Metric,
    /// The budget value in the metric's unit.
    pub budget: f64,
    /// Criterion benchmark name (required for enforced mean-ns and
    /// duration-ms rows).
    #[serde(default)]
    pub bench: Option<String>,
    /// Results JSON file (required for enforced ratio rows): written by the
    /// row's harness (e.g. `xtask perf-render` writes
    /// `bench/render-results.json`).
    #[serde(default)]
    pub results: Option<String>,
    /// Top-level key of the ratio inside `results` (required for ratio rows).
    #[serde(default)]
    pub ratio_key: Option<String>,
    /// Whether the row is enforced or pending its harness.
    pub status: Status,
    /// The regression gate for measuring rows.
    #[serde(default)]
    pub gate: Option<Gate>,
    /// Why the row is not measurable yet, and what harness will measure it.
    #[serde(default)]
    pub notes: Option<String>,
}

/// The root of `xtask/perf-budgets.toml`.
#[derive(serde::Deserialize, Debug, Clone)]
pub(crate) struct PerfBudgets {
    /// All budget rows.
    pub budget: Vec<PerfBudget>,
}

/// The outcome of evaluating one row. `NotImplemented` exists so that rows
/// without a harness fail loudly rather than silently passing the gate.
#[derive(Debug, PartialEq)]
pub(crate) enum PerfVerdict {
    /// Measured and within budget (and regression gate, if any).
    Ok,
    /// Measured over the absolute budget.
    OverBudget,
    /// Within budget but regressed beyond [`MAX_REGRESSION_PCT`] vs baseline.
    Regressed { pct: f64 },
    /// The row has no harness yet; it cannot pass.
    NotImplemented,
}

/// Evaluate one measuring mean-ns row: budget first, then regression gate.
pub(crate) fn perf_verdict(
    measured: Option<f64>,
    budget: f64,
    gate: Option<Gate>,
    baseline: Option<f64>,
) -> PerfVerdict {
    let Some(mean) = measured else {
        return PerfVerdict::NotImplemented;
    };
    if mean > budget {
        return PerfVerdict::OverBudget;
    }
    match (gate, baseline) {
        (Some(Gate::Baseline2), Some(base)) => {
            let pct = if base == 0.0 {
                if mean == 0.0 {
                    0.0
                } else {
                    f64::INFINITY
                }
            } else {
                (mean - base) / base * 100.0
            };
            let delta = mean - base;
            if pct > MAX_REGRESSION_PCT && delta > regression_floor_ns(base) {
                PerfVerdict::Regressed { pct }
            } else {
                PerfVerdict::Ok
            }
        }
        _ => PerfVerdict::Ok,
    }
}

/// Run the budget comparison. With `bench_first`, `cargo bench` runs even
/// when every enforced row is already known to fail (used to refresh data).
pub fn run(strict: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(BUDGETS).map_err(|e| format!("{BUDGETS}: {e}"))?;
    let cfg: PerfBudgets = toml::from_str(&text).map_err(|e| format!("{BUDGETS}: {e}"))?;

    let enforced: Vec<&PerfBudget> = cfg
        .budget
        .iter()
        .filter(|b| b.status == Status::Measuring)
        .collect();
    // Rows whose data comes from criterion (means in ns).
    let enforced_criterion: Vec<&PerfBudget> = enforced
        .iter()
        .copied()
        .filter(|b| b.metric == Metric::MeanNs || b.metric == Metric::DurationMs)
        .collect();

    // Measure only when some criterion-backed row needs data.
    let results: BTreeMap<String, f64> = if enforced_criterion.is_empty() {
        BTreeMap::new()
    } else {
        run_cargo_bench()?
    };

    let baseline: BTreeMap<String, f64> = match std::fs::read_to_string(BASELINE_FILE) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{BASELINE_FILE}: {e}"))?,
        Err(_)
            if enforced_criterion
                .iter()
                .any(|b| b.gate == Some(Gate::Baseline2)) =>
        {
            return Err(format!(
                "bench baselines missing at {BASELINE_FILE}.\n\
                 Run `cargo xtask bench --record-baseline` on the reference \
                 machine and commit the file."
            ));
        }
        Err(_) => BTreeMap::new(),
    };

    // Ratio rows read one results file each (usually the same file twice —
    // the parse is cheap, and per-row errors name the row's file).
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    for row in &cfg.budget {
        let unit = unit_of(row.metric);
        match row.status {
            Status::Measuring => match row.metric {
                Metric::MeanNs => {
                    let measured = row
                        .bench
                        .as_deref()
                        .and_then(|name| results.get(name).copied());
                    let base = row
                        .bench
                        .as_deref()
                        .and_then(|name| baseline.get(name).copied());
                    match perf_verdict(measured, row.budget, row.gate, base) {
                        PerfVerdict::Ok => {
                            let mean_txt = measured
                                .map(|m| format!("{m:.1}"))
                                .unwrap_or_else(|| "-".to_string());
                            println!(
                                "  {:62} {mean_txt:>12} {unit} — budget {} ok",
                                row.path, row.budget
                            );
                        }
                        PerfVerdict::OverBudget => {
                            let mean_txt = measured
                                .map(|m| format!("{m:.1}"))
                                .unwrap_or_else(|| "-".to_string());
                            failures.push(format!(
                                "  {} [{unit}]: {mean_txt} > budget {}",
                                row.path, row.budget
                            ));
                        }
                        PerfVerdict::Regressed { pct } => {
                            let mean_txt = measured
                                .map(|m| format!("{m:.1}"))
                                .unwrap_or_else(|| "-".to_string());
                            let base_txt = base
                                .map(|b| format!("{b:.1}"))
                                .unwrap_or_else(|| "none".to_string());
                            failures.push(format!(
                                "  {} [{unit}]: {mean_txt} regressed {pct:+.2}% (>{}% vs baseline {base_txt})",
                                row.path, MAX_REGRESSION_PCT
                            ));
                        }
                        PerfVerdict::NotImplemented => {
                            let msg = format!(
                                "  {} [{}]: not implemented — benchmark `{:?}` did not report a mean",
                                row.path, unit, row.bench
                            );
                            if strict {
                                failures.push(msg);
                            } else {
                                warnings.push(msg);
                            }
                        }
                    }
                }
                Metric::DurationMs => {
                    // Criterion reports ns; the budget is ms. The regression
                    // gate compares in ns (the baseline file's unit).
                    let measured_ns = row
                        .bench
                        .as_deref()
                        .and_then(|name| results.get(name).copied());
                    let base_ns = row
                        .bench
                        .as_deref()
                        .and_then(|name| baseline.get(name).copied());
                    let budget_ns = row.budget * 1e6;
                    match perf_verdict(measured_ns, budget_ns, row.gate, base_ns) {
                        PerfVerdict::Ok => {
                            let ms_txt = measured_ns
                                .map(|m| format!("{:.3}", m / 1e6))
                                .unwrap_or_else(|| "-".to_string());
                            println!(
                                "  {:62} {ms_txt:>12} {unit} — budget {} ok",
                                row.path, row.budget
                            );
                        }
                        PerfVerdict::OverBudget => {
                            let ms_txt = measured_ns
                                .map(|m| format!("{:.3}", m / 1e6))
                                .unwrap_or_else(|| "-".to_string());
                            failures.push(format!(
                                "  {} [{unit}]: {ms_txt} > budget {}",
                                row.path, row.budget
                            ));
                        }
                        PerfVerdict::Regressed { pct } => {
                            let ms_txt = measured_ns
                                .map(|m| format!("{:.3}", m / 1e6))
                                .unwrap_or_else(|| "-".to_string());
                            let base_txt = base_ns
                                .map(|b| format!("{:.3}", b / 1e6))
                                .unwrap_or_else(|| "none".to_string());
                            failures.push(format!(
                                "  {} [{unit}]: {ms_txt} regressed {pct:+.2}% (>{}% vs baseline {base_txt})",
                                row.path, MAX_REGRESSION_PCT
                            ));
                        }
                        PerfVerdict::NotImplemented => {
                            let msg = format!(
                                "  {} [{}]: not implemented — benchmark `{:?}` did not report a mean",
                                row.path, unit, row.bench
                            );
                            if strict {
                                failures.push(msg);
                            } else {
                                warnings.push(msg);
                            }
                        }
                    }
                }
                Metric::Ratio => {
                    // Ratios never come from criterion: the row names a
                    // results file (written by its harness) and the key
                    // holding the headline ratio. Higher is better.
                    match read_ratio(row) {
                        Ok(ratio) if ratio >= row.budget => {
                            println!(
                                "  {:62} {ratio:>12.3} {unit} — budget {} ok",
                                row.path, row.budget
                            );
                        }
                        Ok(ratio) => {
                            failures.push(format!(
                                "  {} [{unit}]: {ratio:.3} < budget {}",
                                row.path, row.budget
                            ));
                        }
                        Err(msg) => {
                            let msg = format!("  {} [{unit}]: not implemented — {msg}", row.path);
                            if strict {
                                failures.push(msg);
                            } else {
                                warnings.push(msg);
                            }
                        }
                    }
                }
                Metric::Percent | Metric::Bytes => {
                    let msg = format!(
                        "  {} [{}]: not implemented — no enforcement path for this metric yet",
                        row.path, unit
                    );
                    if strict {
                        failures.push(msg);
                    } else {
                        warnings.push(msg);
                    }
                }
            },
            Status::NotMeasurable => {
                let msg = format!(
                    "  {} [{}]: not implemented — {}",
                    row.path,
                    unit,
                    row.notes.as_deref().unwrap_or("no harness yet")
                );
                if strict {
                    failures.push(msg);
                } else {
                    warnings.push(msg);
                }
            }
        }
    }

    println!(
        "perf-check: {} budget(s) enforced, {} awaiting a harness",
        enforced.len(),
        cfg.budget.len() - enforced.len()
    );
    for w in &warnings {
        println!("  WARNING {w}");
    }
    if failures.is_empty() {
        println!("perf-check: all measurable budgets met");
        Ok(())
    } else {
        println!("perf-check FAILED:");
        for f in &failures {
            println!("{f}");
        }
        Err("performance budgets or regression rule violated".to_string())
    }
}

/// Read a ratio row's headline number from its results file. `Ok` carries
/// the ratio; `Err` carries the reason it is unavailable (missing file,
/// missing key, or a JSON null where the harness recorded no comparison —
/// e.g. PDFium absent on the recording host).
fn read_ratio(row: &PerfBudget) -> Result<f64, String> {
    if row.gate == Some(Gate::Baseline2) {
        return Err("ratio rows support gate = \"none\" only (absolute budget)".to_string());
    }
    let file = row
        .results
        .as_deref()
        .ok_or_else(|| "row names no `results` file".to_string())?;
    let key = row
        .ratio_key
        .as_deref()
        .ok_or_else(|| "row names no `ratio_key`".to_string())?;
    let text = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("cannot parse {file}: {e}"))?;
    match json.get(key) {
        Some(serde_json::Value::Number(n)) => n
            .as_f64()
            .ok_or_else(|| format!("`{key}` in {file} is not a number")),
        _ => Err(format!(
            "`{key}` missing or null in {file} — regenerate it with the row's harness"
        )),
    }
}

/// The display unit for a metric.
fn unit_of(metric: Metric) -> &'static str {
    match metric {
        Metric::MeanNs => "ns",
        Metric::Ratio => "x",
        Metric::Percent => "%",
        Metric::Bytes => "B",
        Metric::DurationMs => "ms",
    }
}

/// Run `cargo bench -p selis-bench` and parse the criterion means exactly as
/// `xtask bench` does (see `bench.rs` for the format contract). The extra
/// criterion flags stabilise the measurement: the default warm-up is too
/// short to settle thermally-throttled clocks, which showed up as ±50-100%
/// run-to-run swings on the cache benches.
fn run_cargo_bench() -> Result<BTreeMap<String, f64>, String> {
    let out = std::process::Command::new("cargo")
        .args(["bench", "-p", "selis-bench"])
        .args([
            "--",
            "--warm-up-time",
            "2",
            "--measurement-time",
            "5",
            "--sample-size",
            "50",
        ])
        .output()
        .map_err(|e| format!("cannot run cargo bench: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        if !stderr.trim().is_empty() {
            eprintln!("{stderr}");
        }
        return Err("benchmarks failed".to_string());
    }
    parse_criterion_means(&stdout)
}

/// Parse criterion's `bench_name    time:   [min mean max]` lines into a
/// name → mean-in-ns map. Unit tokens are normalised to nanoseconds.
pub(crate) fn parse_criterion_means(stdout: &str) -> Result<BTreeMap<String, f64>, String> {
    let mut results: BTreeMap<String, f64> = BTreeMap::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(pos) = trimmed.find("time:") {
            let name = trimmed
                .split_whitespace()
                .next()
                .unwrap_or("?")
                .trim_end_matches(':')
                .to_string();
            // "time:   [3.0 ns 3.2 ns 3.5 ns]" → tokens: "[3.0","ns","3.2","ns","3.5","ns]"
            let rest = &trimmed[pos + 5..];
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 4 {
                let mean_val = parts[2].replace(',', "").parse::<f64>().ok();
                let unit = parts[3].trim_end_matches(']');
                if let Some(v) = mean_val {
                    let ns = match unit {
                        "s" => v * 1e9,
                        "ms" => v * 1e6,
                        "µs" => v * 1e3,
                        _ => v,
                    };
                    results.insert(name, ns);
                }
            }
        }
    }
    if results.is_empty() {
        return Err("no benchmark results found in criterion output".to_string());
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use super::*;

    // ---- verdicts ----

    #[test]
    fn missing_measurement_is_never_ok() {
        assert_eq!(
            perf_verdict(None, 5.0, Some(Gate::Baseline2), Some(4.0)),
            PerfVerdict::NotImplemented
        );
        assert_eq!(
            perf_verdict(None, 5.0, None, None),
            PerfVerdict::NotImplemented
        );
    }

    #[test]
    fn over_absolute_budget_fails_even_if_improved() {
        assert_eq!(
            perf_verdict(Some(6.0), 5.0, Some(Gate::Baseline2), Some(10.0)),
            PerfVerdict::OverBudget
        );
    }

    #[test]
    fn regression_boundary_is_exclusive() {
        // A regression is: >5% AND a delta above the scaled noise floor
        // (max(500 ns, baseline/2)). With base = 5000 ns the floor is
        // 2500 ns, so +5% (250 ns) is noise while +60% (3000 ns) regresses.
        let base = NOISE_FLOOR_NS * 10.0; // 5000 ns
        assert_eq!(
            perf_verdict(
                Some(base * 1.05),
                base * 4.0,
                Some(Gate::Baseline2),
                Some(base)
            ),
            PerfVerdict::Ok
        );
        assert_eq!(
            perf_verdict(
                Some(base * 1.60),
                base * 4.0,
                Some(Gate::Baseline2),
                Some(base)
            ),
            PerfVerdict::Regressed { pct: 60.0 }
        );
    }

    #[test]
    fn sub_floor_deltas_never_regress() {
        // +50% of a 15 ns bench is 7.5 ns of real movement: timer noise, not
        // a regression.
        assert_eq!(
            perf_verdict(Some(22.5), 40.0, Some(Gate::Baseline2), Some(15.0)),
            PerfVerdict::Ok
        );
    }

    #[test]
    fn allocation_bench_swings_are_noise_until_1_5x() {
        // Observed reality: the insert+evict bench swings ±40% between
        // identical invocations. A +42% drift (below the 1.5x floor) is
        // reported, not failed; a true doubling fails both gates.
        let base = 977_180.0;
        assert_eq!(
            perf_verdict(
                Some(1_390_100.0),
                4_000_000.0,
                Some(Gate::Baseline2),
                Some(base)
            ),
            PerfVerdict::Ok
        );
        assert_eq!(
            perf_verdict(
                Some(base * 2.0),
                4_000_000.0,
                Some(Gate::Baseline2),
                Some(base)
            ),
            PerfVerdict::Regressed { pct: 100.0 }
        );
    }

    #[test]
    fn improvement_and_no_gate_pass() {
        assert_eq!(
            perf_verdict(Some(50.0), 200.0, Some(Gate::Baseline2), Some(100.0)),
            PerfVerdict::Ok
        );
        assert_eq!(
            perf_verdict(Some(90.0), 200.0, Some(Gate::None), Some(1.0)),
            PerfVerdict::Ok
        );
        // A measuring row without a baseline entry passes on the absolute
        // budget alone (first CI run records the baseline).
        assert_eq!(
            perf_verdict(Some(90.0), 200.0, Some(Gate::Baseline2), None),
            PerfVerdict::Ok
        );
    }

    // ---- criterion parsing (same format contract as bench.rs) ----

    #[test]
    fn parse_criterion_means_handles_units() {
        let out = "budget_charge      time:   [3.0 ns 4.0 ns 5.0 ns]\n\
                   big_bench          time:   [1.0 ms 2.0 ms 3.0 ms]\n\
                   micro              time:   [2.0 µs 2.5 µs 3.0 µs]\n";
        let m = parse_criterion_means(out).expect("parse");
        assert!((m["budget_charge"] - 4.0).abs() < 1e-9);
        assert!((m["big_bench"] - 2.0e6).abs() < 1e-3);
        assert!((m["micro"] - 2500.0).abs() < 1e-9);
    }

    #[test]
    fn parse_criterion_means_errors_on_empty_output() {
        assert!(parse_criterion_means("nothing here").is_err());
    }

    // ---- budgets file loading (real file, no wasm/bench needed) ----

    #[test]
    fn perf_budgets_toml_loads_and_every_section12_row_is_present() {
        // Tests run with CWD = xtask/, the tool with CWD = repo root.
        let text = std::fs::read_to_string(BUDGETS)
            .or_else(|_| std::fs::read_to_string("perf-budgets.toml"))
            .expect("read perf-budgets.toml");
        let cfg: PerfBudgets = toml::from_str(&text).expect("parse perf-budgets.toml");
        // 03-CONVENTIONS.md §12 lists exactly these 10 rows; plus the 4
        // kernel/cache rows measured since PERF.01.
        assert_eq!(cfg.budget.len(), 14);
        let paths: Vec<&str> = cfg.budget.iter().map(|b| b.path.as_str()).collect();
        for expected in [
            "Open + first page painted, 5 MB linearised, local",
            "Open + first page painted, 5 MB over Fast 3G, linearised",
            "Render A4 text page @150 DPI (native)",
            "Render A4 text page @150 DPI vs PDFium (throughput ratio)",
            "Text extraction, 300-page report (native)",
            "Incremental save, 1 annotation on a 200 MB file",
            "Scroll frame budget, all shells",
            "Peak RSS, 200 MB PDF, viewer profile (native)",
            "Core WASM bundle (brotli)",
            "Cold app launch to interactive, mobile mid-range",
        ] {
            assert!(
                paths.contains(&expected),
                "§12 row missing from perf-budgets.toml: {expected}"
            );
        }
        // Every measuring row must name its data source: criterion-backed
        // rows (mean-ns, duration-ms) name `bench`; ratio rows name
        // `results` + `ratio_key`.
        for b in &cfg.budget {
            if b.status == Status::Measuring {
                match b.metric {
                    Metric::MeanNs | Metric::DurationMs => {
                        assert!(b.bench.is_some(), "measuring row without bench: {}", b.path);
                    }
                    Metric::Ratio => {
                        assert!(
                            b.results.is_some() && b.ratio_key.is_some(),
                            "ratio row without results+ratio_key: {}",
                            b.path
                        );
                    }
                    Metric::Percent | Metric::Bytes => {
                        assert!(false, "measuring row with no enforcement path: {}", b.path);
                    }
                }
            } else {
                assert!(
                    b.notes.is_some(),
                    "not-measurable row without notes: {}",
                    b.path
                );
            }
        }
    }

    #[test]
    fn read_ratio_parses_headline_or_explains_absence() {
        let dir = std::env::temp_dir().join("selis-perf-check-ratio-test");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("results.json");
        std::fs::write(&file, "{\"geomean_vs_pdfium\": 2.5}").expect("write");
        let row = PerfBudget {
            path: "ratio test".to_string(),
            section: "render".to_string(),
            metric: Metric::Ratio,
            budget: 0.6,
            bench: None,
            results: Some(file.to_string_lossy().to_string()),
            ratio_key: Some("geomean_vs_pdfium".to_string()),
            status: Status::Measuring,
            gate: Some(Gate::None),
            notes: None,
        };
        assert!((read_ratio(&row).expect("ratio") - 2.5).abs() < 1e-12);
        let missing_key = PerfBudget {
            ratio_key: Some("geomean_vs_pdfjs".to_string()),
            ..row.clone()
        };
        assert!(read_ratio(&missing_key).is_err());
        std::fs::write(&file, "{\"geomean_vs_pdfium\": null}").expect("write null");
        assert!(read_ratio(&row).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
