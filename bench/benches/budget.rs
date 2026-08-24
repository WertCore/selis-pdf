//! Budget guard benchmark — SL-0.PERF.01.
//!
//! Baseline: an empty budget guard should be cheap enough that it does not
//! dominate the cost of a single object parse. The budget at the time of
//! writing is ~3 ns/charge on a 2023 x86 desktop.
//!
//! Benchmarks are measurement harnesses, not shipped API; missing_docs is
//! relaxed here so the criterion macros can generate their glue.

#![allow(missing_docs)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Benchmark the overhead of budget charging and tick.
fn bench_charge(c: &mut Criterion) {
    use selis_sandbox::{Budget, Resource};

    c.bench_function("budget_charge", |b| {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let r = Resource::Bytes;
        b.iter(|| {
            let _ = black_box(g.charge(black_box(r), black_box(1)));
        });
    });

    c.bench_function("budget_tick", |b| {
        use selis_sandbox::Clock;
        let clock = selis_sandbox::FixedClock(0);
        let budget = Budget::unlimited();
        let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
        b.iter(|| {
            let _ = black_box(g.tick());
        });
    });
}

criterion_group!(benches, bench_charge);
criterion_main!(benches);
