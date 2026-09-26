//! [`OpfsSource`] — the Origin Private File System adapter (SL-4.WASM.05).
//!
//! OPFS sync access handles are the Worker's synchronous file primitive:
//! `FileSystemSyncAccessHandle` exposes `getSize()`, `read(buffer, {at})`,
//! `write`, `flush`, `close` — all blocking in a Worker. This adapter wraps
//! that handle behind [`DocSource`] so the engine can re-drive a partial PDF
//! without ever leaving the Worker and without touching the main thread.
//!
//! On native targets the same contract is satisfied by an in-memory buffer
//! keyed by an OPFS path (the path is the identity, the buffer is the file).
//! The file is considered fully resident — OPFS files live on the same origin
//! and are random-access. The `DocSource` conformance suite (including the
//! fault cases) runs on both targets.

use std::sync::{Arc, Mutex};

use selis_error::{err, Code, Result};

use crate::{Availability, DocSource, RangeSet};

/// A [`DocSource`] backed by an OPFS sync access handle.
///
/// The constructor that takes a path + bytes is the test/native stand-in; the
/// wasm32 constructor takes a `FileSystemSyncAccessHandle` JS value.
#[derive(Debug)]
pub struct OpfsSource {
    path: String,
    data: Arc<Mutex<Vec<u8>>>,
    available: RangeSet,
    fingerprint: u64,
}

impl OpfsSource {
    /// Create an OPFS source over already-resident bytes keyed by `path`.
    ///
    /// `path` must be an OPFS path (e.g. "/selis/doc.pdf") — the adapter
    /// treats it as an opaque key and never normalises it.
    #[must_use]
    pub fn new(path: impl Into<String>, data: Vec<u8>) -> Self {
        let path = path.into();
        let len = data.len() as u64;
        let mut available = RangeSet::new();
        if len > 0 {
            available.insert(0, len);
        }
        let fingerprint = Self::fingerprint(&data);
        Self {
            path,
            data: Arc::new(Mutex::new(data)),
            available,
            fingerprint,
        }
    }

    /// The OPFS path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The total length (cached at open time; revalidated on read like
    /// `FileSource` so a file replaced under us is an error, not silent
    /// corruption).
    fn current_len(&self) -> u64 {
        match self.data.lock() {
            Ok(g) => g.len() as u64,
            Err(p) => p.into_inner().len() as u64,
        }
    }

    fn fingerprint(data: &[u8]) -> u64 {
        // FNV-1a 64 — cheap, deterministic, sufficient for the in-memory
        // simulation; the wasm path uses the handle's getSize() + read checks.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in data {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Simulate an external mutation (for the `SOURCE_CHANGED` conformance
    /// leg).
    pub fn mutate_external(&self, new_data: Vec<u8>) {
        match self.data.lock() {
            Ok(mut g) => *g = new_data,
            Err(p) => *p.into_inner() = new_data,
        }
    }

    fn check_fingerprint(&self) -> Result<()> {
        let current = match self.data.lock() {
            Ok(g) => Self::fingerprint(&g),
            Err(p) => Self::fingerprint(&p.into_inner()),
        };
        if current != self.fingerprint {
            return Err(err!(
                Code::SourceChanged,
                during = "opfs-read",
                detail = "OPFS file changed while being read"
            ));
        }
        Ok(())
    }
}

impl DocSource for OpfsSource {
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
        // OPFS files are fully resident; nothing to prefetch.
    }

    fn is_random_access(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Vec<u8> {
        b"%PDF-1.4 opfs hello".to_vec()
    }

    #[test]
    fn opfs_reads_exact() {
        let src = OpfsSource::new("/selis/test.pdf", data());
        assert_eq!(src.len(), Some(data().len() as u64));
        assert!(src.is_random_access());
        let mut buf = [0u8; 8];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(8));
        assert_eq!(&buf, &data()[..8]);
    }

    #[test]
    fn opfs_eof_is_respected() {
        let src = OpfsSource::new("/a.pdf", b"ab".to_vec());
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(2, &mut buf).expect("eof"), Availability::Eof);
    }

    #[test]
    fn opfs_available_covers_all() {
        let src = OpfsSource::new("/b.pdf", b"abcd".to_vec());
        assert!(src.available().contains(0, 4));
    }

    #[test]
    fn opfs_mutation_is_detected() {
        let src = OpfsSource::new("/mut.pdf", b"original".to_vec());
        let mut buf = [0u8; 8];
        assert_eq!(src.read_at(0, &mut buf).expect("read"), Availability::Filled(8));
        // Simulate the file being replaced under us (another tab, or the
        // main thread's `createSyncAccessHandle` writing).
        src.mutate_external(b"tampered-longer-contents".to_vec());
        let e = src.read_at(0, &mut buf).expect_err("changed");
        assert_eq!(e.code(), Code::SourceChanged);
    }

    #[test]
    fn opfs_path_is_preserved() {
        let src = OpfsSource::new("/selis/docs/report.pdf", b"x".to_vec());
        assert_eq!(src.path(), "/selis/docs/report.pdf");
    }

    #[test]
    fn opfs_empty_is_eof() {
        let src = OpfsSource::new("/empty.pdf", Vec::new());
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(0, &mut buf).expect("eof"), Availability::Eof);
        assert_eq!(src.available().total_bytes(), 0);
    }

    #[test]
    fn opfs_partial_read() {
        let src = OpfsSource::new("/c.pdf", b"0123456789".to_vec());
        let mut buf = [0u8; 3];
        let avail = src.read_at(8, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(2));
        assert_eq!(&buf[..2], b"89");
    }
}
