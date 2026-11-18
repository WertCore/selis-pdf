//! Kill-test and verification-before-replace tests for SL-5.EDIT.04.
//!
//! The nightly suite simulates a kill at 500 deterministic points by injecting
//! a fault into the commit (truncate the temp write at byte N, fail before the
//! fsync, fail before the rename) and checking the on-disk file afterwards.
//! A real `process::abort()` would kill the test runner, so the hook returns a
//! typed `WRITE_FAILED` at exactly the same phases — Drop runs and the temp is
//! removed, which is what matters for the invariant under test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::path::Path;

use selis_bytes::Bytes;
use selis_error::Code;
use selis_pdf_cos::Obj;
use selis_sandbox::Budget;

use crate::incremental::{materialise, save_incremental, ObjectMutation};
use crate::save::{save_atomic, save_atomic_with, SaveFault, SaveOptions};

fn intended_output() -> (Vec<u8>, Vec<u8>) {
    let src = crate::incremental_tests::base_pdf();
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let model = materialise(&src, &budget, &mut g).expect("materialise");
    let mutations = vec![ObjectMutation::SetObject {
        num: 5,
        value: Obj::String(Bytes::copy_from_slice(b"crash-safe-edit")),
    }];
    let (out, report) =
        save_incremental(&src, &model, &mutations, &budget, &mut g).expect("save_incremental");
    assert!(report.prefix_preserved, "EDIT.03 produced a valid prefix");
    assert!(out.len() > src.len(), "the mutation appended a revision");
    (src, out)
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("selis-edit04-{tag}-{}", std::process::id()))
}

fn no_temp_litter(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            !rd.filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().contains("selis-tmp"))
        })
        .unwrap_or(false)
}

fn valid_document(bytes: &[u8]) -> bool {
    let budget = Budget::unlimited();
    let mut g = budget.guard();
    let Some(start) = selis_pdf_cos::xref::find_startxref(bytes, 4096) else {
        return false;
    };
    let strict = selis_pdf_cos::parse_revisions(bytes, start, &budget, &mut g).is_ok();
    let resilient = selis_pdf_cos::parse_revisions_resilient(bytes, start, &budget, &mut g)
        .ok()
        .flatten()
        .is_some();
    let pages_ok = selis_pdf_cos::verify::survey(bytes, &budget, &mut g)
        .map(|o| o.pages == 1)
        .unwrap_or(false);
    strict && resilient && pages_ok
}

/// 500 deterministic pseudo-random kill points across the whole commit:
/// byte positions in the temp write plus the fsync and rename phase boundaries.
/// A fixed seed makes the nightly run reproducible.
fn kill_points(count: usize, len: u64) -> Vec<SaveFault> {
    let span = len.max(1);
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..count)
        .map(|i| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let pick = state >> 33;
            match i % 11 {
                0 => SaveFault::BeforeFsync,
                1 => SaveFault::BeforeRename,
                _ => SaveFault::WriteAfter(1u64.saturating_add(pick % span).min(len)),
            }
        })
        .collect()
}

#[test]
fn kill_test_500_points_never_damage_the_destination() {
    let (src, intended) = intended_output();
    let dir = temp_dir("kill-500");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dest = dir.join("doc.pdf");
    let budget = Budget::unlimited();
    let points = kill_points(500, u64::try_from(intended.len()).unwrap_or(u64::MAX));

    let mut passed = 0usize;
    for (i, fault) in points.iter().copied().enumerate() {
        std::fs::write(&dest, &src).expect("reset original");
        let mut g = budget.guard();
        let result = save_atomic_with(
            &dest,
            &src,
            &intended,
            &SaveOptions { fault: Some(fault) },
            &budget,
            &mut g,
        );
        let err = result.expect_err("every injected fault aborts the save");
        assert_eq!(err.code(), Code::WriteFailed, "kill point {i} ({fault:?})");

        let on_disk = std::fs::read(&dest).expect("read destination");
        // The DoD predicate: original, OR a valid document holding a prefix of
        // the intended changes. `intended` starts with `src`, so both branches
        // satisfy `intended[..on_disk.len()] == on_disk`.
        let original_or_valid_prefix = on_disk == src
            || (intended.get(..on_disk.len()) == Some(on_disk.as_slice())
                && valid_document(&on_disk));
        assert!(
            original_or_valid_prefix,
            "kill point {i} ({fault:?}) left an invalid file"
        );
        // Our commit is stronger than the DoD: every fault lands before the
        // rename, so the destination is exactly the original bytes.
        assert_eq!(
            on_disk, src,
            "kill point {i}: pre-rename fault changed the file"
        );
        assert!(no_temp_litter(&dir), "kill point {i}: temp file leaked");
        passed = passed.saturating_add(1);
    }

    assert_eq!(passed, 500, "all 500 kill points must be exercised");
    eprintln!("SL-5.EDIT.04 kill-test: {passed}/500 kill points left the original intact");

    // The complementary outcome: with no fault the full new document lands.
    std::fs::write(&dest, &src).expect("reset original");
    let mut g = budget.guard();
    save_atomic(&dest, &src, &intended, &budget, &mut g).expect("clean save");
    let on_disk = std::fs::read(&dest).expect("read destination");
    assert_eq!(
        on_disk, intended,
        "a clean kill-free save commits every byte"
    );
    assert!(valid_document(&on_disk));
    assert!(no_temp_litter(&dir));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn torn_output_is_refused_before_rename_with_no_file() {
    let (src, intended) = intended_output();
    let dir = temp_dir("verify-torn");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dest = dir.join("doc.pdf");
    std::fs::write(&dest, &src).expect("write original");
    let budget = Budget::unlimited();

    // Corrupt the appended revision by chopping its final `%%EOF`. The original
    // startxref is still present, so a naive verifier would accept this file.
    let keep = intended.len().saturating_sub(8);
    let corrupt = intended.get(..keep).expect("truncate tail").to_vec();
    assert!(corrupt.get(..src.len()) == Some(src.as_slice()));

    let mut g = budget.guard();
    let err = save_atomic(&dest, &src, &corrupt, &budget, &mut g)
        .expect_err("corrupt output must be refused");
    assert_eq!(err.code(), Code::VerifyFailed);
    assert!(
        format!("{err:?}").contains("%%EOF"),
        "the typed error names the discrepancy"
    );
    assert_eq!(
        std::fs::read(&dest).expect("read destination"),
        src,
        "the original survived byte-for-byte"
    );
    assert!(no_temp_litter(&dir), "no partial file was left behind");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupted_xref_is_refused_before_rename_with_no_file() {
    let (src, intended) = intended_output();
    let dir = temp_dir("verify-xref");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dest = dir.join("doc.pdf");
    std::fs::write(&dest, &src).expect("write original");
    let budget = Budget::unlimited();

    // Point the appended `startxref` at a bogus offset: the prefix and the
    // %%EOF tail still hold, but the strict parse must fail and no file may be
    // committed. This is a real xref corruption, not a harmless text edit.
    let mut corrupt = intended.clone();
    let keyword = b"startxref";
    let appended = intended.get(src.len()..).expect("appended tail");
    let rel = appended
        .windows(keyword.len())
        .position(|w| w == keyword)
        .expect("appended startxref");
    let mut j = src.len().saturating_add(rel).saturating_add(keyword.len());
    while matches!(corrupt.get(j), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        j = j.saturating_add(1);
    }
    while matches!(corrupt.get(j), Some(b'0'..=b'9')) {
        if let Some(b) = corrupt.get_mut(j) {
            *b = b'9';
        }
        j = j.saturating_add(1);
    }
    let mut g = budget.guard();
    let err = save_atomic(&dest, &src, &corrupt, &budget, &mut g)
        .expect_err("corrupt xref must be refused");
    assert_ne!(err.code(), Code::WriteFailed, "refused before any I/O");
    assert_eq!(
        std::fs::read(&dest).expect("read destination"),
        src,
        "the original survived byte-for-byte"
    );
    assert!(no_temp_litter(&dir));
    std::fs::remove_dir_all(&dir).ok();
}
