//! [`FsaSource`] — the File System Access handle adapter (SL-4.WASM.05).
//!
//! The File System Access API (`showOpenFilePicker` / `showSaveFilePicker`) is
//! the path to true save-in-place on desktop Chrome/Edge: the handle can be
//! kept across sessions and written back in place. For reading, the handle's
//! `getFile()` yields a `File` (a `Blob`) that the Worker reads with
//! `FileReaderSync` — synchronous in a Worker, async on the main thread. The
//! adapter exposes that file as a [`DocSource`] so the engine never blocks on
//! the network and can seek randomly.
//!
//! Like the other web adapters, the native test stand-in is an in-memory
//! buffer keyed by a handle id. The wasm32 path acquires a real
//! `FileSystemFileHandle` before constructing the source.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use selis_error::{err, Code, Result};

use crate::{Availability, DocSource, RangeSet};

/// A [`DocSource`] backed by a File System Access handle.
///
/// `handle_id` is the shell's opaque id for the handle (the map key the JS
/// glue uses to retrieve the real `FileSystemFileHandle`). The bytes are
/// cached at open time and revalidated on read — a file edited externally
/// between reads is `SOURCE_CHANGED`, not silent corruption.
#[derive(Debug)]
pub struct FsaSource {
    handle_id: String,
    name: String,
    data: Arc<Mutex<Vec<u8>>>,
    available: RangeSet,
    fingerprint: AtomicU64,
}

impl FsaSource {
    /// Create an FSA source over already-resident bytes keyed by `handle_id`.
    #[must_use]
    pub fn new(handle_id: impl Into<String>, name: impl Into<String>, data: Vec<u8>) -> Self {
        let handle_id = handle_id.into();
        let name = name.into();
        let len = data.len() as u64;
        let mut available = RangeSet::new();
        if len > 0 {
            available.insert(0, len);
        }
        let fingerprint = Self::fingerprint(&data);
        Self {
            handle_id,
            name,
            data: Arc::new(Mutex::new(data)),
            available,
            fingerprint: AtomicU64::new(fingerprint),
        }
    }

    /// The shell's handle id.
    #[must_use]
    pub fn handle_id(&self) -> &str {
        &self.handle_id
    }

    /// The file name reported by the handle.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn current_len(&self) -> u64 {
        match self.data.lock() {
            Ok(g) => g.len() as u64,
            Err(p) => p.into_inner().len() as u64,
        }
    }

    fn fingerprint(data: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in data {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Simulate an external edit (for the `SOURCE_CHANGED` leg).
    pub fn mutate_external(&self, new_data: Vec<u8>) {
        if let Ok(mut g) = self.data.lock() {
            *g = new_data;
        }
    }

    fn check_fingerprint(&self) -> Result<()> {
        let current = match self.data.lock() {
            Ok(g) => Self::fingerprint(&g),
            Err(p) => Self::fingerprint(&p.into_inner()),
        };
        if current != self.fingerprint.load(Ordering::Relaxed) {
            return Err(err!(
                Code::SourceChanged,
                during = "fsa-read",
                detail = "File System Access file changed while being read"
            ));
        }
        Ok(())
    }

    /// Replace the cached bytes (the save-in-place path writes through this).
    pub fn update(&self, new_data: Vec<u8>) {
        // In the real web path the handle's `createWritable()` writes; here we
        // update the in-memory stand-in and refresh the fingerprint so a
        // subsequent read sees the new file as the current file, not a
        // mutation.
        let fp = Self::fingerprint(&new_data);
        match self.data.lock() {
            Ok(mut g) => *g = new_data,
            Err(p) => *p.into_inner() = new_data,
        }
        self.fingerprint.store(fp, Ordering::Relaxed);
    }
}

impl DocSource for FsaSource {
    fn len(&self) -> Option<u64> {
        Some(self.current_len())
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<Availability> {
        self.check_fingerprint()?;
        let guard = match self.data.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let len = guard.len() as u64;
        if off >= len {
            return Ok(Availability::Eof);
        }
        let remaining = len.saturating_sub(off);
        let want = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let n64 = remaining.min(want);
        let n = usize::try_from(n64).unwrap_or(usize::MAX).min(buf.len());
        let start = usize::try_from(off).unwrap_or(usize::MAX);
        let end = start.saturating_add(n);
        let src = guard.get(start..end).unwrap_or(&[]);
        if let Some(dst) = buf.get_mut(..n) {
            dst.copy_from_slice(src);
        }
        Ok(Availability::Filled(n))
    }

    fn available(&self) -> &RangeSet {
        &self.available
    }

    fn request(&self, _ranges: &RangeSet) {
        // Fully resident; nothing to prefetch.
    }

    fn is_random_access(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Vec<u8> {
        b"%PDF-1.4 fsa hello".to_vec()
    }

    #[test]
    fn fsa_reads_exact() {
        let src = FsaSource::new("h1", "doc.pdf", data());
        assert_eq!(src.len(), Some(data().len() as u64));
        assert!(src.is_random_access());
        let mut buf = [0u8; 8];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(8));
        assert_eq!(&buf, &data()[..8]);
    }

    #[test]
    fn fsa_eof_is_respected() {
        let src = FsaSource::new("h1", "x.pdf", b"ab".to_vec());
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(2, &mut buf).expect("eof"), Availability::Eof);
    }

    #[test]
    fn fsa_available_covers_all() {
        let src = FsaSource::new("h1", "y.pdf", b"abcd".to_vec());
        assert!(src.available().contains(0, 4));
    }

    #[test]
    fn fsa_mutation_is_detected() {
        let src = FsaSource::new("h1", "mut.pdf", b"original".to_vec());
        let mut buf = [0u8; 8];
        assert_eq!(src.read_at(0, &mut buf).expect("read"), Availability::Filled(8));
        src.mutate_external(b"tampered-longer".to_vec());
        let e = src.read_at(0, &mut buf).expect_err("changed");
        assert_eq!(e.code(), Code::SourceChanged);
    }

    #[test]
    fn fsa_handles_and_names_preserved() {
        let src = FsaSource::new("handle-42", "report.pdf", b"x".to_vec());
        assert_eq!(src.handle_id(), "handle-42");
        assert_eq!(src.name(), "report.pdf");
    }

    #[test]
    fn fsa_empty_is_eof() {
        let src = FsaSource::new("h1", "empty.pdf", Vec::new());
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(0, &mut buf).expect("eof"), Availability::Eof);
    }

    #[test]
    fn fsa_partial_read() {
        let src = FsaSource::new("h1", "c.pdf", b"0123456789".to_vec());
        let mut buf = [0u8; 3];
        let avail = src.read_at(8, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(2));
        assert_eq!(&buf[..2], b"89");
    }

    #[test]
    fn fsa_update_refreshes_fingerprint() {
        let src = FsaSource::new("h1", "doc.pdf", b"v1".to_vec());
        src.update(b"v2-longer".to_vec());
        let mut buf = [0u8; 16];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(9));
        assert_eq!(&buf[..9], b"v2-longer");
        // Not a SOURCE_CHANGED — the update is intentional.
        assert_eq!(src.len(), Some(9));
    }
}
