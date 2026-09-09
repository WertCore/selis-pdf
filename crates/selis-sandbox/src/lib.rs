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
//! # No clocks, no threads, no allocator hooks
//!
//! Time arrives through an injected [`Clock`] (ADR-P0011 forbids
//! `std::time::Instant` below L4). Cancellation is cooperative. Allocation is
//! charged by [`alloc`] wrappers rather than a global allocator hook, because
//! `wasm32-unknown-unknown` has no useful hook and a global one cannot attribute
//! an allocation to an operation anyway.

#![forbid(unsafe_code)]

pub mod alloc;
mod budget;
mod cancel;
mod clock;
mod depth;
mod profiles;
pub mod trampoline;

pub use alloc::{boxed_slice, copy_slice, grow, vec_with_capacity};
pub use budget::{Budget, BudgetGuard, Resource, Usage};
pub use cancel::CancelToken;
pub use clock::{Clock, FixedClock, ManualClock, Nanos};
pub use depth::DepthGuard;
pub use profiles::Surface;
pub use trampoline::catch;
