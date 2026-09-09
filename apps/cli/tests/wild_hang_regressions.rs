//! SL-1.ROB.01 hang-regression fixtures, hash-only (corpus policy §2).
//!
//! Two wild files that exceeded the open sweep's hang watchdog are pinned
//! here by SHA-256. The files themselves live outside the repo (wild cache,
//! never committed, never uploaded). When the cache holds a byte-identical
//! file, this test opens it under a fresh Viewer budget with a generous
//! watchdog and asserts it **terminates** with `Ok` or a **typed** error —
//! never a hang, never `INTERNAL_PANIC`.
//!
//! - `0002/0002365.pdf` (4.7 MB, `00138370…`): garbage xref offsets forced a
//!   full O(revisions × entries) map merge per resolved object; fixed by
//!   hoisting one merged revision view per resolve. Now fails typed
//!   `BUDGET_POISONED` in under a second.
//! - `0002/0002657.pdf` (25.5 MB, `0015f2b3…`): 47k-entry xref, same fix;
//!   now opens.
//!
//! Absent wild cache → skip (CI stays green without it). Cache present but a
//! pinned file missing or hash-changed → fail (the fixture drifted, which is
//! itself a signal, never a silent pass).
//!
//! ```text
//! cargo test -p selis-cli --test wild_hang_regressions -- --ignored --nocapture
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use selis_sandbox::{Budget, Surface};
use sha2::{Digest, Sha256};
use std::sync::mpsc;
use std::time::Duration;

/// Watchdog per file. Generous on purpose: this test detects *hangs*, not
/// slowness — the sweep's own tighter watchdog owns the wall budget.
const WATCHDOG_SECS: u64 = 180;

/// (relative id inside any wild batch, lowercase hex SHA-256).
const CASES: &[(&str, &str)] = &[
    (
        "0002/0002365.pdf",
        "00138370e7bf7863477e2bed5c25cdd6254beeb425e7847f4463980e98146021",
    ),
    (
        "0002/0002657.pdf",
        "0015f2b369c3506c50dac77bb96614b757f0d38c6967e246b70165f0b03b8ce0",
    ),
];

fn wild_root() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let root = std::path::PathBuf::from(home)
        .join(".cache")
        .join("selis-corpus")
        .join("wild");
    root.is_dir().then_some(root)
}

/// Find the batch file with a matching hash, whatever batch holds it.
fn locate(root: &std::path::Path, id: &str, sha: &str) -> Option<Vec<u8>> {
    let name = id.rsplit('/').next().unwrap_or(id);
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == name) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let mut h = Sha256::new();
                h.update(&bytes);
                if format!("{:x}", h.finalize()) == sha {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

#[test]
#[ignore = "needs the local wild cache (see pdf-plan/10 SL-0.CORP.05)"]
fn wild_hang_regressions_terminate_typed() {
    let Some(root) = wild_root() else {
        eprintln!("no wild cache; skipping hang-regression fixtures");
        return;
    };
    let mut evaluated = 0usize;
    for (id, sha) in CASES {
        let Some(bytes) = locate(&root, id, sha) else {
            panic!("hang-regression fixture missing or changed: {id} (expected sha {sha})");
        };
        let started = std::time::Instant::now();
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("hang-regression:{id}"))
            .spawn(move || {
                let budget = Budget::profile(Surface::Viewer);
                let result = selis_sandbox::trampoline::catch("hang-regression", || {
                    selis_pdf_engine::Session::open(bytes, &budget)
                });
                let _ = tx.send(result.map(|_| ()).map_err(|e| e.code().name().to_owned()));
            })
            .expect("spawn regression worker");
        match rx.recv_timeout(Duration::from_secs(WATCHDOG_SECS)) {
            Err(_) => panic!("hang regression: {id} ({}s watchdog)", WATCHDOG_SECS),
            Ok(Err(code)) => {
                assert_ne!(code, "INTERNAL_PANIC", "panic regression: {id} (sha {sha})");
                evaluated += 1;
                eprintln!(
                    "  {id} -> typed {code} ({}ms)",
                    started.elapsed().as_millis()
                );
            }
            Ok(Ok(())) => {
                evaluated += 1;
                eprintln!("  {id} -> ok ({}ms)", started.elapsed().as_millis());
            }
        }
    }
    assert!(evaluated > 0, "no hang-regression fixture evaluated");
}
