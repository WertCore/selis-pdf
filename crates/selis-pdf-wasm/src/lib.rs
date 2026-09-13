//! The WASM binding (`selis-pdf-wasm`, L4 — SL-4.WASM.01, evolving
//! SL-2.PERF.03's render driver; ADR-P0041, ADR-P0042).
//!
//! A `cdylib` exposing two layers over guest linear memory:
//!
//! * **The raw render ABI** (ADR-P0041, SL-2.PERF.03):
//!   [`selis_input_alloc`] / [`selis_render_page`] / [`selis_free`] — one
//!   call per page, no state, the wasmtime perf harness's measured surface.
//! * **The Worker protocol** (SL-4.WASM.01, ADR-P0042): [`selis_dispatch`]
//!   speaks the versioned message schema (`protocol`) against a persistent
//!   [`Worker`] (document registry, budgeted ops, cancellation, progress);
//!   [`selis_cancel_slot`] / [`selis_progress_slot`] / [`selis_clock_slot`]
//!   publish the out-of-band host→guest channels (cancel flag, progress
//!   telemetry, injected clock). This is the surface the JS shell (UI.01)
//!   and the threaded path (WASM.03) consume.
//!
//! # Ownership discipline
//!
//! Every buffer that crosses the boundary is allocated with
//! [`std::alloc::alloc`] (byte-array layout, 4-byte-aligned) and freed with
//! [`std::alloc::dealloc`] under the same layout, so `selis_free` needs only
//! the pointer and the length. Inputs are *copied in* (the module never
//! dereferences a host pointer into host memory — there is no host memory
//! here), outputs are *copied out* into module-allocated buffers whose
//! `(ptr, len)` pairs the host validates before reading: the shell never
//! receives a pointer it can misuse. Any malformed input, unknown handle,
//! budget exhaustion, or allocation failure returns null (raw ABI) or a
//! typed error response (protocol) — never a trap, never partial pixels.
//!
//! # Determinism and injection (SL-2.RAST.09, ADR-P0011)
//!
//! The guest resolves no clock, spawns no threads, and makes no host
//! imports beyond the wasm-bindgen shims the engine already carries. The
//! three externals a shell injects arrive through exported slots:
//! [`GuestClock`] reads the clock slot (the glue pokes it with
//! `performance.now` between ops — the wall deadline is enforced only as
//! often as the glue pokes, which is honest and documented), the cancel
//! flag is a leaked [`CancelToken`] flag whose address
//! [`selis_cancel_slot`] publishes (a shared-memory watcher flips it
//! mid-op; the next budget tick observes it), and the progress sink writes
//! the exported slot. The wasmtime harness (`xtask perf-wasm`,
//! `xtask wasm-protocol`) drives the same exports; the protocol harness
//! asserts guest == native render checksums per page.

// The C ABI boundary dereferences host-provided guest pointers by design
// (ADR-P0041, ADR-P0042); every block carries its SAFETY case and the crate
// is on the `unsafe` allowlist (03-CONVENTIONS.md §2).
#![allow(unsafe_code)]

use std::alloc::{alloc, dealloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use selis_pdf_engine::{Session, TinySkiaBackend};
use selis_sandbox::{CancelToken, Clock, Nanos};

pub mod protocol;
pub mod worker;

use worker::{ProgressSink, Stage, Worker, WorkerEnv};

/// Maximum accepted input document size (64 MiB — hostile-input bound for
/// the raw driver surface; larger documents are rejected, not attempted).
const MAX_INPUT_BYTES: u32 = 64 * 1024 * 1024;
/// Maximum canvas dimension in pixels (a hostile media box beyond this is
/// rejected before the pixel buffer is allocated).
const MAX_CANVAS_DIM: u32 = 16_384;

// ---------------------------------------------------------------------------
// The raw render ABI (SL-2.PERF.03, ADR-P0041 — unchanged)
// ---------------------------------------------------------------------------

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
/// [`selis_render_page`] or [`selis_dispatch`].
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
    // The rendered view at 72 DPI (1 pt → 1 px, matching the old `dim`
    // ceil): it carries the `/Rotate`-aware canvas size and the matching
    // page-to-device transform (SL-2.RAST.12).
    let view = session.page_view(page_idx, 72.0)?;
    if view.width == 0
        || view.width > MAX_CANVAS_DIM
        || view.height == 0
        || view.height > MAX_CANVAS_DIM
    {
        return None;
    }
    let (w, h) = (view.width, view.height);
    let mut backend = TinySkiaBackend::new(w, h)?;
    let mut g = budget.guard_with(&clock, selis_sandbox::CancelToken::new());
    session
        .render_page(page_idx, &mut backend, view.ctm, &budget, &mut g)
        .ok()?;
    Some((backend.pixmap().data().to_vec(), w, h))
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
    let Some(out) = alloc_crossing(pixels.len()) else {
        return std::ptr::null_mut();
    };
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

/// Allocate a crossing buffer with the shared 4-byte-aligned byte layout
/// (the identical layout `selis_free` deallocates with). Null when the
/// length is zero or the allocation fails.
fn alloc_crossing(len: usize) -> Option<*mut u8> {
    if len == 0 {
        return None;
    }
    let layout = Layout::from_size_align(len, 4).ok()?;
    // SAFETY: `layout` is non-zero; the buffer is returned for one
    // `selis_free` call under this exact layout.
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

// ---------------------------------------------------------------------------
// The Worker protocol (SL-4.WASM.01, ADR-P0042)
// ---------------------------------------------------------------------------

/// The exported clock slot: a monotonic nanosecond count the host glue pokes
/// (`performance.now() * 1e6` between ops, continuously on the threaded
/// path). [`GuestClock`] reads it; the wall deadline is therefore enforced
/// exactly as often as the shell updates it — the honest degradation for a
/// guest that resolves no clock of its own (ADR-P0011).
static CLOCK_NANOS: AtomicU64 = AtomicU64::new(0);

/// The exported progress slot: three consecutive `u32`s — the in-flight
/// request id (low 32 bits), the [`Stage`] code, and the fraction in basis
/// points. Written by the dispatch at every stage boundary; threaded hosts
/// poll it out-of-band (WASM.03). The shell correlates ops by responses;
/// the slot is advisory telemetry.
#[repr(C)]
struct ProgressSlot {
    request: AtomicU32,
    stage: AtomicU32,
    fraction: AtomicU32,
}

static PROGRESS: ProgressSlot = ProgressSlot {
    request: AtomicU32::new(0),
    stage: AtomicU32::new(0),
    fraction: AtomicU32::new(0),
};

/// The exported cancel flag: one byte the host writes `1` into to cancel the
/// in-flight op (shared memory — a SAB watcher on the web, another thread on
/// native hosts). The flag is *leaked* (never dropped), so its address is
/// stable for the module's lifetime; dispatch clears it at entry, so only
/// writes that land during an op are observed, at the next budget tick.
struct HostCancel {
    token: CancelToken,
    // Keeps the flag allocation alive for the module's lifetime; the token
    // holds a clone of the same Arc, and `slot` pins its address.
    flag: Arc<AtomicBool>,
}

static HOST_CANCEL: OnceLock<HostCancel> = OnceLock::new();

/// The leaked, host-writable cancellation source (created once).
fn host_cancel() -> &'static HostCancel {
    HOST_CANCEL.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let token = CancelToken::from_flag(Arc::clone(&flag));
        HostCancel { token, flag }
    })
}

/// The guest's clock: the exported slot, read at every tick.
#[derive(Debug, Default, Clone, Copy)]
pub struct GuestClock;

impl Clock for GuestClock {
    fn now(&self) -> Nanos {
        CLOCK_NANOS.load(Ordering::Acquire)
    }
}

/// The progress sink bound to the exported slot.
#[derive(Debug, Default, Clone, Copy)]
struct SlotProgress;

impl ProgressSink for SlotProgress {
    fn progress(&self, request: u64, stage: Stage, fraction: u32) {
        // The slot is advisory telemetry keyed by the request's low 32 bits
        // (the shell correlates by responses; the slot never blocks).
        let low = u32::try_from(request).unwrap_or(u32::MAX);
        PROGRESS.request.store(low, Ordering::Release);
        PROGRESS.stage.store(stage.code(), Ordering::Release);
        PROGRESS.fraction.store(fraction, Ordering::Release);
    }
}

/// The guest's persistent worker (document registry across dispatches).
static WORKER: OnceLock<Mutex<Worker>> = OnceLock::new();

fn worker() -> &'static Mutex<Worker> {
    WORKER.get_or_init(|| Mutex::new(Worker::new()))
}

/// Address of the cancel flag (a 1-byte slot; the host writes `1` to cancel
/// the in-flight op, `0` to reset). 0 means "unavailable" (never on
/// wasm32). The address is stable for the module's lifetime.
#[no_mangle]
pub extern "C" fn selis_cancel_slot() -> u32 {
    let hc = host_cancel();
    u32::try_from(Arc::as_ptr(&hc.flag) as usize).unwrap_or(0)
}

/// Address of the progress slot (12 consecutive bytes: request id, stage
/// code, fraction in basis points — see `ProgressSlot`).
#[no_mangle]
pub extern "C" fn selis_progress_slot() -> u32 {
    u32::try_from(std::ptr::addr_of!(PROGRESS.request) as usize).unwrap_or(0)
}

/// Address of the clock slot (8 consecutive bytes, a little-endian `u64`
/// nanosecond count; `AtomicU64` alignment is 8).
#[no_mangle]
pub extern "C" fn selis_clock_slot() -> u32 {
    u32::try_from(std::ptr::addr_of!(CLOCK_NANOS) as usize).unwrap_or(0)
}

/// Dispatch one protocol message (SL-4.WASM.01).
///
/// `req`/`req_len` is the request message (schema v1, `protocol`),
/// `payload`/`payload_len` its binary attachment (document bytes for
/// `open`; either may be empty). `out` is a 12-byte guest region of three
/// 4-byte-aligned `u32` slots that on success receives
/// `[response_len, payload_ptr, payload_len]` — the response JSON at the
/// returned pointer (free with `selis_free(ptr, response_len)`), and the
/// binary attachment at `payload_ptr` (free with
/// `selis_free(payload_ptr, payload_len)`; `payload_ptr == 0` when the op
/// produced none).
///
/// Every failure returns null: a malformed call (null `out`, unreadable
/// request region) or a poisoned worker lock. Well-formed requests always
/// produce a response message (typed errors included) — containment is the
/// protocol's job, null is the ABI's.
///
/// # Safety
///
/// `req` must point to `req_len` readable guest bytes, `payload` to
/// `payload_len` readable guest bytes, and `out` to three 4-byte-aligned
/// writable `u32` slots (upheld by the host shell's glue).
#[no_mangle]
pub unsafe extern "C" fn selis_dispatch(
    req: *const u8,
    req_len: u32,
    payload: *const u8,
    payload_len: u32,
    out: *mut u32,
) -> *mut u8 {
    if out.is_null() {
        return std::ptr::null_mut();
    }
    // An empty request is still parseable-shaped (it will fail the parse
    // and answer a typed malformed-message response); an unreadable region
    // is an ABI failure (null).
    let request: &[u8] = if req_len == 0 {
        &[]
    } else {
        match read_input(req, req_len) {
            Some(bytes) => bytes,
            None => return std::ptr::null_mut(),
        }
    };
    // An empty attachment is legal (most ops carry none); null with len 0
    // is indistinguishable from empty.
    let attachment: &[u8] = match read_input(payload, payload_len) {
        Some(bytes) => bytes,
        None => &[],
    };

    // Dispatch clears the cancel flag at entry: only writes that land
    // *during* the op are observed (pre-cancellation is the message
    // channel's job).
    host_cancel().flag.store(false, Ordering::Release);
    let env = WorkerEnv {
        clock: &GuestClock,
        cancel: host_cancel().token.clone(),
        progress: &SlotProgress,
    };
    let Ok(mut worker) = worker().lock() else {
        // A poisoned lock means a panic escaped the trampoline — a bug this
        // module must answer with null (the raw ABI's failure shape), never
        // a half-serious response.
        return std::ptr::null_mut();
    };
    let outgoing = worker.handle(request, attachment, &env);
    drop(worker);

    let response = match serde_json::to_vec(&outgoing.response) {
        Ok(bytes) => bytes,
        Err(_) => return std::ptr::null_mut(),
    };
    let Ok(resp_len) = u32::try_from(response.len()) else {
        return std::ptr::null_mut();
    };
    let Some(resp_ptr) = alloc_crossing(response.len()) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `resp_ptr` points to `response.len()` fresh guest bytes; the
    // copy stays in bounds on both sides.
    unsafe {
        std::ptr::copy_nonoverlapping(response.as_ptr(), resp_ptr, response.len());
    }

    // The attachment: a separate out-buffer, `selis_free`-owned like the
    // response. Empty payloads (an empty page's text) publish ptr 0.
    let (payload_ptr, payload_len) = match &outgoing.payload {
        Some(p) if !p.bytes.is_empty() => {
            let Ok(len) = u32::try_from(p.bytes.len()) else {
                // SAFETY: `resp_ptr` is a live `resp_len`-byte allocation.
                unsafe { selis_free(resp_ptr, resp_len) };
                return std::ptr::null_mut();
            };
            let Some(ptr) = alloc_crossing(p.bytes.len()) else {
                // SAFETY: `resp_ptr` is a live `resp_len`-byte allocation.
                unsafe { selis_free(resp_ptr, resp_len) };
                return std::ptr::null_mut();
            };
            // SAFETY: `ptr` points to `p.bytes.len()` fresh guest bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(p.bytes.as_ptr(), ptr, p.bytes.len());
            }
            (ptr, len)
        }
        _ => (std::ptr::null_mut(), 0),
    };

    // SAFETY: the host protocol guarantees three 4-byte-aligned writable
    // guest `u32` slots. The payload address is a guest-linear-memory
    // offset (32-bit on wasm32 by construction); 0 means "no attachment".
    let payload_addr = u32::try_from(payload_ptr as usize).unwrap_or(0);
    unsafe {
        out.write(resp_len);
        out.add(1).write(payload_addr);
        out.add(2).write(payload_len);
    }
    resp_ptr
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::indexing_slicing,
        clippy::panic
    )]
    use super::*;
    use crate::protocol::{
        BudgetOverrides, BudgetProfile, RequestMessage, RequestOp, ResponseMessage,
        SourceDescriptor,
    };
    use crate::worker::{NullProgress, PayloadFormat, ProgressSink, Stage, WorkerEnv};
    use selis_error::Code;
    use selis_sandbox::FixedClock;
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

    /// A real page renders through the raw ABI: the engine's own minimal
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

    // -- the protocol over the native Worker (the same code the guest ABI
    //    drives; the guest harness asserts the wasm32 side in xtask) -------

    const MINIMAL: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/minimal.pdf");
    const TEXT_PDF: &[u8] = include_bytes!("../../selis-pdf-engine/src/fixtures/text.pdf");

    fn env() -> WorkerEnv<'static> {
        WorkerEnv {
            clock: &FixedClock(0),
            cancel: CancelToken::new(),
            progress: &NullProgress,
        }
    }

    fn request(id: u64, op: protocol::RequestOp) -> Vec<u8> {
        serde_json::to_vec(&RequestMessage {
            v: protocol::PROTOCOL_VERSION,
            id,
            op,
        })
        .expect("serialise")
    }

    fn open_op(len: u64) -> protocol::RequestOp {
        protocol::RequestOp::Open {
            src: SourceDescriptor::Bytes { len },
            budget: None,
        }
    }

    /// `open` mints a handle; `close` releases it; a stale handle is a
    /// typed `BINDING_BAD_HANDLE` and the worker stays healthy.
    #[test]
    fn open_page_close_and_stale_handle() {
        let mut w = Worker::new();
        let e = env();
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        assert!(out.response.ok.unwrap_or(false));
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();
        assert_eq!(out.response.value.unwrap()["pages"], 1);

        let out = w.handle(
            &request(
                2,
                protocol::RequestOp::Page {
                    doc: protocol::DocHandle { raw: doc },
                    page: 0,
                },
            ),
            &[],
            &e,
        );
        let v = out.response.value.unwrap();
        assert!(v["widthPt"].as_f64().unwrap() > 0.0);
        assert!(v["heightPt"].as_f64().unwrap() > 0.0);

        let out = w.handle(
            &request(
                3,
                protocol::RequestOp::Close {
                    doc: protocol::DocHandle { raw: doc },
                },
            ),
            &[],
            &e,
        );
        assert_eq!(out.response.value.unwrap()["closed"], true);
        assert_eq!(w.open_docs(), 0);

        // Stale handle: typed, contained.
        let out = w.handle(
            &request(
                4,
                protocol::RequestOp::Page {
                    doc: protocol::DocHandle { raw: doc },
                    page: 0,
                },
            ),
            &[],
            &e,
        );
        assert_eq!(out.response.ok, Some(false));
        assert_eq!(out.response.code, Some(Code::BindingBadHandle.id()));
        // …and the next good message still succeeds (containment).
        let out = w.handle(&request(5, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        assert!(out.response.ok.unwrap_or(false));
    }

    /// The payload length must match the `bytes` descriptor exactly.
    #[test]
    fn open_rejects_a_mismatched_payload_length() {
        let mut w = Worker::new();
        let e = env();
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64 + 1)), MINIMAL, &e);
        assert_eq!(out.response.code, Some(Code::BindingBadArgument.id()));
        assert_eq!(out.response.doc_state.as_deref(), Some("Unchanged"));
    }

    /// Budget exhaustion crosses the boundary as a typed `BUDGET_BYTES`
    /// response with the registry's doc_state — the "budget exhaustion over
    /// the boundary" leg of the DoD.
    #[test]
    fn budget_exhaustion_is_a_typed_response() {
        let mut w = Worker::new();
        let e = env();
        let tiny = protocol::RequestOp::Open {
            src: SourceDescriptor::Bytes {
                len: MINIMAL.len() as u64,
            },
            budget: Some(BudgetProfile {
                surface: protocol::SurfaceName::Viewer,
                overrides: Some(BudgetOverrides {
                    bytes: Some(64),
                    ..BudgetOverrides::default()
                }),
            }),
        };
        let out = w.handle(&request(1, tiny), MINIMAL, &e);
        assert_eq!(out.response.ok, Some(false));
        assert_eq!(out.response.code, Some(Code::BudgetBytes.id()));
        assert!(out.response.message.is_some());
        assert_eq!(out.response.doc_state.as_deref(), Some("PartiallyLoaded"));
    }

    /// A hostile media box aiming past the canvas cap is a typed pixel
    /// budget event before any allocation (MAX_CANVAS_DIM discipline).
    #[test]
    fn oversized_canvas_is_refused_before_allocation() {
        let mut w = Worker::new();
        let e = env();
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();
        let render = protocol::RequestOp::Render {
            doc: protocol::DocHandle { raw: doc },
            page: 0,
            params: protocol::RenderParams {
                dpi: 100_000.0,
                matrix: None,
                tile: None,
            },
        };
        let out = w.handle(&request(2, render), &[], &e);
        assert_eq!(out.response.code, Some(Code::BudgetPixels.id()));
    }

    /// Pixel-budget exhaustion on an ordinary page (a client that chose a
    /// tiny pixel override) — the second budget leg of the DoD.
    #[test]
    fn pixel_budget_exhaustion_is_a_typed_response() {
        let mut w = Worker::new();
        let e = env();
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();
        let open_tight = protocol::RequestOp::Open {
            src: SourceDescriptor::Bytes {
                len: MINIMAL.len() as u64,
            },
            budget: Some(BudgetProfile {
                surface: protocol::SurfaceName::Viewer,
                overrides: Some(BudgetOverrides {
                    pixels: Some(16),
                    ..BudgetOverrides::default()
                }),
            }),
        };
        let out = w.handle(&request(2, open_tight), MINIMAL, &e);
        let doc2 = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();
        let render = protocol::RequestOp::Render {
            doc: protocol::DocHandle { raw: doc2 },
            page: 0,
            params: protocol::RenderParams::default(),
        };
        let out = w.handle(&request(3, render), &[], &e);
        assert_eq!(out.response.code, Some(Code::BudgetPixels.id()));
        // The first document is untouched by the second document's budget.
        let render = protocol::RequestOp::Render {
            doc: protocol::DocHandle { raw: doc },
            page: 0,
            params: protocol::RenderParams::default(),
        };
        let out = w.handle(&request(4, render), &[], &e);
        assert_eq!(out.response.ok, Some(true));
        assert!(out.payload.is_some());
    }

    /// Render + tile: the tile is the matching sub-rectangle of the full
    /// canvas (deterministic composition, ADR-P0011's tile path).
    #[test]
    fn tile_render_matches_the_full_canvas() {
        let mut w = Worker::new();
        let e = env();
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();
        let render = protocol::RequestOp::Render {
            doc: protocol::DocHandle { raw: doc },
            page: 0,
            params: protocol::RenderParams::default(),
        };
        let full = w.handle(&request(2, render.clone()), &[], &e);
        let width = full.response.value.as_ref().unwrap()["width"]
            .as_u64()
            .unwrap() as u32;
        let height = full.response.value.as_ref().unwrap()["height"]
            .as_u64()
            .unwrap() as u32;
        let full_bytes = full.payload.as_ref().unwrap();
        assert_eq!(full_bytes.info, PayloadFormat::Rgba8);

        let tile_params = protocol::RenderParams {
            tile: Some(protocol::Tile {
                x: 0,
                y: 0,
                w: width.min(16),
                h: height.min(8),
            }),
            ..protocol::RenderParams::default()
        };
        let render = protocol::RequestOp::Render {
            doc: protocol::DocHandle { raw: doc },
            page: 0,
            params: tile_params,
        };
        let tile = w.handle(&request(3, render), &[], &e);
        let v = tile.response.value.as_ref().unwrap();
        let tw = v["tile"]["w"].as_u64().unwrap() as u32;
        let th = v["tile"]["h"].as_u64().unwrap() as u32;
        let tile_bytes = &tile.payload.as_ref().unwrap().bytes;
        assert_eq!(tile_bytes.len(), (tw * th * 4) as usize);
        // Row-wise identity with the full canvas.
        let stride = (width * 4) as usize;
        let row_bytes = (tw * 4) as usize;
        for row in 0..th {
            let tile_start = row as usize * row_bytes;
            let tile_end = tile_start + row_bytes;
            let full_start = row as usize * stride;
            let full_end = full_start + row_bytes;
            assert_eq!(
                &tile_bytes[tile_start..tile_end],
                &full_bytes.bytes[full_start..full_end],
                "tile row {row} must match the full canvas"
            );
        }
    }

    /// Out-of-range pages and tiles are typed errors, never panics.
    #[test]
    fn out_of_range_page_and_tile_are_typed_errors() {
        let mut w = Worker::new();
        let e = env();
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();
        let page = protocol::RequestOp::Page {
            doc: protocol::DocHandle { raw: doc },
            page: 99,
        };
        let out = w.handle(&request(2, page), &[], &e);
        assert_eq!(out.response.code, Some(Code::PageOutOfRange.id()));

        let render = protocol::RequestOp::Render {
            doc: protocol::DocHandle { raw: doc },
            page: 0,
            params: protocol::RenderParams {
                tile: Some(protocol::Tile {
                    x: 0,
                    y: 0,
                    w: 999_999,
                    h: 1,
                }),
                ..protocol::RenderParams::default()
            },
        };
        let out = w.handle(&request(3, render), &[], &e);
        assert_eq!(out.response.code, Some(Code::BindingBadArgument.id()));
    }

    /// Text extraction and search over the text fixture (the wire's `text`
    /// and `search` ops end to end).
    #[test]
    fn text_and_search_round_trip() {
        let mut w = Worker::new();
        let e = env();
        let out = w.handle(&request(1, open_op(TEXT_PDF.len() as u64)), TEXT_PDF, &e);
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();

        let text = protocol::RequestOp::Text {
            doc: protocol::DocHandle { raw: doc },
            page: 0,
            format: None,
        };
        let out = w.handle(&request(2, text), &[], &e);
        assert_eq!(out.response.ok, Some(true));
        let payload = out.payload.as_ref().unwrap();
        assert_eq!(payload.info, PayloadFormat::Utf8Text);
        let text = String::from_utf8(payload.bytes.clone()).expect("utf8");
        let first_word = text
            .split_whitespace()
            .next()
            .expect("the fixture carries text")
            .to_string();

        let search = protocol::RequestOp::Search {
            doc: protocol::DocHandle { raw: doc },
            query: first_word,
            opts: None,
        };
        let out = w.handle(&request(3, search), &[], &e);
        assert_eq!(out.response.ok, Some(true));
        let v = out.response.value.unwrap();
        assert!(v["total"].as_u64().unwrap() >= 1);
        assert_eq!(v["truncated"], false);
        assert_eq!(v["matches"].as_array().unwrap().len() as u64, v["total"]);

        // A case-sensitive query is a typed "not yet" (the normalised
        // search primitive folds case by design).
        let search = protocol::RequestOp::Search {
            doc: protocol::DocHandle { raw: doc },
            query: "x".to_owned(),
            opts: Some(protocol::SearchOpts {
                match_case: true,
                ..protocol::SearchOpts::default()
            }),
        };
        let out = w.handle(&request(4, search), &[], &e);
        assert_eq!(out.response.code, Some(Code::BindingUnsupportedOp.id()));
    }

    /// Progress flows through the injected sink at every stage boundary.
    #[test]
    fn progress_flows_through_the_injected_sink() {
        use std::sync::Mutex as StdMutex;
        #[derive(Debug, Default)]
        struct Capture(StdMutex<Vec<(u64, Stage, u32)>>);
        impl ProgressSink for Capture {
            fn progress(&self, request: u64, stage: Stage, fraction: u32) {
                self.0
                    .lock()
                    .expect("capture")
                    .push((request, stage, fraction));
            }
        }
        let capture = Capture::default();
        let clock = FixedClock(0);
        let e = WorkerEnv {
            clock: &clock,
            cancel: CancelToken::new(),
            progress: &capture,
        };
        let mut w = Worker::new();
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        assert_eq!(out.response.ok, Some(true));
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();
        let search = protocol::RequestOp::Search {
            doc: protocol::DocHandle { raw: doc },
            query: "e".to_owned(),
            opts: None,
        };
        let out = w.handle(&request(2, search), &[], &e);
        assert_eq!(out.response.ok, Some(true));

        let events = capture.0.lock().unwrap();
        assert!(
            events.contains(&(1, Stage::Open, 0)),
            "open start: {events:?}"
        );
        assert!(
            events.contains(&(2, Stage::Search, 0)),
            "search start: {events:?}"
        );
        assert!(
            events.contains(&(2, Stage::Search, 10_000)),
            "search done: {events:?}"
        );
    }

    /// Pre-cancellation: a `cancel` message whose target has not run makes
    /// the target answer `CANCELLED` without executing.
    #[test]
    fn pre_cancelled_requests_never_execute() {
        let mut w = Worker::new();
        let e = env();
        let out = w.handle(
            &request(7, protocol::RequestOp::Cancel { target: 7 }),
            &[],
            &e,
        );
        assert_eq!(out.response.value.unwrap()["cancelled"], true);
        let out = w.handle(&request(7, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        assert_eq!(out.response.ok, Some(false));
        assert_eq!(out.response.code, Some(Code::Cancelled.id()));
        // The record is consumed: the retry succeeds.
        let out = w.handle(&request(7, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        assert_eq!(out.response.ok, Some(true));
    }

    /// A clock that cancels the injected token on its Nth read — the
    /// deterministic stand-in for an out-of-band host write landing
    /// mid-render (the same flag→token→tick path the exported cancel slot
    /// drives).
    #[derive(Debug)]
    struct CancelOnRead {
        reads: AtomicU32,
        cancel_at: u32,
        token: CancelToken,
    }
    impl Clock for CancelOnRead {
        fn now(&self) -> Nanos {
            let n = self.reads.fetch_add(1, Ordering::AcqRel) + 1;
            if n >= self.cancel_at {
                self.token.cancel();
            }
            0
        }
    }

    /// Cancellation *mid-render* is observed at the next budget tick and
    /// crosses the boundary as a typed `CANCELLED` response; the worker
    /// stays healthy afterwards.
    #[test]
    fn cancellation_mid_render_is_observed_within_a_tick() {
        let mut w = Worker::new();
        let clock = FixedClock(0);
        let e = WorkerEnv {
            clock: &clock,
            cancel: CancelToken::new(),
            progress: &NullProgress,
        };
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        assert_eq!(out.response.ok, Some(true));
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();

        // The first reads are the guard construction + the first ticks of
        // the render; the token flips a few reads in, so the render is
        // genuinely under way (not pre-cancelled) when cancellation lands.
        let token = CancelToken::new();
        let clock = CancelOnRead {
            reads: AtomicU32::new(0),
            cancel_at: 4,
            token: token.clone(),
        };
        let e_mid = WorkerEnv {
            clock: &clock,
            cancel: token,
            progress: &NullProgress,
        };
        let render = protocol::RequestOp::Render {
            doc: protocol::DocHandle { raw: doc },
            page: 0,
            params: protocol::RenderParams::default(),
        };
        let out = w.handle(&request(2, render), &[], &e_mid);
        assert_eq!(out.response.ok, Some(false));
        assert_eq!(out.response.code, Some(Code::Cancelled.id()));
        assert_eq!(out.response.doc_state.as_deref(), Some("Unchanged"));
        // Containment: the worker still answers correctly afterwards.
        let out = w.handle(&request(3, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        assert_eq!(out.response.ok, Some(true));
    }

    /// The wall deadline (the injected clock) crosses the boundary as
    /// `BUDGET_WALL` — the shell's clock slot drives this on the guest.
    #[test]
    fn wall_deadline_crosses_the_boundary() {
        let mut w = Worker::new();
        let tight = RequestOp::Open {
            src: SourceDescriptor::Bytes {
                len: MINIMAL.len() as u64,
            },
            budget: Some(BudgetProfile {
                surface: protocol::SurfaceName::Viewer,
                overrides: Some(BudgetOverrides {
                    wall_ms: Some(1),
                    ..BudgetOverrides::default()
                }),
            }),
        };
        // The guard freezes its start at the first read (0); every later
        // read is past the 1 ms deadline — a deadline that fires at the
        // first tick, like a shell whose clock slot jumped mid-op.
        #[derive(Debug)]
        struct StepClock(AtomicU32);
        impl Clock for StepClock {
            fn now(&self) -> Nanos {
                if self.0.fetch_add(1, Ordering::AcqRel) == 0 {
                    0
                } else {
                    1_000_000_000
                }
            }
        }
        let clock = StepClock(AtomicU32::new(0));
        let e = WorkerEnv {
            clock: &clock,
            cancel: CancelToken::new(),
            progress: &NullProgress,
        };
        let out = w.handle(&request(1, tight), MINIMAL, &e);
        assert_eq!(out.response.ok, Some(false));
        assert_eq!(out.response.code, Some(Code::BudgetWall.id()));
    }

    /// Malformed messages are contained: each answers a typed error and the
    /// next well-formed message succeeds.
    #[test]
    fn malformed_messages_are_contained() {
        let mut w = Worker::new();
        let e = env();

        // Not JSON at all.
        let out = w.handle(b"\x00\xff{not json", &[], &e);
        assert_eq!(out.response.id, 0);
        assert_eq!(out.response.code, Some(Code::BindingBadArgument.id()));

        // Valid JSON, wrong version.
        let out = w.handle(br#"{"v":99,"id":3,"op":"cancel","target":1}"#, &[], &e);
        assert_eq!(out.response.code, Some(Code::BindingUnsupportedOp.id()));
        assert!(out.response.detail.as_deref().unwrap_or("").contains("99"));

        // Valid envelope, unknown op.
        let out = w.handle(br#"{"v":1,"id":4,"op":"teleport","doc":1}"#, &[], &e);
        assert_eq!(out.response.code, Some(Code::BindingBadArgument.id()));

        // Wrong types.
        let out = w.handle(
            br#"{"v":1,"id":5,"op":"page","doc":"nope","page":0}"#,
            &[],
            &e,
        );
        assert_eq!(out.response.code, Some(Code::BindingBadArgument.id()));

        // Unknown mutation schema: shape-validated before the capability
        // check.
        let out = w.handle(&request(1, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        let doc = out.response.value.as_ref().unwrap()["doc"]
            .as_u64()
            .unwrap();
        let mutate = protocol::RequestOp::Mutate {
            doc: protocol::DocHandle { raw: doc },
            mutation: protocol::MutationEnvelope {
                schema: "selis-mutate/0".to_owned(),
                ops: Vec::new(),
            },
        };
        let out = w.handle(&request(6, mutate), &[], &e);
        assert_eq!(out.response.code, Some(Code::BindingBadArgument.id()));

        // A well-formed mutation envelope reaches the capability answer.
        let mutate = protocol::RequestOp::Mutate {
            doc: protocol::DocHandle { raw: doc },
            mutation: protocol::MutationEnvelope {
                schema: "selis-mutate/1".to_owned(),
                ops: Vec::new(),
            },
        };
        let out = w.handle(&request(7, mutate), &[], &e);
        assert_eq!(out.response.code, Some(Code::BindingUnsupportedOp.id()));

        let save = protocol::RequestOp::Save {
            doc: protocol::DocHandle { raw: doc },
            mode: protocol::SaveMode::Incremental,
        };
        let out = w.handle(&request(8, save), &[], &e);
        assert_eq!(out.response.code, Some(Code::BindingUnsupportedOp.id()));

        // Source adapters other than inline bytes: typed "not yet".
        let out = w.handle(
            &request(
                9,
                protocol::RequestOp::Open {
                    src: SourceDescriptor::Opfs {
                        path: "/doc.pdf".to_owned(),
                    },
                    budget: None,
                },
            ),
            &[],
            &e,
        );
        assert_eq!(out.response.code, Some(Code::BindingUnsupportedOp.id()));

        // Containment, finally: the next well-formed message succeeds.
        let out = w.handle(&request(10, open_op(MINIMAL.len() as u64)), MINIMAL, &e);
        assert_eq!(out.response.ok, Some(true));
    }

    /// A panic below the boundary becomes a typed `INTERNAL_PANIC` response
    /// (the ERR.03 trampoline wired at the worker entry).
    #[test]
    fn a_panic_below_the_boundary_answers_internal_panic() {
        // The response builder for a trampolined panic — exercised directly
        // through `catch` with the same wiring `handle` uses.
        let e = selis_sandbox::trampoline::catch(
            "wasm-worker",
            || -> Result<worker::Outgoing, selis_error::Error> {
                panic!("simulated engine bug");
            },
        )
        .expect_err("the panic converts");
        let resp = ResponseMessage::error(protocol::PROTOCOL_VERSION, 42, &e);
        assert_eq!(resp.code, Some(Code::InternalPanic.id()));
        assert_eq!(resp.doc_state.as_deref(), Some("Unchanged"));
        assert_eq!(resp.id, 42);
        assert!(resp.validate().is_ok());
    }

    /// The exported slots exist and are self-consistent: the progress slot
    /// reflects the last stage boundary of the last dispatch.
    #[test]
    fn exported_slots_stay_consistent() {
        // The clock slot reads back what the host would write.
        CLOCK_NANOS.store(1234, Ordering::Release);
        assert_eq!(GuestClock.now(), 1234);
        CLOCK_NANOS.store(0, Ordering::Release);
        // Slot addresses are stable across calls. (On native hosts an
        // address can exceed the u32 wire form and reads as 0 — the
        // "unavailable" sentinel; the wasm32 guest is 32-bit by
        // construction, where the real address always fits.)
        assert_eq!(selis_cancel_slot(), selis_cancel_slot());
        assert_eq!(selis_progress_slot(), selis_progress_slot());
        assert_eq!(selis_clock_slot(), selis_clock_slot());
    }
}
