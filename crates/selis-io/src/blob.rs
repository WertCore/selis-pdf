//! [`BlobSource`] — the `Blob` / `File` adapter (SL-4.WASM.05).
//!
//! In the browser a picked file is a `Blob` the main thread holds; the Worker
//! glue (`apps/web/host`) reads it with `FileReaderSync` on
//! `blob.slice(off, off+len)` — synchronous by construction in a Worker — and
//! registers the bytes with the WASM Worker before `open`. This crate holds
//! the same bytes behind [`DocSource`] so the conformance suite (including
//! the fault cases) runs on every target without a browser.

use std::sync::Arc;

use crate::{Availability, DocSource, RangeSet};

/// A [`DocSource`] backed by a browser `Blob`/`File` (or its in-memory
/// stand-in on native targets).
///
/// Every byte is resident from construction — there is never a `Pending`
/// moment — so the display-list and render paths can re-drive without a
/// network wake. `is_random_access` is always `true`.
#[derive(Debug, Clone)]
pub struct BlobSource {
    data: Arc<[u8]>,
    available: RangeSet,
    name: String,
}

impl BlobSource {
    /// Wrap an already-read `Blob` buffer.
    ///
    /// `name` is the file name the picker reported (used only for diagnostics).
    #[must_use]
    pub fn new(data: Vec<u8>, name: impl Into<String>) -> Self {
        let len = data.len() as u64;
        let mut available = RangeSet::new();
        if len > 0 {
            available.insert(0, len);
        }
        Self {
            data: data.into_boxed_slice().into(),
            available,
            name: name.into(),
        }
    }

    /// Wrap a shared slice (zero-copy clone).
    #[must_use]
    pub fn from_arc(data: Arc<[u8]>, name: impl Into<String>) -> Self {
        let len = data.len() as u64;
        let mut available = RangeSet::new();
        if len > 0 {
            available.insert(0, len);
        }
        Self {
            data,
            available,
            name: name.into(),
        }
    }

    /// The file name reported by the picker.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The total length in bytes.
    #[must_use]
    pub fn len_bytes(&self) -> u64 {
        self.data.len() as u64
    }

    /// Whether the blob is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The underlying bytes (for verification only — never document bytes in
    /// error payloads).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

impl DocSource for BlobSource {
    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> selis_error::Result<Availability> {
        let len = self.data.len() as u64;
        if off >= len {
            return Ok(Availability::Eof);
        }
        let start = usize::try_from(off).unwrap_or(usize::MAX);
        let remaining = len.saturating_sub(off);
        let want = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let n64 = remaining.min(want);
        let n = usize::try_from(n64).unwrap_or(usize::MAX).min(buf.len());
        let end = start.saturating_add(n);
        let src = self.data.get(start..end).unwrap_or(&[]);
        if let Some(dst) = buf.get_mut(..n) {
            dst.copy_from_slice(src);
        }
        Ok(Availability::Filled(n))
    }

    fn available(&self) -> &RangeSet {
        &self.available
    }

    fn request(&self, _ranges: &RangeSet) {
        // Everything is already resident.
    }

    fn is_random_access(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Vec<u8> {
        b"%PDF-1.4 hello world".to_vec()
    }

    #[test]
    fn blob_reads_exact() {
        let src = BlobSource::new(data(), "a.pdf");
        assert_eq!(src.len(), Some(data().len() as u64));
        assert!(src.is_random_access());
        assert_eq!(src.available().total_bytes(), data().len() as u64);
        let mut buf = [0u8; 8];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(8));
        assert_eq!(&buf, &data()[..8]);
    }

    #[test]
    fn blob_eof_is_respected() {
        let src = BlobSource::new(b"ab".to_vec(), "x.pdf");
        let mut buf = [0u8; 4];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(2));
        assert_eq!(src.read_at(2, &mut buf).expect("eof"), Availability::Eof);
        assert_eq!(src.read_at(100, &mut buf).expect("eof"), Availability::Eof);
    }

    #[test]
    fn blob_available_covers_all() {
        let src = BlobSource::new(b"abcd".to_vec(), "y.pdf");
        assert!(src.available().contains(0, 4));
        assert!(!src.available().contains(0, 5));
    }

    #[test]
    fn blob_empty_is_eof() {
        let src = BlobSource::new(Vec::new(), "empty.pdf");
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(0, &mut buf).expect("eof"), Availability::Eof);
        assert_eq!(src.available().total_bytes(), 0);
    }

    #[test]
    fn blob_request_is_noop() {
        let src = BlobSource::new(data(), "a.pdf");
        src.request(&RangeSet::one(0, 100));
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(0, &mut buf).expect("read"), Availability::Filled(4));
    }

    #[test]
    fn blob_conformance_partial_read() {
        // The conformance harness checks that a short buffer gets a short fill,
        // not an error.
        let src = BlobSource::new(b"0123456789".to_vec(), "n.pdf");
        let mut buf = [0u8; 3];
        let avail = src.read_at(8, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(2));
        assert_eq!(&buf[..2], b"89");
    }

    #[test]
    fn blob_large_slice() {
        let big = vec![0xABu8; 1 << 20];
        let src = BlobSource::new(big.clone(), "big.pdf");
        let mut buf = vec![0u8; 1 << 20];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(1 << 20));
        assert_eq!(buf, big);
    }
}
