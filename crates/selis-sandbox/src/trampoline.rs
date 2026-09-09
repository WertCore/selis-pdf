//! The panic→error trampoline (SL-0.ERR.03).
//!
//! A panic anywhere below the binding boundary is a bug, but the boundary must
//! still answer for it with a *typed* outcome: the host process survives and the
//! caller receives [`Code::InternalPanic`] — never a torn-down process, never a
//! foreign-language exception carrying engine internals (01-ARCHITECTURE.md §4
//! Tier 1, §6 "the panic→error trampoline").
//!
//! The trampoline lives in the sandbox kernel — not in the individual binding
//! crates — so every binding (WASM, C ABI, JNI, CLI) installs the same one-line
//! wrapper and the conversion cannot drift between them. `selis-pdf-wasm` and
//! `selis-pdf-ffi` do not exist yet (planned — Phase 7, `xtask/layers.toml`);
//! today the binding boundary is `apps/cli`, which wraps its whole command
//! dispatch in [`catch`].
//!
//! # What the error carries, and what it must never carry
//!
//! A panic payload is arbitrary data chosen by the panicking code. Parser code
//! that formats a document slice into a panic message (a bug, but exactly the
//! kind of bug a hostile file is hunting for) would otherwise turn the
//! trampoline into a document-exfiltration channel: the payload would ride out
//! of the engine inside the error. ADR-P0017 therefore governs the shape:
//!
//! * The payload is dropped **unread** — never downcast, never formatted, never
//!   logged by this crate.
//! * The error carries the **code path** (the caller's `during` label) and the
//!   **panic site** (`file:line:column` of the `panic!`), both engine-owned
//!   facts. The panic site is captured by a panic hook installed once per
//!   process; the hook chains to whatever hook was installed before it, so a
//!   host that installed its own reporting keeps it.
//!
//! # Unwinding is a workspace invariant
//!
//! `catch` only works while panics unwind. The workspace pins
//! `panic = "unwind"` in `[profile.release]` (`Cargo.toml`, SL-0.ERR.03); a
//! profile that switches to `panic = "abort"` turns every caught bug into a
//! lost process. Cargo takes profile sections only from the workspace root, so
//! no member crate can override this. What the trampoline still cannot catch is
//! a **stack overflow** (abort, not unwind) — which is why 01-ARCHITECTURE.md
//! §6 forbids native recursion in the parser crates.
//!
//! # Unwind safety
//!
//! `catch` wraps the operation in [`AssertUnwindSafe`]. That assertion is
//! justified at this boundary and nowhere else: the operation's outputs are
//! either returned (`Ok`) or discarded with it (`Err`), so no engine state
//! survives a panic in a state the caller could observe as valid. A caller that
//! shares *external* mutable state with the operation still owns the job of
//! repairing or discarding it.
//!
//! # Budget
//!
//! The trampoline does not replace the budget (ADR-P0006): the wrapped
//! operation is expected to run under a [`crate::Budget`] and return typed
//! `BUDGET_*` errors on exhaustion. A panic reaching the trampoline is by
//! definition a case the budget did not convert into a typed error — a bug.
//!
//! # Malformed Input
//!
//! Malformed input must never reach the trampoline as a panic; it is either
//! recorded as a deviation or returned as a typed error by the parser. If a
//! crafted file does manage to panic the parser, the outcome is
//! `INTERNAL_PANIC` with the panic site, and the file is a regression: it gets
//! a corpus entry (ADR-P0032) and a filed task.

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::OnceLock;

use selis_error::{Code, Ctx, Error};

thread_local! {
    /// The site of the most recent panic on this thread, set by the recorder
    /// hook and consumed by [`catch`]. `None` between operations. The file is
    /// copied into an owned `String` because the hook's `location()` borrows
    /// the panic info.
    static LAST_PANIC_SITE: Cell<Option<(String, u32, u32)>> = const { Cell::new(None) };
}

/// Installed exactly once per process, before the first [`catch`].
static RECORDER: OnceLock<()> = OnceLock::new();

/// Install the panic-site recorder, chaining to any hook the host installed.
///
/// The hook runs for *every* panic in the process, caught or not; recording a
/// site that is never consumed is harmless because the slot is cleared before
/// each trampolined operation on the same thread.
fn ensure_recorder() {
    RECORDER.get_or_init(|| {
        let prev = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let site = info
                .location()
                .map(|l| (l.file().to_owned(), l.line(), l.column()));
            LAST_PANIC_SITE.with(|slot| slot.set(site));
            prev(info);
        }));
    });
}

/// Run `op`, converting a panic into the typed [`Code::InternalPanic`] error.
///
/// This is the one wrapping every binding must install at its boundary (Tier 1
/// of 01-ARCHITECTURE.md §4). On `Ok`/`Err` from `op` the result passes through
/// unchanged; only a panic is converted. The error context carries the
/// caller's `during` label (the code path) and the panic site — never the
/// payload (ADR-P0017, see the module docs).
///
/// The error type is generic so each binding keeps its own: the engine's
/// [`Error`] for L3 callers, the CLI's `CliError`, a future FFI status code.
///
/// # Errors
///
/// [`Code::InternalPanic`] when `op` panics, with `during` recorded. The
/// registry marks the code `doc_state = Unchanged`: a caught panic is a bug,
/// and the caller must treat everything the operation produced as discarded.
///
/// # Panics
///
/// Never propagates `op`'s panic. A panic in the panic machinery itself (hook,
/// unwinding) is an abort by process rules and is out of reach here.
pub fn catch<T, E>(during: &'static str, op: impl FnOnce() -> Result<T, E>) -> Result<T, E>
where
    E: From<Error>,
{
    ensure_recorder();
    LAST_PANIC_SITE.with(|slot| slot.set(None));

    match panic::catch_unwind(AssertUnwindSafe(op)) {
        Ok(result) => result,
        Err(payload) => {
            // ADR-P0017: the payload may embed document-derived text, so it is
            // dropped without inspection — not downcast, not formatted, not
            // logged. The recorded site is engine-owned (a source path).
            drop(payload);
            let site = LAST_PANIC_SITE.with(Cell::take);
            let detail = match site {
                Some((file, line, column)) => format!("panicked at {file}:{line}:{column}"),
                None => "panic (site unavailable)".to_owned(),
            };
            Err(E::from(Error::with(
                Code::InternalPanic,
                Ctx::new().during(during).detail(detail),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DoD shape: a panic raised many frames below the boundary comes back
    /// as the typed `INTERNAL_PANIC` error and the host process lives on.
    #[test]
    fn deep_call_panic_becomes_typed_internal_panic() {
        // A parser-shaped descent: each frame hands a narrower span to the
        // next, the deepest frame panics. `checked_sub` is the arithmetic
        // escape hatch the workspace lints require.
        fn descend(depth: u32) -> Result<(), Error> {
            match depth.checked_sub(1) {
                Some(next) => descend(next),
                None => panic!("parser invariant broken at the bottom of the stack"),
            }
        }

        let Some(err) = catch("simulated-parse", || descend(512)).err() else {
            panic!("a panic must convert to an error");
        };
        assert_eq!(err.code(), Code::InternalPanic);
        assert_eq!(err.kind(), selis_error::Kind::Internal);
        assert_eq!(err.ctx().during, Some("simulated-parse"));
        assert_eq!(err.doc_state(), selis_error::DocState::Unchanged);
        let site = err.ctx().detail.as_deref().unwrap_or_default();
        assert!(
            site.starts_with("panicked at") && site.contains("trampoline.rs"),
            "must carry the panic site, got {site:?}"
        );
    }

    /// DoD: the payload must contain no document bytes. The panic message
    /// embeds a document-shaped secret; the error must not carry it.
    #[test]
    fn error_payload_carries_no_document_bytes() {
        const SECRET: &str = "%%BoundingBox[0 0 612 792] SECRET-DOCUMENT-BYTES";

        fn hostile_parser() -> Result<(), Error> {
            panic!("malformed token near {}", SECRET);
        }

        let Some(err) = catch("simulated-parse", hostile_parser).err() else {
            panic!("a panic must convert to an error");
        };

        // Every channel the caller could read: Display, the log line, and the
        // raw context. None may contain the payload text.
        let rendered = format!("{}|{}|{:?}", err, err.to_log_line(), err.ctx());
        assert!(!rendered.contains("SECRET-DOCUMENT-BYTES"));
        assert!(!rendered.contains("BoundingBox"));
        assert!(
            err.to_log_line().contains("during=simulated-parse"),
            "the code path must still be present"
        );
    }

    #[test]
    fn ok_and_typed_err_pass_through_unchanged() {
        let ok = catch("pass-through", || Ok::<u32, Error>(7)).expect("Ok passes through");
        assert_eq!(ok, 7);

        let Some(typed) = catch("pass-through", || -> Result<(), Error> {
            Err(Error::new(Code::BudgetWall))
        })
        .err() else {
            panic!("Err passes through");
        };
        assert_eq!(typed.code(), Code::BudgetWall);
        assert_eq!(typed.ctx().during, None, "no boundary context is added");
    }

    #[test]
    fn non_string_payload_is_dropped_safely() {
        struct Opaque;
        fn hostile() -> Result<(), Error> {
            std::panic::panic_any(Opaque);
        }
        let Some(err) = catch("simulated-parse", hostile).err() else {
            panic!("a non-string payload still converts");
        };
        assert_eq!(err.code(), Code::InternalPanic);
    }

    /// SL-0.ERR.03: release profiles of the shipped libraries must keep
    /// unwinding, or the trampoline turns every caught bug into a lost
    /// process. Cargo takes `[profile.*]` only from the workspace root, so
    /// asserting on the root manifest is the whole story.
    #[test]
    fn no_workspace_profile_sets_panic_abort() {
        let manifest = include_str!("../../../Cargo.toml");
        let mut section = String::new();
        for line in manifest.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                section = trimmed.to_owned();
            }
            if trimmed.contains("\"abort\"") {
                panic!("{section} sets panic = \"abort\" — SL-0.ERR.03 needs unwinding");
            }
        }
        assert!(
            manifest.contains("panic = \"unwind\""),
            "[profile.release] must pin panic = \"unwind\" explicitly"
        );
    }
}
