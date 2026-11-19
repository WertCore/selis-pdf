//! `JsBudget` — resource limits for one form-script run.
//!
//! QuickJS calls the interrupt handler at loop back-edges and function calls,
//! not on every bytecode. `max_ticks` therefore counts *handler invocations* as
//! an instruction proxy: an infinite loop reaches them quickly and is stopped.
//! This is documented rather than disguised as an exact instruction count.

use selis_error::{err, Code, Result};

/// All limits applied to one script run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsBudget {
    /// Interrupt-handler invocations allowed before the script is stopped.
    pub max_ticks: u64,
    /// QuickJS heap limit, in bytes.
    pub max_memory_bytes: usize,
    /// QuickJS value-stack limit, in bytes (bounds recursion).
    pub max_stack_bytes: usize,
    /// Maximum accepted script length, in bytes.
    pub max_script_bytes: usize,
    /// Maximum number of fields in a request.
    pub max_fields: usize,
    /// Maximum length of any single field name/value or event string, in bytes.
    pub max_string_bytes: usize,
}

impl JsBudget {
    /// A conservative default for a viewer running an untrusted form script.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            max_ticks: 100_000,
            max_memory_bytes: 8 * 1024 * 1024,
            max_stack_bytes: 1024 * 1024,
            max_script_bytes: 64 * 1024,
            max_fields: 512,
            max_string_bytes: 64 * 1024,
        }
    }

    /// A deliberately tiny budget for the instruction-interrupt test.
    #[must_use]
    pub const fn tiny() -> Self {
        Self {
            max_ticks: 256,
            max_memory_bytes: 2 * 1024 * 1024,
            max_stack_bytes: 256 * 1024,
            max_script_bytes: 16 * 1024,
            max_fields: 16,
            max_string_bytes: 1024,
        }
    }

    /// Reject nonsensical limits before they reach the engine.
    ///
    /// # Errors
    ///
    /// `JS_SCRIPT_REJECTED` if a limit is zero where the engine needs a positive
    /// value, or if the heap limit is below the stack limit's minimum.
    pub fn validate(&self) -> Result<()> {
        if self.max_ticks == 0
            || self.max_memory_bytes == 0
            || self.max_stack_bytes == 0
            || self.max_script_bytes == 0
            || self.max_fields == 0
            || self.max_string_bytes == 0
            || self.max_memory_bytes < self.max_stack_bytes
        {
            return Err(err!(
                Code::JsScriptRejected,
                during = "js-budget",
                detail = "invalid script budget"
            ));
        }
        Ok(())
    }
}

/// Measured usage of one run, returned on success and used in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JsUsage {
    /// Interrupt-handler invocations consumed.
    pub ticks: u64,
    /// QuickJS heap bytes in use when the run ended.
    pub memory_bytes: usize,
    /// Whether the instruction interrupt was the reason the run stopped.
    pub interrupted: bool,
}
