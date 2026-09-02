//! `xtask check-wild-hygiene` — the automated controls of `pdf-plan/06-CORPUS-POLICY.md` §7.
//!
//! Two mechanical checks:
//!
//! 1. **CI exclusion.** Wild corpora (Common Crawl-derived data, govdocs1, SAFEDOCS) carry
//!    third-party copyright and possible PII; CI must never fetch them — a pipeline that did
//!    would embed redistributed content into compressed artefacts. This walks the workflow
//!    files and fails if a wild-corpus source domain or the wild fetch script is referenced.
//!
//! 2. **Expectation content leak.** `corpus/expect/*.toml` records hold outcomes, hashes, and
//!    page counts only — never document content (policy §2). The generator emits records well
//!    under 128 bytes; an oversized record or a long literal means document-derived bytes were
//!    pasted into a committable file.

use std::path::{Path, PathBuf};

/// Domains and file markers that indicate wild-corpus acquisition. Keep in sync with
/// `corpus/tools/fetch-wild.ps1` and `06-CORPUS-POLICY.md` §1.
const WILD_MARKERS: &[&str] = &[
    // Common Crawl data planes + index
    "data.commoncrawl.org",
    "index.commoncrawl.org",
    "commoncrawl.org",
    // Digital Corpora (SAFEDOCS, govdocs1)
    "digitalcorpora.org",
    "digitalcorpora.s3.amazonaws.com",
    "downloads.digitalcorpora.org",
    // the wild fetch script
    "fetch-wild.ps1",
    // the subcommand that would fetch wild data from the harness
    "corpus wild",
];

/// No expectation record may exceed this many bytes (records are metadata-only; the
/// generator emits at most ~64).
const MAX_EXPECT_FILE_BYTES: u64 = 128;

/// No single literal line in an expectation record may exceed this many chars.
const MAX_EXPECT_LINE_CHARS: usize = 64;

pub fn check() -> Result<(), String> {
    check_at(Path::new("."))
}

pub fn check_at(root: &Path) -> Result<(), String> {
    let mut violations = Vec::new();

    violations.extend(check_ci_workflows(&root.join(".github/workflows"))?);
    violations.extend(check_expectations(&root.join("corpus/expect"))?);

    if violations.is_empty() {
        println!(
            "check-wild-hygiene: clean (CI excludes wild corpora; expectations carry no content)"
        );
        return Ok(());
    }
    Err(format!(
        "wild-corpus policy violations (pdf-plan/06-CORPUS-POLICY.md):\n  - {}",
        violations.join("\n  - ")
    ))
}

/// Fail if any CI workflow references a wild-corpus source (policy §2: CI runs on seed
/// corpora only).
fn check_ci_workflows(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("cannot read {dir:?}: {e}"))? {
        let path = entry.map_err(|e| format!("{dir:?}: {e}"))?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.ends_with(".yml") || name.ends_with(".yaml")) {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        for (line_no, line) in text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            for marker in WILD_MARKERS {
                if lower.contains(marker) {
                    out.push(format!(
                        "{}:{}: CI references wild-corpus source `{marker}`",
                        name,
                        line_no + 1
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// Fail on expectation records that are too large to be metadata-only (policy §2) or that
/// carry a long literal — a document-content leak would end up committed.
fn check_expectations(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut files = walk_toml(dir, 0)?;
    files.sort();
    for path in files {
        let meta =
            std::fs::metadata(&path).map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
        if meta.len() > MAX_EXPECT_FILE_BYTES {
            out.push(format!(
                "{}: {} bytes exceeds the {MAX_EXPECT_FILE_BYTES}-byte metadata-only bound",
                path.display(),
                meta.len()
            ));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        for (line_no, line) in text.lines().enumerate() {
            if line.len() > MAX_EXPECT_LINE_CHARS {
                out.push(format!(
                    "{}:{}: literal of {} chars exceeds the {MAX_EXPECT_LINE_CHARS}-char bound",
                    path.display(),
                    line_no + 1,
                    line.len()
                ));
            }
        }
    }
    Ok(out)
}

/// Collect `*.toml` files under `dir`, recursively (bounded depth to survive cycles).
fn walk_toml(dir: &Path, depth: usize) -> Result<Vec<PathBuf>, String> {
    if depth > 8 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("cannot read {dir:?}: {e}"))? {
        let path = entry.map_err(|e| format!("{dir:?}: {e}"))?.path();
        if path.is_dir() {
            out.extend(walk_toml(&path, depth + 1)?);
        } else if path.extension().is_some_and(|e| e == "toml") {
            out.push(path);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_cover_every_wild_source() {
        for marker in [
            "commoncrawl.org",
            "digitalcorpora.org",
            "fetch-wild.ps1",
            "corpus wild",
        ] {
            assert!(
                WILD_MARKERS.contains(&marker),
                "wild marker `{marker}` missing from the CI-exclusion list"
            );
        }
    }

    #[test]
    fn committed_tree_passes() {
        // The checked-in tree must always pass: CI files reference no wild sources and
        // every expectation record is generator-sized. Anchor at the workspace root —
        // `cargo test` runs with the package root as CWD.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives in the workspace root");
        check_at(root).expect("check-wild-hygiene must pass on the committed tree");
    }

    #[test]
    fn oversized_expectation_is_flagged() {
        let tmp = std::env::temp_dir().join(format!("selis-hygiene-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let long_line = "x".repeat(MAX_EXPECT_LINE_CHARS + 1);
        std::fs::write(tmp.join("leak.toml"), format!("note = \"{long_line}\"\n")).unwrap();
        let violations = check_expectations(&tmp).unwrap();
        assert!(!violations.is_empty(), "oversized literal must be flagged");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
