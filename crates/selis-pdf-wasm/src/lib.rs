//! The WASM render driver (SL-2.PERF.03, ADR-P0041).
//!
//! A `cdylib` exposing page rendering over a C ABI for WASM shells: the
//! wasmtime-driven perf harness (`xtask perf-wasm`) loads the compiled
//! `.wasm` directly, and browsers use the same exports through the wasm
//! shell (Phase 5). The design keeps the JS/wasmtime boundary crossings at
//! exactly two per page — input bytes in, pixels out — with the display
//! list, tiling, and rasterisation all inside the module (the tile path
//! never crosses the boundary).
//!
//! Ownership discipline: every buffer that crosses the boundary is allocated
//! with [`std::alloc::alloc`] (byte-array layout, 4-byte-aligned) and freed
//! with [`std::alloc::dealloc`] under the same layout, so `selis_free` needs
//! only the pointer and the length. The render call copies the finished pixmap
//! into a fresh out-buffer (that copy is the honest boundary cost, included
//! in the measured numbers). Any malformed input, unknown page, allocation
//! failure, or budget exhaustion returns null — never a trap, never partial
//! pixels (a hostile page with an absurd media box is rejected before the
//! canvas is allocated).
//!
//! Determinism (SL-2.RAST.09): the driver resolves no clock (the guest has
//! none — [`selis_sandbox::FixedClock`] stands in), spawns no threads, and
//! holds no cross-call state. The `xtask perf-wasm` harness asserts the
//! guest pixmap checksum equals the native one per page.

// The C ABI boundary dereferences host-provided guest pointers by design
// (ADR-P0041); every block carries its SAFETY case and the crate is on the
// `unsafe` allowlist (03-CONVENTIONS.md §2).
#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};

use selis_pdf_engine::{Session, TinySkiaBackend};

/// Maximum accepted input document size (64 MiB — hostile-input bound for
/// the driver surface; larger documents are rejected, not attempted).
const MAX_INPUT_BYTES: u32 = 64 * 1024 * 1024;
/// Maximum canvas dimension in pixels (a hostile media box beyond this is
/// rejected before the pixel buffer is allocated).
const MAX_CANVAS_DIM: u32 = 16_384;

/// Allocate `len` bytes of guest memory for the input document (or for the
/// three out-word slots — every allocation is 4-byte-aligned, so out-word
/// slots are always valid `u32` targets).
///
/// Returns null when `len` is zero, exceeds [`MAX_INPUT_BYTES`], or the
/// allocation fails. Free with [`selis_free`].
/// The returned buffer must be freed exactly once with [`selis_free`].
#[no_mangle]
pub extern "C" fn selis_input_alloc(len: u32) -> *mut u8 {
    if len == 0 || len > MAX_INPUT_BYTES {
        return std::ptr::null_mut();
    }
    let bytes = match usize::try_from(len) {
        Ok(bytes) => bytes,
        Err(_) => return std::ptr::null_mut(),
    };
    let layout = match Layout::from_size_align(bytes, 4) {
        Ok(layout) => layout,
        Err(_) => return std::ptr::null_mut(),
    };
    // SAFETY: `layout` has non-zero size (len > 0 checked above); the
    // pointer is freed with `selis_free` under the identical layout, exactly
    // once, by the caller upholding the documented protocol.
    unsafe { alloc(layout) }
}

/// Free a buffer allocated by [`selis_input_alloc`] or returned from
/// [`selis_render_page`].
///
/// # Safety
///
/// `ptr` must be a buffer previously returned by this module with the same
/// `len`, not yet freed (upheld by the host shell). Null pointers are
/// ignored.
#[no_mangle]
pub unsafe extern "C" fn selis_free(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let bytes = match usize::try_from(len) {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    let layout = match Layout::array::<u8>(bytes) {
        Ok(layout) => layout,
        Err(_) => return,
    };
    // SAFETY: by the documented protocol `ptr`/`len` identify a live module
    // allocation with this exact layout; deallocating once is sound.
    unsafe {
        dealloc(ptr, layout);
    }
}

/// Render one page to an RGBA8 pixmap at 72 DPI (1 pt = 1 px — the same
/// native scale the `perf-render` harness measures, so the numbers compare).
///
/// `pdf`/`pdf_len` is the document, `page` the 0-based page number.
/// On success writes the canvas width, height, and pixel-byte length to
/// `out_w`/`out_h`/`out_len` (4-byte-aligned guest pointers) and returns the
/// pixel buffer (free with [`selis_free`]). Returns null on any failure:
/// null input, unknown page, missing media box, oversized canvas, budget
/// exhaustion, or an unrenderable page.
///
/// # Safety
///
/// `pdf` must point to `pdf_len` readable guest bytes; `out_w`, `out_h`,
/// and `out_len` must be 4-byte-aligned guest pointers to writable `u32`s
/// (upheld by the host shell).
#[no_mangle]
pub unsafe extern "C" fn selis_render_page(
    pdf: *const u8,
    pdf_len: u32,
    page: u32,
    out_w: *mut u32,
    out_h: *mut u32,
    out_len: *mut u32,
) -> *mut u8 {
    let pdf = match read_input(pdf, pdf_len) {
        Some(pdf) => pdf,
        None => return std::ptr::null_mut(),
    };
    let (pixels, w, h) = match render(pdf, page) {
        Some(rendered) => rendered,
        None => return std::ptr::null_mut(),
    };
    write_out(out_w, out_h, out_len, &pixels, w, h)
}

/// Borrow the input document as a slice (null unless valid).
fn read_input(pdf: *const u8, pdf_len: u32) -> Option<&'static [u8]> {
    if pdf.is_null() || pdf_len == 0 || pdf_len > MAX_INPUT_BYTES {
        return None;
    }
    // SAFETY: the host protocol guarantees `pdf` points to `pdf_len` live
    // guest bytes for the duration of this call; the borrow never outlives
    // the call (the pixmap is computed before return).
    let bytes = usize::try_from(pdf_len).ok()?;
    unsafe { Some(std::slice::from_raw_parts(pdf, bytes)) }
}

/// Render `page` of `pdf` to `(pixels, width, height)` (`None` on any
/// failure — the null propagates to the caller, never a trap).
fn render(pdf: &[u8], page: u32) -> Option<(Vec<u8>, u32, u32)> {
    // The guest has no clock: the fixed clock stands in (same as the native
    // harness, so the comparison is apples-to-apples). The viewer profile
    // matches a real shell's budget surface.
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let clock = selis_sandbox::FixedClock(0);
    let page_idx = usize::try_from(page).ok()?;
    let session = Session::open(pdf.to_vec(), &budget, &clock).ok()?;
    if page_idx >= session.len() {
        return None;
    }
    let (w_pt, h_pt) = session.page_size(page_idx)?;
    let (w, h) = (dim(w_pt)?, dim(h_pt)?);
    let mut backend = TinySkiaBackend::new(w, h)?;
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    session
        .render_page(page_idx, &mut backend, &budget, &mut g)
        .ok()?;
    Some((backend.pixmap().data().to_vec(), w, h))
}

/// A finite, positive f64 dimension as a u32 canvas size (ceil), rejecting
/// absurd canvases before the pixel buffer exists.
fn dim(v: f64) -> Option<u32> {
    if !v.is_finite() || v <= 0.0 || v > f64::from(MAX_CANVAS_DIM) {
        return None;
    }
    let c = v.ceil();
    if c < 1.0 || c > f64::from(MAX_CANVAS_DIM) {
        return None;
    }
    // c is in [1, MAX_CANVAS_DIM]: the cast is exact, and std offers no
    // fallible f64→u32 conversion, so the pedantic cast lints are allowed
    // here with this justification.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(c as u32)
}

/// Copy the pixmap into a fresh out-buffer and publish its geometry.
/// Returns null when any out-pointer is null or the copy cannot allocate.
fn write_out(
    out_w: *mut u32,
    out_h: *mut u32,
    out_len: *mut u32,
    pixels: &[u8],
    w: u32,
    h: u32,
) -> *mut u8 {
    if out_w.is_null() || out_h.is_null() || out_len.is_null() {
        return std::ptr::null_mut();
    }
    let len = match u32::try_from(pixels.len()) {
        Ok(len) => len,
        Err(_) => return std::ptr::null_mut(),
    };
    // The identical layout `selis_free` deallocates with (see its SAFETY
    // case) — every crossing buffer shares it.
    let layout = match Layout::from_size_align(pixels.len(), 4) {
        Ok(layout) => layout,
        Err(_) => return std::ptr::null_mut(),
    };
    // SAFETY: `layout` is non-zero (a rendered page always has pixels);
    // on success the buffer is returned for `selis_free` under this layout.
    let out = unsafe { alloc(layout) };
    if out.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `out` points to `pixels.len()` fresh guest bytes; the copy
    // stays in bounds on both sides.
    unsafe {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), out, pixels.len());
    }
    // SAFETY: the host protocol guarantees three 4-byte-aligned writable
    // guest `u32` slots.
    unsafe {
        out_w.write(w);
        out_h.write(h);
        out_len.write(len);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )]
    use super::*;

    /// The ABI surface works natively too: allocate, reject garbage, free.
    #[test]
    fn alloc_reject_free_roundtrip() {
        assert!(selis_input_alloc(0).is_null());
        assert!(selis_input_alloc(MAX_INPUT_BYTES + 1).is_null());
        let ptr = selis_input_alloc(1024);
        assert!(!ptr.is_null());
        // SAFETY: `ptr` is a live 1024-byte module allocation from above,
        // freed exactly once here (and the null call below is a no-op).
        unsafe {
            selis_free(ptr, 1024);
            selis_free(std::ptr::null_mut(), 1024);
        }
        // Garbage is not a document: null, never a trap.
        let mut w = 0u32;
        let mut h = 0u32;
        let mut len = 0u32;
        let garbage = [0x25u8, 0x50, 0x44, 0x46, 0x2d, 0x39, 0x39, 0x39];
        // SAFETY: `garbage` outlives the call; the out-words are live
        // aligned `u32` slots; the null result needs no freeing.
        let out = unsafe {
            selis_render_page(
                garbage.as_ptr(),
                garbage.len() as u32,
                0,
                &mut w,
                &mut h,
                &mut len,
            )
        };
        assert!(out.is_null());
    }

    /// A real page renders through the ABI: the engine's own minimal
    /// fixture exercises open → render → out-copy end to end.
    #[test]
    fn minimal_fixture_renders_through_the_abi() {
        let pdf = include_bytes!("../../selis-pdf-engine/src/fixtures/minimal.pdf");
        let mut w = 0u32;
        let mut h = 0u32;
        let mut len = 0u32;
        // SAFETY: `pdf` (an embedded fixture) outlives the call; the
        // out-words are live aligned `u32` slots; `out` is a live module
        // allocation of `len` bytes, freed exactly once below.
        unsafe {
            let out =
                selis_render_page(pdf.as_ptr(), pdf.len() as u32, 0, &mut w, &mut h, &mut len);
            assert!(!out.is_null());
            assert!(w > 0 && h > 0);
            assert_eq!(len, w * h * 4);
            let pixels = std::slice::from_raw_parts(out, len as usize);
            assert_eq!(pixels.len(), len as usize);
            selis_free(out, len);
        }
    }
}
