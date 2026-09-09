//! The runtime-independent Tier-2 codec-sandbox contract (SL-0.SBX.06).
//!
//! # The two paths, one protocol
//!
//! Tier 2 (`01-ARCHITECTURE.md §4`) runs untrusted codec code (OpenJPEG in
//! Phase 2, Tesseract in Phase 5) inside a WASM linear-memory sandbox. There
//! are two execution paths:
//!
//! | Path | Runtime | Where |
//! |---|---|---|
//! | native | `wasmtime` (the `wasm-host` cargo feature) | CLI, desktop, server, mobile |
//! | web | the browser's own WASM engine | `selis-pdf-wasm` workers |
//!
//! Both implement the same [`wasm`] module contract, so a codec integrates
//! once and a host can swap runtimes without the codec or the caller knowing.
//!
//! # The buffer protocol — why copy-in/copy-out
//!
//! The module never receives a host pointer. Input bytes are copied into its
//! linear memory at an offset the module itself allocates and reports; output
//! bytes are written into a region the module allocated and declares via
//! `(ptr, len)` return pairs that the host validates against the memory
//! limits *before* reading. A hostile module can therefore corrupt only its
//! own linear memory — it has nothing to corrupt ours with, and every
//! declared region is bounds-checked by the host (the module cannot point a
//! length at memory it does not own, and a bad region is a typed
//! [`Code::SandboxProtocol`] error, never an out-of-bounds read).
//!
//! # What the module may and may not assume
//!
//! * It declares **no imports**. There is no filesystem, no clock, no
//!   environment, no network — a module with any import section is refused
//!   outright ([`Code::SandboxImportDenied`]).
//! * It exports the [`exports`] functions described below, plus `memory`.
//! * Time exists only as the host's fuel meter: an infinite loop runs out of
//!   fuel ([`Code::SandboxFuel`]) at a point derived from the caller's
//!   [`Budget`](crate::Budget) and is interrupted at that fuel-tick
//!   boundary; it can neither observe nor await a wall clock.
//! * Memory growth beyond the hard cap fails with a trap that the host maps
//!   to [`Code::SandboxMemoryCap`]; the cap comes from the caller's budget
//!   allocation, not from the module.

use selis_error::{Error, Result};

/// The WASM memory page size (PDF-independent; fixed by the WASM spec:
/// WebAssembly Core Specification §5.3.1).
pub const PAGE_SIZE: u32 = 65_536;

/// The exported name of the linear memory.
pub const EXPORT_MEMORY: &str = "memory";

/// The exported name of the one-shot initialisation function.
pub const EXPORT_INIT: &str = "init";

/// The exported name of the decode entry point.
pub const EXPORT_DECODE: &str = "decode";

/// The exported name of the output-region reader.
pub const EXPORT_OUTPUT: &str = "output";

/// The exported name of the output-region pointer reader.
///
/// Split from [`EXPORT_OUTPUT`] so the protocol stays implementable from C
/// (the OpenJPEG module): C toolchains do not produce wasm multi-value
/// returns, so `output(max_len) -> len` and `output_ptr() -> ptr` are two
/// plain i32 calls. The browser path benefits identically.
pub const EXPORT_OUTPUT_PTR: &str = "output_ptr";

/// The exported name of the output-region finaliser.
pub const EXPORT_FINISH: &str = "finish";

/// Status codes a well-formed module returns from `decode`/`finish`.
///
/// Everything else the module can do is either a trap (contained by the host)
/// or a budget event (typed by the host). This status is the module's only
/// channel for reporting *its own* view of its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Status {
    /// The module produced output.
    Ok = 0,
    /// The module's input is not in the format it decodes.
    InputMalformed = 1,
    /// The module needs more input than the host provided (truncated
    /// codestream).
    InputTruncated = 2,
    /// The module's input is structurally valid but unsupported by this
    /// build/profile.
    InputUnsupported = 3,
}

impl TryFrom<i32> for Status {
    type Error = Error;

    fn try_from(v: i32) -> Result<Self> {
        match v {
            0 => Ok(Self::Ok),
            1 => Ok(Self::InputMalformed),
            2 => Ok(Self::InputTruncated),
            3 => Ok(Self::InputUnsupported),
            other => Err(selis_error::err!(
                selis_error::Code::SandboxProtocol,
                during = "wasm-status",
                detail = std::format!("unknown status {other}")
            )),
        }
    }
}

/// What a successful sandbox run produced.
///
/// `output` is a host-owned copy; the module's memory is dropped with the
/// instance and nothing in it outlives the run.
#[derive(Debug)]
pub struct CodecOutput {
    /// The module's `status` code for its input.
    pub status: Status,
    /// A copy of the module's declared output region, empty when the module
    /// reported failure.
    pub output: Vec<u8>,
}

/// The `init` convention, in host terms.
///
/// `(input_len) -> ptr`: the module reserves a region of at least
/// `input_len` bytes for the input and returns the offset of the first byte,
/// or `0` on failure. The host copies the input there and then calls
/// `decode`. A module returning a pointer that does not satisfy the
/// declared minimum is a protocol violation.
pub const INIT_CONTRACT: &str = "fn init(input_len: i32) -> i32  // returns input ptr or 0";

/// The `decode` convention, in host terms.
///
/// `(input_len) -> status`: the module reads `input_len` bytes from the
/// pointer `init` returned, decodes, and stores its output internally.
/// Growth beyond the hard cap traps and is contained.
pub const DECODE_CONTRACT: &str = "fn decode(input_len: i32) -> i32  // returns Status";

/// The `output` convention, in host terms.
///
/// `(max_len: i32) -> len` then `(output_ptr: ()) -> ptr`: the module
/// reports where its output lives as two i32 calls (C has no wasm
/// multi-value returns; OpenJPEG is the first real client). The host
/// clamps the module's declared length against `max_len` (the caller's
/// output budget), validates the region against the memory bounds, and
/// copies it out. A region outside linear memory is a protocol violation,
/// not a read.
pub const OUTPUT_CONTRACT: &str = "fn output(max_len: i32) -> i32 len; fn output_ptr() -> i32 ptr";

/// The `finish` convention, in host terms.
///
/// `() -> status`: after a `decode` failure, lets a module release its
/// internal buffers and report a final status. The host calls it exactly
/// once per run, after the output copy, before dropping the instance.
pub const FINISH_CONTRACT: &str = "fn finish() -> i32  // returns Status";

/// The complete export set a conforming module must provide.
pub const REQUIRED_EXPORTS: [&str; 5] = [
    EXPORT_INIT,
    EXPORT_DECODE,
    EXPORT_OUTPUT,
    EXPORT_OUTPUT_PTR,
    EXPORT_FINISH,
];

#[cfg(test)]
mod tests {
    use super::*;
    use selis_error::Code;

    #[test]
    fn status_round_trips() {
        assert_eq!(Status::try_from(0), Ok(Status::Ok));
        assert_eq!(Status::try_from(3), Ok(Status::InputUnsupported));
    }

    #[test]
    fn an_unknown_status_is_a_protocol_error() {
        let e = Status::try_from(42).expect_err("42 is not a status");
        assert_eq!(e.code(), Code::SandboxProtocol);
        let e = Status::try_from(-1).expect_err("negative is not a status");
        assert_eq!(e.code(), Code::SandboxProtocol);
    }

    #[test]
    fn page_size_matches_the_wasm_spec() {
        assert_eq!(PAGE_SIZE, 1 << 16);
    }
}
