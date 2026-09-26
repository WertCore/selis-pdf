//! JPX lazy chunk (SL-4.WASM.02).
//!
//! Separate `cdylib` from the core viewer (`selis-pdf-wasm`). The viewer
//! never downloads this module unless a document actually contains a
//! `JPXDecode` stream. On native the same crate is exercised through the
//! `wasm-host` Tier-2 sandbox (OpenJPEG 2.5.3 pinned to wasm32); on the web
//! the browser's engine executes the same OpenJPEG wasm module via the chunk
//! loader.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};

/// Chunk identifier (stable, matches the manifest).
pub const CHUNK_ID: &str = "jpx";
/// Chunk version (engine workspace version).
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum JPX input accepted by this chunk (hostile-input bound).
const MAX_JPX_BYTES: usize = 64 * 1024 * 1024;

/// Returns the chunk id in guest memory (free with `selis_jpx_free`).
/// Null when allocation fails — never a trap.
///
/// # Safety
/// `out_len` must be a writable 4-byte-aligned guest `u32` slot.
#[no_mangle]
pub unsafe extern "C" fn selis_jpx_chunk_id(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let bytes = CHUNK_ID.as_bytes();
    let Ok(len_u32) = u32::try_from(bytes.len()) else {
        return std::ptr::null_mut();
    };
    let layout = match Layout::from_size_align(bytes.len(), 1) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    // SAFETY: layout non-zero; freed via `selis_jpx_free` once.
    let out = unsafe { alloc(layout) };
    if out.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `out` points to `bytes.len()` fresh bytes; `out_len` is a live
    // writable slot per the documented protocol.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        out_len.write(len_u32);
    }
    out
}

/// Free a buffer returned by this chunk.
///
/// # Safety
/// `ptr`/`len` must identify a live allocation from this module, freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn selis_jpx_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let bytes = match usize::try_from(len) {
        Ok(b) => b,
        Err(_) => return,
    };
    let Ok(layout) = Layout::from_size_align(bytes, 1) else {
        return;
    };
    // SAFETY: documented protocol.
    unsafe { dealloc(ptr, layout) };
}

/// Stub JPX probe: validates the input is non-empty and under the 64 MiB bound
/// without allocating the decoded pixels. Real decoding is the OpenJPEG wasm
/// module (`crates/selis-pdf-filter/assets/selis-jpx.wasm`) executed under the
/// Tier-2 sandbox on native and via the browser engine on the web. This export
/// proves the chunk is a separate module with its own budget-aware entry.
///
/// # Safety
/// `input_ptr` must point to `input_len` readable guest bytes when non-null.
#[no_mangle]
pub unsafe extern "C" fn selis_jpx_probe(input_ptr: *const u8, input_len: u32) -> u32 {
    if input_ptr.is_null() || input_len == 0 {
        return 0;
    }
    let Ok(len) = usize::try_from(input_len) else {
        return 0;
    };
    if len > MAX_JPX_BYTES {
        return 0;
    }
    // A non-zero return means "probe passed, decode would be attempted via the
    // sandboxed codec". The value is the budget cost hint (bytes charged).
    input_len
}

/// Budget-aware decode entry (stub): charges `pixels` against the caller's
/// `BudgetGuard` would happen here before the pixel buffer allocation. For
/// now the stub copies the probe discipline: hostile inputs are refused via
/// typed return, never a trap, and the engine's JPX path still surfaces as
/// a typed deviation when this chunk is absent (core viewer behaviour).
#[no_mangle]
pub extern "C" fn selis_jpx_decode_budget_hint(width: u32, height: u32) -> u64 {
    let w = u64::from(width);
    let h = u64::from(height);
    w.saturating_mul(h).saturating_mul(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunk_id_not_empty() {
        assert_eq!(CHUNK_ID, "jpx");
    }
    #[test]
    fn probe_refuses_empty_and_oversized() {
        // SAFETY: null with len 0 / MAX is the documented refusal probe; the
        // callee checks null before dereferencing.
        unsafe {
            assert_eq!(selis_jpx_probe(std::ptr::null(), 0), 0);
            assert_eq!(selis_jpx_probe(std::ptr::null(), u32::MAX), 0);
        }
    }
    #[test]
    fn budget_hint_matches_canvas_math() {
        assert_eq!(selis_jpx_decode_budget_hint(100, 100), 40_000);
    }
}
