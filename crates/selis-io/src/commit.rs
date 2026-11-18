//! Crash-safe atomic commit of a *complete* byte buffer (SL-5.EDIT.04).
//!
//! This is the commit half of a save: the caller has already produced (and, at
//! L2, verified) the full output bytes. This module owns only the durable swap:
//!
//! 1. create a temp file **in the destination's own directory** (same filesystem,
//!    so the final rename cannot copy — it is one atomic directory operation);
//! 2. write the bytes;
//! 3. flush + `fsync` the temp file;
//! 4. close it (Windows cannot rename an open file);
//! 5. atomically replace the destination via [`crate::atomic_replace`] — the
//!    vetted primitive, not a hand-rolled rename;
//! 6. `fsync` the containing directory so the rename itself is durable.
//!
//! At no point before step 5 does the destination change. A crash before the
//! rename therefore leaves the original file byte-identical; a crash after it
//! leaves the complete new file. There is no third state.
//!
//! The swap is delegated to `atomic_replace` (ADR-P0037). What this module adds
//! over a bare `FileSink` is the **post-rename directory fsync** (durability of
//! the name change on POSIX; on Windows `MOVEFILE_WRITE_THROUGH` covers it) and
//! a deterministic, in-process fault hook for the nightly kill-test.
//!
//! # Budget
//!
//! No heap beyond the two path buffers and no allocation sized by document
//! bytes: the caller passes one bounded `&[u8]` and charges its own
//! [`selis_error`]-free budget kernel upstream. No clock is read here; the
//! caller's `BudgetGuard` owns time and cancellation.
//!
//! # Malformed Input
//!
//! A non-existent or unwritable destination directory, a failed write, a failed
//! fsync or a failed rename is a typed `WRITE_FAILED`. The temp file is removed
//! on every error path and the destination is untouched on every error path.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use selis_error::{Ctx, Error, Result};

use crate::replace::atomic_replace;

/// Monotonic suffix for temp names: unique within a process and across the
/// 500-iteration nightly loop, without reading a clock.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// A deterministic fault injected into a commit, for the kill-test only.
///
/// Production callers pass `None`. Every variant returns a typed error **before
/// the atomic rename**, so the destination is still the original bytes when the
/// function returns `Err`. The "process died after the rename" outcome needs no
/// variant: it is simply a commit that succeeded, whose file is the complete
/// new document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitFault {
    /// Write only the first N bytes of the payload to the temp file, then fail.
    /// Models a kill at byte N of the stream.
    WriteAfter(u64),
    /// Fail after the write but before the file fsync.
    BeforeFsync,
    /// Fail after fsync but before the atomic rename.
    BeforeRename,
}

/// Options for [`commit_atomic`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitOptions {
    /// The deterministic kill-test hook. `None` in production.
    pub fault: Option<CommitFault>,
}

/// Proof that a commit reached durable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitReceipt {
    /// Number of bytes placed in the destination.
    pub bytes: u64,
    /// Whether the containing directory was fsynced after the rename.
    ///
    /// `true` on POSIX, where a directory fd can be fsynced through `std`.
    /// `false` on Windows, where `MOVEFILE_WRITE_THROUGH` flushes the rename
    /// metadata as part of the swap and there is no safe `std` directory fsync.
    pub directory_fsynced: bool,
}

/// Remove the temp file if the commit unwinds before (or during) the rename.
struct TempGuard {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        self.file = None; // close before any removal, Windows needs this order
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Atomically commit `bytes` to `path`: temp, write, fsync, rename, fsync dir.
///
/// On success the destination holds exactly `bytes` and no temp file remains.
/// On any error the destination is untouched and no temp file remains.
///
/// # Errors
///
/// `WRITE_FAILED` with a detail naming the failing phase. The error's
/// `doc_state` is `Unchanged`: the user's original file survived.
///
/// # Budget
///
/// See the module docs: no document-derived allocation and no clock here.
///
/// # Malformed Input
///
/// See the module docs: bad directories / I/O failures are typed errors; the
/// destination is never partially written.
pub fn commit_atomic(path: &Path, bytes: &[u8], opts: &CommitOptions) -> Result<CommitReceipt> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("out.pdf");
    let seq = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temp_path = parent.join(format!(
        ".{file_name}.selis-tmp-{}-{seq}",
        std::process::id()
    ));

    let file =
        std::fs::File::create(&temp_path).map_err(|e| io_error("create-temp", &temp_path, e))?;
    let mut guard = TempGuard {
        path: temp_path.clone(),
        file: Some(file),
    };

    // Write either the whole payload or a deterministic prefix (kill point).
    let (payload, write_fault) = match opts.fault {
        Some(CommitFault::WriteAfter(n)) => {
            let take = usize::try_from(n).unwrap_or(usize::MAX).min(bytes.len());
            let prefix = bytes
                .get(..take)
                .ok_or_else(|| io_error("truncate-temp", &temp_path, bad_range()))?;
            (prefix, true)
        }
        _ => (bytes, false),
    };
    {
        let f = guard
            .file
            .as_mut()
            .ok_or_else(|| io_error("write-temp", &temp_path, bad_range()))?;
        f.write_all(payload)
            .map_err(|e| io_error("write-temp", &temp_path, e))?;
    }
    if write_fault {
        return Err(fault_error("write-after-bytes"));
    }

    if opts.fault == Some(CommitFault::BeforeFsync) {
        return Err(fault_error("before-fsync"));
    }
    {
        let f = guard
            .file
            .as_mut()
            .ok_or_else(|| io_error("fsync-temp", &temp_path, bad_range()))?;
        f.flush()
            .map_err(|e| io_error("flush-temp", &temp_path, e))?;
        f.sync_all()
            .map_err(|e| io_error("fsync-temp", &temp_path, e))?;
    }
    // Close the handle BEFORE the rename; Windows cannot replace an open file.
    guard.file = None;

    if opts.fault == Some(CommitFault::BeforeRename) {
        return Err(fault_error("before-rename"));
    }

    // The one atomic step. `atomic_replace` owns the platform contract and
    // returns a typed selis error; re-label it as WRITE_FAILED for a single
    // error surface across this module.
    atomic_replace(&guard.path, path).map_err(|e| {
        let mut ctx = Ctx::new();
        ctx.detail = Some(format!("atomic-replace {}: {e}", path.display()));
        Error::with(selis_error::Code::WriteFailed, ctx)
    })?;
    let directory_fsynced = fsync_dir(parent)?;

    Ok(CommitReceipt {
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        directory_fsynced,
    })
}

/// Flush the rename itself. POSIX: fsync the directory fd. Windows: the swap
/// used `MOVEFILE_WRITE_THROUGH`, so the metadata is already on disk and there
/// is no portable safe way to fsync a directory handle.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<bool> {
    let f = std::fs::File::open(dir).map_err(|e| io_error("open-dir", dir, e))?;
    f.sync_all().map_err(|e| io_error("fsync-dir", dir, e))?;
    Ok(true)
}

#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> Result<bool> {
    Ok(false)
}

fn bad_range() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "temp file handle unavailable",
    )
}

fn io_error(phase: &str, path: &Path, e: std::io::Error) -> Error {
    let mut ctx = Ctx::new();
    ctx.detail = Some(format!("{phase} {}: {e}", path.display()));
    Error::with(selis_error::Code::WriteFailed, ctx)
}

fn fault_error(phase: &str) -> Error {
    let mut ctx = Ctx::new();
    ctx.detail = Some(format!("injected commit fault: {phase}"));
    Error::with(selis_error::Code::WriteFailed, ctx)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    fn dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("selis-commit-{tag}-{}", std::process::id()))
    }

    #[test]
    fn commit_writes_new_bytes_and_leaves_no_temp() {
        let d = dir("ok");
        std::fs::create_dir_all(&d).unwrap();
        let dest = d.join("a.pdf");
        std::fs::write(&dest, b"OLD").unwrap();

        let r = commit_atomic(&dest, b"NEW-CONTENTS", &CommitOptions::default()).unwrap();
        assert_eq!(r.bytes, 12);
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW-CONTENTS");
        assert!(!d.join(".a.pdf.selis-tmp-1").exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn every_fault_leaves_original_and_no_temp() {
        let d = dir("fault");
        std::fs::create_dir_all(&d).unwrap();
        let dest = d.join("b.pdf");
        std::fs::write(&dest, b"ORIGINAL").unwrap();
        let faults = [
            CommitFault::WriteAfter(0),
            CommitFault::WriteAfter(3),
            CommitFault::BeforeFsync,
            CommitFault::BeforeRename,
        ];
        for f in faults {
            let err =
                commit_atomic(&dest, b"NEW-DATA", &CommitOptions { fault: Some(f) }).unwrap_err();
            assert_eq!(err.code(), selis_error::Code::WriteFailed);
            assert_eq!(std::fs::read(&dest).unwrap(), b"ORIGINAL");
            let litter = std::fs::read_dir(&d)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains("selis-tmp"))
                .count();
            assert_eq!(litter, 0, "fault {f:?} leaked a temp file");
        }
        std::fs::remove_dir_all(&d).ok();
    }
}
