//! CJK lazy chunk (SL-4.WASM.02 + SL-4.WASM.07).
//!
//! Separate `cdylib` + lazy font assets. The core viewer ships only the
//! Latin/Greek/Cyrillic base font coverage; CJK ranges are fetched on demand
//! as `cjk/<range>.ttf` payloads plus the `cjk/manifest.json` built by
//! `xtask cjk-build` (SL-3.FONT.10). This module proves the chunk is a
//! separate binary and exposes the range-availability probe the loader polls.

#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};

/// Chunk identifier (stable, matches the manifest).
pub const CHUNK_ID: &str = "cjk";
/// Chunk version.
pub const CHUNK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// CJK Unicode ranges covered lazily (mirrors `xtask/src/cjk_assets.rs`).
/// Each range's font payload is a separate `fetch` — the chunk never
/// downloads the whole CJK font at once.
pub const CJK_RANGES: &[(&str, u32, u32)] = &[
    ("cjk-core", 0x4E00, 0x9FFF),   // CJK Unified Ideographs (common)
    ("cjk-ext-a", 0x3400, 0x4DBF),  // Extension A
    ("cjk-compat", 0xF900, 0xFAFF), // Compatibility Ideographs
    ("hangul", 0xAC00, 0xD7AF),     // Hangul Syllables
    ("kana", 0x3040, 0x30FF),       // Hiragana + Katakana
];

/// Returns the chunk id as guest memory (free with `selis_cjk_free`).
///
/// # Safety
/// `out_len` must be a writable 4-byte-aligned guest `u32` slot.
#[no_mangle]
pub unsafe extern "C" fn selis_cjk_chunk_id(out_len: *mut u32) -> *mut u8 {
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
pub unsafe extern "C" fn selis_cjk_free(ptr: *mut u8, len: u32) {
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

/// Returns `1` if `codepoint` lies in a CJK range covered by this chunk,
/// `0` otherwise. The viewer uses this to decide whether to fetch a CJK
/// payload — pure documents never trigger the fetch.
#[no_mangle]
pub extern "C" fn selis_cjk_covers(codepoint: u32) -> u32 {
    for (_, lo, hi) in CJK_RANGES {
        if codepoint >= *lo && codepoint <= *hi {
            return 1;
        }
    }
    0
}

/// Number of lazily-fetched CJK ranges (the manifest's chunk count).
#[no_mangle]
pub extern "C" fn selis_cjk_range_count() -> u32 {
    u32::try_from(CJK_RANGES.len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cjk_id_is_cjk() {
        assert_eq!(CHUNK_ID, "cjk");
    }
    #[test]
    fn covers_cjk_and_not_latin() {
        assert_eq!(selis_cjk_covers(0x4E00), 1);
        assert_eq!(selis_cjk_covers(0x0041), 0);
        assert_eq!(selis_cjk_range_count(), 5);
    }
}
