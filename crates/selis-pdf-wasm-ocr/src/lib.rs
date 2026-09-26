//! OCR lazy chunk (SL-4.WASM.02).
//!
//! Separate `cdylib` from the core viewer. The viewer never downloads this
//! module — it is fetched on demand when the user invokes OCR (Phase 5,
//! Tesseract compiled to WASM via the Tier-2 sandbox on native and the
//! browser engine on the web).

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};

/// Chunk identifier (stable, matches the manifest).
pub const CHUNK_ID: &str = "ocr";
/// Chunk version.
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum image bytes accepted for OCR (hostile-input bound).
const MAX_OCR_BYTES: usize = 64 * 1024 * 1024;

/// Returns the chunk id as guest memory (free with `selis_ocr_free`).
///
/// # Safety
/// `out_len` must be a writable 4-byte-aligned guest `u32` slot.
#[no_mangle]
pub unsafe extern "C" fn selis_ocr_chunk_id(out_len: *mut u32) -> *mut u8 {
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
    // SAFETY: layout non-zero; freed via `selis_ocr_free` once.
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
pub unsafe extern "C" fn selis_ocr_free(ptr: *mut u8, len: u32) {
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

/// Probe: validates image bounds without decoding. Real OCR is the
/// Tesseract wasm module under the Tier-2 sandbox (SL-1.OCR.02) on native
/// and via `OffscreenCanvas` preprocessing on the web.
///
/// # Safety
/// `image_ptr` must point to `image_len` readable guest bytes when non-null.
#[no_mangle]
pub unsafe extern "C" fn selis_ocr_probe(image_ptr: *const u8, image_len: u32) -> u32 {
    if image_ptr.is_null() || image_len == 0 {
        return 0;
    }
    let Ok(len) = usize::try_from(image_len) else {
        return 0;
    };
    if len > MAX_OCR_BYTES {
        return 0;
    }
    image_len
}

/// Returns `1` when this chunk is available (native always `1`; web checks
/// the module was instantiated). The viewer uses this to decide whether to
/// offer OCR UI — never to block first paint.
#[no_mangle]
pub extern "C" fn selis_ocr_available() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunk_id_is_ocr() {
        assert_eq!(CHUNK_ID, "ocr");
    }
    #[test]
    fn probe_refuses_null() {
        // SAFETY: null with len 0 is the documented refusal probe.
        unsafe {
            assert_eq!(selis_ocr_probe(std::ptr::null(), 0), 0);
        }
    }
}
