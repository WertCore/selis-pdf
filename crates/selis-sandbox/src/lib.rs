#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! The safety kernel: budgets, cancellation, depth guards, and budget-aware
//! allocation.
//!
//! This crate implements **Rule 1** of `00-INDEX.md §5` and ADR-P0006:
//!
//! > Untrusted bytes are always parsed under a `Budget`. Every entry point that
//! > touches a document takes an explicit allocation, wall-clock,
//! > recursion-depth, and object-count budget, and returns a typed error when it
//! > is exhausted.
//!
//! # Why this is a crate and not a convention
//!
//! The dominant real-world failure mode for PDF software is not a wrong pixel; it
//! is a crafted file that allocates 40 GB, recurses until the stack dies, or
//! renders for six hours. A tab that hangs is a security bug. Making the budget a
//! *required parameter* turns resource exhaustion into a typed, testable outcome
//! rather than an incident.
//!
//! # The shape of it
//!
//! ```
//! use selis_sandbox::{Budget, Resource, Surface};
//!
//! let budget = Budget::profile(Surface::Viewer);
//! let mut g = budget.guard();
//!
//! // A document claims a 40 GB stream length. Charging it fails in constant
//! // time and constant memory.
//! assert!(g.charge(Resource::Bytes, 40_000_000_000).is_err());
//!
//! // And the guard is now poisoned, so a later charge cannot quietly succeed.
//! assert!(g.charge(Resource::Bytes, 1).is_err());
//! ```
//!
//! # No clocks below L4, no threads, no allocator hooks
//!
//! Time arrives through an injected [`Clock`] (ADR-P0011 forbids
//! `std::time::Instant` in L0–L3 code). The kernel itself is clock-free;
//! [`FixedClock`] and [`ManualClock`] are the deterministic test sources. The
//! one exception is [`InstantClock`] — a real-clock *adapter for L4/L5
//! shells*, kept here so every shell shares one audited implementation. It is
//! behind the `instant-clock` feature (a platform capability: shells enable
//! it, L0–L3 crates do not) and compiled out for `wasm32`, where the browser
//! shell wraps `performance.now` instead. Cancellation is cooperative.
//! Allocation is charged by [`alloc`] wrappers rather than a global allocator
//! hook, because `wasm32-unknown-unknown` has no useful hook and a global one
//! cannot attribute an allocation to an operation anyway.

#![forbid(unsafe_code)]

pub mod alloc;
mod budget;
mod cancel;
mod clock;
mod depth;
mod profiles;
pub mod trampoline;
/// The Tier-2 WASM codec sandbox. The [`wasm`] protocol module is always
/// present (the web path implements it against the browser engine); the
/// native `wasmtime` host lives behind the `wasm-host` feature.
pub mod wasm;

#[cfg(feature = "wasm-host")]
mod wasm_host;

pub use alloc::{boxed_slice, copy_slice, grow, vec_with_capacity};
pub use budget::{Budget, BudgetGuard, Resource, Usage};
pub use cancel::CancelToken;
pub use clock::{Clock, FixedClock, ManualClock, Nanos};
pub use depth::DepthGuard;
pub use profiles::Surface;
pub use trampoline::catch;

pub use clock::shell_clock;
/// The real-clock adapter for L4/L5 shells (SL-0.SBX.07). Native only: a
/// WASM shell injects its own `performance.now`-backed [`Clock`].
#[cfg(all(feature = "instant-clock", not(target_arch = "wasm32")))]
pub use clock::InstantClock;

#[cfg(feature = "wasm-host")]
pub use wasm_host::WasmCodec;
