//! The CLI's process runtime: the cancel token wired to the platform Ctrl-C
//! hook, and the clock + budget guards every long operation runs under
//! (SL-1A.UI.06, SBX.07).
//!
//! The engine cancels cooperatively — every budget tick and every output
//! chunk observes a [`CancelToken`]. This module owns the CLI's token so a
//! Ctrl-C during any tool operation surfaces as the typed `CANCELLED` error
//! and the honest "no partial file" message instead of a process kill. (A
//! hard kill remains safe — WRITE.06 crash-atomicity — the hook is what
//! makes it *clean*.) The wall clock itself is [`crate::shell_clock`]
//! (SBX.07): one real-clock guard per operation, constructed by the caller
//! alongside the guard so the borrow lives as long as the guard.

use std::sync::OnceLock;

use selis_sandbox::{Budget, BudgetGuard, CancelToken, Clock};

static TOKEN: OnceLock<CancelToken> = OnceLock::new();

/// Install the platform Ctrl-C hook and bind it to the CLI's cancel token.
/// Called once from `main`. When the platform offers no hook (`wasm`-like
/// targets), the token simply never fires and Ctrl-C keeps its default
/// behavior — a process kill, which WRITE.06 already makes safe.
pub(crate) fn init_cancel() {
    let token = selis_io::install_ctrl_c_flag()
        .map(selis_sandbox::CancelToken::from_flag)
        .unwrap_or_default();
    let _ = TOKEN.set(token);
}

/// The CLI's cancel token. In test binaries (which never call
/// [`init_cancel`]) this is a fresh, never-cancelled token.
#[must_use]
pub(crate) fn token() -> CancelToken {
    TOKEN.get_or_init(CancelToken::new).clone()
}

/// A guard against `budget`, wired to the shell's real clock (SBX.07) and
/// the Ctrl-C token (SL-1A.UI.06).
///
/// The caller constructs the clock (`crate::shell_clock()`) so it lives as
/// long as the guard — one clock per operation, per the SBX.07 design.
///
/// # Budget
///
/// Charges against `budget` exactly as `Budget::guard_with` would; the token
/// turns Ctrl-C into a typed `CANCELLED` at the next tick or charge, and the
/// clock makes `Budget::wall` a genuine deadline.
#[must_use]
pub(crate) fn cli_guard<'c>(budget: &Budget, clock: &'c dyn Clock) -> BudgetGuard<'c> {
    budget.guard_with(clock, token())
}
