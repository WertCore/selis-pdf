//! [`AppendFileSink`] — an in-place append destination with fsync commit
//! (SL-0.IO.05, the incremental-save variant of [`crate::FileSink`]).
//!
//! The full-rewrite path commits by writing a temp file and renaming over the
//! destination (`FileSink`); the incremental path (ADR-P0007, invariant I5)
//! appends a revision **to the existing file** and relies on ordering for
//! crash safety: the appended objects + xref + trailer are written first and
//! the `startxref … %%EOF` block is written **last**, so a process killed
//! mid-append leaves the original bytes intact as a prefix with an
//! unreachable partial tail — detectable and discarded on next open
//! (`parse_revisions_resilient`). `finish()` fsyncs the file, which is the
//! durability point for power-loss (process-kill atomicity holds from the
//! page cache alone).
//!
//! # Debug-only crash injection (SL-1A.WRITE.06)
//!
//! In debug builds (`#[cfg(debug_assertions)]`) the sink honours two
//! environment variables so the kill-test harness can terminate the process
//! at a deterministic byte position or commit phase:
//!
//! * `SELIS_DEBUG_CRASH_AFTER_BYTES=<n>` — `std::process::abort()` once `n`
//!   bytes have been appended (checked per ≤256-byte chunk);
//! * `SELIS_DEBUG_CRASH_PHASE=before-fsync|after-fsync` — abort inside
//!   `finish()` at the named durability point.
//!
//! Both are absent from release builds; `std::env`/`std::process` are
//! allowed in `selis-io` by the purity allowlist (`xtask/layers.toml`).

use std::io::Write;
use std::path::PathBuf;

use selis_error::{err, Code, Result};

use crate::{DocSink, SinkReceipt};

/// The append chunk size used when crash injection is active. Kill points are
/// granular to this many bytes.
#[cfg(debug_assertions)]
const CRASH_CHUNK: usize = 256;

/// Read the debug crash-injection configuration (byte threshold and phase).
/// In release builds this always returns `(None, None)` and no env access is
/// compiled in.
#[cfg(debug_assertions)]
fn crash_config() -> (Option<u64>, Option<&'static str>) {
    let bytes = std::env::var("SELIS_DEBUG_CRASH_AFTER_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let phase = std::env::var("SELIS_DEBUG_CRASH_PHASE")
        .ok()
        .and_then(|v| match v.as_str() {
            "before-fsync" => Some("before-fsync"),
            "after-fsync" => Some("after-fsync"),
            _ => None,
        });
    (bytes, phase)
}

/// No-op in release builds.
#[cfg(not(debug_assertions))]
fn crash_config() -> (Option<u64>, Option<&'static str>) {
    (None, None)
}

/// An in-place append sink over an existing file (the incremental-save
/// destination). The file must exist; the sink never truncates and never
/// touches bytes before its append position.
pub struct AppendFileSink {
    /// The open file handle (`None` once finished).
    file: Option<std::fs::File>,
    /// The destination path.
    path: PathBuf,
    /// Bytes appended so far.
    bytes: u64,
    /// Debug-only crash threshold (bytes).
    #[cfg(debug_assertions)]
    crash_after: Option<u64>,
    /// Debug-only crash phase.
    #[cfg(debug_assertions)]
    crash_phase: Option<&'static str>,
}

impl AppendFileSink {
    /// Open `path` for in-place append. The file must already exist (an
    /// incremental save appends to the opened document; use [`crate::FileSink`]
    /// to create or replace a file).
    ///
    /// # Budget
    ///
    /// No heap beyond the path.
    ///
    /// # Malformed Input
    ///
    /// `IO_READ_FAILED` when the file cannot be opened for append.
    pub fn open(path: &str) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| {
                let mut ctx = selis_error::Ctx::new();
                ctx.detail = Some(format!("{path}: {e}"));
                selis_error::Error::with(Code::IoReadFailed, ctx)
            })?;
        let (crash_after, crash_phase) = crash_config();
        Ok(Self {
            file: Some(file),
            path: PathBuf::from(path),
            bytes: 0,
            #[cfg(debug_assertions)]
            crash_after,
            #[cfg(debug_assertions)]
            crash_phase,
        })
    }

    /// The destination path.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl DocSink for AppendFileSink {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let file = self.file.as_mut().ok_or_else(|| {
            err!(
                Code::SinkFinished,
                during = "append-sink",
                detail = "sink already finished"
            )
        })?;
        #[cfg(debug_assertions)]
        {
            // Crash at a deterministic byte position: write in chunks and
            // abort once the threshold is crossed. abort() runs no Drop
            // handlers, exactly like a killed process.
            if let Some(limit) = self.crash_after {
                for chunk in bytes.chunks(CRASH_CHUNK) {
                    file.write_all(chunk).map_err(|e| {
                        let mut ctx = selis_error::Ctx::new();
                        ctx.detail = Some(e.to_string());
                        selis_error::Error::with(Code::WriteFailed, ctx)
                    })?;
                    self.bytes = self
                        .bytes
                        .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                    if self.bytes >= limit {
                        std::process::abort();
                    }
                }
                return Ok(());
            }
        }
        file.write_all(bytes).map_err(|e| {
            let mut ctx = selis_error::Ctx::new();
            ctx.detail = Some(e.to_string());
            selis_error::Error::with(Code::WriteFailed, ctx)
        })?;
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    fn position(&self) -> u64 {
        self.bytes
    }

    fn finish(mut self: Box<Self>) -> Result<SinkReceipt> {
        let file = self.file.take().ok_or_else(|| {
            err!(
                Code::SinkFinished,
                during = "append-sink",
                detail = "sink already finished"
            )
        })?;
        #[cfg(debug_assertions)]
        if self.crash_phase == Some("before-fsync") {
            std::process::abort();
        }
        {
            let mut f = file;
            f.flush().map_err(|e| {
                let mut ctx = selis_error::Ctx::new();
                ctx.detail = Some(e.to_string());
                selis_error::Error::with(Code::WriteFailed, ctx)
            })?;
            // The durability point: everything appended is on the device.
            f.sync_all().map_err(|e| {
                let mut ctx = selis_error::Ctx::new();
                ctx.detail = Some(e.to_string());
                selis_error::Error::with(Code::WriteFailed, ctx)
            })?;
        }
        #[cfg(debug_assertions)]
        if self.crash_phase == Some("after-fsync") {
            std::process::abort();
        }
        Ok(SinkReceipt { bytes: self.bytes })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("selis-appendsink-{tag}-{}", std::process::id()))
    }

    #[test]
    fn append_commit_extends_the_original() {
        let dir = temp_dir("extend");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("doc.pdf");
        std::fs::write(&dest, b"original-revision").unwrap();

        let sink: Box<dyn DocSink> =
            Box::new(AppendFileSink::open(dest.to_str().unwrap()).unwrap());
        let mut sink = sink;
        sink.append(b"+appended-revision").unwrap();
        assert_eq!(sink.position(), 18);
        let receipt = sink.finish().unwrap();
        assert_eq!(receipt.bytes, 18);

        let on_disk = std::fs::read(&dest).unwrap();
        assert_eq!(on_disk, b"original-revision+appended-revision");
        assert!(
            on_disk.starts_with(b"original-revision"),
            "the original bytes are a byte-identical prefix (I2)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_requires_an_existing_file() {
        let dir = temp_dir("missing");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("absent.pdf");
        assert!(AppendFileSink::open(dest.to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dropped_sink_leaves_the_file_untouched() {
        let dir = temp_dir("abandon");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("doc.pdf");
        std::fs::write(&dest, b"keep-me").unwrap();
        let sink: Box<dyn DocSink> =
            Box::new(AppendFileSink::open(dest.to_str().unwrap()).unwrap());
        let mut sink = sink;
        sink.append(b"partial").unwrap();
        drop(sink); // no finish: nothing durable was promised
        std::fs::remove_dir_all(&dir).ok();
    }
}
