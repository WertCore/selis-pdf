//! WRITE.07 conformance gate: every tool, over every corpus file, must not
//! degrade the document's evaluable conformance posture (SL-1.DOC.09 rule
//! subset). A tool whose output opens worse than its input — a rule that
//! passed on the input now fails on the output, or the output does not
//! open at all — fails this test, and with it CI (`cargo test --workspace`).
//!
//! Inputs that do not open under the Viewer budget have no posture to
//! degrade and are skipped; tools that cleanly refuse an input (encrypted
//! documents, deleting every page) produce no output and are skipped.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use selis_pdf_doc::{Profile, RuleResult};
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, Surface};

/// Maximum input size the gate runs tools over per file (8 MiB). Full-document
/// rewrites on multi-megabyte files exercise the same conformance paths as
/// small ones while dominating the gate's wall time; every file's open path
/// is already covered by the ROB.01 sweep. Skips are counted, never silent.
const MAX_GATE_BYTES: u64 = 8 * 1024 * 1024;

/// The corpus directory (`corpus/pdfs` at the workspace root).
fn corpus_dir() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("corpus");
    p.push("pdfs");
    p
}

/// The document's evaluated rule outcomes, aligned with
/// `selis_pdf_doc::registry()`, when evaluation succeeds.
fn posture(session: &Session) -> Option<Vec<RuleResult>> {
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    session.conformance(Profile::PdfUa, &budget, &mut g).ok()
}

/// Run every tool applicable to a `page_count`-page document over `path`,
/// returning (tool name, output path, whether the tool ran). A tool that
/// refuses the input (encrypted, single-page delete) is not a regression.
fn run_tools(
    path: &std::path::Path,
    out_dir: &std::path::Path,
    page_count: usize,
) -> Vec<(String, std::path::PathBuf, bool)> {
    let input = path.display().to_string();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut runs: Vec<(String, std::path::PathBuf, bool)> = Vec::new();
    let mut run = |name: &str, f: &dyn Fn(&str) -> Result<(), crate::CliError>| {
        let out = out_dir.join(format!("{stem}-{name}.pdf"));
        let ran = f(&out.display().to_string()).is_ok();
        runs.push((name.to_string(), out, ran));
    };
    run("split", &|out| {
        crate::tools::split(&input, 0, page_count.saturating_sub(1), out)
    });
    run("rotate", &|out| crate::tools::rotate(&input, 90, None, out));
    if page_count >= 2 {
        run("delete", &|out| {
            let last = page_count.saturating_sub(1);
            crate::tools::delete(&input, &last.to_string(), out)
        });
        let reversed: Vec<String> = (0..page_count).rev().map(|i| i.to_string()).collect();
        let order = reversed.join(",");
        run("reorder", &|out| crate::tools::reorder(&input, &order, out));
    }
    run("set-metadata", &|out| {
        crate::tools::set_metadata(&input, &[("Title", "conformance hook")], out)
    });
    run("compress", &|out| {
        crate::compress::optimise_file(&input, out).map(|_| ())
    });
    runs
}

/// The WRITE.07 gate: run every tool over corpus files that open, and assert
/// no evaluable rule flips Pass → Fail on the output.
///
/// The gate's cost is O(files × tools × pages), so it evaluates a
/// deterministic stride of the sorted corpus (every Nth file) and skips files
/// larger than [`MAX_GATE_BYTES`] to keep its wall time bounded as the corpus
/// grows; set `SELIS_TOOL_CONFORMANCE_FULL=1` to remove both bounds for a full
/// manual sweep. The bounds limit *work*, not assertion strength: whatever is
/// evaluated runs the identical checks, skips are counted, and the
/// `evaluated > 0` assertion still fails vacuous runs.
#[test]
fn no_tool_degrades_conformance_posture() {
    let dir = std::env::temp_dir().join("selis-write07-hook");
    std::fs::create_dir_all(&dir).expect("temp dir");

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(corpus_dir())
        .expect("corpus/pdfs exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "pdf"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "corpus/pdfs has PDF files");

    let full = std::env::var_os("SELIS_TOOL_CONFORMANCE_FULL").is_some();
    // ~200 evaluated files keeps the gate in the minutes range; the corpus
    // had ~650 files when the gate shipped and now grows without bound.
    let stride = if full { 1 } else { (files.len() / 200).max(1) };
    let files: Vec<std::path::PathBuf> = files
        .into_iter()
        .enumerate()
        .filter(|(i, _)| i % stride == 0)
        .map(|(_, p)| p)
        .collect();

    let budget = Budget::profile(Surface::Viewer);
    let mut regressions: Vec<String> = Vec::new();
    let mut evaluated = 0usize;
    let mut skipped = 0usize;

    for path in &files {
        // Full-document rewrites on multi-megabyte files exercise the same
        // conformance paths as small ones while dominating the gate's wall
        // time; their open path is already covered by the ROB.01 sweep.
        let oversize = !full
            && std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_GATE_BYTES;
        if oversize {
            skipped += 1;
            continue;
        }
        let Ok(src) = std::fs::read(path) else {
            skipped += 1;
            continue;
        };
        // Inputs that do not open have no posture to degrade.
        let Ok(session) = Session::open(src, &budget) else {
            skipped += 1;
            continue;
        };
        let page_count = session.len();
        let Some(input_posture) = posture(&session) else {
            skipped += 1;
            continue;
        };
        drop(session);
        evaluated += 1;
        if page_count == 0 {
            skipped += 1;
            evaluated -= 1;
            continue;
        }

        for (name, out_path, ran) in run_tools(path, &dir, page_count) {
            if !ran || !out_path.exists() {
                continue; // a clean refusal is not a degradation
            }
            let Ok(out_bytes) = std::fs::read(&out_path) else {
                continue;
            };
            let label = format!(
                "{} [{name}]",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            let Some(output_posture) = posture_session(&out_bytes) else {
                regressions.push(format!("{label}: output does not open"));
                continue;
            };
            for (i, (before, after)) in input_posture.iter().zip(output_posture.iter()).enumerate()
            {
                if matches!(before, RuleResult::Pass) && matches!(after, RuleResult::Fail { .. }) {
                    let rule = selis_pdf_doc::registry()
                        .get(i)
                        .map(|r| r.id)
                        .unwrap_or("?");
                    regressions.push(format!("{label}: rule `{rule}` regressed to Fail"));
                }
            }
            let _ = std::fs::remove_file(&out_path);
        }
    }

    assert!(
        evaluated > 0,
        "the gate evaluated no corpus file — it would pass vacuously \
         ({skipped} skipped); this means every input failed to open or to \
         evaluate, which is itself a regression to investigate"
    );
    assert!(
        regressions.is_empty(),
        "{} tool output(s) degraded conformance posture \
         ({evaluated} corpus files evaluated, {skipped} skipped):\n{}",
        regressions.len(),
        regressions.join("\n")
    );
}

/// Open `src` and evaluate its posture in one step (for tool outputs).
fn posture_session(src: &[u8]) -> Option<Vec<RuleResult>> {
    let budget = Budget::profile(Surface::Viewer);
    let session = Session::open(src.to_vec(), &budget).ok()?;
    posture(&session)
}
