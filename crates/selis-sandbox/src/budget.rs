//! [`Budget`] and [`BudgetGuard`] — the kernel of ADR-P0006.
//!
//! # The contract
//!
//! A [`Budget`] is a set of limits. A [`BudgetGuard`] is a running tally against
//! them. Every charge is checked arithmetic; exhaustion returns a typed error and
//! **poisons** the guard, so a caller that ignores one `Err` cannot then succeed
//! at the next charge and continue as though nothing happened.
//!
//! Poisoning is the load-bearing detail. Without it, the common bug is a parser
//! that swallows a budget error in a helper, keeps going, and produces a
//! wrong-but-plausible result — which is worse than failing, because it is
//! invisible.
//!
//! # Five dimensions
//!
//! | Resource | Bounds |
//! |---|---|
//! | [`Resource::Bytes`] | peak allocation attributable to the operation |
//! | [`Resource::Wall`] | a deadline, checked at every `tick` |
//! | [`Resource::Depth`] | nesting: form XObjects, patterns, object streams, `/Prev` chains |
//! | [`Resource::Objects`] | indirect objects resolved |
//! | [`Resource::Pixels`] | rasterised samples — bounds the "one page, 40 gigapixels" file |

use selis_error::{err, Code, Error, Result};

use crate::cancel::CancelToken;
use crate::clock::{Clock, FixedClock, Nanos};
use crate::profiles::Surface;

/// A budgeted resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    /// Bytes allocated.
    Bytes,
    /// Nanoseconds of wall-clock time.
    Wall,
    /// Nesting depth.
    Depth,
    /// Indirect objects resolved.
    Objects,
    /// Rasterised samples.
    Pixels,
}

impl Resource {
    /// The error code this resource's exhaustion reports.
    #[must_use]
    pub const fn code(self) -> Code {
        match self {
            Resource::Bytes => Code::BudgetBytes,
            Resource::Wall => Code::BudgetWall,
            Resource::Depth => Code::BudgetDepth,
            Resource::Objects => Code::BudgetObjects,
            Resource::Pixels => Code::BudgetPixels,
        }
    }

    /// A short stable identifier for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Resource::Bytes => "bytes",
            Resource::Wall => "wall",
            Resource::Depth => "depth",
            Resource::Objects => "objects",
            Resource::Pixels => "pixels",
        }
    }
}

/// Resource limits for one operation.
///
/// Constructed from a [`Surface`] profile in normal use; constructed by hand in
/// tests and when a caller genuinely knows better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Peak allocation, in bytes.
    pub bytes: u64,
    /// Deadline, in nanoseconds from the guard's construction.
    pub wall: Nanos,
    /// Maximum nesting depth.
    pub depth: u16,
    /// Maximum indirect objects resolved.
    pub objects: u32,
    /// Maximum rasterised samples.
    pub pixels: u64,
}

impl Budget {
    /// The limits for a surface (SL-0.SBX.05).
    #[must_use]
    pub const fn profile(surface: Surface) -> Budget {
        surface.budget()
    }

    /// A budget with no limits, for tests only.
    ///
    /// Never reachable from a shipped code path: every public engine entry point
    /// takes a `Budget` chosen by `selis-policy`, and none of them choose this
    /// one. It exists so that a unit test of a *non-budget* property does not have
    /// to reason about limits.
    #[must_use]
    pub const fn unlimited() -> Budget {
        Budget {
            bytes: u64::MAX,
            wall: Nanos::MAX,
            depth: u16::MAX,
            objects: u32::MAX,
            pixels: u64::MAX,
        }
    }

    /// The limit for one resource.
    #[must_use]
    pub const fn limit(&self, r: Resource) -> u64 {
        match r {
            Resource::Bytes => self.bytes,
            Resource::Wall => self.wall,
            Resource::Depth => self.depth as u64,
            Resource::Objects => self.objects as u64,
            Resource::Pixels => self.pixels,
        }
    }

    /// A guard against this budget, with a frozen clock and no cancel token.
    #[must_use]
    pub fn guard(&self) -> BudgetGuard<'static> {
        BudgetGuard::new(*self, &NO_CLOCK, CancelToken::new())
    }

    /// A guard against this budget with a real clock and a cancel token.
    #[must_use]
    pub fn guard_with<'c>(&self, clock: &'c dyn Clock, cancel: CancelToken) -> BudgetGuard<'c> {
        BudgetGuard::new(*self, clock, cancel)
    }

    /// A budget scaled down for a sub-operation, e.g. parsing one embedded font
    /// inside a page render.
    ///
    /// Scaling rather than sharing means an inner operation cannot consume the
    /// whole outer allowance. `numerator/denominator` is applied to bytes,
    /// objects and pixels; wall and depth are inherited unchanged because a
    /// deadline is absolute and depth is already relative.
    #[must_use]
    pub fn scaled(&self, numerator: u32, denominator: u32) -> Budget {
        let scale = |v: u64| -> u64 {
            if denominator == 0 {
                return v;
            }
            v.saturating_mul(u64::from(numerator))
                .checked_div(u64::from(denominator))
                .unwrap_or(v)
        };
        Budget {
            bytes: scale(self.bytes),
            wall: self.wall,
            depth: self.depth,
            objects: u32::try_from(scale(u64::from(self.objects))).unwrap_or(u32::MAX),
            pixels: scale(self.pixels),
        }
    }
}

impl Default for Budget {
    /// The viewer profile — the most conservative surface a document is likely to
    /// be opened in.
    fn default() -> Self {
        Budget::profile(Surface::Viewer)
    }
}

static NO_CLOCK: FixedClock = FixedClock(0);

/// What a guard has consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// Bytes charged, cumulative — the peak, not the live total, because the
    /// engine charges on acquisition and does not credit on release. Peak is what
    /// the limit is about.
    pub bytes: u64,
    /// Nanoseconds elapsed at the last `tick`.
    pub wall: Nanos,
    /// Current nesting depth.
    pub depth: u16,
    /// Maximum nesting depth reached.
    pub peak_depth: u16,
    /// Indirect objects resolved.
    pub objects: u32,
    /// Rasterised samples.
    pub pixels: u64,
    /// Number of `tick` calls — a useful proxy for work done, and the thing a
    /// fuzz target asserts about.
    pub ticks: u64,
}

/// A running tally against a [`Budget`].
///
/// Not `Clone`: a guard is the single authority for one operation's consumption,
/// and a copy would let two code paths each spend the whole allowance.
#[derive(Debug)]
pub struct BudgetGuard<'c> {
    budget: Budget,
    usage: Usage,
    clock: &'c dyn Clock,
    start: Nanos,
    cancel: CancelToken,
    /// Set on the first exhaustion. Never cleared.
    poisoned: Option<Resource>,
    /// Set when the cancel token was observed.
    cancelled: bool,
}

impl<'c> BudgetGuard<'c> {
    /// A fresh guard.
    #[must_use]
    pub fn new(budget: Budget, clock: &'c dyn Clock, cancel: CancelToken) -> Self {
        let start = clock.now();
        Self {
            budget,
            usage: Usage::default(),
            clock,
            start,
            cancel,
            poisoned: None,
            cancelled: false,
        }
    }

    /// The budget being charged against.
    #[must_use]
    pub const fn budget(&self) -> &Budget {
        &self.budget
    }

    /// What has been consumed.
    #[must_use]
    pub const fn usage(&self) -> &Usage {
        &self.usage
    }

    /// Whether a prior charge exhausted the budget.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned.is_some()
    }

    /// The resource that poisoned the guard, if any.
    #[must_use]
    pub const fn poisoned_by(&self) -> Option<Resource> {
        self.poisoned
    }

    /// Charge `n` units of `r`.
    ///
    /// # Errors
    ///
    /// * The resource's `BUDGET_*` code if the charge would exceed the limit.
    /// * `BUDGET_POISONED` if a prior charge already failed.
    /// * `CANCELLED` if the cancel token was signalled.
    ///
    /// Overflow in the running total is an error, never a wrap: a wrapped total
    /// would silently reset the budget to zero, which is precisely the bug a
    /// crafted document is looking for.
    pub fn charge(&mut self, r: Resource, n: u64) -> Result<()> {
        self.check_alive()?;

        let (current, limit) = (self.consumed(r), self.budget.limit(r));
        let Some(next) = current.checked_add(n) else {
            self.poisoned = Some(r);
            return Err(Self::exceeded(r, limit, u64::MAX));
        };
        if next > limit {
            self.poisoned = Some(r);
            return Err(Self::exceeded(r, limit, next));
        }
        self.store(r, next);
        Ok(())
    }

    /// Charge one unit of `r`.
    ///
    /// # Errors
    ///
    /// As [`BudgetGuard::charge`].
    pub fn charge_one(&mut self, r: Resource) -> Result<()> {
        self.charge(r, 1)
    }

    /// Check whether `n` units of `r` *could* be charged, without charging them.
    ///
    /// For the "the document claims this stream is 40 GB — before we do anything
    /// else, is that even plausible?" check. Does not poison on failure, because
    /// a caller may legitimately be probing.
    #[must_use]
    pub fn can_charge(&self, r: Resource, n: u64) -> bool {
        if self.poisoned.is_some() || self.cancelled {
            return false;
        }
        self.consumed(r)
            .checked_add(n)
            .is_some_and(|next| next <= self.budget.limit(r))
    }

    /// The deadline and cancellation check.
    ///
    /// **Every loop whose condition reads parsed data must call this**
    /// (01-ARCHITECTURE.md §6; `check-contracts` enforces it). It is cheap: one
    /// clock read and one atomic load.
    ///
    /// # Errors
    ///
    /// * `BUDGET_WALL` when the deadline has passed.
    /// * `CANCELLED` when the token was signalled.
    /// * `BUDGET_POISONED` when a prior charge failed.
    pub fn tick(&mut self) -> Result<()> {
        self.check_alive()?;
        self.usage.ticks = self.usage.ticks.saturating_add(1);

        let elapsed = self.clock.now().saturating_sub(self.start);
        self.usage.wall = elapsed;
        if elapsed > self.budget.wall {
            self.poisoned = Some(Resource::Wall);
            return Err(Self::exceeded(Resource::Wall, self.budget.wall, elapsed));
        }
        Ok(())
    }

    /// Enter a nesting level.
    ///
    /// Prefer [`crate::DepthGuard`], which cannot forget to leave.
    ///
    /// # Errors
    ///
    /// `BUDGET_DEPTH` when the depth limit is reached.
    pub fn enter(&mut self) -> Result<()> {
        self.check_alive()?;
        let next = self.usage.depth.saturating_add(1);
        if u64::from(next) > self.budget.limit(Resource::Depth) {
            self.poisoned = Some(Resource::Depth);
            return Err(Self::exceeded(
                Resource::Depth,
                self.budget.limit(Resource::Depth),
                u64::from(next),
            ));
        }
        self.usage.depth = next;
        self.usage.peak_depth = self.usage.peak_depth.max(next);
        Ok(())
    }

    /// Leave a nesting level. Saturates at zero rather than underflowing.
    pub fn leave(&mut self) {
        self.usage.depth = self.usage.depth.saturating_sub(1);
    }

    /// The cancel token this guard observes.
    #[must_use]
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Nanoseconds elapsed since construction.
    #[must_use]
    pub fn elapsed(&self) -> Nanos {
        self.clock.now().saturating_sub(self.start)
    }

    fn check_alive(&mut self) -> Result<()> {
        if let Some(r) = self.poisoned {
            return Err(err!(
                Code::BudgetPoisoned,
                during = "budget-charge",
                detail = r.as_str()
            ));
        }
        if self.cancel.is_cancelled() {
            self.cancelled = true;
            return Err(err!(Code::Cancelled));
        }
        Ok(())
    }

    fn consumed(&self, r: Resource) -> u64 {
        match r {
            Resource::Bytes => self.usage.bytes,
            Resource::Wall => self.usage.wall,
            Resource::Depth => u64::from(self.usage.depth),
            Resource::Objects => u64::from(self.usage.objects),
            Resource::Pixels => self.usage.pixels,
        }
    }

    fn store(&mut self, r: Resource, v: u64) {
        match r {
            Resource::Bytes => self.usage.bytes = v,
            Resource::Wall => self.usage.wall = v,
            Resource::Depth => {
                let d = u16::try_from(v).unwrap_or(u16::MAX);
                self.usage.depth = d;
                self.usage.peak_depth = self.usage.peak_depth.max(d);
            }
            Resource::Objects => self.usage.objects = u32::try_from(v).unwrap_or(u32::MAX),
            Resource::Pixels => self.usage.pixels = v,
        }
    }

    fn exceeded(r: Resource, limit: u64, requested: u64) -> Error {
        // The detail is engine-controlled text: a resource name and two numbers.
        // No document bytes (ADR-P0017).
        err!(
            r.code(),
            during = "budget-charge",
            detail = std::format!("{} limit={} requested={}", r.as_str(), limit, requested)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    fn small() -> Budget {
        Budget {
            bytes: 1024,
            wall: 1_000_000,
            depth: 4,
            objects: 10,
            pixels: 100,
        }
    }

    /// SL-0.SBX.01 DoD: no sequence of charges can exceed a limit.
    #[test]
    fn no_charge_sequence_exceeds_a_limit() {
        let mut g = small().guard();
        let mut total = 0u64;
        for _ in 0..10_000 {
            if g.charge(Resource::Bytes, 7).is_err() {
                break;
            }
            total = total.saturating_add(7);
        }
        assert!(total <= 1024);
        assert_eq!(g.usage().bytes, total);
    }

    /// SL-0.SBX.01 DoD: a poisoned guard rejects all further charges.
    #[test]
    fn poisoning_is_permanent_and_covers_other_resources() {
        let mut g = small().guard();
        let e = g.charge(Resource::Bytes, 99_999).expect_err("must exceed");
        assert_eq!(e.code(), Code::BudgetBytes);
        assert_eq!(g.poisoned_by(), Some(Resource::Bytes));

        // Not merely the same resource: everything.
        for r in [
            Resource::Bytes,
            Resource::Objects,
            Resource::Pixels,
            Resource::Depth,
        ] {
            assert_eq!(
                g.charge(r, 1).expect_err("poisoned").code(),
                Code::BudgetPoisoned,
                "{r:?} succeeded on a poisoned guard"
            );
        }
        assert_eq!(g.tick().expect_err("poisoned").code(), Code::BudgetPoisoned);
        assert!(!g.can_charge(Resource::Bytes, 0));
    }

    /// SL-0.SBX.01 DoD: overflow in a charge is an error, not a wrap.
    #[test]
    fn overflow_is_an_error_not_a_wrap() {
        let mut g = Budget::unlimited().guard();
        g.charge(Resource::Bytes, u64::MAX).expect("first fits");
        let e = g.charge(Resource::Bytes, 1).expect_err("must overflow");
        assert_eq!(e.code(), Code::BudgetBytes);
        assert_eq!(
            g.usage().bytes,
            u64::MAX,
            "the total must not have wrapped to a small number"
        );
    }

    /// SL-0.SBX.02 DoD: a 40 GB length field yields BudgetExceeded in constant
    /// memory and constant time.
    #[test]
    fn a_forty_gigabyte_length_field_fails_immediately() {
        let mut g = Budget::profile(Surface::Viewer).guard();
        let e = g
            .charge(Resource::Bytes, 40 * 1024 * 1024 * 1024)
            .expect_err("must exceed the viewer profile");
        assert_eq!(e.code(), Code::BudgetBytes);
        // No allocation happened: we charged before allocating, which is the point.
        assert_eq!(g.usage().bytes, 0);
    }

    #[test]
    fn deadline_is_observed_at_the_next_tick() {
        let clock = ManualClock::new();
        let mut g = small().guard_with(&clock, CancelToken::new());
        assert!(g.tick().is_ok());
        clock.advance(999_999);
        assert!(g.tick().is_ok(), "at the deadline is not past it");
        clock.advance(2);
        let e = g.tick().expect_err("deadline passed");
        assert_eq!(e.code(), Code::BudgetWall);
        assert_eq!(g.poisoned_by(), Some(Resource::Wall));
    }

    /// SL-0.SBX.03 DoD: a long operation cancels within one tick interval.
    #[test]
    fn cancellation_lands_within_one_tick() {
        let clock = ManualClock::new();
        let token = CancelToken::new();
        let mut g = Budget::unlimited().guard_with(&clock, token.clone());
        let mut ticks = 0u64;
        loop {
            match g.tick() {
                Ok(()) => {
                    ticks = ticks.saturating_add(1);
                    if ticks == 5 {
                        token.cancel();
                    }
                    if ticks > 100 {
                        panic!("cancellation was not observed");
                    }
                }
                Err(e) => {
                    assert_eq!(e.code(), Code::Cancelled);
                    assert_eq!(ticks, 5, "must be observed on the very next tick");
                    break;
                }
            }
        }
    }

    #[test]
    fn depth_is_bounded_and_symmetric() {
        let mut g = small().guard();
        for _ in 0..4 {
            g.enter().expect("within depth");
        }
        let e = g.enter().expect_err("depth exceeded");
        assert_eq!(e.code(), Code::BudgetDepth);
        assert_eq!(g.usage().peak_depth, 4);
    }

    #[test]
    fn leave_saturates_rather_than_underflowing() {
        let mut g = small().guard();
        g.leave();
        g.leave();
        assert_eq!(g.usage().depth, 0);
        g.enter().expect("still usable");
        assert_eq!(g.usage().depth, 1);
    }

    #[test]
    fn can_charge_does_not_mutate_or_poison() {
        let g = small().guard();
        assert!(g.can_charge(Resource::Bytes, 1024));
        assert!(!g.can_charge(Resource::Bytes, 1025));
        assert!(!g.is_poisoned(), "probing must not poison");
        assert_eq!(g.usage().bytes, 0);
    }

    #[test]
    fn scaling_bounds_a_sub_operation() {
        let outer = Budget {
            bytes: 1000,
            wall: 500,
            depth: 8,
            objects: 100,
            pixels: 1000,
        };
        let inner = outer.scaled(1, 4);
        assert_eq!(inner.bytes, 250);
        assert_eq!(inner.objects, 25);
        assert_eq!(inner.wall, 500, "a deadline is absolute");
        assert_eq!(inner.depth, 8, "depth is already relative");
        // Degenerate denominators must not divide by zero.
        assert_eq!(outer.scaled(1, 0).bytes, 1000);
    }

    #[test]
    fn every_resource_reports_its_own_code() {
        for (r, code) in [
            (Resource::Bytes, Code::BudgetBytes),
            (Resource::Wall, Code::BudgetWall),
            (Resource::Depth, Code::BudgetDepth),
            (Resource::Objects, Code::BudgetObjects),
            (Resource::Pixels, Code::BudgetPixels),
        ] {
            assert_eq!(r.code(), code);
            let mut g = Budget {
                bytes: 0,
                wall: 0,
                depth: 0,
                objects: 0,
                pixels: 0,
            }
            .guard();
            assert_eq!(g.charge(r, 1).expect_err("zero limit").code(), code);
        }
    }

    /// A cancelled token beats an available budget: the user's intent wins.
    #[test]
    fn cancellation_takes_precedence_over_an_available_budget() {
        let token = CancelToken::new();
        let mut g = Budget::unlimited().guard_with(&FixedClock(0), token.clone());
        token.cancel();
        assert_eq!(
            g.charge(Resource::Bytes, 1).expect_err("cancelled").code(),
            Code::Cancelled
        );
    }
}
