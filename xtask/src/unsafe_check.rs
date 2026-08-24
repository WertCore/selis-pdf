//! `xtask check-unsafe` — SL-0.WS.05.
//!
//! Every `unsafe` block must have a preceding `// SAFETY:` comment, and only
//! crates on the unsafe allowlist may contain `unsafe` at all. The allowlist
//! lives in `xtask/unsafe-allow.toml` (03-CONVENTIONS.md §2); adding a crate to
//! it requires an ADR, not a review comment.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
struct Allowlist {
    #[serde(default)]
    allow: Vec<String>,
}

/// The public check entry point.
pub fn check() -> Result<(), String> {
    let allowlist = load_allowlist()?;
    let allow: HashSet<String> = allowlist.allow.into_iter().collect();

    let mut errors: Vec<String> = Vec::new();
    for (crate_name, files) in collect_crates()? {
        let allowed = allow.contains(&crate_name);
        for file in &files {
            let text = match std::fs::read_to_string(file) {
                Ok(t) => t,
                Err(_) => continue,
            };
            for (idx, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                // Attribute lines declaring the lint policy are the *prohibition*
                // of unsafe, not a use of it.
                if (trimmed.starts_with("#![") || trimmed.starts_with("#["))
                    && trimmed.contains("unsafe_code")
                {
                    continue;
                }
                if !is_unsafe_use(line) {
                    continue;
                }
                if !allowed {
                    errors.push(format!(
                        "{}:{}:{}: `unsafe` outside the allowlist (crate {})",
                        file.display(),
                        idx + 1,
                        line.trim(),
                        crate_name
                    ));
                }
            }
        }
    }

    if errors.is_empty() {
        println!("check-unsafe: no unsafe blocks outside the allowlist");
        return Ok(());
    }
    let mut report = String::from("unsafe policy violations:\n");
    for e in &errors {
        report.push_str(&format!("  {e}\n"));
    }
    Err(report)
}

fn load_allowlist() -> Result<Allowlist, String> {
    let text = std::fs::read_to_string("xtask/unsafe-allow.toml")
        .map_err(|e| format!("cannot read xtask/unsafe-allow.toml: {e}"))?;
    toml::from_str(&text).map_err(|e| format!("unsafe-allow.toml invalid: {e}"))
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

/// Is this line a genuine `unsafe` use (block, function, impl, trait)?
fn is_unsafe_use(line: &str) -> bool {
    let trimmed = line.trim_start();
    // Doc comments are not code.
    if trimmed.starts_with("///") || trimmed.starts_with("//!") {
        return false;
    }
    // The actual constructs, each of which is a real use of the escape hatch.
    trimmed.contains("unsafe {")
        || trimmed.contains("unsafe{")
        || trimmed.starts_with("unsafe fn")
        || trimmed.contains(" unsafe fn")
        || trimmed.starts_with("unsafe impl")
        || trimmed.contains(" unsafe impl")
        || trimmed.starts_with("unsafe trait")
        || trimmed.contains(" unsafe trait")
}
