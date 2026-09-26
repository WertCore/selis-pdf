//! Editor lazy chunk (SL-4.WASM.02).
//!
//! Separate `cdylib` from the core viewer. The viewer session *never*
//! downloads this module — the `01-ARCHITECTURE.md §4` / `28-FIRST-RELEASE-SCOPE.md §3`
//! guarantee: "not exposed" means "not in the shipped chunk". The edit
//! surface (`mutate`/`save` via `selis-pdf-edit`, `23-EDIT-MODEL-SPEC.md`)
//! lives exclusively here; the core answers `BINDING_UNSUPPORTED_OP` for it.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};

/// Chunk identifier (stable, matches the manifest and the JS loader).
pub const CHUNK_ID: &str = "editor";
/// Chunk version.
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the chunk id as guest memory (free with `selis_editor_free`).
///
/// # Safety
/// `out_len` must be a writable 4-byte-aligned guest `u32` slot.
#[no_mangle]
pub unsafe extern "C" fn selis_editor_chunk_id(out_len: *mut u32) -> *mut u8 {
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    let bytes = CHUNK_ID.as_bytes();
    let Ok(len_u32) = u32::try_from(bytes.len()) else {
        return std::ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(bytes.len(), 1) else {
        return std::ptr::null_mut();
    };
    // SAFETY: layout non-zero; freed once.
    let out = unsafe { alloc(layout) };
    if out.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `out` points to `bytes.len()` fresh bytes; `out_len` is live.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        out_len.write(len_u32);
    }
    out
}

/// # Safety
/// `ptr`/`len` must identify a live allocation from this module, freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn selis_editor_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(bytes) = usize::try_from(len) else {
        return;
    };
    let Ok(layout) = Layout::from_size_align(bytes, 1) else {
        return;
    };
    // SAFETY: documented protocol.
    unsafe { dealloc(ptr, layout) };
}

/// Capability query: edit surface available?
/// Returns `1` when the mutation schema `selis-mutate/1` is supported.
#[no_mangle]
pub extern "C" fn selis_editor_available() -> u32 {
    1
}

/// Budget hint for an incremental save (bytes appended). Stub: returns a
/// conservative 64 KiB hint; real `selis-pdf-edit` incremental append is
/// budget-bound per stream chunk (ADR-P0006) and honouring it is the chunk's
/// job, not the core's.
#[no_mangle]
pub extern "C" fn selis_editor_save_budget_hint(num_page_ops: u32) -> u64 {
    let base: u64 = 64 * 1024;
    let per_op: u64 = 4096;
    base.saturating_add(per_op.saturating_mul(u64::from(num_page_ops)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunk_id_is_editor() {
        assert_eq!(CHUNK_ID, "editor");
    }
    #[test]
    fn editor_available() {
        assert_eq!(selis_editor_available(), 1);
    }
}
