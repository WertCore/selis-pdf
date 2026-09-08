//! Criterion benchmark harness (SL-0.PERF.01).
//! `xtask bench --record-baseline` saves baselines; `xtask bench --compare-baseline` checks
//! for regressions beyond the configured threshold against the saved baseline. Parses
//! criterion's standard output.
//!
//! Thresholds: the committed `bench/baselines.json` is a *reference record*
//! (recorded once on the reference machine) — the >2% cross-run rule from
//! PERF.01 is not runnable on noisy laptop hardware (±100% swings observed
//! for the cache benches). The enforced CI gate is in `perf_check.rs` (>5%,
//! 03-CONVENTIONS.md §9); the CI job records a runner-fresh baseline before
//! comparing, so the comparison is same-machine.

use std::collections::BTreeMap;

const BASELINE_FILE: &str = "bench/baselines.json";

pub fn run(record: bool, compare: bool) -> Result<(), String> {
    let out = std::process::Command::new("cargo")
        .args(["bench", "-p", "selis-bench"])
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

    // Parse criterion output: "bench_name    time:   [min mean max]"
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
                let mean_val = parts[2].replace(",", "").parse::<f64>().ok();
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

    if record {
        let json = serde_json::to_string_pretty(&results)
            .map_err(|e| format!("serialize baseline: {e}"))?;
        std::fs::write(BASELINE_FILE, &json)
            .map_err(|e| format!("cannot write {BASELINE_FILE}: {e}"))?;
        println!("bench: recorded baseline ({})", BASELINE_FILE);
        for (name, mean) in &results {
            println!("  {name:30} {mean:.3} ns");
        }
        return Ok(());
    }

    if compare {
        let baseline_text = std::fs::read_to_string(BASELINE_FILE).map_err(|e| {
            format!("cannot read {BASELINE_FILE}: {e}.\nRun `xtask bench --record-baseline` first.")
        })?;
        let baseline: BTreeMap<String, f64> =
            serde_json::from_str(&baseline_text).map_err(|e| format!("{BASELINE_FILE}: {e}"))?;

        let mut failures = Vec::new();
        for (name, &new_mean) in &results {
            if let Some(&base_mean) = baseline.get(name) {
                let change = (new_mean - base_mean) / base_mean * 100.0;
                let status = if change > 2.0 {
                    "FAIL"
                } else if change < -2.0 {
                    "IMPROVED"
                } else {
                    "ok"
                };
                println!("  {name:30} {new_mean:.3} ns (baseline {base_mean:.3} ns, {change:+.1}%) {status}");
                if change > 2.0 {
                    failures.push(format!(
                        "{name}: {change:+.1}% regression (>{:.0}% threshold)",
                        2.0
                    ));
                }
            } else {
                println!("  {name:30} {new_mean:.3} ns (no baseline)");
            }
        }
        for name in baseline.keys() {
            if !results.contains_key(name) {
                println!("  {name:30} (baseline only — bench was removed)");
            }
        }
        if failures.is_empty() {
            println!("bench: all baselines met (no regressions >2%)");
            return Ok(());
        }
        let mut msg = String::from("bench: regressions detected:\n");
        for f in &failures {
            msg.push_str(&format!("  {f}\n"));
        }
        return Err(msg);
    }

    println!("bench: current results:");
    for (name, mean) in &results {
        println!("  {name:30} {mean:.3} ns");
    }
    Ok(())
}
