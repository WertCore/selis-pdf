//! SL-1.ROB.01 — wild-corpus open sweep.
//!
//! Opens every fetched wild-corpus PDF under the Viewer budget, classifies
//! every non-open outcome by its typed error code (grouped by `(code, during)`
//! as a root-cause proxy), and prints a report. This is the measurement loop
//! for the robustness campaign: each distinct root-cause group is a candidate
//! for its own fix + regression task.
//!
//! The wild corpus is fetched (`cargo xtask corpus fetch`) and never committed
//! (SL-0.LEGAL.04), so this sweep is opt-in: it is `#[ignore]`d and is skipped
//! when `corpus/pdfs` is empty. Run it explicitly after fetching:
//!
//! ```text
//! cargo test -p selis-cli --test rob01_baseline -- --ignored --nocapture
//! ```
//!
//! The open-rate floor below ratchets upward as root causes are fixed; it
//! guards the campaign against silent regressions.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use selis_sandbox::{Budget, Surface};

/// The minimum fraction of the wild corpus that must open under the Viewer
/// budget. Ratcheted from the 897/977 pre-depth-fix baseline to 919/977
/// after the depth-leak fixes (parse/page-tree/name-tree/number-tree) and
/// the tolerant tree walks, to 956/977 after the stream `/Length` scan
/// fallback, tolerant xref entry lines, the missing-`startxref` reconstruct
/// fallback, and newest-wins multi-revision resolution, and to 963/977
/// after the xref-stream entry-count cap and the document-resolve
/// reconstruct retry. Raise this as more root causes land.
const OPEN_RATE_FLOOR: f64 = 0.98;

fn corpus_dir() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("corpus");
    p.push("pdfs");
    p
}

#[test]
#[ignore = "needs the fetched wild corpus (cargo xtask corpus fetch)"]
fn wild_corpus_open_sweep() {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(corpus_dir())
        .expect("corpus/pdfs exists — run `cargo xtask corpus fetch` first")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "pdf"))
        .collect();
    files.sort();
    if files.is_empty() {
        eprintln!("corpus/pdfs is empty; skipping sweep (fetch it first)");
        return;
    }

    let budget = Budget::profile(Surface::Viewer);
    let mut opened = 0usize;
    let mut by_key: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();
    for path in &files {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(src) = std::fs::read(path) else {
            by_key.entry("IO_READ".to_string()).or_default().push(name);
            continue;
        };
        match selis_pdf_engine::Session::open(src, &budget) {
            Ok(_) => opened += 1,
            Err(e) => {
                let key = format!("{:?} | during={:?}", e.code(), e.ctx().during);
                failures.push(format!(
                    "{key}|detail={:?}|{name}",
                    e.ctx().detail.as_deref().unwrap_or("")
                ));
                by_key.entry(key).or_default().push(name);
            }
        }
    }

    let total = files.len();
    let rate = opened as f64 / total as f64;
    println!("SL-1.ROB.01: {opened}/{total} opened under Viewer budget ({rate:.1}%); {} root-cause groups", by_key.len());
    // Root-cause report: every failing file grouped by (code, during).
    let mut groups: Vec<(&String, &Vec<String>)> = by_key.iter().collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    for (key, names) in &groups {
        println!("  {key}  ({} files): {}", names.len(), names.join(", "));
    }
    // Machine-readable dump for diffing between campaign runs.
    let mut dump = failures;
    dump.sort();
    let dump_path = std::env::temp_dir().join("rob01_failures.txt");
    std::fs::write(&dump_path, dump.join("\n")).expect("write failure dump");

    assert!(
        rate >= OPEN_RATE_FLOOR,
        "open rate {rate:.3} fell below the {OPEN_RATE_FLOOR:.3} campaign floor — \
         a root cause regressed"
    );
    // No outcome may be an internal/unknown crash classification: every
    // failure must carry a typed, user-visible code (ROB.01 "0 panics").
    for key in by_key.keys() {
        assert!(
            !key.starts_with("Internal"),
            "an open failure was classified Internal (a bug, not a typed outcome): {key}"
        );
    }
}
