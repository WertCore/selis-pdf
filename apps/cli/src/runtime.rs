//! The CLI's process runtime: the cancel token wired to the platform Ctrl-C
//! hook, and the budget guards every long operation runs under
//! (SL-1A.UI.06).
//!
//! The engine cancels cooperatively — every budget tick and every output
//! chunk observes a [`CancelToken`]. This module owns the CLI's token and
//! hands out guards wired to it, so a Ctrl-C during any tool operation
//! surfaces as the typed `CANCELLED` error and the honest "no partial file"
//! message instead of a process kill. (A hard kill remains safe — WRITE.06
//! crash-atomicity — the hook is what makes it *clean*.)

use std::sync::OnceLock;

use selis_sandbox::{Budget, BudgetGuard, CancelToken, FixedClock};

/// The CLI freezes the clock: no wall deadline of its own is enforced today
/// (progress and cancellation are the user-facing controls), so elapsed time
/// stays a constant zero and cannot trip a `BUDGET_WALL` that the operator
/// never asked for.
static CLOCK: FixedClock = FixedClock(0);

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

/// A guard against `budget`, wired to the CLI clock and the Ctrl-C token.
///
/// # Budget
///
/// Charges against `budget` exactly as `Budget::guard` would; the only
/// difference is the token, which turns Ctrl-C into a typed `CANCELLED` at
/// the next tick or charge.
#[must_use]
pub(crate) fn cli_guard(budget: &Budget) -> BudgetGuard<'static> {
    budget.guard_with(&CLOCK, token())
}
