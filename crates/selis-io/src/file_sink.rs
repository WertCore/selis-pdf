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
        Ok(Self {
            file: Some(file),
            temp_path,
            final_path,
            bytes: 0,
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
        std::fs::rename(&self.temp_path, &self.final_path).map_err(|e| {
            let mut ctx = selis_error::Ctx::new();
            ctx.detail = Some(format!(
                "rename {} -> {}: {e}",
                self.temp_path.display(),
                self.final_path.display()
            ));
            selis_error::Error::with(Code::IoReadFailed, ctx)
        })?;
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
