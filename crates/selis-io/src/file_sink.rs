//! Native [`DocSink`] with atomic commit (SL-0.IO.05).
//!
//! Writes go to a temp file in the destination directory; `finish()` flushes,
//! fsyncs, and atomically renames over the destination. A crash at any point
//! leaves either the original file or the complete new one — never a torn
//! document. This is the "write-to-temp-then-rename" variant for the rewrite
//! path; the incremental-save path appends in place (ADR-P0007).
//!
//! Compiled only under the `native-io` feature.

use std::io::Write;
use std::path::{Path, PathBuf};

use selis_error::{err, Code, Result};

use crate::{DocSink, SinkReceipt};

/// The write chunk size used when crash injection is active. Kill points are
/// granular to this many bytes.
#[cfg(debug_assertions)]
const CRASH_CHUNK: usize = 256;

/// Read the debug crash-injection configuration (byte threshold and phase)
/// for SL-1A.WRITE.06. Debug builds only; see `append_sink.rs`.
#[cfg(debug_assertions)]
fn crash_config() -> (Option<u64>, Option<&'static str>) {
    let bytes = std::env::var("SELIS_DEBUG_CRASH_AFTER_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let phase = std::env::var("SELIS_DEBUG_CRASH_PHASE")
        .ok()
        .and_then(|v| match v.as_str() {
            "before-rename" => Some("before-rename"),
            "after-rename" => Some("after-rename"),
            "before-fsync" => Some("before-fsync"),
            _ => None,
        });
    (bytes, phase)
}

/// A file-backed sink that commits atomically.
pub struct FileSink {
    /// The temp file being written (`None` once finished).
    file: Option<std::fs::File>,
    /// The temp path.
    temp_path: PathBuf,
    /// The final destination (renamed over on finish).
    final_path: PathBuf,
    /// Bytes written so far.
    bytes: u64,
    /// Debug-only crash threshold (bytes written to the temp file).
    #[cfg(debug_assertions)]
    crash_after: Option<u64>,
    /// Debug-only crash phase.
    #[cfg(debug_assertions)]
    crash_phase: Option<&'static str>,
}

impl FileSink {
    /// Create a sink that will atomically replace `path` on finish.
    ///
    /// The temp file is created in the same directory as `path` (required for
    /// an atomic rename on the same filesystem).
    ///
    /// # Budget
    ///
    /// No heap beyond the paths.
    ///
    /// # Malformed Input
    ///
    /// `IO_READ_FAILED` when the destination directory is not writable.
    pub fn create(path: &str) -> Result<Self> {
        let final_path = PathBuf::from(path);
        let parent = final_path.parent().unwrap_or(Path::new("."));
        let file_name = final_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("out.pdf");
        let temp_name = format!(".{file_name}.selis-tmp-{}", std::process::id());
        let temp_path = parent.join(temp_name);
        let file = std::fs::File::create(&temp_path).map_err(|e| {
            let mut ctx = selis_error::Ctx::new();
            ctx.detail = Some(format!("{}: {e}", temp_path.display()));
            selis_error::Error::with(Code::IoReadFailed, ctx)
        })?;
        #[cfg(debug_assertions)]
        let (crash_after, crash_phase) = crash_config();
        Ok(Self {
            file: Some(file),
            temp_path,
            final_path,
            bytes: 0,
            #[cfg(debug_assertions)]
            crash_after,
            #[cfg(debug_assertions)]
            crash_phase,
        })
    }

    /// The final destination path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.final_path
    }
}

impl DocSink for FileSink {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let file = self.file.as_mut().ok_or_else(|| {
            err!(
                Code::SinkFinished,
                during = "file-sink",
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
                        selis_error::Error::with(Code::IoReadFailed, ctx)
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
            selis_error::Error::with(Code::IoReadFailed, ctx)
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
        // Flush, fsync, then atomically rename over the destination.
        let file = self.file.take().ok_or_else(|| {
            err!(
                Code::SinkFinished,
                during = "file-sink",
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
                selis_error::Error::with(Code::IoReadFailed, ctx)
            })?;
            f.sync_all().map_err(|e| {
                let mut ctx = selis_error::Ctx::new();
                ctx.detail = Some(e.to_string());
                selis_error::Error::with(Code::IoReadFailed, ctx)
            })?;
            // f drops here, closing the temp handle BEFORE the rename
            // (Windows cannot rename an open file).
        }
        #[cfg(debug_assertions)]
        if self.crash_phase == Some("before-rename") {
            std::process::abort();
        }
        // Atomic swap (ADR-P0037): MoveFileEx(REPLACE_EXISTING|WRITE_THROUGH)
        // on Windows, rename(2) on POSIX. The temp handle is closed above
        // (Windows cannot replace an open file) and the temp content was
        // fsynced; the swap itself cannot tear.
        crate::replace::atomic_replace(&self.temp_path, &self.final_path)?;
        #[cfg(debug_assertions)]
        if self.crash_phase == Some("after-rename") {
            std::process::abort();
        }
        Ok(SinkReceipt { bytes: self.bytes })
    }
}

/// A `Drop` that removes the temp file if the sink is abandoned before
/// `finish`, so a failed write never leaves litter.
impl Drop for FileSink {
    fn drop(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.temp_path);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("selis-filesink-{tag}-{}", std::process::id()))
    }

    #[test]
    fn atomic_commit_replaces_the_destination() {
        let dir = temp_dir("atomic");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let dest = dir.join("out.pdf");
        // Original file exists.
        std::fs::write(&dest, b"original").expect("write original");

        let sink: Box<dyn DocSink> =
            Box::new(FileSink::create(dest.to_str().expect("path")).expect("create"));
        let mut sink = sink;
        sink.append(b"new contents").expect("append");
        let receipt = sink.finish().expect("finish");

        assert_eq!(receipt.bytes, 12);
        assert_eq!(std::fs::read(&dest).expect("read"), b"new contents");
        // No temp litter remains.
        let leftovers = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("selis-tmp"))
            .count();
        assert_eq!(leftovers, 0, "temp file must be cleaned up");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn abandoned_sink_leaves_original_intact() {
        let dir = temp_dir("abandon");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let dest = dir.join("out.pdf");
        std::fs::write(&dest, b"original").expect("write original");

        // Create, append, then abandon (drop without finish).
        let sink: Box<dyn DocSink> =
            Box::new(FileSink::create(dest.to_str().expect("path")).expect("create"));
        let mut sink = sink;
        sink.append(b"partial").expect("append");
        drop(sink);

        // The original is untouched.
        assert_eq!(std::fs::read(&dest).expect("read"), b"original");
        std::fs::remove_dir_all(&dir).ok();
    }
}
