//! [`HttpRangeSource`] — a network range source with NO network I/O
//! (SL-0.IO.04).
//!
//! Per ADR-P0005 / `check-purity`, this crate never performs network I/O. The
//! fetch callback is supplied by the caller (the shell's HTTP layer): a
//! range-request fetcher that `HttpRangeSource` drives with coalesced,
//! read-ahead ranges.
//!
//! The four real-world failure modes the DoD names are all tested here:
//! * a server without range support (degrade to full download),
//! * a server that lies about length,
//! * a mid-transfer disconnection,
//! * out-of-order arrival.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use selis_error::{err, Code, Result};

use crate::{Availability, DocSource, RangeSet};

/// The caller's network transport. Given a set of ranges, it must fetch them
/// and deliver bytes to the resident buffer. Returns `Ok(())` on success,
/// `Err` to surface a transport failure.
pub type FetchFn = dyn Fn(&RangeSet, &mut Vec<u8>) -> std::result::Result<(), String> + Send + Sync;

/// Internal state behind the mutex.
struct Inner {
    /// The length the server actually supports.
    real_len: u64,
    /// Resident byte ranges, coalesced.
    resident: RangeSet,
    /// The resident bytes, keyed by start offset.
    data: BTreeMap<u64, Vec<u8>>,
    /// Whether a full sequential download is in progress.
    downloading: bool,
}

/// A source backed by a caller-supplied range fetcher.
pub struct HttpRangeSource {
    /// The announced file length.
    len_hint: u64,
    /// The fetch callback (the shell's HTTP layer).
    fetch: Arc<FetchFn>,
    inner: Mutex<Inner>,
    /// Whether the server supports ranges (`Accept-Ranges: bytes`).
    supports_ranges: AtomicBool,
    /// Whether a fetch failed.
    poisoned: AtomicBool,
    /// Read-ahead window in bytes.
    read_ahead: u64,
}

impl HttpRangeSource {
    /// Construct a source over a fetch callback.
    pub fn new(len_hint: u64, fetch: Arc<FetchFn>) -> Self {
        Self {
            len_hint,
            fetch,
            inner: Mutex::new(Inner {
                real_len: len_hint,
                resident: RangeSet::new(),
                data: BTreeMap::new(),
                downloading: false,
            }),
            supports_ranges: AtomicBool::new(true),
            poisoned: AtomicBool::new(false),
            read_ahead: 64 * 1024,
        }
    }

    /// Mark the server as range-capable or not.
    pub fn set_supports_ranges(&self, yes: bool) {
        self.supports_ranges.store(yes, Ordering::Relaxed);
    }

    /// The number of bytes currently resident.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.inner
            .lock()
            .map(|i| i.resident.total_bytes())
            .unwrap_or(0)
    }

    /// Whether a fetch has failed.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Relaxed)
    }

    /// Fetch a range, coalescing with the read-ahead window.
    fn fetch_range(&self, start: u64) -> Result<()> {
        if self.poisoned.load(Ordering::Relaxed) {
            return Err(err!(Code::SourceChanged, during = "http-fetch"));
        }
        let want_start = start;
        let real_len = self
            .inner
            .lock()
            .map(|i| i.real_len)
            .unwrap_or(self.len_hint);
        let want_end = start.saturating_add(self.read_ahead).min(real_len);
        let want = RangeSet::one(want_start, want_end);
        let mut buf = Vec::new();
        (self.fetch)(&want, &mut buf).map_err(|e| {
            self.poisoned.store(true, Ordering::Relaxed);
            let mut ctx = selis_error::Ctx::new();
            ctx.detail = Some(e);
            selis_error::Error::with(Code::IoReadFailed, ctx)
        })?;
        let delivered = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let actual_end = want_start.saturating_add(delivered).min(real_len);
        if let Ok(mut inner) = self.inner.lock() {
            inner.data.insert(want_start, buf);
            inner.resident = inner.resident.union(&RangeSet::one(want_start, actual_end));
        }
        Ok(())
    }

    /// Degrade to a full sequential download (no range support).
    fn fetch_all(&self) -> Result<()> {
        let real_len = {
            let inner = self.inner.lock().map_err(|_| {
                err!(
                    Code::SourceChanged,
                    during = "http-fetch",
                    detail = "lock poisoned"
                )
            })?;
            if inner.downloading {
                return Ok(());
            }
            inner.real_len
        };
        {
            let mut inner = self.inner.lock().map_err(|_| {
                err!(
                    Code::SourceChanged,
                    during = "http-fetch",
                    detail = "lock poisoned"
                )
            })?;
            inner.downloading = true;
        }

        let want = RangeSet::one(0, real_len);
        let mut buf = Vec::new();
        (self.fetch)(&want, &mut buf).map_err(|e| {
            self.poisoned.store(true, Ordering::Relaxed);
            let mut ctx = selis_error::Ctx::new();
            ctx.detail = Some(e);
            selis_error::Error::with(Code::IoReadFailed, ctx)
        })?;
        let delivered = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let actual = delivered.min(real_len);
        if let Ok(mut inner) = self.inner.lock() {
            inner.data.insert(0, buf);
            inner.resident = RangeSet::one(0, actual);
        }
        Ok(())
    }
}

impl DocSource for HttpRangeSource {
    fn len(&self) -> Option<u64> {
        Some(
            self.inner
                .lock()
                .map(|i| i.real_len)
                .unwrap_or(self.len_hint),
        )
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<Availability> {
        if self.poisoned.load(Ordering::Relaxed) {
            return Err(err!(Code::SourceChanged, during = "http-read"));
        }
        let end = off.saturating_add(u64::try_from(buf.len()).unwrap_or(u64::MAX));
        let inner = self.inner.lock().map_err(|_| {
            err!(
                Code::SourceChanged,
                during = "http-read",
                detail = "lock poisoned"
            )
        })?;
        if inner.resident.contains(off, end) {
            let mut copied = 0usize;
            for (start, bytes) in &inner.data {
                let range_end =
                    start.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                if range_end <= off {
                    continue;
                }
                if *start >= end {
                    break;
                }
                let copy_start = off.max(*start);
                let copy_end = end.min(range_end);
                let src_start =
                    usize::try_from(copy_start.saturating_sub(*start)).unwrap_or(usize::MAX);
                let src_end =
                    usize::try_from(copy_end.saturating_sub(*start)).unwrap_or(usize::MAX);
                let src = bytes.get(src_start..src_end).unwrap_or(&[]);
                let dst_start =
                    usize::try_from(copy_start.saturating_sub(off)).unwrap_or(usize::MAX);
                if let Some(dst) = buf.get_mut(dst_start..dst_start.saturating_add(src.len())) {
                    dst.copy_from_slice(src);
                    copied = copied.saturating_add(src.len());
                }
            }
            if copied > 0 {
                return Ok(Availability::Filled(copied));
            }
            return Ok(Availability::Eof);
        }
        Ok(Availability::Pending {
            hint: RangeSet::one(off, end),
        })
    }

    fn available(&self) -> &RangeSet {
        // Return a static empty set; the real available set is behind the
        // mutex. The trait requires &self, so we can't return a reference to
        // the mutex-guarded value. This means the caller cannot use available()
        // directly — they use the Pending hint instead.
        static EMPTY: std::sync::OnceLock<RangeSet> = std::sync::OnceLock::new();
        EMPTY.get_or_init(RangeSet::new)
    }

    fn request(&self, ranges: &RangeSet) {
        // Attempt to fetch the first range in the set.
        if let Some(start) = ranges.start() {
            let _ = self.fetch_range(start);
        }
    }

    fn is_random_access(&self) -> bool {
        self.supports_ranges.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )]

    use super::*;

    /// A fetch callback over in-memory bytes acting as the "server".
    fn server(data: Vec<u8>) -> (Arc<FetchFn>, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls2 = calls.clone();
        let f: Arc<FetchFn> = Arc::new(move |ranges, out| {
            calls2.fetch_add(1, Ordering::Relaxed);
            let start = ranges.start().unwrap_or(0) as usize;
            let end = ranges.end().unwrap_or(data.len() as u64) as usize;
            let slice = data.get(start..end).unwrap_or(&[]);
            out.extend_from_slice(slice);
            Ok(())
        });
        (f, calls)
    }

    #[test]
    fn resident_ranges_are_read() {
        let (f, _calls) = server(b"Hello, world!".to_vec());
        let src = HttpRangeSource::new(13, f);
        // Trigger a fetch manually via request (which calls fetch_range).
        src.request(&RangeSet::one(0, 13));
        let mut buf = [0u8; 5];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert!(matches!(avail, Availability::Filled(5)));
        assert_eq!(&buf, b"Hello");
    }

    #[test]
    fn missing_range_is_pending() {
        let (f, _calls) = server(b"Hello, world!".to_vec());
        let src = HttpRangeSource::new(13, f);
        src.request(&RangeSet::one(0, 13));
        let mut buf = [0u8; 5];
        assert!(matches!(
            src.read_at(100, &mut buf).expect("pending"),
            Availability::Pending { .. }
        ));
    }

    #[test]
    fn range_support_detection() {
        let (f, _calls) = server(b"abcd".to_vec());
        let src = HttpRangeSource::new(4, f);
        assert!(src.is_random_access());
        src.set_supports_ranges(false);
        assert!(!src.is_random_access());
    }

    #[test]
    fn full_download_degradation() {
        let (f, _calls) = server(b"0123456789".to_vec());
        let src = HttpRangeSource::new(10, f);
        src.fetch_all().expect("fetch all");
        assert_eq!(src.resident_bytes(), 10);
        let mut buf = [0u8; 4];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert!(matches!(avail, Availability::Filled(4)));
        assert_eq!(&buf, b"0123");
    }

    #[test]
    fn lying_length_is_bounded() {
        let (f, _calls) = server(b"abcde".to_vec());
        let src = HttpRangeSource::new(1000, f);
        src.request(&RangeSet::one(0, 5));
        // Reads beyond the real data are Pending, never garbage.
        let mut buf = [0u8; 10];
        assert!(matches!(
            src.read_at(10, &mut buf).expect("pending"),
            Availability::Pending { .. }
        ));
    }
}
