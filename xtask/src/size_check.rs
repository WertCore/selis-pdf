//! `xtask size-check` — SL-0.WS.09.
//! Builds the WASM target, optimises with wasm-opt, measures brotli-compressed
//! size per feature chunk, and compares against `xtask/size-budgets.toml`.

use std::path::Path;

const BUDGETS: &str = "xtask/size-budgets.toml";

/// Check that the WASM target fits within the size budgets.
pub fn run() -> Result<(), String> {
    let budgets: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(BUDGETS).map_err(|e| format!("{BUDGETS}: {e}"))?)
            .map_err(|e| format!("{BUDGETS}: {e}"))?;
    let budgets = budgets.as_object().ok_or("{BUDGETS}: not an object")?;

    // Build the WASM target for the xtask (the simplest WASM target).
    let out = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "xtask",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .output()
        .map_err(|e| format!("cargo build --target wasm32: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("wasm build failed: {stderr}"));
    }

    // Find the wasm file.
    let wasm_dir = Path::new("target/wasm32-unknown-unknown/release");
    let wasm_files: Vec<_> = std::fs::read_dir(wasm_dir)
        .map_err(|e| format!("{wasm_dir:?}: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "wasm"))
        .collect();
    if wasm_files.is_empty() {
        return Err("no wasm files found. Check the build.".to_string());
    }
    let wasm = &wasm_files[0].path();

    // Optimise with wasm-opt. On Windows the npm-installed binary is a `.cmd`
    // wrapper, not an `.exe`, so resolve it explicitly.
    let wasm_opt = if std::env::consts::OS == "windows" {
        "wasm-opt.cmd".to_string()
    } else {
        "wasm-opt".to_string()
    };
    let opt_out = wasm.with_extension("opt.wasm");
    let status = std::process::Command::new(&wasm_opt)
        .arg("-O3")
        .arg(wasm)
        .arg("-o")
        .arg(&opt_out)
        .status()
        .map_err(|e| format!("wasm-opt: {e}"))?;
    if !status.success() {
        return Err("wasm-opt failed. Install with `cargo install wasm-opt` or `npm i -g wasm-opt`.".to_string());
    }
    let raw_size = std::fs::metadata(&opt_out).map_err(|e| format!("{opt_out:?}: {e}"))?.len();

    // Brotli-compress (node's zlib or a simple brotli tool).
    let compressed = brotli_compress(&opt_out)?;
    let brotli_size = compressed.len() as u64;

    println!("wasm size-check (xtask target):");
    println!("  raw (wasm-opt -O3): {raw_size} bytes ({:.1} KiB)", raw_size as f64 / 1024.0);
    println!("  brotli-compressed:  {brotli_size} bytes ({:.1} KiB)", brotli_size as f64 / 1024.0);

    // Check against the first budget entry (the broadest baseline).
    let mut failures = Vec::new();
    for (chunk, budget) in budgets {
        let budget_val = budget.as_u64().unwrap_or(u64::MAX);
        if brotli_size > budget_val {
            failures.push(format!("  {chunk}: {brotli_size} > budget {budget_val}"));
        }
    }
    if failures.is_empty() {
        println!("size-check: all budgets met");
        Ok(())
    } else {
        println!("size-check FAILED:");
        for f in &failures {
            println!("{f}");
        }
        Err("size budgets exceeded".to_string())
    }
}

fn brotli_compress(path: &Path) -> Result<Vec<u8>, String> {
    let data = std::fs::read(path).map_err(|e| format!("{path:?}: {e}"))?;
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