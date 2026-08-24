//! `xtask check-codes` — SL-0.ERR.01 DoD.
//!
//! Enforces the registry invariants from `03-CONVENTIONS.md §3` on
//! `crates/selis-error/codes.toml`:
//!
//!   * a code id is never reused;
//!   * a code name is never reused;
//!   * every `kind` and `doc_state` is one of the defined values;
//!   * every qualified `Code::` reference used in workspace source exists in the registry.
//!
//! ("A meaning is never changed" is enforced by code review of the registry
//! diff, since the registry is the immutable record; this command catches the
//! mechanical failures that review misses.)

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

const KINDS: &[&str] = &[
    "Malformed",
    "Unsupported",
    "Invalid",
    "Auth",
    "Policy",
    "Budget",
    "Cancelled",
    "Io",
    "Internal",
];

const DOC_STATES: &[&str] = &[
    "NotLoaded",
    "Loaded",
    "PartiallyLoaded",
    "Unchanged",
    "Modified",
];

#[derive(Deserialize)]
struct Registry {
    code: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: u32,
    name: String,
    kind: String,
    meaning: String,
    doc_state: String,
}

pub fn check() -> Result<(), String> {
    let registry = load_registry()?;
    let mut errors: Vec<String> = Vec::new();

    // Duplicate ids and names.
    let mut ids: BTreeMap<u32, &str> = BTreeMap::new();
    let mut names: BTreeMap<&str, u32> = BTreeMap::new();
    for e in &registry.code {
        if let Some(prev) = ids.insert(e.id, &e.name) {
            errors.push(format!(
                "duplicate code id {}: {} and {}",
                e.id, prev, e.name
            ));
        }
        if let Some(prev) = names.insert(&e.name, e.id) {
            errors.push(format!(
                "duplicate code name {}: ids {} and {}",
                e.name, prev, e.id
            ));
        }
        if !KINDS.contains(&e.kind.as_str()) {
            errors.push(format!("{}: unknown kind {:?}", e.id, e.kind));
        }
        if !DOC_STATES.contains(&e.doc_state.as_str()) {
            errors.push(format!("{}: unknown doc_state {:?}", e.id, e.doc_state));
        }
        if e.meaning.trim().is_empty() {
            errors.push(format!("{}: empty meaning", e.id));
        }
    }

    // Used-but-unregistered: every qualified `Code::` reference in workspace source.
    // Source uses the PascalCase enum variants (build.rs `pascal()`); the
    // registry stores SCREAMING_SNAKE_CASE names. Both spellings are known.
    let known: BTreeSet<String> = registry
        .code
        .iter()
        .flat_map(|e| [e.name.clone(), pascal(&e.name)])
        .collect();
    let mut used = BTreeSet::new();
    for file in source_files(Path::new("crates"))? {
        scan_code_names(&file, &mut used)?;
    }
    for file in source_files(Path::new("apps"))? {
        scan_code_names(&file, &mut used)?;
    }
    for file in source_files(Path::new("xtask"))? {
        scan_code_names(&file, &mut used)?;
    }
    for name in used {
        if !known.contains(&name) {
            errors.push(format!("used in source but not registered: Code::{name}"));
        }
    }

    if errors.is_empty() {
        println!(
            "check-codes: {} codes, all ids unique, all uses registered",
            registry.code.len()
        );
        return Ok(());
    }
    let mut report = String::from("code registry violations:\n");
    for e in &errors {
        report.push_str(&format!("  - {e}\n"));
    }
    Err(report)
}

fn load_registry() -> Result<Registry, String> {
    let text = std::fs::read_to_string("crates/selis-error/codes.toml")
        .map_err(|e| format!("cannot read codes.toml: {e}"))?;
    toml::from_str(&text).map_err(|e| format!("codes.toml is not valid TOML: {e}"))
}

/// Recursively collect `.rs` files under `dir`, skipping `target/`.
fn source_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    walk(dir, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir error in {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

fn scan_code_names(file: &Path, used: &mut BTreeSet<String>) -> Result<(), String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    // A qualified `Code::` reference — PascalCase variants in source
    // (build.rs `pascal()`), matched by capturing the identifier that follows.
    let mut rest = text.as_str();
    while let Some(idx) = find_code(rest) {
        let after = &rest[idx + "Code::".len()..];
        // All registry variants are PascalCase; a lowercase `Code::retryable`
        // method reference is not a registry use.
        let name: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.starts_with(|c: char| c.is_ascii_uppercase()) && !name.is_empty() {
            used.insert(name);
        }
        rest = after;
    }
    Ok(())
}

/// Find the next `Code::` whose `Code` is a whole word (not `ExitCode::`).
fn find_code(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 6 <= bytes.len() {
        if bytes[i..].starts_with(b"Code::") {
            let prev_is_word = i > 0 && bytes[i - 1].is_ascii_alphanumeric();
            if !prev_is_word {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Mirror of `selis-error/build.rs::pascal`: SCREAMING_SNAKE → PascalCase.
fn pascal(name: &str) -> String {
    let mut out = String::new();
    for part in name.split('_') {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.extend(chars.flat_map(char::to_lowercase));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_id_detected() {
        let r = Registry {
            code: vec![
                Entry {
                    id: 1000,
                    name: "A".into(),
                    kind: "Malformed".into(),
                    meaning: "one".into(),
                    doc_state: "NotLoaded".into(),
                },
                Entry {
                    id: 1000,
                    name: "B".into(),
                    kind: "Malformed".into(),
                    meaning: "two".into(),
                    doc_state: "NotLoaded".into(),
                },
            ],
        };
        let mut errors = Vec::new();
        let mut ids = BTreeMap::new();
        for e in &r.code {
            if let Some(prev) = ids.insert(e.id, &e.name) {
                errors.push(format!(
                    "duplicate code id {}: {} and {}",
                    e.id, prev, e.name
                ));
            }
        }
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn scan_finds_qualified_names_but_not_compound_identifiers() {
        let src = "let e = selis_error::Code::BudgetBytes; err!(Code::Cancelled);\nlet c = ExitCode::SUCCESS;";
        let mut used = BTreeSet::new();
        let mut rest = src;
        while let Some(idx) = find_code(rest) {
            let after = &rest[idx + "Code::".len()..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                used.insert(name);
            }
            rest = after;
        }
        assert!(used.contains("BudgetBytes"));
        assert!(used.contains("Cancelled"));
        assert!(
            !used.contains("SUCCESS"),
            "ExitCode::SUCCESS is not a registry ref"
        );
    }

    #[test]
    fn pascal_matches_build_script_convention() {
        assert_eq!(pascal("XREF_UNRECOVERABLE"), "XrefUnrecoverable");
        assert_eq!(pascal("NOT_A_PDF"), "NotAPdf");
    }
}
