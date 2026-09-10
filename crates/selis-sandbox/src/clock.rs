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

/// A real monotonic clock for the binding boundary (SL-0.SBX.07).
///
/// This is the one sanctioned `std::time::Instant` in the stack. It exists so
/// that every L4/L5 shell (the CLI today; the WASM and FFI bindings when they
/// land) shares a single audited adapter instead of growing its own: a shell
/// constructs one per operation and hands it to `Session::open` /
/// [`crate::Budget::guard_with`], and from there the kernel measures
/// `Budget::wall` against genuine elapsed time, so the wall deadline actually
/// fires on real parse paths instead of only in tests.
///
/// The construction rule is enforced two ways: the type lives behind the
/// `instant-clock` cargo feature (a platform capability — shells enable it,
/// L0–L3 crates do not), and it is compiled out entirely for `wasm32`, where
/// the browser shell wraps `performance.now` in its own [`Clock`] instead
/// (ADR-P0011). No crate below L4 may name `std::time::Instant`; they receive
/// `&dyn Clock` from their caller.
#[cfg(all(feature = "instant-clock", not(target_arch = "wasm32")))]
#[derive(Debug, Clone, Copy)]
pub struct InstantClock {
    start: std::time::Instant,
}

#[cfg(all(feature = "instant-clock", not(target_arch = "wasm32")))]
impl InstantClock {
    /// A clock whose zero is *now*.
    ///
    /// Construct one per operation: `Budget::wall` is a deadline relative to
    /// the guard's construction, so a fresh clock bounds that operation, not
    /// the process.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

#[cfg(all(feature = "instant-clock", not(target_arch = "wasm32")))]
impl Default for InstantClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(feature = "instant-clock", not(target_arch = "wasm32")))]
impl Clock for InstantClock {
    fn now(&self) -> Nanos {
        // `Instant` is monotonic, so the elapsed reading never decreases;
        // only the narrowing to Nanos can saturate (after ~584 years).
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(Nanos::MAX)
    }
}

/// The shell's default clock for SL-0.SBX.07 call sites: the sanctioned
/// monotonic [`InstantClock`] where the platform provides one.
///
/// On wasm32 there is no real clock available to pure Rust — the browser
/// shell injects its own `performance.now`-backed [`Clock`] (ADR-P0011), and
/// the pure-Rust wasm artifacts (the xtask size canary, the workspace build
/// matrix) only need the wall-deadline plumbing to *compile* — so this
/// returns a stopped [`FixedClock`]: the same never-firing semantics the
/// code base had before SBX.07, confined to targets where no real clock
/// exists.
#[cfg(all(feature = "instant-clock", not(target_arch = "wasm32")))]
#[must_use]
pub fn shell_clock() -> impl Clock {
    InstantClock::new()
}

/// Stopped-clock variant for targets without a sanctioned real clock. See
/// [`shell_clock`].
#[cfg(any(not(feature = "instant-clock"), target_arch = "wasm32"))]
#[must_use]
pub fn shell_clock() -> impl Clock {
    FixedClock(0)
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

    /// SL-0.SBX.07: the adapter reports real elapsed time — monotonically,
    /// and strictly increasing across a real wait (a frozen clock would
    /// report the same reading twice).
    #[cfg(all(feature = "instant-clock", not(target_arch = "wasm32")))]
    #[test]
    fn instant_clock_reports_real_elapsed_time() {
        let c = InstantClock::new();
        let first = c.now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = c.now();
        assert!(second >= first, "monotonic: never decreases");
        assert!(second > first, "real time passes: strictly increasing");
    }
}
