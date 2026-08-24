//! Cooperative cancellation (SL-0.SBX.03).
//!
//! Every long operation is cancellable and cancellation is observable within one
//! [`crate::BudgetGuard::tick`] — which the `check-contracts` rule requires in
//! every loop whose condition reads parsed data.
//!
//! Cooperative rather than pre-emptive because the engine is synchronous
//! (ADR-P0004) and because a pre-empted parser leaves a half-built index, whereas
//! a cooperatively cancelled one returns a typed `CANCELLED` with the document
//! state intact.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A shared cancellation flag.
///
/// Cloning shares the flag. Cancelling any clone cancels them all.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// A token that has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. Idempotent, and safe from any thread.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_shared_and_idempotent() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!a.is_cancelled());
        b.cancel();
        b.cancel();
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
    }

    #[test]
    fn cancellation_crosses_threads() {
        let token = CancelToken::new();
        let worker = token.clone();
        let handle = std::thread::spawn(move || worker.cancel());
        handle.join().expect("worker thread");
        assert!(token.is_cancelled());
    }
}
