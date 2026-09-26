//! The guest memory strategy (SL-4.WASM.04, `24-BINDINGS-SPEC.md` §2 Memory).
//!
//! # Growth policy
//!
//! `wasm32` linear memory grows in 64 KiB pages (`WASM_PAGE_BYTES`) up to
//! 65 536 pages (`WASM_MAX_BYTES` = 4 GiB, the WebAssembly ceiling). The guest
//! itself never tunes the engine's allocator: it **pre-sizes every buffer
//! exactly** (input, canvas, out-copy) and **refuses before growing** — a
//! document that would exceed the effective cap fails with a typed
//! budget error, never with a trap or a dead tab.
//!
//! Growth, in order:
//!
//! 1. The shell copies the request + attachment into guest memory with
//!    `selis_input_alloc` (already 4-byte-aligned, already bounded by
//!    `PROTOCOL_MAX_INPUT_BYTES`).
//! 2. `Worker::open` checks the *claimed* length against the wasm ceiling,
//!    the document budget, the protocol bound, and the tab cap **before**
//!    touching the payload bytes (so a 1.5 GiB claim fails typed without a
//!    1.5 GiB copy).
//! 3. Renders charge the canvas (`w × h` pixels, `× 4` bytes) against the
//!    document budget **before** the backend exists, then allocate the
//!    backend fallibly (`None` maps to `BUDGET_PIXELS`, never a trap).
//! 4. The host caps growth out-of-band: wasmtime `StoreLimits` (native
//!    harnesses, 512 MiB) and the browser tab cap (web). The guest never
//!    relies on the host trap — the pre-checks above fire first.
//!
//! # `Budget.bytes` / `Budget.wall` ↔ the JS cap
//!
//! The JS shell owns two numbers the guest cannot observe directly:
//!
//! * the tab heap cap (`performance.memory.jsHeapSizeLimit`, or a configured
//!   budget — 512 MiB by default, `JS_DEFAULT_CAP_BYTES`);
//! * the UX deadline for the operation (how long the shell will wait before
//!   advising the user).
//!
//! The mapping is explicit and bidirectional:
//!
//! * JS → guest: `BudgetProfile.overrides.bytes` carries the heap cap for the
//!   document (`effective_byte_cap` intersects it with the surface profile
//!   and the 4 GiB ceiling); `overrides.wall_ms` carries the deadline in
//!   milliseconds (converted to nanoseconds for `Budget.wall`, overflow is
//!   `BINDING_BAD_ARGUMENT`, never a silent clamp). The shell pokes the
//!   exported clock slot with `performance.now`, so the wall deadline fires
//!   exactly as often as the shell updates it.
//! * Guest → JS: `memoryStats` reports `liveBytes`/`peakBytes` so the shell
//!   can evict its own caches (transferred buffers, `ImageBitmap`s) before
//!   the browser kills the tab; `memoryPressure` is the shell's way to ask
//!   the guest to drop what it can first.
//!
//! # Typed OOM, never a crash
//!
//! Every exhaustion maps to the registry, never to a trap:
//!
//! | Condition | Code |
//! |---|---|
//! | claimed length past the 4 GiB ceiling | `BUDGET_BYTES` |
//! | claimed length past the document `bytes` budget | `BUDGET_BYTES` |
//! | claimed length past the 64 MiB inline bound | `SOURCE_TOO_LARGE` |
//! | live + claimed past the tab cap | `BUDGET_BYTES` |
//! | canvas past the pixel budget | `BUDGET_PIXELS` |
//! | canvas allocation refused | `BUDGET_PIXELS` |
//! | registry full (too many open docs) | `BUDGET_BYTES` |
//!
//! `SOURCE_TOO_LARGE` is kind `Budget` in the registry — a budget outcome,
//! not a malformed-input outcome.
//!
//! # Explicit release
//!
//! `close` drops the `Session` (source bytes, resolved model) and subtracts
//! its length from the live tally. Opening N documents sequentially and
//! closing each shows no live growth — asserted by test, not by inspection.
//!
//! # Pressure
//!
//! `Worker::on_memory_pressure` drops the pre-cancel records (the only
//! guest-side queue that grows without a document) and reports the live
//! tally so the shell can evict its caches. Future tile/display-list caches
//! evict here first; the level (`0` low, `1` moderate, `2` critical) selects
//! how aggressively, currently all levels drop the queue.

use selis_error::{err, Code, Result};

/// One wasm linear-memory page, in bytes (WebAssembly Core §5.3.1).
pub const WASM_PAGE_BYTES: u64 = 65_536;

/// Maximum wasm linear-memory pages (the 4 GiB `wasm32` ceiling).
pub const WASM_MAX_PAGES: u64 = 65_536;

/// Maximum wasm linear-memory bytes (4 GiB — the documented, tested boundary).
pub const WASM_MAX_BYTES: u64 = 4_294_967_296;

/// Default tab heap cap, in bytes (matches the wasmtime harness limiter;
/// the browser shell replaces it with `performance.memory.jsHeapSizeLimit`
/// when known).
pub const JS_DEFAULT_CAP_BYTES: u64 = 536_870_912;

/// Inline-document bound, in bytes (mirrors the worker/protocol bound;
/// larger sources arrive as streaming adapters in WASM.05/06).
pub const PROTOCOL_MAX_INPUT_BYTES: u64 = 67_108_864;

/// Canvas dimension bound, in device pixels (mirrors the worker bound).
pub const PROTOCOL_MAX_CANVAS_DIM: u32 = 16_384;

/// Bytes per rendered pixel (RGBA8).
pub const BYTES_PER_PIXEL: u64 = 4;

/// Clamped pressure levels (`0` low, `1` moderate, `2` critical).
pub const PRESSURE_LOW: u32 = 0;
/// Clamped pressure levels (`0` low, `1` moderate, `2` critical).
pub const PRESSURE_MODERATE: u32 = 1;
/// Clamped pressure levels (`0` low, `1` moderate, `2` critical).
pub const PRESSURE_CRITICAL: u32 = 2;

/// The number of wasm pages needed to hold `bytes` (ceiling division,
/// saturating — `0` bytes needs `0` pages).
#[must_use]
pub fn pages_for(bytes: u64) -> u64 {
    if bytes == 0 {
        return 0;
    }
    let adjust = WASM_PAGE_BYTES.saturating_sub(1);
    let rounded = bytes.saturating_add(adjust);
    rounded.checked_div(WASM_PAGE_BYTES).unwrap_or(u64::MAX)
}

/// Whether `bytes` exceeds the 4 GiB `wasm32` ceiling.
#[must_use]
pub fn exceeds_wasm_ceiling(bytes: u64) -> bool {
    bytes > WASM_MAX_BYTES
}

/// The effective per-document byte cap: the surface budget intersected with
/// the JS tab cap and the 4 GiB ceiling (the minimum wins — a weaker cap
/// never raises a stronger one).
#[must_use]
pub fn effective_byte_cap(budget_bytes: u64, js_cap_bytes: u64) -> u64 {
    let capped = budget_bytes.min(js_cap_bytes);
    capped.min(WASM_MAX_BYTES)
}

/// The canvas pixel count for `w × h` (saturating — a hostile geometry
/// saturates instead of wrapping).
#[must_use]
pub fn canvas_pixels(w: u32, h: u32) -> u64 {
    u64::from(w).saturating_mul(u64::from(h))
}

/// The canvas byte count for `w × h` RGBA8 (saturating).
#[must_use]
pub fn canvas_bytes(w: u32, h: u32) -> u64 {
    canvas_pixels(w, h).saturating_mul(BYTES_PER_PIXEL)
}

/// Clamp a shell-supplied pressure level to `0..=2`.
#[must_use]
pub fn clamp_pressure_level(level: u32) -> u32 {
    if level > PRESSURE_CRITICAL {
        PRESSURE_CRITICAL
    } else {
        level
    }
}

/// Validate a claimed inline length *before* touching the payload.
///
/// The order is load-bearing: the wasm ceiling first (a 1.5 GiB claim fails
/// typed without a 1.5 GiB copy), then the document budget, then the tab
/// cap, then the protocol bound. Every failure is a typed budget outcome,
/// never a trap.
///
/// # Errors
///
/// * `BUDGET_BYTES` when the claim exceeds the wasm ceiling, the document
///   budget, or the tab cap.
/// * `SOURCE_TOO_LARGE` when the claim exceeds the inline protocol bound.
pub fn check_inline_len(
    len: u64,
    budget_bytes: u64,
    live_bytes: u64,
    js_cap_bytes: u64,
) -> Result<()> {
    if exceeds_wasm_ceiling(len) {
        return Err(err!(
            Code::BudgetBytes,
            during = "wasm-memory",
            detail = "claimed length exceeds the 4 GiB wasm32 ceiling"
        ));
    }
    if len > budget_bytes {
        return Err(err!(
            Code::BudgetBytes,
            during = "wasm-memory",
            detail = "claimed length exceeds the document byte budget"
        ));
    }
    let live_next = live_bytes.saturating_add(len);
    if live_next > js_cap_bytes {
        return Err(err!(
            Code::BudgetBytes,
            during = "wasm-memory",
            detail = "tab heap cap would be exceeded; close a document first"
        ));
    }
    if len > PROTOCOL_MAX_INPUT_BYTES {
        return Err(err!(
            Code::SourceTooLarge,
            during = "wasm-memory",
            detail = "inline source exceeds the 64 MiB protocol bound"
        ));
    }
    Ok(())
}

/// Validate a canvas against the pixel budget and the wasm ceiling before
/// the backend exists.
///
/// # Errors
///
/// * `BUDGET_PIXELS` when the pixel count exceeds `budget_pixels`, when the
///   byte count exceeds the wasm ceiling, or when either dimension exceeds
///   `PROTOCOL_MAX_CANVAS_DIM`.
pub fn check_canvas(w: u32, h: u32, budget_pixels: u64) -> Result<()> {
    if w == 0 || h == 0 || w > PROTOCOL_MAX_CANVAS_DIM || h > PROTOCOL_MAX_CANVAS_DIM {
        return Err(err!(
            Code::BudgetPixels,
            during = "wasm-memory",
            detail = "canvas exceeds the protocol dimension cap"
        ));
    }
    let pixels = canvas_pixels(w, h);
    if pixels > budget_pixels {
        return Err(err!(
            Code::BudgetPixels,
            during = "wasm-memory",
            detail = "canvas exceeds the pixel budget"
        ));
    }
    if exceeds_wasm_ceiling(canvas_bytes(w, h)) {
        return Err(err!(
            Code::BudgetPixels,
            during = "wasm-memory",
            detail = "canvas exceeds the 4 GiB wasm32 ceiling"
        ));
    }
    Ok(())
}

/// A snapshot of guest memory accounting (the `memoryStats` payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryStats {
    /// Currently open documents.
    pub live_docs: u64,
    /// Sum of open document source lengths, in bytes.
    pub live_bytes: u64,
    /// Maximum `live_bytes` observed since the worker was created.
    pub peak_bytes: u64,
    /// The 4 GiB `wasm32` ceiling.
    pub wasm_max_bytes: u64,
    /// The tab heap cap the guest enforces against.
    pub js_cap_bytes: u64,
    /// The inline-document bound.
    pub max_input_bytes: u64,
    /// The canvas dimension bound.
    pub max_canvas_dim: u32,
}

impl MemoryStats {
    /// A snapshot from the worker tallies.
    #[must_use]
    pub const fn new(live_docs: u64, live_bytes: u64, peak_bytes: u64, js_cap_bytes: u64) -> Self {
        Self {
            live_docs,
            live_bytes,
            peak_bytes,
            wasm_max_bytes: WASM_MAX_BYTES,
            js_cap_bytes,
            max_input_bytes: PROTOCOL_MAX_INPUT_BYTES,
            max_canvas_dim: PROTOCOL_MAX_CANVAS_DIM,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects,
        clippy::panic
    )]
    use super::*;
    use selis_error::Code;

    #[test]
    fn pages_cover_the_wasm_ceiling() {
        assert_eq!(pages_for(0), 0);
        assert_eq!(pages_for(1), 1);
        assert_eq!(pages_for(WASM_PAGE_BYTES), 1);
        assert_eq!(pages_for(WASM_PAGE_BYTES + 1), 2);
        assert_eq!(pages_for(WASM_MAX_BYTES), WASM_MAX_PAGES);
    }

    #[test]
    fn effective_cap_is_the_minimum() {
        assert_eq!(effective_byte_cap(100, 200), 100);
        assert_eq!(effective_byte_cap(300, 200), 200);
        assert_eq!(effective_byte_cap(u64::MAX, u64::MAX), WASM_MAX_BYTES);
    }

    #[test]
    fn a_one_point_five_gigabyte_claim_fails_typed() {
        let big: u64 = 1_610_612_736;
        assert!(!exceeds_wasm_ceiling(big));
        let e = check_inline_len(big, 268_435_456, 0, JS_DEFAULT_CAP_BYTES)
            .expect_err("1.5 GiB exceeds the viewer budget");
        assert_eq!(e.code(), Code::BudgetBytes);
        let past_ceiling: u64 = 5_000_000_000;
        let e = check_inline_len(past_ceiling, u64::MAX, 0, u64::MAX)
            .expect_err("past 4 GiB is the wasm ceiling");
        assert_eq!(e.code(), Code::BudgetBytes);
    }

    #[test]
    fn inline_bound_is_source_too_large() {
        let over: u64 = 67_108_864 + 1;
        let e = check_inline_len(over, u64::MAX, 0, u64::MAX).expect_err("past the inline bound");
        assert_eq!(e.code(), Code::SourceTooLarge);
    }

    #[test]
    fn tab_cap_blocks_when_live_plus_new_exceeds() {
        let e = check_inline_len(100, u64::MAX, JS_DEFAULT_CAP_BYTES, JS_DEFAULT_CAP_BYTES)
            .expect_err("live + new exceeds the tab cap");
        assert_eq!(e.code(), Code::BudgetBytes);
    }

    #[test]
    fn canvas_checks_are_typed() {
        assert!(check_canvas(10, 10, 1_000_000).is_ok());
        let e = check_canvas(0, 10, u64::MAX).expect_err("empty canvas");
        assert_eq!(e.code(), Code::BudgetPixels);
        let e = check_canvas(20_000, 10, u64::MAX).expect_err("past the dim cap");
        assert_eq!(e.code(), Code::BudgetPixels);
        let e = check_canvas(100, 100, 10).expect_err("past the pixel budget");
        assert_eq!(e.code(), Code::BudgetPixels);
        assert_eq!(canvas_bytes(16_384, 16_384), 1_073_741_824);
    }

    #[test]
    fn pressure_levels_clamp() {
        assert_eq!(clamp_pressure_level(0), 0);
        assert_eq!(clamp_pressure_level(2), 2);
        assert_eq!(clamp_pressure_level(99), 2);
    }
}
