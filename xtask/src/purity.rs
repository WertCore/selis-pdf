//! `xtask check-purity` — ADR-P0005 / P0011 / P0016, SL-0.WS.04.
//!
//! No crate at L0–L3 may name a filesystem, network, clock, or environment
//! API. All four are injected (`DocSource`/`DocSink`, `Clock`, `Rng`, `Env`).
//! This is the test that makes the local-first claim *structural* rather than
//! aspirational: a crate that cannot name `std::fs` cannot read the disk.
//!
//! The banned paths come from the `[purity]` section of `xtask/layers.toml`
//! with per-crate allowlists (`[[purity_allow]]`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Value;

#[derive(Deserialize)]
struct PurityCfg {
    #[serde(default)]
    banned: Vec<String>,
    #[serde(default)]
    purity_allow: Vec<PurityAllow>,
}

#[derive(Deserialize)]
struct PurityAllow {
    #[serde(rename = "crate")]
    crate_name: Option<String>,
    paths: Vec<String>,
}

/// One banned-path hit in one file.
struct Violation {
    crate_name: String,
    file: String,
    line: usize,
    path: String,
}

/// The public check entry point.
pub fn check() -> Result<(), String> {
    let cfg = load_cfg()?;
    let allowed: HashMap<String, Vec<String>> = cfg
        .purity_allow
        .iter()
        .map(|a| {
            let key = a.crate_name.clone().unwrap_or_default();
            (key, a.paths.clone())
        })
        .collect();

    let mut violations: Vec<Violation> = Vec::new();
    let mut crates = collect_crates()?;
    crates.sort_unstable();

    for (crate_name, files) in &crates {
        for file in files {
            scan_file(crate_name, file, &cfg.banned, &allowed, &mut violations);
        }
    }

    if violations.is_empty() {
        println!(
            "check-purity: {} crates scanned, no banned path references",
            crates.len()
        );
        return Ok(());
    }

    let mut report = String::from("purity violations (L0–L3 must not name fs/net/env/clock):\n");
    for v in &violations {
        report.push_str(&format!(
            "  {}:{}:{} references {}\n",
            v.crate_name, v.file, v.line, v.path
        ));
    }
    Err(report)
}

fn load_cfg() -> Result<PurityCfg, String> {
    let text = std::fs::read_to_string("xtask/layers.toml")
        .map_err(|e| format!("cannot read xtask/layers.toml: {e}"))?;
    let value: Value = toml::from_str(&text).map_err(|e| format!("layers.toml invalid: {e}"))?;
    let purity = value
        .get("purity")
        .cloned()
        .unwrap_or(Value::Table(Default::default()));
    purity
        .try_into::<PurityCfg>()
        .map_err(|e| format!("[purity] section invalid: {e}"))
}

/// Collect `(crate_name, source_files)` for every crate at L0–L3.
fn collect_crates() -> Result<Vec<(String, Vec<PathBuf>)>, String> {
    let layers = load_layers()?;
    let mut out: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let entries = std::fs::read_dir("crates").map_err(|e| format!("cannot read crates/: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir error: {e}"))?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(layer) = layers.get(&name) else {
            continue; // not yet layered (planned crate)
        };
        if *layer > 3 {
            continue; // bindings and apps are allowed the outside world
        }
        let mut files = Vec::new();
        walk_source(&dir, &mut files)?;
        out.push((name, files));
    }
    Ok(out)
}

fn load_layers() -> Result<HashMap<String, u8>, String> {
    let text = std::fs::read_to_string("xtask/layers.toml")
        .map_err(|e| format!("cannot read xtask/layers.toml: {e}"))?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| format!("layers.toml invalid: {e}"))?;
    let mut map = HashMap::new();
    if let Some(layers) = value.get("layers").and_then(|l| l.as_table()) {
        for (name, layer) in layers {
            if let Some(l) = layer.as_integer() {
                map.insert(name.clone(), l as u8);
            }
        }
    }
    Ok(map)
}

fn walk_source(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir error in {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            walk_source(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            // build.rs runs at compile time, not in the shipped crate; it may
            // name the filesystem and environment.
            if path.file_name().is_some_and(|n| n == "build.rs") {
                continue;
            }
            out.push(path);
        }
    }
    Ok(())
}

fn scan_file(
    crate_name: &str,
    file: &Path,
    banned: &[String],
    allowed: &HashMap<String, Vec<String>>,
    violations: &mut Vec<Violation>,
) {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => return,
    };
    for (idx, line) in text.lines().enumerate() {
        let lineno = idx + 1;
        // Doc comments explain *why* a path is banned; they do not call it.
        if line.trim_start().starts_with("//!") || line.trim_start().starts_with("///") {
            continue;
        }
        for path in banned {
            if line.contains(path) && !is_allowed(crate_name, allowed, path) {
                violations.push(Violation {
                    crate_name: crate_name.to_string(),
                    file: file.display().to_string(),
                    line: lineno,
                    path: path.clone(),
                });
            }
        }
    }
}

fn is_allowed(crate_name: &str, allowed: &HashMap<String, Vec<String>>, path: &str) -> bool {
    // A crate-level allow (key = crate name), or a global allow (key = "").
    allowed
        .get(crate_name)
        .or_else(|| allowed.get(""))
        .is_some_and(|paths| paths.iter().any(|p| p == path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_lookup_is_crate_scoped() {
        let mut allowed = HashMap::new();
        allowed.insert("selis-io".to_string(), vec!["std::fs".to_string()]);
        allowed.insert(String::new(), vec!["std::process".to_string()]);
        assert!(is_allowed("selis-io", &allowed, "std::fs"));
        assert!(
            is_allowed("any-crate", &allowed, "std::process"),
            "global allows apply everywhere"
        );
        assert!(
            !is_allowed("selis-sandbox", &allowed, "std::fs"),
            "crate allow must not leak"
        );
        assert!(!is_allowed("selis-io", &allowed, "std::net"));
    }
}
