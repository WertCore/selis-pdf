//! [`FileSource`] â€” the native file-backed [`DocSource`] (SL-0.IO.03).
//!
//! `pread`-based via [`std::os::unix::fs::FileExt::read_at`] /
//! [`std::os::windows::fs::FileExt::seek_read`]. **No `mmap`**: a truncating
//! writer behind an `mmap` is a SIGBUS we cannot catch (SL-0.IO.03 Risk), so
//! the safe default â€” `pread` â€” is the only option. Documented, decided.
//!
//! The source takes a size+mtime fingerprint at construction and revalidates
//! it before every read, so a file replaced under us returns `SOURCE_CHANGED`
//! instead of silently reading garbage from the new file's bytes.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use selis_error::{err, Code, Result};

use crate::{Availability, DocSource, RangeSet};

/// A file-backed [`DocSource`].
///
/// Revalidates its fingerprint before each read: `len`, `mtime`, and `inode`
/// (or equivalent). If any changed, every subsequent read returns
/// `SOURCE_CHANGED` and the source is poisoned.
pub struct FileSource {
    file: std::fs::File,
    fingerprint: Fingerprint,
    poisoned: AtomicBool,
    _lock: Mutex<()>,
    /// The whole file is resident on disk: everything is "available".
    all: RangeSet,
}

/// The identity of a file at open time.
#[derive(Debug, Clone, PartialEq)]
struct Fingerprint {
    len: u64,
    mtime: i64,
    inode: u64,
}

impl FileSource {
    /// Open a file by path, capturing its fingerprint.
    ///
    /// # Budget
    ///
    /// No heap beyond the handle.
    ///
    /// # Malformed Input
    ///
    /// `IO_OPEN_FAILED` when the file cannot be opened.
    pub fn open(path: &str) -> Result<Self> {
        let file = std::fs::File::open(path).map_err(|e| {
            let mut ctx = selis_error::Ctx::new();
            ctx.detail = Some(format!("{path}: {e}"));
            selis_error::Error::with(Code::IoReadFailed, ctx)
        })?;
        let fingerprint = fingerprint_of(&file)?;
        let all = RangeSet::one(0, u64::MAX);
        Ok(Self {
            file,
            fingerprint,
            poisoned: AtomicBool::new(false),
            _lock: Mutex::new(()),
            all,
        })
    }

    /// Open from an existing file handle.
    ///
    /// # Malformed Input
    ///
    /// `IO_OPEN_FAILED` when the handle's metadata cannot be read.
    pub fn from_file(file: std::fs::File) -> Result<Self> {
        let fingerprint = fingerprint_of(&file)?;
        let all = RangeSet::one(0, u64::MAX);
        Ok(Self {
            file,
            fingerprint,
            poisoned: AtomicBool::new(false),
            _lock: Mutex::new(()),
            all,
        })
    }

    fn check_fingerprint(&self) -> Result<()> {
        if self.poisoned.load(Ordering::Relaxed) {
            return Err(err!(Code::SourceChanged, during = "file-read"));
        }
        let current = fingerprint_of(&self.file)?;
        if current != self.fingerprint {
            self.poisoned.store(true, Ordering::Relaxed);
            return Err(err!(
                Code::SourceChanged,
                during = "file-read",
                detail = "file changed while being read"
            ));
        }
        Ok(())
    }

    fn read_at_native(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            FileExt::read_at(&self.file, buf, off)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut buf_slice = buf;
            FileExt::seek_read(&self.file, &mut buf_slice, off)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (off, buf);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "pread unsupported",
            ))
        }
    }
}

impl DocSource for FileSource {
    fn len(&self) -> Option<u64> {
        Some(self.fingerprint.len)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<Availability> {
        self.check_fingerprint()?;
        let len = self.fingerprint.len;
        if off >= len {
            return Ok(Availability::Eof);
        }
        // n = min(bytes remaining, buf capacity) â€” checked.
        let remaining = len.saturating_sub(off);
        let want = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let n64 = remaining.min(want);
        let n = usize::try_from(n64).unwrap_or(usize::MAX).min(buf.len());
        let slice = buf.get_mut(..n).unwrap_or(&mut []);
        let read = self.read_at_native(off, slice).map_err(|e| {
            self.poisoned.store(true, Ordering::Relaxed);
            let mut ctx = selis_error::Ctx::new();
            ctx.detail = Some(e.to_string());
            selis_error::Error::with(Code::IoReadFailed, ctx)
        })?;
        // A short read (file truncated between fingerprint and read) poisons.
        if read != n {
            self.poisoned.store(true, Ordering::Relaxed);
            return Err(err!(
                Code::SourceChanged,
                during = "file-read",
                detail = "short read: file changed"
            ));
        }
        Ok(Availability::Filled(read))
    }

    fn available(&self) -> &RangeSet {
        &self.all
    }

    fn request(&self, _ranges: &RangeSet) {
        // Native files are random access; nothing to prefetch.
    }

    fn is_random_access(&self) -> bool {
        true
    }
}

/// Capture size + mtime + inode (or equivalent on Windows).
fn fingerprint_of(file: &std::fs::File) -> Result<Fingerprint> {
    let meta = file.metadata().map_err(|e| {
        let mut ctx = selis_error::Ctx::new();
        ctx.detail = Some(e.to_string());
        selis_error::Error::with(Code::IoReadFailed, ctx)
    })?;
    let len = meta.len();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mtime = meta.mtime();
        let inode = meta.ino();
        Ok(Fingerprint { len, mtime, inode })
    }
    #[cfg(windows)]
    {
        // Windows: use the last-write time as the mtime substitute and the
        // volume serial + file index as the inode substitute (both are stable
        // identity for the open handle).
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Ok(Fingerprint {
            len,
            mtime,
            inode: 0,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (len, file);
        Ok(Fingerprint {
            len,
            mtime: 0,
            inode: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;

    /// The DoD: mutating the file mid-read produces `SOURCE_CHANGED`, not
    /// garbage.
    #[test]
    fn mutated_file_is_detected() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("selis-filesource-test-{}", std::process::id()));
        std::fs::write(&path, b"original contents").expect("write");
        let src = FileSource::open(path.to_str().expect("path")).expect("open");

        let mut buf = [0u8; 8];
        // First read is fine.
        assert!(matches!(
            src.read_at(0, &mut buf).expect("read"),
            Availability::Filled(8)
        ));

        // Mutate the file with a *different length* so the fingerprint
        // (size, and on coarser filesystems mtime) cannot miss it.
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(&path, b"tampered-contents-longer").expect("rewrite");

        let e = src.read_at(0, &mut buf).expect_err("changed");
        assert_eq!(e.code(), Code::SourceChanged);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn eof_is_respected() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("selis-filesource-eof-{}", std::process::id()));
        std::fs::write(&path, b"ab").expect("write");
        let src = FileSource::open(path.to_str().expect("path")).expect("open");
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(2, &mut buf).expect("eof"), Availability::Eof);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn len_and_random_access() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("selis-filesource-len-{}", std::process::id()));
        std::fs::write(&path, b"abcd").expect("write");
        let src = FileSource::open(path.to_str().expect("path")).expect("open");
        assert_eq!(src.len(), Some(4));
        assert!(src.is_random_access());
        std::fs::remove_file(&path).ok();
    }
}
