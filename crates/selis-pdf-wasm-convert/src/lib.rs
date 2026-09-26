//! Convert lazy chunk (SL-4.WASM.02).
//!
//! Separate `cdylib` from the core viewer. The viewer never downloads this
//! module; it is fetched when the user invokes PDF→Office/HTML/Markdown
//! conversion (Phase 8, `selis-pdf-convert`).

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};

/// Chunk identifier (stable, matches the manifest).
pub const CHUNK_ID: &str = "convert";
/// Chunk version.
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the chunk id as guest memory (free with `selis_convert_free`).
///
/// # Safety
/// `out_len` must be a writable 4-byte-aligned guest `u32` slot.
#[no_mangle]
pub unsafe extern "C" fn selis_convert_chunk_id(out_len: *mut u32) -> *mut u8 {
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
pub unsafe extern "C" fn selis_convert_free(ptr: *mut u8, len: u32) {
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

/// Capability query: which converters this chunk offers (bitmask).
/// Bit 0: pdf→text, Bit 1: pdf→json, Bit 2: pdf→markdown, Bit 3: pdf→html
#[no_mangle]
pub extern "C" fn selis_convert_capabilities() -> u32 {
    0b1111
}

/// Availability probe for the convert surface — always `1` for this stub.
/// Real conversion charges `Budget`/`CancelToken` per page via `selis-pdf-convert`.
#[no_mangle]
pub extern "C" fn selis_convert_available() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunk_id_is_convert() {
        assert_eq!(CHUNK_ID, "convert");
    }
    #[test]
    fn caps_cover_all_current_formats() {
        assert_eq!(selis_convert_capabilities(), 0b1111);
    }
}
