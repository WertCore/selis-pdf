//! An injected monotonic clock (ADR-P0011, SL-0.SBX.03).
//!
//! No crate below L4 may name `std::time::Instant` or `SystemTime`. Deadlines are
//! therefore measured against a [`Clock`] the shell supplies: `performance.now()`
//! in a browser worker, `Instant` in the CLI, a counter in a fuzz harness.
//!
//! This is not purism. A clock that a test can advance by hand is what makes
//! "this operation cancels within one tick" a *deterministic* assertion rather
//! than a flaky one.

use core::sync::atomic::{AtomicU64, Ordering};

/// Nanoseconds since an unspecified, monotonically increasing epoch.
///
/// Only differences are meaningful. Not a wall-clock time and deliberately not
/// convertible to one — a document must never be able to learn the user's clock.
pub type Nanos = u64;

/// A monotonic time source.
///
/// `Debug` is a supertrait so a [`BudgetGuard`](crate::BudgetGuard) — which
/// holds a `&dyn Clock` — can implement `Debug` for diagnostics.
pub trait Clock: Send + Sync + core::fmt::Debug {
    /// The current reading. Must never decrease.
    fn now(&self) -> Nanos;
}

/// A clock frozen at one instant.
///
/// The right default for a batch or fuzz surface, where a wall-clock deadline is
/// meaningless and only the other four budget dimensions matter.
#[derive(Debug, Clone, Copy, Default)]
pub struct FixedClock(pub Nanos);

impl Clock for FixedClock {
    fn now(&self) -> Nanos {
        self.0
    }
}

/// A clock a test advances by hand.
///
/// Makes deadline behaviour reproducible: advance past the deadline and the very
/// next [`crate::BudgetGuard::tick`] must fail, every time, on every platform.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: AtomicU64,
}

impl ManualClock {
    /// A clock starting at zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            now: AtomicU64::new(0),
        }
    }

    /// A clock starting at `start`.
    #[must_use]
    pub const fn starting_at(start: Nanos) -> Self {
        Self {
            now: AtomicU64::new(start),
        }
    }

    /// Advance by `delta` nanoseconds, saturating.
    // `fetch_update` is renamed to `try_update` on new toolchains; the MSRV
    // (1.85) has only the old name, so silence the deprecation until the
    // MSRV moves past the rename. Nightly fuzz/coverage builds run with
    // `-D warnings` and fail the soak otherwise (SL-1.ROB.02).
    #[allow(deprecated)]
    pub fn advance(&self, delta: Nanos) {
        let _ = self
            .now
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_add(delta))
            });
    }

    /// Advance by whole milliseconds.
    pub fn advance_ms(&self, ms: u64) {
        self.advance(ms.saturating_mul(1_000_000));
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Nanos {
        self.now.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_advances_monotonically_and_saturates() {
        let c = ManualClock::new();
        assert_eq!(c.now(), 0);
        c.advance_ms(5);
        assert_eq!(c.now(), 5_000_000);
        c.advance(Nanos::MAX);
        c.advance(Nanos::MAX);
        assert_eq!(c.now(), Nanos::MAX, "must saturate, never wrap backwards");
    }

    #[test]
    fn fixed_clock_never_moves() {
        let c = FixedClock(42);
        assert_eq!(c.now(), 42);
        assert_eq!(c.now(), 42);
    }
}
