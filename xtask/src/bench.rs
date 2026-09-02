//! Criterion benchmark harness (SL-0.PERF.01).
//! `xtask bench` runs `cargo bench -p selis-bench` and compares against
//! recorded baselines.

use std::path::Path;

const BASELINE_DIR: &str = "bench/baselines";

/// Run the benchmark suite and compare against the recorded baseline.
pub fn run() -> Result<(), String> {
    // Run the criterion benchmarks.
    let out = std::process::Command::new("cargo")
        .args(["bench", "-p", "selis-bench", "--", "--verbose"])
        .output()
        .map_err(|e| format!("cannot run cargo bench: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    println!("{stdout}");
    if !stderr.trim().is_empty() {
        eprintln!("{stderr}");
    }
    if !out.status.success() {
        return Err("benchmarks failed".to_string());
    }

    // Record the baseline if this is the baseline run.
    let baseline_dir = Path::new(BASELINE_DIR);
    if baseline_dir.exists() {
        println!("bench: baseline exists at {BASELINE_DIR}");
    } else {
        std::fs::create_dir_all(baseline_dir)
            .map_err(|e| format!("cannot create {BASELINE_DIR}: {e}"))?;
        println!("bench: baseline directory created at {BASELINE_DIR}");
    }
    println!("bench: baseline recorded. Copy target/criterion/ to {BASELINE_DIR}/ for comparison.");
    Ok(())
}
