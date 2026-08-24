//! [`FaultSource`] — a deterministic fault injector (SL-0.IO.02).
//!
//! A [`DocSource`] that reproduces a test's failure mode exactly, seeded and
//! reproducible:
//!
//! * truncation at offset N (the file "ends" early)
//! * byte corruption at rate R
//! * latency (reads return `Pending` until the test advances the clock)
//! * range refusal (pretend the server does not support ranges)
//! * lying about the length (announce more than we hold)
//!
//! The corpus and fuzz harnesses use this to prove the engine's behaviour on
//! damaged documents without shipping damaged documents.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::{Availability, DocSource, RangeSet};

/// Deterministic faults to inject into an underlying source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaultConfig {
    /// Truncate the source at this byte offset (`None` = no truncation).
    pub truncate_at: Option<u64>,
    /// Corrupt this fraction (0.0–1.0) of returned bytes.
    pub corrupt_rate: f64,
    /// Whether to lie about `len()`, adding `length_lying_offset`.
    pub length_lying: bool,
    /// Extra bytes to claim beyond the real length.
    pub length_lying_offset: u64,
    /// Reads return `Pending` until the caller calls `FaultSource::release()`.
    pub hold_reads: bool,
    /// Refuse range requests: `is_random_access()` reports `false`.
    pub refuse_ranges: bool,
}

impl Default for FaultConfig {
    fn default() -> Self {
        Self {
            truncate_at: None,
            corrupt_rate: 0.0,
            length_lying: false,
            length_lying_offset: 0,
            hold_reads: false,
            refuse_ranges: false,
        }
    }
}

/// A deterministic fault-injecting [`DocSource`] over in-memory bytes.
#[derive(Debug)]
pub struct FaultSource {
    data: Vec<u8>,
    config: FaultConfig,
    released: Mutex<bool>,
    /// Always empty: the fault source reports no partial availability.
    available: RangeSet,
    /// Deterministic counter for the corruption pattern.
    tick: AtomicU64,
    /// How many times `Pending` was returned (observable progress).
    pending_count: AtomicU64,
}

impl FaultSource {
    /// Construct from raw bytes and a fault configuration.
    #[must_use]
    pub fn new(data: Vec<u8>, config: FaultConfig) -> Self {
        Self {
            data,
            config,
            released: Mutex::new(!config.hold_reads),
            available: RangeSet::new(),
            tick: AtomicU64::new(0),
            pending_count: AtomicU64::new(0),
        }
    }

    /// Stop holding reads (only meaningful when `hold_reads` is set).
    ///
    /// # Panics
    ///
    /// If the lock is poisoned — a panic inside this source would be a bug.
    pub fn release(&self) {
        let mut released = match self.released.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        *released = true;
    }

    /// How many reads returned `Pending`.
    #[must_use]
    pub fn pending_count(&self) -> u64 {
        self.pending_count.load(Ordering::Relaxed)
    }

    /// The real length before any truncation or lying.
    #[must_use]
    pub fn real_len(&self) -> u64 {
        self.data.len() as u64
    }

    fn corrupt(&self, buf: &mut [u8]) {
        let rate = self.config.corrupt_rate;
        if rate <= 0.0 {
            return;
        }
        for b in buf.iter_mut() {
            let t = self.tick.fetch_add(1, Ordering::Relaxed);
            // A cheap deterministic pseudo-random: LCG over the counter.
            let r = (t.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)) & 0xffff;
            let f = f64::from(r as u32) / 65_535.0;
            if f < rate {
                *b ^= 0xff;
            }
        }
    }
}

impl DocSource for FaultSource {
    fn len(&self) -> Option<u64> {
        let real = self.real_len();
        if self.config.length_lying {
            Some(real.saturating_add(self.config.length_lying_offset))
        } else {
            Some(real)
        }
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> selis_error::Result<Availability> {
        if self.config.hold_reads {
            let held = match self.released.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if !*held {
                self.pending_count.fetch_add(1, Ordering::Relaxed);
                return Ok(Availability::Pending {
                    hint: RangeSet::one(off, off.saturating_add(buf.len() as u64)),
                });
            }
        }

        let real = self.real_len();
        let limit = self.config.truncate_at.unwrap_or(real);
        if off >= limit {
            return Ok(Availability::Eof);
        }
        // n = min(bytes until the truncation limit, buffer capacity) — checked.
        let remaining = limit.saturating_sub(off);
        let want = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let n64 = remaining.min(want);
        let n = usize::try_from(n64).unwrap_or(usize::MAX).min(buf.len());
        let start = usize::try_from(off).unwrap_or(usize::MAX);
        let end = start.saturating_add(n);
        let src = self.data.get(start..end).unwrap_or(&[]);
        if let Some(dst) = buf.get_mut(..n) {
            dst.copy_from_slice(src);
            self.corrupt(dst);
        }
        Ok(Availability::Filled(n))
    }

    fn available(&self) -> &RangeSet {
        &self.available
    }

    fn request(&self, _ranges: &RangeSet) {
        // No-op: a fault source has no prefetch to perform.
    }

    fn is_random_access(&self) -> bool {
        !self.config.refuse_ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Vec<u8> {
        (0..=255u8).cycle().take(1024).collect()
    }

    #[test]
    fn truncated_source_stops_at_the_cut() {
        let src = FaultSource::new(
            data(),
            FaultConfig {
                truncate_at: Some(100),
                ..FaultConfig::default()
            },
        );
        assert_eq!(src.len(), Some(1024), "truncation must not lie about len()");
        let mut buf = [0u8; 256];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(100));
        assert_eq!(src.read_at(100, &mut buf).expect("eof"), Availability::Eof);
    }

    #[test]
    fn corrupting_source_alters_some_bytes() {
        let src = FaultSource::new(
            data(),
            FaultConfig {
                corrupt_rate: 0.5,
                ..FaultConfig::default()
            },
        );
        let mut buf = [0u8; 512];
        let avail = src.read_at(0, &mut buf).expect("read");
        assert_eq!(avail, Availability::Filled(512));
        let altered = buf
            .iter()
            .zip(data().iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(altered > 0, "corruption must alter at least one byte");
    }

    #[test]
    fn pending_reads_wait_for_release() {
        let src = FaultSource::new(
            data(),
            FaultConfig {
                hold_reads: true,
                ..FaultConfig::default()
            },
        );
        let mut buf = [0u8; 16];
        assert!(matches!(
            src.read_at(0, &mut buf).expect("pending"),
            Availability::Pending { .. }
        ));
        assert_eq!(src.pending_count(), 1);
        src.release();
        assert_eq!(
            src.read_at(0, &mut buf).expect("filled"),
            Availability::Filled(16)
        );
    }

    #[test]
    fn lying_length_claims_more_than_held() {
        let src = FaultSource::new(
            data(),
            FaultConfig {
                length_lying: true,
                length_lying_offset: 4096,
                ..FaultConfig::default()
            },
        );
        assert_eq!(src.len(), Some(1024 + 4096));
        // Reads still stop at the real end.
        let mut buf = [0u8; 8];
        assert_eq!(src.read_at(1024, &mut buf).expect("eof"), Availability::Eof);
    }

    #[test]
    fn range_refusal_disables_random_access() {
        let src = FaultSource::new(
            data(),
            FaultConfig {
                refuse_ranges: true,
                ..FaultConfig::default()
            },
        );
        assert!(!src.is_random_access());
    }
}
