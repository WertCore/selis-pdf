//! SL-0.SBX.07 — the wall deadline fires on the real parse path.
//!
//! `Session::open` (and the render path behind it) builds every guard on the
//! clock the caller injects. These tests prove the three properties the task
//! asks for:
//!
//! 1. A `ManualClock` advanced past the Viewer wall *while the open is
//!    underway* aborts the open with the typed `BUDGET_WALL` error, mid-stream
//!    (after real parsing work, not at the first tick).
//! 2. The deadline is relative to the guard's construction: a clock that
//!    already reads a huge value but stops moving does not abort an open.
//! 3. The display-list path (the below-L4 helpers behind `page_display_list`)
//!    measures against the same injected clock, so a deadline that passes
//!    mid-interpretation aborts there too.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::arithmetic_side_effects, clippy::integer_division)]

use std::sync::atomic::{AtomicU64, Ordering};

use selis_error::Code;
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, CancelToken, Clock, FixedClock, ManualClock, Nanos, Surface};

/// A `ManualClock` that advances a fixed step on every read: time passes as
/// the parse does work. Each `tick` inside the lexer/interpreter advances the
/// clock, so the deadline trips *mid-stream* — the deterministic stand-in for
/// a file that outlives its deadline.
#[derive(Debug)]
struct ParseClock {
    inner: ManualClock,
    step: Nanos,
    reads: AtomicU64,
}

impl ParseClock {
    fn new(step: Nanos) -> Self {
        Self {
            inner: ManualClock::new(),
            step,
            reads: AtomicU64::new(0),
        }
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
}

impl Clock for ParseClock {
    fn now(&self) -> Nanos {
        let _ = self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.advance(self.step);
        self.inner.now()
    }
}

fn minimal_pdf() -> Vec<u8> {
    include_bytes!("../src/fixtures/minimal.pdf").to_vec()
}

/// DoD: a ManualClock advanced past the Viewer wall fails the open with
/// `BUDGET_WALL`, mid-parse. One second of clock time passes per clock read;
/// the lexer ticks per token, so the open dies part-way through lexing —
/// after several successful ticks, not at the first one.
#[test]
fn a_manual_clock_advanced_past_the_viewer_wall_fails_the_open_mid_parse() {
    let clock = ParseClock::new(1_000_000_000); // 1 s per clock read
    let budget = Budget::profile(Surface::Viewer); // wall = 5 s
    let result = Session::open(minimal_pdf(), &budget, &clock);
    match result {
        Ok(_) => panic!("an open racing past the Viewer wall must abort"),
        Err(e) => {
            assert!(e.is_budget(), "expected a budget error, got {e}");
            assert_eq!(e.code(), Code::BudgetWall);
        }
    }
    // Mid-stream: the guard's construction consumed one read, several ticks
    // succeeded, and only then did the elapsed time pass the wall.
    assert!(
        clock.reads() > 2,
        "the deadline must fire after parsing work, reads={}",
        clock.reads()
    );
}

/// The wall deadline is relative to the guard's construction (a `Duration`
/// from the guard's start, ADR-P0006): a clock that already reads a large
/// value but does not advance during the open must not abort it. This pins
/// the semantics the sweep and shells rely on.
#[test]
fn a_stopped_clock_that_already_reads_a_huge_value_does_not_abort() {
    let clock = ManualClock::starting_at(Nanos::MAX / 2);
    let budget = Budget::profile(Surface::Viewer);
    let session = Session::open(minimal_pdf(), &budget, &clock).expect("open succeeds");
    assert_eq!(session.len(), 1);
}

/// The display-list path is built on sub-guards that measure against the
/// caller's clock (`BudgetGuard::clock`), so a deadline that passes during
/// interpretation aborts the render path with the same typed error.
#[test]
fn the_display_list_path_aborts_on_the_injected_deadline() {
    let budget = Budget::profile(Surface::Viewer);
    let session = Session::open(minimal_pdf(), &budget, &FixedClock(0)).expect("open");
    // Advance 5 s + 1 ns per read: the first interpreter tick inside
    // `page_display_list` is already past the wall, while `open` (on the
    // frozen clock) succeeded.
    let clock = ParseClock::new(budget.wall + 1);
    let mut g = budget.guard_with(&clock, CancelToken::new());
    let result = session.page_display_list(0, &budget, &mut g);
    match result {
        Ok(_) => panic!("a display list built past the deadline must abort"),
        Err(e) => {
            assert!(e.is_budget(), "expected a budget error, got {e}");
            assert_eq!(e.code(), Code::BudgetWall);
        }
    }
    assert!(
        clock.reads() >= 2,
        "the deadline must be observed at a tick, reads={}",
        clock.reads()
    );
}
