//! Platform Ctrl-C / SIGINT hook (SL-1A.UI.06).
//!
//! The engine cancels cooperatively: a [`CancelToken`] observed at every
//! [`crate::DocSink`] chunk boundary and budget tick. This module is the
//! shell-side half — it turns the platform's interrupt signal into a shared
//! flag the CLI can hand to a `CancelToken`, so Ctrl-C maps to a typed
//! `CANCELLED` error and a clean exit instead of a process kill. (The WRITE.06
//! crash-atomicity guarantee already makes a hard kill safe; the hook is what
//! makes it *clean*.)
//!
//! The hook is best-effort: when the platform offers no way to intercept the
//! signal (`wasm32`), or installation fails, [`install_ctrl_c_flag`] returns
//! `None` and the caller keeps the default behavior — the process dies on
//! Ctrl-C exactly as before, still leaving no torn file.
//!
//! Compiled for every target; the platform branches are `cfg`-gated and both
//! branches are warning-free.

// The `unsafe` blocks below (console/signal FFI) are allowlisted for this
// crate under 03-CONVENTIONS.md §2 / xtask/unsafe-allow.toml.
#![cfg_attr(windows, allow(unsafe_code))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Set once a handler has been installed, so a second call is a no-op.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install a platform interrupt hook that sets the returned flag on Ctrl-C
/// (SIGINT on POSIX, `CTRL_C_EVENT`/`CTRL_BREAK_EVENT` on Windows).
///
/// Returns `None` when a hook is already installed or the platform provides
/// no hook. The flag is shared: wrap it in a
/// `selis_sandbox::CancelToken::from_flag` and every guard observing that
/// token cancels on the user's Ctrl-C.
///
/// The returned flag lives for the rest of the process (the handler needs it
/// after the caller's borrows end); treat it as a singleton.
pub fn install_ctrl_c_flag() -> Option<Arc<AtomicBool>> {
    if INSTALLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    imp::install()
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};

    /// The flag the console handler sets. `OnceLock` is safe here: Windows
    /// console control handlers run on a normal thread (not a signal
    /// context), so ordinary synchronised reads are fine.
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;

    type ConsoleHandler = Option<extern "system" fn(u32) -> i32>;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(handler: ConsoleHandler, add: i32) -> i32;
    }

    extern "system" fn on_console_ctrl(ctrl: u32) -> i32 {
        match ctrl {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                if let Some(flag) = FLAG.get() {
                    flag.store(true, Ordering::Release);
                }
                1 // handled: the engine observes the flag and exits cleanly
            }
            _ => 0, // close/logoff/... : default processing
        }
    }

    pub(super) fn install() -> Option<Arc<AtomicBool>> {
        let flag = Arc::new(AtomicBool::new(false));
        if FLAG.set(flag.clone()).is_err() {
            return None;
        }
        // SAFETY: `on_console_ctrl` is a valid console handler routine; it
        // only reads the handler-static FLAG and stores to an AtomicBool.
        // A zero return means no console is attached (CI, redirected I/O) —
        // harmless, the flag simply never fires.
        let _ = unsafe { SetConsoleCtrlHandler(Some(on_console_ctrl), 1) };
        Some(flag)
    }
}

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    const SIGINT: i32 = 2;

    /// Raw pointer to the leaked flag `Arc`'s `AtomicBool`, stored *before*
    /// the handler is registered and never changed or freed afterwards.
    static FLAG_PTR: AtomicUsize = AtomicUsize::new(0);

    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }

    extern "C" fn on_sigint(_sig: i32) {
        let ptr = FLAG_PTR.load(Ordering::Acquire) as *const AtomicBool;
        if !ptr.is_null() {
            // SAFETY: the pointer was produced by `Arc::into_raw` before the
            // handler was registered, is never mutated or freed (leaked for
            // the process lifetime), and only a plain atomic store happens
            // here — the async-signal-safe subset.
            (*ptr).store(true, Ordering::Release);
        }
    }

    pub(super) fn install() -> Option<Arc<AtomicBool>> {
        if FLAG_PTR.load(Ordering::Acquire) != 0 {
            return None;
        }
        let flag = Arc::new(AtomicBool::new(false));
        // Leak one reference into the handler: never recovered, so the
        // pointer stays valid for the process lifetime.
        let raw = Arc::into_raw(flag.clone());
        FLAG_PTR.store(raw as usize, Ordering::Release);
        // SAFETY: `on_sigint` touches only the static above (see its SAFETY
        // note). SIGINT's default disposition (terminate) is replaced; when
        // the hook is not wanted the caller must not install it.
        let _ = unsafe { signal(SIGINT, on_sigint as extern "C" fn(i32) as usize) };
        Some(flag)
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    pub(super) fn install() -> Option<Arc<AtomicBool>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hook installs exactly once and yields a usable shared flag; on
    /// platforms without a hook it reports `None` honestly.
    #[test]
    fn installs_once_and_flag_is_shareable() {
        let first = install_ctrl_c_flag();
        let second = install_ctrl_c_flag();
        match first {
            Some(flag) => {
                assert!(second.is_none(), "a second install must be refused");
                assert!(!flag.load(Ordering::Acquire));
                // The same store the platform handler performs must be
                // observable through the shared handle.
                flag.store(true, Ordering::Release);
                assert!(flag.load(Ordering::Acquire));
            }
            None => {
                // No platform hook (wasm-like target): honest absence.
                assert!(second.is_none());
            }
        }
    }
}
