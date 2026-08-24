//! In-memory [`DocSource`] — the test and fuzz workhorse (SL-0.IO.02).

use std::sync::Arc;

use crate::{Availability, DocSource, RangeSet};

/// A [`DocSource`] backed by an already-resident byte buffer.
///
/// The fuzz and test workhorse, and the default source for a local file that
/// has already been loaded. Every read returns `Filled` immediately — there is
/// never a `Pending` moment.
#[derive(Debug, Clone)]
pub struct MemSource {
    data: Arc<[u8]>,
    available: RangeSet,
}

impl MemSource {
    /// Wrap a buffer.
    #[must_use]
    pub fn new(data: &[u8]) -> Self {
        let len = data.len() as u64;
        let mut available = RangeSet::new();
        if len > 0 {
            available.insert(0, len);
        }
        Self {
            data: data.to_vec().into_boxed_slice().into(),
            available,
        }
    }

    /// The total length.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.data.len() as u64
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl DocSource for MemSource {
    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> selis_error::Result<Availability> {
        let len = self.data.len() as u64;
        if off >= len {
            return Ok(Availability::Eof);
        }
        // `off` is inside the buffer, so the usize cast is exact on every
        // supported target (the buffer itself fits in usize).
        let start = usize::try_from(off).unwrap_or(usize::MAX);
        // n = min(bytes remaining, buf capacity) — checked, never wraps.
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
        // Everything is already resident; this is a no-op.
    }

    fn is_random_access(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_source_reads_exact() {
        let src = MemSource::new(b"Hello, world!");
        let mut buf = [0u8; 5];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(5));
        assert_eq!(&buf, b"Hello");
    }

    #[test]
    fn mem_source_eof_is_respected() {
        let src = MemSource::new(b"ab");
        let mut buf = [0u8; 4];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(2));
        assert_eq!(src.read_at(2, &mut buf).expect("eof"), Availability::Eof);
    }

    #[test]
    fn mem_source_len_and_random_access() {
        let src = MemSource::new(b"abcd");
        assert_eq!(src.len(), 4);
        assert_eq!(DocSource::len(&src), Some(4));
        assert!(src.is_random_access());
        assert!(!src.is_empty());
    }
}
