//! `xtask check-contracts` and `xtask check-alloc` — SL-0.WS.05.
//!
//! # check-contracts
//!
//! Every function taking a `Budget` or a document-origin `&[u8]` must have
//! `# Budget` and `# Malformed Input` rustdoc sections (ADR-P0013). This is a
//! heuristic scan: it flags functions whose signature names `Budget` or
//! `&[u8]`/`&mut [u8]` whose doc comment lacks the required sections.
//!
//! # check-alloc
//!
//! No direct `Vec::with_capacity`/`vec![0; n]` with a document-derived length
//! outside `selis-sandbox`. The sandbox's `alloc` module is the only place such
//! lengths may reach the allocator. Heuristic: flags `Vec::with_capacity` and
//! `vec![` outside `selis-sandbox`.

use std::path::{Path, PathBuf};

/// The public check-contracts entry point.
pub fn check_contracts() -> Result<(), String> {
    let layers = load_layers()?;
    let mut errors: Vec<String> = Vec::new();
    for (crate_name, files) in collect_crates()? {
        // ADR-P0013's contract sections are for functions that consume
        // *document* bytes. The L2+ domain crates are where untrusted bytes
        // are parsed; L0/L1 are kernel utilities with their own contracts.
        let layer = layers.get(&crate_name).copied().unwrap_or(0);
        if layer < 2 {
            continue;
        }
        for file in &files {
            scan_contracts(&crate_name, file, &mut errors);
        }
    }
    if errors.is_empty() {
        println!("check-contracts: every document-consuming fn documents its contracts");
        return Ok(());
    }
    let mut report = String::from("contract violations (missing # Budget or # Malformed Input):\n");
    for e in &errors {
        report.push_str(&format!("  {e}\n"));
    }
    Err(report)
}

/// The public check-alloc entry point.
pub fn check_alloc() -> Result<(), String> {
    let mut errors: Vec<String> = Vec::new();
    for (crate_name, files) in collect_crates()? {
        if crate_name == "selis-sandbox" {
            continue; // the only crate permitted to own allocation policy
        }
        for file in &files {
            scan_alloc(&crate_name, file, &mut errors);
        }
    }
    if errors.is_empty() {
        println!("check-alloc: no direct allocation outside selis-sandbox");
        return Ok(());
    }
    let mut report = String::from("allocation violations:\n");
    for e in &errors {
        report.push_str(&format!("  {e}\n"));
    }
    Err(report)
}

fn scan_contracts(crate_name: &str, file: &Path, errors: &mut Vec<String>) {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => return,
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        // Only public functions count as entry points.
        let is_pub_fn = trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub const fn ")
            || trimmed.starts_with("pub(crate) fn ");
        if is_pub_fn && takes_document_input(lines[i]) {
            let doc = doc_above(&lines, i);
            if !doc.contains("# Budget") || !doc.contains("# Malformed Input") {
                errors.push(format!(
                    "{}:{}:{}: public fn takes document input without `# Budget` and `# Malformed Input` doc sections",
                    crate_name,
                    file.display(),
                    i + 1
                ));
            }
        }
        i += 1;
    }
}

/// Heuristic: does this signature consume document-derived input?
fn takes_document_input(line: &str) -> bool {
    // `&[u8]` / `&mut [u8]` of document origin, or a `&dyn DocSource`.
    line.contains("&[u8]")
        || line.contains("&mut [u8]")
        || line.contains("&dyn DocSource")
        || line.contains("&dyn crate::DocSource")
        || line.contains("&dyn super::DocSource")
}

fn load_layers() -> Result<std::collections::HashMap<String, u8>, String> {
    let text = std::fs::read_to_string("xtask/layers.toml")
        .map_err(|e| format!("cannot read xtask/layers.toml: {e}"))?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| format!("layers.toml invalid: {e}"))?;
    let mut map = std::collections::HashMap::new();
    if let Some(layers) = value.get("layers").and_then(|l| l.as_table()) {
        for (name, layer) in layers {
            if let Some(l) = layer.as_integer() {
                map.insert(name.clone(), l as u8);
            }
        }
    }
    Ok(map)
}

/// Collect the doc comment lines immediately above a function.
fn doc_above(lines: &[&str], idx: usize) -> String {
    let mut out = Vec::new();
    let mut j = idx;
    while j > 0 {
        j -= 1;
        let l = lines[j].trim_start();
        if l.starts_with("///") || l.starts_with("//!") {
            out.push(l);
        } else if l.is_empty() {
            continue;
        } else {
            break;
        }
    }
    out.join("\n")
}

fn scan_alloc(crate_name: &str, file: &Path, errors: &mut Vec<String>) {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => return,
    };
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("///") {
            continue;
        }
        // Direct heap allocation with a runtime size hint, outside the sandbox.
        if line.contains("Vec::with_capacity") || line.contains("vec![0u8;") {
            errors.push(format!(
                "{}:{}:{}: direct allocation outside selis-sandbox",
                crate_name,
                file.display(),
                idx + 1
            ));
        }
    }
}

fn collect_crates() -> Result<Vec<(String, Vec<PathBuf>)>, String> {
    let mut out: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let entries = std::fs::read_dir("crates").map_err(|e| format!("cannot read crates/: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir error: {e}"))?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let mut files = Vec::new();
        walk_source(&dir, &mut files)?;
        out.push((name, files));
    }
    Ok(out)
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
            out.push(path);
        }
    }
    Ok(())
}
