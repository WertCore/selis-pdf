//! `xtask coverage` — SL-0.WS.08.
//!
//! Runs `cargo llvm-cov` (JSON) across the workspace and enforces the per-crate
//! line-coverage floors in `xtask/coverage.toml` (03-CONVENTIONS.md §6.1). A
//! crate below its floor fails the command.

use std::collections::BTreeMap;
use std::path::Path;

const COVERAGE_TOML: &str = "xtask/coverage.toml";

/// Run the coverage harness and enforce the floors.
pub fn run() -> Result<(), String> {
    let floors: BTreeMap<String, f64> = load_floors()?;

    // Produce a JSON report to a temp file (llvm-cov's `-` output path does not
    // stream on all platforms).
    let report_path = std::env::temp_dir().join("selis-coverage.json");
    let out = std::process::Command::new("cargo")
        .arg("llvm-cov")
        .arg("--workspace")
        .arg("--json")
        .arg("--output-path")
        .arg(&report_path)
        .output()
        .map_err(|e| format!("cannot run cargo llvm-cov: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("cargo llvm-cov failed: {stderr}"));
    }
    let report_bytes = std::fs::read(&report_path)
        .map_err(|e| format!("cannot read coverage report {}: {e}", report_path.display()))?;
    let report: serde_json::Value =
        serde_json::from_slice(&report_bytes).map_err(|e| format!("llvm-cov JSON: {e}"))?;

    // llvm-cov JSON: `data[0].files[]` with `name`, `summary.lines.percent`.
    let files = report["data"]
        .as_array()
        .and_then(|d| d.get(0))
        .and_then(|d| d["files"].as_array())
        .ok_or_else(|| "unexpected llvm-cov JSON shape (missing data[0].files)".to_string())?;

    // Aggregate line coverage per crate from the file path (crates/<name>/).
    let mut per_crate: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for f in files {
        let name = f["name"].as_str().unwrap_or("");
        let Some(crate_name) = crate_name_of(name) else { continue };
        let covered = f["summary"]["lines"]["covered"].as_u64().unwrap_or(0);
        let total = f["summary"]["lines"]["count"].as_u64().unwrap_or(0);
        let entry = per_crate.entry(crate_name).or_insert((0, 0));
        entry.0 += covered;
        entry.1 += total;
    }

    let mut failures = Vec::new();
    for (crate_name, &(covered, total)) in &per_crate {
        let Some(&floor) = floors.get(crate_name) else { continue };
        let pct = if total > 0 {
            covered as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let status = if pct >= floor { "ok" } else { "FAIL" };
        println!("  {crate_name:20} {pct:5.1}% (floor {floor}%) {status}");
        if pct < floor {
            failures.push(format!("{crate_name}: {pct:.1}% < floor {floor}%"));
        }
    }

    if failures.is_empty() {
        println!("coverage: all crates meet their floors");
        Ok(())
    } else {
        let mut msg = String::from("coverage floors not met:\n");
        for f in &failures {
            msg.push_str(&format!("  {f}\n"));
        }
        Err(msg)
    }
}

/// The crate a file belongs to, from `crates/<name>/...` or `apps/<name>/...`.
fn crate_name_of(path: &str) -> Option<String> {
    let p = Path::new(path);
    let mut it = p.components();
    let first = it.next()?.as_os_str().to_string_lossy();
    let second = it.next()?.as_os_str().to_string_lossy();
    if first == "crates" {
        Some(second.into_owned())
    } else if first == "apps" {
        Some(format!("apps/{second}"))
    } else {
        None
    }
}

/// Load `xtask/coverage.toml` floors (crate name -> percentage).
fn load_floors() -> Result<BTreeMap<String, f64>, String> {
    let text = std::fs::read_to_string(COVERAGE_TOML)
        .map_err(|e| format!("cannot read {COVERAGE_TOML}: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{COVERAGE_TOML}: {e}"))?;
    let mut out = BTreeMap::new();
    for (k, val) in v.as_object().ok_or("coverage.toml must be an object")? {
        let floor = val.as_u64().ok_or_else(|| format!("{k}: non-numeric floor"))?;
        out.insert(k.clone(), floor as f64);
    }
    Ok(out)
}