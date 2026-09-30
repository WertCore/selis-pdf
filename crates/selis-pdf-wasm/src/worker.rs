//! The Worker entry point (SL-4.WASM.01): one message in, one response out.
//!
//! [`Worker::handle`] is the seam the JS shell (and the wasmtime conformance
//! harness, and the fuzz target) drives. It is deliberately boring:
//!
//! 1. **Parse** the request envelope; anything unparseable answers
//!    `BINDING_BAD_ARGUMENT` with id 0 and changes no state.
//! 2. **Validate** the schema version; a mismatch answers
//!    `BINDING_UNSUPPORTED_OP` (a newer shell must degrade into typed
//!    errors, never silent misbehaviour).
//! 3. **Route** the op to the engine â€” every engine call runs under the
//!    document's [`Budget`](selis_sandbox::Budget) (ADR-P0006), every long
//!    op ticks against the injected clock and cancel token (ADR-P0004), and
//!    the whole route sits inside the ERR.03 panic trampoline, so even a bug
//!    below the boundary answers as a typed `INTERNAL_PANIC` response
//!    (ADR-P0017: code path + panic site, never document bytes).
//!
//! # Handles, not structures
//!
//! Documents live in the guest; the shell holds an opaque [`DocHandle`]
//! (minted on `open`, released on `close`). The registry is bounded; a stale
//! handle is a typed `BINDING_BAD_HANDLE`, never a panic and never a
//! wrong-document operation.
//!
//! # Cancellation (the two channels)
//!
//! * **Pre-cancel (message channel).** A `cancel` message whose target has
//!   not been dispatched yet is recorded; when that request arrives it
//!   answers `CANCELLED` without executing. This is the channel a
//!   single-threaded Worker can actually deliver (a message cannot arrive
//!   mid-op there).
//! * **In-flight (out-of-band).** The host writes the exported cancel slot
//!   (shared memory â€” a SAB watcher on the web, another thread natively);
//!   the op observes it at its next budget tick. The harness proves the
//!   slotâ†’tokenâ†’tickâ†’`CANCELLED` path both natively (deterministic, via the
//!   injected clock) and over the guest ABI.
//!
//! # Progress
//!
//! Every stage boundary writes through the injected [`ProgressSink`] â€” the
//! guest binds that to the exported progress slot (pollable out-of-band by
//! threaded hosts); native tests capture it directly. The wire `progress`
//! response shape is reserved for the threaded shell path (WASM.03).
//!
//! # What v1 does not do (yet, honestly)
//!
//! * The blob/OPFS/FSA source adapters (SL-4.WASM.05) drain through their own
//!   `DocSource`, so the `DocSource` contract is the path every open takes.
//! * A remote document (SL-4.WASM.06) is *not* one of them: it has no bytes
//!   to hand over, so it is fetched by the `rangeOpen` / `rangeChunk` /
//!   `rangeClose` exchange, where the guest names each range and judges each
//!   response. `open` with a `http-range` descriptor answers
//!   `BINDING_UNSUPPORTED_OP` pointing there.
//! * `mutate`/`save` validate their versioned envelopes and answer
//!   `BINDING_UNSUPPORTED_OP` until Phase 5.
//! * `search` with `matchCase` answers `BINDING_UNSUPPORTED_OP` until the
//!   case-preserving search primitive exists.
//!
//! # Budget
//!
//! Every op builds a fresh [`BudgetGuard`](selis_sandbox::BudgetGuard) from
//! the document's budget profile (chosen by the client at `open` — the
//! engine never picks its own limits, ADR-P0006) with the *injected* clock
//! and cancel token. Exhaustion and cancellation cross the boundary as
//! typed `BUDGET_*`/`CANCELLED` responses carrying the registry's
//! `doc_state`.
//!
//! # Memory (SL-4.WASM.04)
//!
//! The worker is the guest side of the memory strategy (`crate::memory`):
//! the claimed inline length is validated against the 4 GiB wasm ceiling,
//! the document byte budget, the tab heap cap, and the inline bound
//! *before* the payload is touched; `close` releases the `Session` and
//! subtracts its length from the live tally; `memoryStats` reports the
//! live/peak tallies so the JS shell can evict its caches; `memoryPressure`
//! drops the pre-cancel queue (the only guest-side queue that grows without
//! a document). See `crate::memory` for the growth policy and the
//! `Budget.bytes`/`wall` ↔ JS-cap mapping.
//!
//! # Malformed Input
//!
//! Malformed messages (bad JSON, wrong types, unknown versions, mismatched
//! payload lengths, out-of-range tiles, unknown handles) are contained:
//! each answers a typed error response, leaves the registry untouched, and
//! the next well-formed message succeeds (asserted by tests and by the fuzz
//! target, which runs this entry over arbitrary bytes).

use std::collections::BTreeMap;

use selis_error::{err, Code, Error, Result};
use selis_geom::Matrix;
use selis_io::Availability;
use selis_pdf_engine::{LazyFonts, Session, TinySkiaBackend};
use selis_pdf_text::{LineWithMcid, TextLine};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, Clock, Resource, Surface};

use crate::cjkchunk::{
    ChunkClaim, ChunkReport as CjkReport, ChunkStep as CjkChunkStep, CjkChunkLoader, CjkRequest,
};
use crate::fallbackchunk::{FaceClaim, FaceReport, FaceRequest, FaceStep, FallbackFaceLoader};
use crate::httprange::{ChunkReport, ChunkStep, HttpRangeDriver, RangeRequest};
use crate::memory::{MemoryStats, JS_DEFAULT_CAP_BYTES, WASM_MAX_BYTES};
use crate::protocol::{
    BudgetOverrides, BudgetProfile, DocHandle, MutationEnvelope, PageRange, RenderParams,
    RequestMessage, RequestOp, ResponseMessage, SaveMode, SearchOpts, SourceDescriptor,
    SurfaceName, TextFormat, MAX_SEARCH_MATCHES, PROTOCOL_VERSION,
};

/// Hard ceiling on one inline document (the same hostile-input bound the
/// raw render ABI applies; larger sources arrive as streaming adapters).
/// Mirrored in `crate::memory::PROTOCOL_MAX_INPUT_BYTES` — the memory
/// strategy is the normative owner, this stays for wire-compatible callers.
pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Hard ceiling on any canvas dimension in device pixels (a hostile media
/// box is rejected before the pixel buffer exists).
/// Mirrored in `crate::memory::PROTOCOL_MAX_CANVAS_DIM`.
pub const MAX_CANVAS_DIM: u32 = 16_384;

/// Maximum simultaneously open documents (the guest registry bound; further
/// opens answer `BUDGET_BYTES` until one is closed).
pub const MAX_OPEN_DOCS: u64 = 64;

/// Maximum simultaneously open range transfers (SL-4.WASM.06). Small on
/// purpose: a transfer holds a partially-assembled document, so this is a
/// memory bound wearing a registry's clothes, and a hostile client must not be
/// able to open transfers without limit.
pub const MAX_OPEN_TRANSFERS: usize = 8;

/// Maximum recorded pre-cancellations (FIFO eviction of stale targets).
const MAX_PRECANCELLED: usize = 256;

/// The default match cap when the request does not name one.
const DEFAULT_SEARCH_MATCHES: u32 = 1_000;

// ---------------------------------------------------------------------------
// The injected environment (the boundary is where sources/clocks arrive)
// ---------------------------------------------------------------------------

/// A long-op stage, as reported through [`ProgressSink`] and the exported
/// progress slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// No op running (slot initial state).
    Idle,
    /// Opening a document.
    Open,
    /// Rendering a page.
    Render,
    /// Extracting text.
    Text,
    /// Searching.
    Search,
    /// Walking the document's navigation structures (outline, links, labels,
    /// destinations). A separate stage from `Search` because a hostile outline
    /// is a whole-tree walk and a shell watching the progress slot needs to be
    /// able to tell "reading the document" from "looking for a word".
    Navigate,
}

impl Stage {
    /// The slot's numeric stage code (schema-documented; stable).
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Stage::Idle => 0,
            Stage::Open => 1,
            Stage::Render => 2,
            Stage::Text => 3,
            Stage::Search => 4,
            Stage::Navigate => 5,
        }
    }

    /// The wire's stage identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Stage::Idle => "idle",
            Stage::Open => "open",
            Stage::Render => "render",
            Stage::Text => "text",
            Stage::Search => "search",
            Stage::Navigate => "navigate",
        }
    }
}

/// Where mid-operation progress goes. The guest binds this to the exported
/// slot; threaded hosts poll it out-of-band. Never blocks, never panics.
pub trait ProgressSink {
    /// Report `request`'s progress: `stage` reached, `fraction` basis points
    /// (0..=10_000) complete within the stage.
    fn progress(&self, request: u64, stage: Stage, fraction: u32);
}

/// A no-op sink (dispatch still works without a progress consumer).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProgress;

impl ProgressSink for NullProgress {
    fn progress(&self, _request: u64, _stage: Stage, _fraction: u32) {}
}

/// The host-injected environment for one dispatch.
///
/// The WASM boundary is where the engine's absent externals arrive
/// (ADR-P0011: no clock below L4, cancellation is cooperative): the clock
/// (the guest's slot-backed [`GuestClock`](crate::GuestClock), the shell's
/// `performance.now` adapter), the cancel token (bound to the exported
/// cancel slot on the guest), and the progress sink (the exported slot).
pub struct WorkerEnv<'a> {
    /// The injected clock (drives `Budget::wall` deadlines).
    pub clock: &'a dyn Clock,
    /// The injected cancellation (observed at every budget tick).
    pub cancel: CancelToken,
    /// The injected progress sink.
    pub progress: &'a dyn ProgressSink,
}

impl core::fmt::Debug for WorkerEnv<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WorkerEnv").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------

/// An open document in the guest registry.
struct OpenDoc {
    session: Session,
    budget: Budget,
    /// Source length, in bytes — the live-tally contribution released on `close`.
    src_len: u64,
    /// The lazy CJK resident set, once `cjkOpen` has installed one (SL-4.WASM.07).
    ///
    /// `None` is the common case and is load-bearing: a document with no loader
    /// attached renders through the plain [`Session::render_page`] path, which
    /// is byte-for-byte what it has always been. Only a document whose shell
    /// called `cjkOpen` takes [`Session::render_page_cjk`], so installing the
    /// loader is the *only* thing that changes a render's behaviour — there is no
    /// second code path that a shell can reach by accident.
    cjk: Option<CjkChunkLoader>,
    /// The lazy-fallback face loader, once `fallbackOpen` has installed one.
    ///
    /// Same `None`-is-load-bearing rule as `cjk`: a document with no loader
    /// renders through the plain [`Session::render_page`] path, byte for byte.
    /// Installing the loader is the only thing that changes a render's
    /// behaviour, so there is no second path a shell can reach by accident.
    fallbacks: Option<FallbackFaceLoader>,
}

/// The binary attachment that leaves with a response.
#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    /// What the bytes are (mirrored in the response `value`).
    pub info: PayloadFormat,
    /// The payload bytes.
    pub bytes: Vec<u8>,
}

/// The payload formats v1 produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    /// Raw RGBA8 pixels, `width`Ã—`height`Ã—4 bytes, row-major, top-left
    /// origin (the rendered canvas or one tile of it).
    Rgba8,
    /// UTF-8 text (extracted page text in the requested format).
    Utf8Text,
}

/// One dispatch's full outcome: the response message plus its optional
/// binary attachment.
#[derive(Debug, Clone)]
pub struct Outgoing {
    /// The response message (always present; a dispatch always answers).
    pub response: ResponseMessage,
    /// The binary attachment, when the op produces one.
    pub payload: Option<Payload>,
}

impl Outgoing {
    /// A `value` response with no attachment.
    fn ok(id: u64, value: serde_json::Value) -> Self {
        Self {
            response: ResponseMessage::ok(PROTOCOL_VERSION, id, value),
            payload: None,
        }
    }

    /// A typed-error response with no attachment.
    fn error(id: u64, e: &Error) -> Self {
        Self {
            response: ResponseMessage::error(PROTOCOL_VERSION, id, e),
            payload: None,
        }
    }
}

/// The engine-side state of one Worker: the document registry and the
/// pre-cancellation records. The guest holds exactly one; the harness and
/// tests hold their own.
///
/// Memory accounting (SL-4.WASM.04): `live_bytes` is the sum of open source
/// lengths, `peak_bytes` its maximum — the `memoryStats` payload and the
/// tab-cap enforcement input. `close` subtracts exactly what `open` added,
/// so sequential open/close shows no live growth.
pub struct Worker {
    docs: BTreeMap<u64, OpenDoc>,
    next_handle: u64,
    pre_cancelled: Vec<u64>,
    live_bytes: u64,
    peak_bytes: u64,
    // SL-4.WASM.05 registries: the shell registers a Blob/OPFS/FSA payload
    // before `open` names it. The Worker copies the bytes through the
    // corresponding `DocSource` so the conformance suite (including the fault
    // legs) runs over the real adapter.
    blobs: BTreeMap<String, Vec<u8>>,
    opfs: BTreeMap<String, Vec<u8>>,
    fsa: BTreeMap<String, Vec<u8>>,
    // SL-4.WASM.06: in-flight range transfers. A remote document is fetched by
    // the guest *asking* for ranges and the shell delivering them, so a
    // transfer is a multi-message session with its own id space and its own
    // bound. The driver owns the request budget; the map owns the lifetime.
    transfers: BTreeMap<u64, HttpRangeDriver>,
    next_transfer: u64,
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

impl Worker {
    /// A fresh, empty worker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            docs: BTreeMap::new(),
            next_handle: 1,
            pre_cancelled: Vec::new(),
            live_bytes: 0,
            peak_bytes: 0,
            blobs: BTreeMap::new(),
            opfs: BTreeMap::new(),
            fsa: BTreeMap::new(),
            transfers: BTreeMap::new(),
            next_transfer: 1,
        }
    }

    /// Register a `Blob` payload the next `open` with `kind: "blob"` may name.
    ///
    /// The shell calls this before dispatching the `open` (the JS glue copies
    /// the `Blob` into the Worker's memory via `FileReaderSync` or an
    /// `ArrayBuffer` transfer; the native harness calls it directly).
    pub fn register_blob(&mut self, source_id: impl Into<String>, bytes: Vec<u8>) {
        self.blobs.insert(source_id.into(), bytes);
    }

    /// Register an OPFS file's bytes keyed by its OPFS path.
    pub fn register_opfs(&mut self, path: impl Into<String>, bytes: Vec<u8>) {
        self.opfs.insert(path.into(), bytes);
    }

    /// Register a File System Access handle's file keyed by `handle_id`.
    pub fn register_fsa(&mut self, handle_id: impl Into<String>, bytes: Vec<u8>) {
        self.fsa.insert(handle_id.into(), bytes);
    }

    /// The number of currently open documents.
    #[must_use]
    pub fn open_docs(&self) -> usize {
        self.docs.len()
    }

    /// Live guest bytes attributable to open documents (the sum of source
    /// lengths — the tab-cap enforcement input).
    #[must_use]
    pub const fn live_bytes(&self) -> u64 {
        self.live_bytes
    }

    /// Maximum `live_bytes` observed since creation.
    #[must_use]
    pub const fn peak_bytes(&self) -> u64 {
        self.peak_bytes
    }

    /// A snapshot of guest memory accounting for `memoryStats`.
    #[must_use]
    pub fn memory_stats(&self) -> MemoryStats {
        let live_docs = u64::try_from(self.docs.len()).unwrap_or(u64::MAX);
        MemoryStats::new(
            live_docs,
            self.live_bytes,
            self.peak_bytes,
            JS_DEFAULT_CAP_BYTES,
        )
    }

    /// Drop what the guest can on memory pressure (SL-4.WASM.04).
    ///
    /// Today that is the pre-cancel queue — the only guest-side queue that
    /// grows without a document. Tile/display-list caches, when they land,
    /// evict here first, more aggressively at higher levels. Returns the
    /// number of records dropped.
    pub fn on_memory_pressure(&mut self, level: u32) -> u64 {
        let _ = crate::memory::clamp_pressure_level(level);
        let dropped = u64::try_from(self.pre_cancelled.len()).unwrap_or(u64::MAX);
        self.pre_cancelled.clear();
        dropped
    }

    /// Handle one request (see the module docs for the pipeline).
    ///
    /// `raw` is the request message; `payload` its binary attachment (may be
    /// empty); `env` the injected clock/cancel/progress. Always answers: the
    /// return value carries the response message and, when the op produced
    /// one, the binary attachment.
    ///
    /// # Budget
    ///
    /// Every engine call inside the route runs under the budget the client
    /// named at `open` (defaulting to the viewer profile), guarded by the
    /// injected clock and cancel token. Exhaustion and cancellation are
    /// typed `BUDGET_*`/`CANCELLED` responses; the guard is poisoned on
    /// exhaustion, so nothing partial escapes (ADR-P0006).
    ///
    /// # Malformed Input
    ///
    /// Unparseable JSON, wrong schema versions, unknown ops, wrong field
    /// types, mismatched payload lengths, out-of-range pages and tiles, and
    /// stale handles each answer a typed error response and leave the
    /// registry untouched; the next well-formed message succeeds. A panic
    /// anywhere below this entry is the trampoline's `INTERNAL_PANIC`
    /// (ADR-P0017), not a lost worker.
    pub fn handle(&mut self, raw: &[u8], payload: &[u8], env: &WorkerEnv<'_>) -> Outgoing {
        // Parse once, outside the trampoline, to keep the correlation id for
        // responses (a panic deeper down still answers the right request).
        let parsed: Option<RequestMessage> = serde_json::from_slice(raw).ok();
        let id = parsed.as_ref().map_or(0, |m| m.id);
        // SL-0.ERR.03: the whole worker entry sits inside the panic
        // trampoline â€” a bug below the boundary is a typed INTERNAL_PANIC
        // response (code path + panic site, never the payload, ADR-P0017),
        // never a lost worker.
        match selis_sandbox::trampoline::catch("wasm-worker", || {
            self.dispatch(parsed, payload, env)
        }) {
            Ok(outgoing) => outgoing,
            Err(e) => Outgoing::error(id, &e),
        }
    }

    /// Route one parsed request. `Err` becomes the response here, so route
    /// bodies can use `?` for engine errors.
    fn dispatch(
        &mut self,
        parsed: Option<RequestMessage>,
        payload: &[u8],
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let Some(msg) = parsed else {
            return Ok(Outgoing::error(
                0,
                &err!(
                    Code::BindingBadArgument,
                    during = "wasm-worker",
                    detail = "request is not a valid protocol message"
                ),
            ));
        };
        let id = msg.id;
        if msg.v != PROTOCOL_VERSION {
            return Ok(Outgoing::error(
                id,
                &err!(
                    Code::BindingUnsupportedOp,
                    during = "wasm-worker",
                    detail = std::format!("protocol version {} unsupported", msg.v)
                ),
            ));
        }
        // Pre-cancelled target: answer CANCELLED without executing.
        if let Some(pos) = self.pre_cancelled.iter().position(|t| *t == id) {
            let _ = self.pre_cancelled.remove(pos);
            return Ok(Outgoing::error(
                id,
                &err!(Code::Cancelled, during = "wasm-worker"),
            ));
        }
        self.route(id, msg.op, payload, env)
    }

    /// The op dispatch table.
    fn route(
        &mut self,
        id: u64,
        op: RequestOp,
        payload: &[u8],
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        match op {
            RequestOp::Open { src, budget } => self.op_open(id, src, budget, payload, env),
            RequestOp::Close { doc } => self.op_close(id, doc),
            RequestOp::Page { doc, page } => self.op_page(id, doc, page),
            RequestOp::Render { doc, page, params } => self.op_render(id, doc, page, params, env),
            RequestOp::Text { doc, page, format } => self.op_text(id, doc, page, format, env),
            RequestOp::TextLayer { doc, page } => self.op_text_layer(id, doc, page, env),
            RequestOp::Search { doc, query, opts } => self.op_search(id, doc, query, opts, env),
            RequestOp::Outline { doc } => self.op_outline(id, doc, env),
            RequestOp::PageLabels { doc } => self.op_page_labels(id, doc, env),
            RequestOp::Destinations { doc } => self.op_destinations(id, doc, env),
            RequestOp::PageLinks { doc, page } => self.op_page_links(id, doc, page, env),
            RequestOp::RangeOpen {
                url,
                size,
                chunk,
                budget,
            } => self.op_range_open(id, url, size, chunk, budget, env),
            op @ RequestOp::RangeChunk { .. } => self.op_range_chunk(id, op, payload, env),
            RequestOp::RangeClose { transfer } => self.op_range_close(id, transfer),
            RequestOp::CjkOpen {
                doc,
                claims,
                unserved,
                core_len,
                budget,
            } => self.op_cjk_open(id, doc, claims, unserved, core_len, budget, payload),
            RequestOp::CjkChunk {
                doc,
                chunk,
                status,
                len,
            } => self.op_cjk_chunk(
                id,
                doc,
                CjkReport {
                    id: chunk,
                    status,
                    len,
                    body: payload.to_vec(),
                },
                env,
            ),
            RequestOp::CjkClose { doc, chunk } => self.op_cjk_close(id, doc, chunk),
            RequestOp::FallbackOpen {
                doc,
                claims,
                unavailable,
                budget,
            } => self.op_fallback_open(id, doc, claims, unavailable, budget),
            RequestOp::FallbackFace {
                doc,
                face,
                status,
                len,
            } => self.op_fallback_face(
                id,
                doc,
                FaceReport {
                    name: face,
                    status,
                    len,
                    body: payload.to_vec(),
                },
                env,
            ),
            RequestOp::FallbackClose { doc, face } => self.op_fallback_close(id, doc, face),
            RequestOp::Mutate { doc, mutation } => self.op_mutate(doc, mutation),
            RequestOp::Save { doc, mode } => self.op_save(doc, mode),
            RequestOp::Cancel { target } => self.op_cancel(id, target),
            RequestOp::MemoryStats => self.op_memory_stats(id),
            RequestOp::MemoryPressure { level } => self.op_memory_pressure(id, level),
        }
    }

    // -- open / close ------------------------------------------------------

    /// Open a session over already-assembled bytes and mint its handle.
    ///
    /// Shared by every source that ends up holding the whole document: the
    /// inline `bytes` attachment, the WASM.05 adapters, and a completed
    /// SL-4.WASM.06 range transfer. Sharing it is the point — the memory
    /// gates, the registry bound and the reply shape (including SL-4.EXT.03's
    /// index-aligned `pageSizes`) must not be one adapter's private idea.
    fn open_with_bytes(
        &mut self,
        id: u64,
        bytes: Vec<u8>,
        budget: Budget,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        if bytes.len() as u64 > MAX_INPUT_BYTES {
            return Err(err!(
                Code::SourceTooLarge,
                during = "wasm-worker",
                detail = "source exceeds the 64 MiB protocol bound"
            ));
        }
        // SL-4.WASM.04: charge the tab cap before the session owns the bytes so
        // an over-cap claim fails typed (BudgetBytes), never as an abort. The
        // adapters already hold the bytes; this is the aggregate tab-cap gate.
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        crate::memory::check_inline_len(len, budget.bytes, self.live_bytes, JS_DEFAULT_CAP_BYTES)?;
        if self.docs.len() as u64 >= MAX_OPEN_DOCS {
            return Err(err!(
                Code::BudgetBytes,
                during = "wasm-worker",
                detail = "document registry full; close a document first"
            ));
        }
        env.progress.progress(id, Stage::Open, 0);
        let session = Session::open(bytes, &budget, env.clock)?;
        let pages = u32::try_from(session.len()).unwrap_or(u32::MAX);
        let page_sizes = page_sizes(&session);
        let handle = self.mint_handle()?;
        self.live_bytes = self.live_bytes.saturating_add(len);
        self.peak_bytes = self.peak_bytes.max(self.live_bytes);
        self.docs.insert(
            handle.raw,
            OpenDoc {
                session,
                budget,
                src_len: len,
                cjk: None,
                fallbacks: None,
            },
        );
        Ok(Outgoing::ok(
            id,
            serde_json::json!({ "doc": handle.raw, "pages": pages, "pageSizes": page_sizes }),
        ))
    }

    fn op_open(
        &mut self,
        id: u64,
        src: SourceDescriptor,
        budget: Option<BudgetProfile>,
        payload: &[u8],
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let profile = budget.unwrap_or_default();
        let budget = budget_from_profile(&profile)?;
        match src {
            SourceDescriptor::Bytes { len } => {
                // SL-4.WASM.04: the claim is validated before the payload is
                // touched — a 1.5 GiB descriptor fails typed without a 1.5 GiB
                // copy, and the tab cap is enforced across open documents.
                crate::memory::check_inline_len(
                    len,
                    budget.bytes,
                    self.live_bytes,
                    JS_DEFAULT_CAP_BYTES,
                )?;
                let arrived = u64::try_from(payload.len()).unwrap_or(u64::MAX);
                if arrived != len {
                    return Err(err!(
                        Code::BindingBadArgument,
                        during = "wasm-worker",
                        detail = "payload length does not match the bytes descriptor"
                    ));
                }
                env.progress.progress(id, Stage::Open, 0);
                // Budgeted copy: charge before allocating so a hostile length
                // yields BUDGET_BYTES in constant memory, never an OOM abort.
                // Uses a frozen clock so the wall deadline keeps its original
                // start inside `Session::open` (the copy itself does no ticks;
                // the deadline fires there, deterministically).
                let frozen = selis_sandbox::FixedClock(0);
                let mut copy_guard = budget.guard_with(&frozen, env.cancel.clone());
                let owned: Vec<u8> = selis_sandbox::alloc::copy_slice(&mut copy_guard, payload)?;
                drop(copy_guard);
                self.open_with_bytes(id, owned, budget, env)
            }
            SourceDescriptor::Blob { source_id } => {
                let stored = self.blobs.get(&source_id).ok_or_else(|| {
                    err!(
                        Code::IoReadFailed,
                        during = "wasm-worker",
                        detail = "blob handle not registered"
                    )
                })?;
                // Drive through BlobSource so the DocSource contract (including
                // the fault legs) is the path every open takes.
                let source = selis_io::BlobSource::new(stored.clone(), source_id.clone());
                let bytes = Self::drain_source(&source)?;
                self.open_with_bytes(id, bytes, budget, env)
            }
            SourceDescriptor::Opfs { path } => {
                let stored = self.opfs.get(&path).ok_or_else(|| {
                    err!(
                        Code::IoReadFailed,
                        during = "wasm-worker",
                        detail = "OPFS path not registered"
                    )
                })?;
                let source = selis_io::OpfsSource::new(path.clone(), stored.clone());
                let bytes = Self::drain_source(&source)?;
                self.open_with_bytes(id, bytes, budget, env)
            }
            SourceDescriptor::Fsa { handle_id } => {
                let stored = self.fsa.get(&handle_id).ok_or_else(|| {
                    err!(
                        Code::IoReadFailed,
                        during = "wasm-worker",
                        detail = "FSA handle not registered"
                    )
                })?;
                let source =
                    selis_io::FsaSource::new(handle_id.clone(), handle_id.clone(), stored.clone());
                let bytes = Self::drain_source(&source)?;
                self.open_with_bytes(id, bytes, budget, env)
            }
            SourceDescriptor::HttpRange { .. } => Err(err!(
                Code::BindingUnsupportedOp,
                during = "wasm-worker",
                detail = "a remote document is fetched by rangeOpen/rangeChunk: open promises a handle and there are no bytes to mint one from yet"
            )),
        }
    }

    /// Drain a fully-resident [`DocSource`] into a `Vec<u8>` via `read_at`.
    ///
    /// Proves the adapter's `DocSource` contract (Eof, Filled, available) is
    /// the path the engine takes — a direct `stored.clone()` would bypass the
    /// conformance surface. Fault injection (truncation, Pending) is exercised
    /// by the adapter's own tests; here we just drain the resident range.
    fn drain_source<S: selis_io::DocSource>(source: &S) -> Result<Vec<u8>> {
        let len = source.len().unwrap_or(0);
        if len > MAX_INPUT_BYTES {
            return Err(err!(
                Code::SourceTooLarge,
                during = "wasm-worker",
                detail = "source exceeds the 64 MiB protocol bound"
            ));
        }
        let n = usize::try_from(len).map_err(|_| {
            err!(
                Code::SourceTooLarge,
                during = "wasm-worker",
                detail = "source length does not fit in memory"
            )
        })?;
        let mut out = vec![0u8; n];
        let mut off: u64 = 0;
        while off < len {
            // `usize::try_from` can only fail for a u64 offset past usize::MAX
            // (unreachable for a drained buffer), but a slice index that far out
            // would panic the guest and take the tab with it. A typed
            // IoReadFailed keeps the drain loop total, which is the same
            // discipline the budget checks above follow.
            let start = usize::try_from(off).unwrap_or(usize::MAX);
            let buf = out.get_mut(start..).ok_or_else(|| {
                err!(
                    Code::IoReadFailed,
                    during = "wasm-worker",
                    detail = "read offset outside the drained buffer"
                )
            })?;
            match source.read_at(off, buf)? {
                Availability::Filled(k) => {
                    if k == 0 {
                        return Err(err!(
                            Code::IoReadFailed,
                            during = "wasm-worker",
                            detail = "source returned empty fill"
                        ));
                    }
                    off = off.saturating_add(k as u64);
                }
                Availability::Eof => break,
                Availability::Pending { .. } => {
                    return Err(err!(
                        Code::IoReadFailed,
                        during = "wasm-worker",
                        detail = "web source unexpectedly pending"
                    ))
                }
            }
        }
        out.truncate(usize::try_from(off).unwrap_or(n));
        Ok(out)
    }

    // -- the HttpRangeSource fetch driver (SL-4.WASM.06) --------------------
    //
    // Three ops, and the direction of travel is the whole design: the guest
    // names the range it wants, the shell fetches it, the guest judges what
    // came back. See `crate::httprange` for why the decisions live here and
    // `RequestOp::RangeOpen` for the shell's obligations (bodyless `GET`, no
    // upload, CORS-exposed headers, `AbortSignal` → `rangeClose`).

    fn op_range_open(
        &mut self,
        id: u64,
        url: String,
        size: Option<u64>,
        chunk: Option<u64>,
        budget: Option<BudgetProfile>,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let budget = budget_from_profile(&budget.unwrap_or_default())?;
        // A claimed length is hostile input until the origin confirms it, so
        // it goes through the same pre-copy gate as an inline attachment: the
        // wasm ceiling, the document byte budget, the 64 MiB source bound and
        // the tab cap, all before a single byte is fetched (SL-4.WASM.04).
        if let Some(claimed) = size {
            crate::memory::check_inline_len(
                claimed,
                budget.bytes,
                self.live_bytes,
                JS_DEFAULT_CAP_BYTES,
            )?;
        }
        if self.transfers.len() >= MAX_OPEN_TRANSFERS {
            return Err(err!(
                Code::BudgetBytes,
                during = "wasm-worker",
                detail = "range transfer registry full; finish or close one first"
            ));
        }
        if url.is_empty() {
            return Err(err!(
                Code::BindingBadArgument,
                during = "wasm-worker",
                detail = "a range transfer needs a url"
            ));
        }
        let raw = self.next_transfer;
        self.next_transfer = raw.checked_add(1).ok_or_else(|| {
            err!(
                Code::BudgetBytes,
                during = "wasm-worker",
                detail = "range transfer id space exhausted"
            )
        })?;
        let mut driver = HttpRangeDriver::new(url.clone(), size, chunk, budget);
        let first = driver.plan()?;
        self.transfers.insert(raw, driver);
        env.progress.progress(id, Stage::Open, 0);
        Ok(Outgoing::ok(
            id,
            serde_json::json!({
                "transfer": raw,
                "url": url,
                "request": first.map(request_value),
            }),
        ))
    }

    /// Deliver one range response, as the host would.
    ///
    /// The op body travels as one value rather than ten parameters: it *is* a
    /// wire message, and flattening it into the signature would be a second,
    /// silently different shape to keep in step with `RequestOp::RangeChunk`.
    fn op_range_chunk(
        &mut self,
        id: u64,
        body: RequestOp,
        payload: &[u8],
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let RequestOp::RangeChunk {
            transfer,
            start,
            status,
            content_range,
            content_length,
            len,
        } = body
        else {
            return Err(err!(
                Code::BindingBadArgument,
                during = "wasm-worker",
                detail = "rangeChunk routed with the wrong op body"
            ));
        };
        let arrived = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        if arrived != len {
            return Err(err!(
                Code::BindingBadArgument,
                during = "wasm-worker",
                detail = "range chunk length does not match its attachment"
            ));
        }
        let Some(mut driver) = self.transfers.remove(&transfer) else {
            return Err(bad_transfer());
        };
        let budget = driver.budget();
        // A failed transfer is dropped whole: a half-assembled document is
        // never a thing here, and holding the prefix would only delay the tab
        // cap. The transfer is removed above, so every exit from here — typed
        // error included — leaves nothing behind.
        let step = self.judge_chunk(
            &mut driver,
            ChunkReport {
                start,
                status,
                content_range,
                content_length,
                body: payload.to_vec(),
            },
            arrived,
            budget,
            env,
        )?;
        match step {
            ChunkStep::Request(req) | ChunkStep::Retry(req) => {
                let received = driver.received();
                self.transfers.insert(transfer, driver);
                Ok(Outgoing::ok(
                    id,
                    serde_json::json!({
                        "transfer": transfer,
                        "received": received,
                        "complete": false,
                        "request": request_value(req),
                    }),
                ))
            }
            ChunkStep::Complete { degraded } => {
                // The bytes leave through the IO layer's own range source, so
                // a remotely-fetched document is read by the same `DocSource`
                // path as every WASM.05 adapter rather than a private one.
                let bytes = driver.drain_through_source()?;
                let received = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                let opened = self.open_with_bytes(id, bytes, budget, env)?;
                let value = opened.response.value.unwrap_or(serde_json::Value::Null);
                // `get` rather than `[]`: the value came from a reply this
                // function built, but a missing key must degrade to `null` in
                // the wire rather than panic the guest (ADR-P0017).
                let field =
                    |name: &str| value.get(name).cloned().unwrap_or(serde_json::Value::Null);
                Ok(Outgoing::ok(
                    id,
                    serde_json::json!({
                        "transfer": transfer,
                        "received": received,
                        "complete": true,
                        "degraded": degraded,
                        "randomAccess": !degraded,
                        "doc": field("doc"),
                        "pages": field("pages"),
                        "pageSizes": field("pageSizes"),
                    }),
                ))
            }
        }
    }

    /// Run one response past the memory gates and into the driver.
    ///
    /// Split out of `op_range_chunk` so the borrow of `self.transfers` ends
    /// before the driver is judged: the tab cap is a *worker*-level fact
    /// (open documents count against it) while the request and byte bounds are
    /// the driver's.
    fn judge_chunk(
        &self,
        driver: &mut HttpRangeDriver,
        report: ChunkReport,
        arrived: u64,
        budget: Budget,
        env: &WorkerEnv<'_>,
    ) -> Result<ChunkStep> {
        // The tab cap and the document budget, checked against the transfer's
        // running total *before* the body is copied — the same gate `open`
        // applies, so a transfer cannot route around it.
        let projected = driver.received().saturating_add(arrived);
        crate::memory::check_inline_len(
            projected,
            budget.bytes,
            self.live_bytes,
            JS_DEFAULT_CAP_BYTES,
        )?;
        let mut guard = budget.guard_with(env.clock, env.cancel.clone());
        driver.accept(&report, &mut guard)
    }

    // -- the lazy CJK chunk loader (SL-4.WASM.07) --------------------------
    //
    // The three ops below are the wire half of `crate::cjkchunk`. The split is
    // the same one the range exchange uses and for the same reason: the guest
    // names the chunk and judges the bytes, the shell only moves them. What is
    // new here is *whose* memory the bytes land in — see the note on
    // `op_cjk_open` about the two copies.

    /// Deliver one chunk file and answer with the next request, or `done`.
    ///
    /// The wire fields arrive already bundled as a [`CjkReport`] — the same
    /// value the loader's `accept` takes — so this does not take them apart
    /// only to rebuild the struct one line later. That also keeps the
    /// parameter list under the workspace's arity lint.
    fn op_cjk_chunk(
        &mut self,
        id: u64,
        doc: DocHandle,
        report: CjkReport,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let opened = self.docs.get_mut(&doc.raw).ok_or_else(bad_handle)?;
        let loader = opened.cjk.as_mut().ok_or_else(bad_cjk_loader)?;
        let mut g = opened.budget.guard_with(env.clock, env.cancel.clone());
        let step = loader.accept(&report, &mut g)?;
        // The next request is planned from the *post-adoption* state, so the
        // response is the whole remaining plan in one message: a shell that has
        // delivered a chunk learns what is still wanted without a second round
        // trip, and one that has delivered everything is told `request: null`.
        let adopted = matches!(step, CjkChunkStep::Adopted { .. });
        let next = if adopted { loader.plan()? } else { None };
        let mut value = cjk_state(loader);
        if let Some(field) = value.as_object_mut() {
            let _ = field.insert("adopted".to_owned(), serde_json::json!(adopted));
            let _ = field.insert("request".to_owned(), cjk_request_value(next.as_ref()));
        }
        Ok(Outgoing::ok(id, value))
    }

    /// Release one resident chunk and report the new resident total.
    fn op_cjk_close(&mut self, id: u64, doc: DocHandle, chunk: String) -> Result<Outgoing> {
        let opened = self.docs.get_mut(&doc.raw).ok_or_else(bad_handle)?;
        let loader = opened.cjk.as_mut().ok_or_else(bad_cjk_loader)?;
        let report = loader.close(&chunk);
        let mut value = cjk_state(loader);
        if let Some(field) = value.as_object_mut() {
            match &report {
                Some(r) => {
                    let _ = field.insert("closed".to_owned(), serde_json::json!(true));
                    let _ = field.insert("released".to_owned(), serde_json::json!(r.id));
                    let _ = field.insert("releasedBytes".to_owned(), serde_json::json!(r.released));
                }
                // Not a repaint, and said so: `closed: false` with the revision
                // unchanged is what stops a polling shell repainting the world
                // because an eviction turned out to be a no-op.
                None => {
                    let _ = field.insert("closed".to_owned(), serde_json::json!(false));
                }
            }
        }
        Ok(Outgoing::ok(id, value))
    }

    fn op_range_close(&mut self, id: u64, transfer: u64) -> Result<Outgoing> {
        // Also the abort path: a shell whose `AbortSignal` fires releases the
        // transfer here instead of abandoning a guest-side buffer.
        if self.transfers.remove(&transfer).is_none() {
            return Err(bad_transfer());
        }
        Ok(Outgoing::ok(
            id,
            serde_json::json!({ "transfer": transfer, "closed": true }),
        ))
    }

    /// Install the resident set and name the first chunk the guest wants.
    ///
    /// The core subset arrives as this request's binary attachment. Everything
    /// else about the payload — which ranges it can serve, what each file
    /// weighs, what it hashes to — arrives as claims, and the loader checks
    /// them rather than believing them (`CjkChunkLoader::new`).
    ///
    /// # The two copies, and why there are two
    ///
    /// After this op the bytes exist in **two** places: the guest's linear
    /// memory (this set) and the shell's HTTP/Cache-API entry. They are not
    /// shared, and cannot be. ADR-P0043 §3 gives the shell ownership of
    /// transport, caching and quota precisely *because* the guest cannot reach
    /// them: the guest has no `fetch` import, no `Cache` handle, and no
    /// `storage` binding, and a wasm module cannot hand a host a pointer into
    /// its own heap and expect the host to keep it valid across a repaint. So
    /// the shell's copy is the durable one — the one that survives a reload and
    /// answers the next `cjkOpen` from cache — and the guest's is the working
    /// one, bounded by `CjkChunkLoader::resident_bytes` and giveable back
    /// through `cjkClose`.
    ///
    /// That duplication is the cost of the boundary, and it is the honest reason
    /// FONT.10-F1 cannot be solved by *not* storing things: both copies count
    /// against the ceiling the shell is trying to stay under.
    ///
    /// # Malformed Input
    ///
    /// A stale `doc`, a `coreLen` that does not match the attachment, a claim
    /// naming an unknown range, a claim with an unparseable digest, and a claim
    /// with an absurd length are each `BINDING_BAD_ARGUMENT` and install
    /// nothing. Re-opening over a live loader replaces it, which is how a shell
    /// swaps payloads without closing the document.
    #[allow(clippy::too_many_arguments)]
    fn op_cjk_open(
        &mut self,
        id: u64,
        doc: DocHandle,
        claims: Vec<crate::protocol::CjkClaimBody>,
        unserved: Vec<String>,
        core_len: u64,
        budget: Option<BudgetProfile>,
        payload: &[u8],
    ) -> Result<Outgoing> {
        let profile = budget.unwrap_or_default();
        let limits = budget_from_profile(&profile)?;
        let arrived = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        if arrived != core_len {
            return Err(err!(
                Code::BindingBadArgument,
                during = "wasm-worker",
                detail = "CJK core length does not match its attachment"
            ));
        }
        // The core is real resident memory, so it goes through the same memory
        // gates as any other attachment: a shell cannot make the guest hold
        // more than the tab cap allows by calling this op with a big core.
        let projected = self.live_bytes.saturating_add(arrived);
        crate::memory::check_inline_len(projected, limits.bytes, 0, JS_DEFAULT_CAP_BYTES)?;
        // The wire's `id` is an untrusted `String`; the chunk table's is
        // `&'static str`. Resolving through the table is what makes the claim
        // checkable — and it means an unknown id is refused here rather than
        // interned, so a shell cannot make the guest retain arbitrary strings
        // for the life of the loader.
        let mut resolved: Vec<ChunkClaim> = Vec::with_capacity(claims.len());
        for c in claims {
            let Some(chunk) = selis_font::cjk::chunk_by_id(&c.id) else {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "wasm-worker",
                    detail = "CJK claim names a range the chunk table does not carry"
                ));
            };
            resolved.push(ChunkClaim {
                id: chunk.id,
                sha256: c.sha256,
                raw_bytes: c.raw_bytes,
                url: c.url,
            });
        }
        let claims = resolved;
        let mut loader =
            CjkChunkLoader::new(selis_bytes::Bytes::copy_from_slice(payload), claims, limits)?;
        let unserved: Vec<&str> = unserved.iter().map(String::as_str).collect();
        loader.mark_unavailable(&unserved);
        // The first request, so a shell learns what to fetch without having to
        // render first. A document that has needed nothing yet gets `null`.
        let first = loader.plan()?;
        let mut value = cjk_state(&loader);
        if let Some(field) = value.as_object_mut() {
            let _ = field.insert("request".to_owned(), cjk_request_value(first.as_ref()));
        }
        let opened = self.docs.get_mut(&doc.raw).ok_or_else(bad_handle)?;
        opened.cjk = Some(loader);
        Ok(Outgoing::ok(id, value))
    }

    /// Install the lazy-fallback face loader and answer with the first face the
    /// document wants.
    ///
    /// # Malformed Input
    ///
    /// A stale or unknown `doc`, a claim naming a face outside the closed table,
    /// a claim with an unparseable digest and a claim with an absurd length are
    /// each `BINDING_BAD_ARGUMENT` and install nothing. Re-opening over a live
    /// loader replaces it, which is how a shell swaps payloads without closing
    /// the document.
    ///
    /// Unlike `op_cjk_open` there is no attachment: the two faces the web module
    /// embeds are already inside the wasm binary, so the loader starts empty.
    fn op_fallback_open(
        &mut self,
        id: u64,
        doc: DocHandle,
        claims: Vec<crate::protocol::FaceClaimBody>,
        unavailable: Vec<String>,
        budget: Option<BudgetProfile>,
    ) -> Result<Outgoing> {
        let profile = budget.unwrap_or_default();
        let limits = budget_from_profile(&profile)?;
        // The wire's `name` is an untrusted `String`; the table's is
        // `&'static str`. Resolving through the table is what makes the claim
        // checkable - and it means an unknown name is refused here rather than
        // interned, so a shell cannot make the guest retain arbitrary strings
        // for the life of the loader.
        let mut resolved: Vec<FaceClaim> = Vec::with_capacity(claims.len());
        for c in claims {
            let Some(name) = crate::fallbackchunk::intern(&c.name) else {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "wasm-worker",
                    detail = "fallback claim names a face this build does not know"
                ));
            };
            resolved.push(FaceClaim {
                name,
                sha256: c.sha256,
                raw_bytes: c.raw_bytes,
                url: c.url,
            });
        }
        let mut loader = FallbackFaceLoader::new(resolved, limits)?;
        let names: Vec<&str> = unavailable.iter().map(String::as_str).collect();
        loader.mark_unavailable(&names);
        let first = loader.plan()?;
        let mut value = fallback_state(&loader);
        if let Some(field) = value.as_object_mut() {
            let _ = field.insert("request".to_owned(), fallback_request_value(first.as_ref()));
        }
        let opened = self.docs.get_mut(&doc.raw).ok_or_else(bad_handle)?;
        opened.fallbacks = Some(loader);
        Ok(Outgoing::ok(id, value))
    }

    /// Deliver one fallback face and answer with the next request, or `done`.
    ///
    /// The wire fields arrive already bundled as a [`FaceReport`] - the same
    /// value the loader's `accept` takes - so they are not taken apart only to
    /// be rebuilt one line later, and the arity lint stays satisfied.
    fn op_fallback_face(
        &mut self,
        id: u64,
        doc: DocHandle,
        report: FaceReport,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let opened = self.docs.get_mut(&doc.raw).ok_or_else(bad_handle)?;
        let loader = opened.fallbacks.as_mut().ok_or_else(bad_fallback_loader)?;
        let mut g = opened.budget.guard_with(env.clock, env.cancel.clone());
        let step = loader.accept(&report, &mut g)?;
        // As for CJK, the next request is planned from the *post-adoption*
        // state, so one delivery carries the whole remaining plan.
        let adopted = matches!(step, FaceStep::Adopted { .. });
        let next = if adopted { loader.plan()? } else { None };
        let mut value = fallback_state(loader);
        if let Some(field) = value.as_object_mut() {
            let _ = field.insert("adopted".to_owned(), serde_json::json!(adopted));
            let _ = field.insert("request".to_owned(), fallback_request_value(next.as_ref()));
        }
        Ok(Outgoing::ok(id, value))
    }

    /// Release one resident fallback face and report the new resident total.
    ///
    /// The mirror of [`Self::op_cjk_close`], field for field, so a shell reads
    /// the two the same way: `closed` says whether bytes actually went back,
    /// `released` is the claim's size (what the shell's own cache should drop),
    /// and the revision is the repaint signal. A close that released nothing
    /// answers `closed: false` and leaves the revision alone, so a polling
    /// shell does not repaint the world for a no-op.
    ///
    /// What this does **not** do is the load-bearing part: the loader stays
    /// installed on the document, and the released face goes back on `needs`.
    /// Detaching it instead would send the next render down the plain
    /// `Session::render_page` path, which reports no face queue — so a session
    /// that had already rendered with the face would go on painting wrong
    /// metrics with nothing to tell it why. The reasoning is argued in full on
    /// [`RequestOp::FallbackClose`]; this is the same rule at the boundary.
    fn op_fallback_close(&mut self, id: u64, doc: DocHandle, face: String) -> Result<Outgoing> {
        let opened = self.docs.get_mut(&doc.raw).ok_or_else(bad_handle)?;
        let loader = opened.fallbacks.as_mut().ok_or_else(bad_fallback_loader)?;
        let report = loader.close(&face);
        let mut value = fallback_state(loader);
        if let Some(field) = value.as_object_mut() {
            match &report {
                Some(r) => {
                    let _ = field.insert("closed".to_owned(), serde_json::json!(true));
                    let _ = field.insert("released".to_owned(), serde_json::json!(r.name));
                    let _ = field.insert("releasedBytes".to_owned(), serde_json::json!(r.released));
                }
                // Not a repaint, and said so, for the same reason as CJK.
                None => {
                    let _ = field.insert("closed".to_owned(), serde_json::json!(false));
                }
            }
        }
        Ok(Outgoing::ok(id, value))
    }

    fn mint_handle(&mut self) -> Result<DocHandle> {
        let raw = self.next_handle;
        self.next_handle = raw.checked_add(1).ok_or_else(|| {
            err!(
                Code::BudgetBytes,
                during = "wasm-worker",
                detail = "handle space exhausted"
            )
        })?;
        Ok(DocHandle { raw })
    }

    fn op_close(&mut self, id: u64, doc: DocHandle) -> Result<Outgoing> {
        // SL-4.WASM.04 explicit release: dropping the Session frees the
        // source bytes and the resolved model; the live tally shrinks by
        // exactly what `open` added.
        let removed = self.docs.remove(&doc.raw).ok_or_else(bad_handle)?;
        self.live_bytes = self.live_bytes.saturating_sub(removed.src_len);
        Ok(Outgoing::ok(id, serde_json::json!({ "closed": true })))
    }

    fn op_memory_stats(&self, id: u64) -> Result<Outgoing> {
        let stats = self.memory_stats();
        Ok(Outgoing::ok(
            id,
            serde_json::json!({
                "liveDocs": stats.live_docs,
                "liveBytes": stats.live_bytes,
                "peakBytes": stats.peak_bytes,
                "wasmMaxBytes": stats.wasm_max_bytes,
                "jsCapBytes": stats.js_cap_bytes,
                "maxInputBytes": stats.max_input_bytes,
                "maxCanvasDim": stats.max_canvas_dim,
            }),
        ))
    }

    fn op_memory_pressure(&mut self, id: u64, level: u32) -> Result<Outgoing> {
        let clamped = crate::memory::clamp_pressure_level(level);
        let evicted = self.on_memory_pressure(clamped);
        Ok(Outgoing::ok(
            id,
            serde_json::json!({
                "level": clamped,
                "evicted": evicted,
                "liveDocs": u64::try_from(self.docs.len()).unwrap_or(u64::MAX),
                "liveBytes": self.live_bytes,
                "peakBytes": self.peak_bytes,
            }),
        ))
    }

    // -- page metadata -----------------------------------------------------

    fn op_page(&mut self, id: u64, doc: DocHandle, page: u32) -> Result<Outgoing> {
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let idx = page_index(page);
        let Some((w, h)) = opened.session.page_size(idx) else {
            return Err(err!(Code::PageOutOfRange, during = "wasm-worker"));
        };
        Ok(Outgoing::ok(
            id,
            serde_json::json!({
                "page": page,
                "widthPt": w,
                "heightPt": h,
            }),
        ))
    }

    // -- render ------------------------------------------------------------

    fn op_render(
        &mut self,
        id: u64,
        doc: DocHandle,
        page: u32,
        params: RenderParams,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        // SL-4.WASM.07: a document with a CJK loader attached renders through
        // `render_page_cjk`, which resolves `Uni…UCS2…` codes the document's own
        // font cannot serve out of the resident set, paints `.notdef` for the
        // rest, and **queues the chunks it wanted** — all without touching the
        // network. A document with no loader takes the plain path, byte for byte
        // what it has always been; installing the loader is the only thing that
        // changes a render.
        //
        // The outcome travels back in the response (`cjk.needs` + `cjk.revision`)
        // so a shell learns what to fetch and when to repaint without a second
        // round trip. The fetch itself is the shell's; see `crate::cjkchunk`.
        let opened = self.docs.get_mut(&doc.raw).ok_or_else(bad_handle)?;
        let idx = page_index(page);
        if idx >= opened.session.len() {
            return Err(err!(Code::PageOutOfRange, during = "wasm-worker"));
        }
        let view = opened
            .session
            .page_view(idx, params.dpi)
            .ok_or_else(|| err!(Code::PageOutOfRange, during = "wasm-worker"))?;
        let (w, h) = (view.width, view.height);
        // SL-4.WASM.04: the canvas is validated against the dimension cap,
        // the pixel budget, and the 4 GiB ceiling before the backend exists.
        crate::memory::check_canvas(w, h, opened.budget.pixels)?;
        let tile = match params.tile {
            Some(t) => {
                let x_end = u64::from(t.x).saturating_add(u64::from(t.w));
                let y_end = u64::from(t.y).saturating_add(u64::from(t.h));
                if x_end > u64::from(w) || y_end > u64::from(h) || t.w == 0 || t.h == 0 {
                    return Err(err!(
                        Code::BindingBadArgument,
                        during = "wasm-worker",
                        detail = "tile does not fit the rendered canvas"
                    ));
                }
                Some(t)
            }
            None => None,
        };

        let budget = opened.budget;
        let mut g = budget.guard_with(env.clock, env.cancel.clone());
        // The canvas is the op's pixel claim: a hostile media box aiming at
        // a 16k×16k allocation exhausts the pixel budget *before* the
        // allocation exists (ADR-P0006). Bytes are charged too — the pixmap
        // is `w × h × 4` live bytes the tab cap accounts for.
        let canvas_pixels = crate::memory::canvas_pixels(w, h);
        g.charge(Resource::Pixels, canvas_pixels)?;
        let canvas_byte_claim = crate::memory::canvas_bytes(w, h);
        g.charge(Resource::Bytes, canvas_byte_claim)?;
        let ctm = match params.matrix {
            // serde enforces exactly six elements for `[f64; 6]`; a
            // wrong-length array fails the parse as `BINDING_BAD_ARGUMENT`.
            Some([a, b, c, d, e, f]) => Matrix::new(a, b, c, d, e, f),
            None => view.ctm,
        };
        env.progress.progress(id, Stage::Render, 0);
        let mut backend = TinySkiaBackend::new(w, h).ok_or_else(|| {
            err!(
                Code::BudgetPixels,
                during = "wasm-worker",
                detail = "canvas allocation refused"
            )
        })?;
        // The lazy branch. `render_page_lazy` is the *only* difference from the
        // plain walk, and it is the engine's own entry point — the bindings layer
        // does not reimplement glyph resolution, it just hands the sets over.
        //
        // One composable call rather than an if/else between the CJK and
        // fallback entry points: a document may legitimately have both (a
        // Chinese report naming Helvetica for its Latin text), and choosing
        // between them would silently drop one. `LazyFonts` carries whichever
        // sets are attached, and a document with neither takes the plain walk.
        let mut lazy = LazyFonts::default();
        if let Some(loader) = opened.cjk.as_mut() {
            lazy.cjk = Some(loader.set_mut());
        }
        if let Some(loader) = opened.fallbacks.as_mut() {
            lazy.fallbacks = Some(loader.set_mut());
        }
        let lazy_outcome =
            opened
                .session
                .render_page_lazy(idx, &mut backend, ctm, &budget, &mut g, &mut lazy)?;
        // The faces a render found missing, fed back to the loader so its
        // `needs` reflects what the document actually asked for. A shell reads
        // that after the render and fetches before the next one.
        if let Some(loader) = opened.fallbacks.as_mut() {
            for name in &lazy_outcome.fallbacks {
                loader.set_mut().request(name);
            }
        }
        let outcome = lazy_outcome.cjk;
        let fallback_needs = lazy_outcome.fallbacks;
        let pixels = backend.pixmap().data().to_vec();
        drop(g);

        let (payload_bytes, tile_json) = match tile {
            Some(t) => {
                let bytes = crop_tile(&pixels, w, t)?;
                (
                    bytes,
                    serde_json::json!({ "x": t.x, "y": t.y, "w": t.w, "h": t.h }),
                )
            }
            None => (pixels, serde_json::Value::Null),
        };
        let len = u64::try_from(payload_bytes.len()).unwrap_or(u64::MAX);
        let mut value = if tile_json.is_null() {
            serde_json::json!({
                "page": page,
                "width": w,
                "height": h,
                "format": "rgba8",
                "payload": { "len": len },
            })
        } else {
            serde_json::json!({
                "page": page,
                "width": w,
                "height": h,
                "format": "rgba8",
                "payload": { "len": len },
                "tile": tile_json,
            })
        };
        // SL-4.WASM.07: the CJK outcome rides along with the pixels, so the
        // notdef→repaint sequence needs no second round trip. It is present
        // **only** when a loader is installed — a document without one gets the
        // exact response shape it has always got, which is what keeps every
        // existing render leg (including the guest==native checksum) untouched.
        if let (Some(outcome), Some(loader)) = (outcome.as_ref(), opened.cjk.as_ref()) {
            if let Some(field) = value.as_object_mut() {
                let _ = field.insert(
                    "cjk".to_owned(),
                    serde_json::json!({
                        "needs": outcome.needs,
                        "revision": outcome.revision,
                        "residentBytes": loader.resident_bytes(),
                    }),
                );
            }
        }
        // The fallback needs ride along with the pixels for the same reason
        // the CJK outcome does: the shell cannot learn them any other way.
        // Re-reading the loader's queue would clobber the set, and a second
        // round trip for state the guest already has is the round trip this
        // field exists to avoid. Present **only** when a loader is installed, so
        // a document without one gets the exact response shape it always got.
        if let Some(loader) = opened.fallbacks.as_ref() {
            if !fallback_needs.is_empty() || loader.loaded().len() > 1 {
                if let Some(field) = value.as_object_mut() {
                    let _ = field.insert(
                        "fallbacks".to_owned(),
                        serde_json::json!({
                            "needs": fallback_needs,
                            "revision": loader.revision(),
                            "residentBytes": loader.resident_bytes(),
                        }),
                    );
                }
            }
        }
        env.progress.progress(id, Stage::Render, 10_000);
        Ok(Outgoing {
            response: ResponseMessage::ok(PROTOCOL_VERSION, id, value),
            payload: Some(Payload {
                info: PayloadFormat::Rgba8,
                bytes: payload_bytes,
            }),
        })
    }

    // -- text --------------------------------------------------------------

    fn op_text(
        &mut self,
        id: u64,
        doc: DocHandle,
        page: u32,
        format: Option<TextFormat>,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let idx = page_index(page);
        if idx >= opened.session.len() {
            return Err(err!(Code::PageOutOfRange, during = "wasm-worker"));
        }
        let budget = opened.budget;
        let mut g = budget.guard_with(env.clock, env.cancel.clone());
        env.progress.progress(id, Stage::Text, 0);
        let dl = opened
            .session
            .page_text_display_list(idx, &budget, &mut g)?;
        let mcid = opened
            .session
            .mcid_order(&budget, &mut g)
            .ok()
            .filter(|v| !v.is_empty());
        let assembled = page_text(&opened.session, idx, &budget, &dl, mcid.as_deref(), &mut g)?;
        let (lines, line_texts) = (&assembled.lines, &assembled.line_texts);
        let fmt = format.unwrap_or_default();
        let out = match fmt {
            TextFormat::Text => {
                selis_pdf_text::to_text(lines, line_texts, assembled.low_confidence)
            }
            TextFormat::Json => {
                let mut structured =
                    selis_pdf_text::structured(lines, line_texts, &assembled.run_texts);
                structured.low_confidence = assembled.low_confidence;
                selis_pdf_text::to_json(&structured)
            }
            TextFormat::Markdown => {
                selis_pdf_text::to_markdown(lines, line_texts, assembled.low_confidence)
            }
        };
        let len = u64::try_from(out.len()).unwrap_or(u64::MAX);
        let fmt_str = match fmt {
            TextFormat::Text => "text",
            TextFormat::Json => "json",
            TextFormat::Markdown => "markdown",
        };
        env.progress.progress(id, Stage::Text, 10_000);
        Ok(Outgoing {
            response: ResponseMessage::ok(
                PROTOCOL_VERSION,
                id,
                serde_json::json!({
                    "page": page,
                    "format": fmt_str,
                    "payload": { "len": len },
                }),
            ),
            payload: Some(Payload {
                info: PayloadFormat::Utf8Text,
                bytes: out.into_bytes(),
            }),
        })
    }

    // -- text layer (SL-4.EXT.03) -------------------------------------------

    /// One page's text layer: per-character selection geometry.
    ///
    /// ## Why the geometry is built here and not in a shell
    ///
    /// The glyphs are the only place character geometry exists, and
    /// `selis_pdf_text::page_layer` is the only projection of them that
    /// keeps `chars` index-aligned with the text - the property the whole
    /// selection and copy story rests on (SL-4.UI.04). A shell that rebuilt
    /// the quads from a rendered tile would couple selection to the render
    /// ladder, which deliberately shows a previous scale's bitmap while
    /// zooming: selection would be wrong exactly when it has to survive.
    ///
    /// ## The wire shape
    ///
    /// JSON, in the message body, with no binary attachment: a layer is
    /// rectangles and flags, and a base64 attachment of the same numbers
    /// would be a decode step on every page for no saving. Rectangles are
    /// `{x, y, width, height}` - the shape `apps/ui`'s `Rect` already has,
    /// so the MV3 host (SL-4.EXT.03) maps the reply without a rename that
    /// could be done on the wrong field. The cost is honest and is stated
    /// in `REUSE.md`: a dense page is a few hundred kilobytes of JSON, and
    /// `chrome.runtime` ports base64 it, so a text layer is the most
    /// expensive thing this transport carries.
    fn op_text_layer(
        &mut self,
        id: u64,
        doc: DocHandle,
        page: u32,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let idx = page_index(page);
        let Some((width, height)) = opened.session.page_size(idx) else {
            return Err(err!(Code::PageOutOfRange, during = "wasm-worker"));
        };
        let budget = opened.budget;
        let mut g = budget.guard_with(env.clock, env.cancel.clone());
        env.progress.progress(id, Stage::Text, 0);
        let dl = opened
            .session
            .page_text_display_list(idx, &budget, &mut g)?;
        let mcid = opened
            .session
            .mcid_order(&budget, &mut g)
            .ok()
            .filter(|v| !v.is_empty());
        let assembled = page_text(&opened.session, idx, &budget, &dl, mcid.as_deref(), &mut g)?;
        let widths = advance_table(&assembled.lines);
        let width_of = |code: u16| widths.get(&code).copied().unwrap_or(0.0);
        let mut layer = selis_pdf_text::page_layer(
            idx,
            (width, height),
            &assembled.lines,
            &assembled.line_texts,
            &width_of,
        );
        // SL-3.TEXT.10: the assembler's own verdict, carried through so the
        // copy path shows the marker instead of an empty layer that reads as
        // a blank page.
        layer.low_confidence = assembled.low_confidence;
        let text = layer.text();
        let lines: Vec<serde_json::Value> = layer
            .lines
            .iter()
            .map(|line| {
                let chars: Vec<serde_json::Value> = line
                    .chars
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "rect": rect_value(&c.rect),
                            "advance": c.advance,
                            "inked": c.inked,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "text": line.text,
                    "rect": rect_value(&line.rect),
                    "direction": if line.direction.is_rtl() { "rtl" } else { "ltr" },
                    "chars": chars,
                })
            })
            .collect();
        env.progress.progress(id, Stage::Text, 10_000);
        Ok(Outgoing::ok(
            id,
            serde_json::json!({
                "page": layer.page,
                "width": layer.width,
                "height": layer.height,
                "lowConfidence": layer.low_confidence,
                "text": text,
                "lines": lines,
            }),
        ))
    }

    // -- navigation (SL-3.DOC-NAV) -----------------------------------------
    //
    // Four ops, one structure each, all read-only and all reporting rather than
    // deciding: the ADR-P0020 action-class policy is the viewer's, and a guest
    // that filtered action classes would make "is the viewer refusing this?"
    // unanswerable from the shell.

    fn op_outline(&mut self, id: u64, doc: DocHandle, env: &WorkerEnv<'_>) -> Result<Outgoing> {
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let budget = opened.budget;
        let mut g = budget.guard_with(env.clock, env.cancel.clone());
        env.progress.progress(id, Stage::Navigate, 0);
        let tree = opened.session.outline(&budget, &mut g)?;
        let items = tree
            .items
            .iter()
            .map(outline_item_value)
            .collect::<Vec<serde_json::Value>>();
        env.progress.progress(id, Stage::Navigate, 10_000);
        // `present` and `truncated` are the two flags the viewer's "this host
        // cannot read the outline" state is built on: `present:false` is a
        // document with no outline, `truncated:true` is an outline this engine
        // could not read in full, and neither is a refusal (that is a typed
        // error response, which `?` above already turned into one).
        Ok(Outgoing::ok(
            id,
            serde_json::json!({
                "present": tree.present,
                "truncated": tree.truncated,
                "pruned": tree.pruned,
                "items": items,
            }),
        ))
    }

    fn op_page_labels(&mut self, id: u64, doc: DocHandle, env: &WorkerEnv<'_>) -> Result<Outgoing> {
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let budget = opened.budget;
        let mut g = budget.guard_with(env.clock, env.cancel.clone());
        env.progress.progress(id, Stage::Navigate, 0);
        let labels = opened.session.page_labels(&budget, &mut g)?;
        let ranges = labels
            .iter()
            .map(|label| {
                serde_json::json!({
                    "firstPage": label.page_index,
                    // As **text**, not as a byte array. These are `Bytes`
                    // upstream, and handing `as_slice()` to serde emits a JSON
                    // array of numbers — which is not what the UI's
                    // `PageLabelStyle`/`prefix` are, and would leave a shell
                    // with `[114]` where it expected `"r"`. A lossy decode is
                    // the right trade here because these are PDFDocEncoding
                    // label strings, not document text: the alternative is a
                    // shape no consumer can read.
                    "style": label.style.as_deref().map(text_of),
                    "prefix": label.prefix.as_deref().map(text_of),
                    "firstValue": label.start,
                })
            })
            .collect::<Vec<serde_json::Value>>();
        env.progress.progress(id, Stage::Navigate, 10_000);
        Ok(Outgoing::ok(id, serde_json::json!({ "ranges": ranges })))
    }

    fn op_destinations(
        &mut self,
        id: u64,
        doc: DocHandle,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let budget = opened.budget;
        let mut g = budget.guard_with(env.clock, env.cancel.clone());
        env.progress.progress(id, Stage::Navigate, 0);
        let named = opened.session.named_destinations(&budget, &mut g)?;
        let destinations = named
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "target": target_value(&entry.target),
                })
            })
            .collect::<Vec<serde_json::Value>>();
        env.progress.progress(id, Stage::Navigate, 10_000);
        Ok(Outgoing::ok(
            id,
            serde_json::json!({ "destinations": destinations }),
        ))
    }

    fn op_page_links(
        &mut self,
        id: u64,
        doc: DocHandle,
        page: u32,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let idx = page_index(page);
        // The range check is here, not left to the engine's empty-list answer:
        // "page 9000 of a 3-page document has no links" and "this host cannot
        // read links" must be distinguishable, and only a typed error says so.
        if opened.session.page_size(idx).is_none() {
            return Err(err!(Code::PageOutOfRange, during = "wasm-worker"));
        }
        let budget = opened.budget;
        let mut g = budget.guard_with(env.clock, env.cancel.clone());
        env.progress.progress(id, Stage::Navigate, 0);
        let links = opened.session.page_links(idx, &budget, &mut g)?;
        let items = links
            .iter()
            .map(|link| {
                serde_json::json!({
                    "index": link.index,
                    "object": link.object,
                    "rect": annot_rect_value(link.rect),
                    "action": link.action.as_ref().map(action_value),
                    "target": link.target.as_ref().map(target_value),
                    "contents": link.contents,
                })
            })
            .collect::<Vec<serde_json::Value>>();
        env.progress.progress(id, Stage::Navigate, 10_000);
        Ok(Outgoing::ok(
            id,
            serde_json::json!({ "page": page, "links": items }),
        ))
    }

    // -- search ------------------------------------------------------------

    fn op_search(
        &mut self,
        id: u64,
        doc: DocHandle,
        query: String,
        opts: Option<SearchOpts>,
        env: &WorkerEnv<'_>,
    ) -> Result<Outgoing> {
        let opts = opts.unwrap_or_default();
        if opts.match_case {
            // The normalised search primitive folds case by design; a
            // case-preserving match needs the case-aware index and is not
            // quietly answered as if it were case-insensitive.
            return Err(err!(
                Code::BindingUnsupportedOp,
                during = "wasm-worker",
                detail = "matchCase arrives with the case-aware search index"
            ));
        }
        let max_matches = opts
            .max_matches
            .unwrap_or(DEFAULT_SEARCH_MATCHES)
            .min(MAX_SEARCH_MATCHES);
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let page_count = opened.session.len();
        let (from, to) = search_range(&opts, page_count)?;
        let budget = opened.budget;
        let mut g = budget.guard_with(env.clock, env.cancel.clone());
        env.progress.progress(id, Stage::Search, 0);

        let mut matches: Vec<serde_json::Value> = Vec::new();
        let mut total: u64 = 0;
        let mut truncated = false;
        // SL-3.TEXT.10: pages whose display list drew text but recovered none of
        // it. They contribute no matches, so a reply of `total: 0` would read
        // as "this document does not contain the query" when the truth is "we
        // could not read these pages". The page list makes the difference
        // checkable by the caller instead of indistinguishable.
        let mut low_confidence_pages: Vec<u32> = Vec::new();
        let span = u64::from(to)
            .saturating_sub(u64::from(from))
            .saturating_add(1);
        for p in from..=to {
            // Cancellation and the deadline land between pages (and inside
            // the engine, at its own ticks).
            g.tick()?;
            let idx = page_index(p);
            let dl = opened
                .session
                .page_text_display_list(idx, &budget, &mut g)?;
            let mcid = opened
                .session
                .mcid_order(&budget, &mut g)
                .ok()
                .filter(|v| !v.is_empty());
            let assembled = page_text(&opened.session, idx, &budget, &dl, mcid.as_deref(), &mut g)?;
            if assembled.low_confidence {
                low_confidence_pages.push(p);
            }
            for m in selis_pdf_text::search_lines(&assembled.lines, &assembled.line_texts, &query) {
                total = total.saturating_add(1);
                if (matches.len() as u64) < u64::from(max_matches) {
                    let rect = m.rect;
                    matches.push(serde_json::json!({
                        "page": p,
                        "line": m.line,
                        "start": m.range.start,
                        "end": m.range.end,
                        "text": m.text,
                        "rect": { "x": rect.x0, "y": rect.y0, "w": rect.width(), "h": rect.height() },
                    }));
                } else {
                    truncated = true;
                }
            }
            let done = u64::from(p)
                .saturating_sub(u64::from(from))
                .saturating_add(1);
            let fraction = done
                .saturating_mul(10_000)
                .checked_div(span.max(1))
                .unwrap_or(10_000);
            let fraction = u32::try_from(fraction.min(10_000)).unwrap_or(10_000);
            env.progress.progress(id, Stage::Search, fraction);
        }
        Ok(Outgoing::ok(
            id,
            serde_json::json!({
                "query": query,
                "total": total,
                "truncated": truncated,
                // SL-3.TEXT.10: pages that drew text and recovered none of it.
                // Non-empty means `total: 0` is not a clean "not found".
                "lowConfidencePages": low_confidence_pages,
                "matches": matches,
            }),
        ))
    }

    // -- Phase 5 surfaces: schema-validated, capability-gated --------------

    fn op_mutate(&mut self, doc: DocHandle, mutation: MutationEnvelope) -> Result<Outgoing> {
        if !self.docs.contains_key(&doc.raw) {
            return Err(bad_handle());
        }
        validate_mutation(&mutation)?;
        Err(err!(
            Code::BindingUnsupportedOp,
            during = "wasm-worker",
            detail = "the edit surface arrives in Phase 5; the envelope schema is locked now"
        ))
    }

    fn op_save(&mut self, doc: DocHandle, _mode: SaveMode) -> Result<Outgoing> {
        if !self.docs.contains_key(&doc.raw) {
            return Err(bad_handle());
        }
        Err(err!(
            Code::BindingUnsupportedOp,
            during = "wasm-worker",
            detail = "save arrives in Phase 5 (SL-5.*); the wire shape is locked now"
        ))
    }

    // -- cancellation bookkeeping -------------------------------------------

    fn op_cancel(&mut self, id: u64, target: u64) -> Result<Outgoing> {
        // Record the target unless it is already answered (dispatched) â€” the
        // FIFO bound keeps a hostile client from growing this forever.
        if self.pre_cancelled.len() >= MAX_PRECANCELLED {
            let _ = self.pre_cancelled.remove(0);
        }
        self.pre_cancelled.push(target);
        Ok(Outgoing::ok(id, serde_json::json!({ "cancelled": true })))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The wire form of a planned range request.
///
/// Deliberately only an offset pair and a header string: the shell's whole
/// vocabulary for talking to the origin is "GET these bytes". There is no
/// method and no body to name, so the no-upload invariant (ADR-P0016) is a
/// property of this value rather than a rule the shell is trusted to follow.
fn request_value(req: RangeRequest) -> serde_json::Value {
    serde_json::json!({
        "start": req.start,
        "end": req.end,
        "header": req.header_value(),
    })
}

/// A stale or unknown range transfer id.
///
/// A transfer id is a handle across the binding boundary, so it gets the
/// registry's handle code rather than a new one — the bindings never invent a
/// taxonomy.
fn bad_transfer() -> Error {
    err!(
        Code::BindingBadHandle,
        during = "wasm-worker",
        detail = "unknown range transfer"
    )
}

/// A fallback-face op against a document that has no loader installed
/// (SL-3.FONT.12).
///
/// Distinct from [`bad_handle`]: the document is real, the shell simply never
/// called `fallbackOpen`. The distinction matters because "you have no fallback
/// payload" and "that document is gone" are different bugs, and a shell that
/// gets this wrong is a shell whose message to the user would be wrong.
fn bad_fallback_loader() -> Error {
    err!(
        Code::BindingBadArgument,
        during = "wasm-worker",
        detail = "no fallback loader on this document; call fallbackOpen first"
    )
}

fn bad_cjk_loader() -> Error {
    err!(
        Code::BindingBadHandle,
        during = "wasm-worker",
        detail = "no CJK loader is installed for this document"
    )
}

/// The loader's state as the wire reports it after every CJK op.
///
/// `revision` is the field a shell watches: **a change is the repaint signal**
/// (ADR-P0043 §3). `needs` is the sticky queue a render fills, so a shell can
/// tell "still loading" from "this payload has no Korean" (in `unavailable`) or
/// "we gave up on this one" (`exhausted`) without parsing prose.
/// The fallback loader's observable state.
///
/// `needs` is the face the *next* render would ask for, which is what lets a
/// shell fetch before it renders rather than painting a page whose text is
/// in the wrong font and correcting it afterwards.
fn fallback_state(loader: &FallbackFaceLoader) -> serde_json::Value {
    serde_json::json!({
        "revision": loader.revision(),
        "residentBytes": loader.resident_bytes(),
        "loaded": loader.loaded(),
        "needs": loader.needs(),
        "unavailable": loader.unavailable(),
        "exhausted": loader.exhausted(),
    })
}

/// The next face to fetch, or `null` when nothing is wanted.
///
/// `null` rather than an absent key, for the same reason as CJK: a shell's
/// `"request" in value` test must not be confusable with a request it failed
/// to read.
fn fallback_request_value(req: Option<&FaceRequest>) -> serde_json::Value {
    match req {
        Some(r) => serde_json::json!({
            "name": r.name,
            "url": r.url,
            "sha256": r.sha256,
            "rawBytes": r.raw_bytes,
        }),
        None => serde_json::Value::Null,
    }
}

fn cjk_state(loader: &CjkChunkLoader) -> serde_json::Value {
    serde_json::json!({
        "revision": loader.revision(),
        "residentBytes": loader.resident_bytes(),
        "loaded": loader.loaded_ids(),
        "needs": loader.set().queued(),
        "unavailable": loader.unavailable_ids(),
        "exhausted": loader.exhausted_ids(),
    })
}

/// The next chunk to fetch, or `null` when nothing is wanted.
///
/// `null` rather than an absent key so a shell's `if ("request" in value)`
/// cannot be confused with a request it failed to read.
fn cjk_request_value(req: Option<&CjkRequest>) -> serde_json::Value {
    match req {
        Some(r) => serde_json::json!({
            "id": r.id,
            "url": r.url,
            "sha256": r.sha256,
            "rawBytes": r.raw_bytes,
        }),
        None => serde_json::Value::Null,
    }
}

/// The page media sizes that travel with every document handle (SL-4.EXT.03).
///
/// Every shell's `DocHandle` is index-aligned with page numbers (a viewer has
/// to lay a page out before it renders it), and one page round trip per page
/// would make opening a 5 000-page report 5 000 messages. `page_size` reads a
/// box the parser has already resolved, so this is a walk and not a re-parse;
/// the sizes are points, like the rest of the wire. A page whose box did not
/// resolve still occupies a slot: dropping it would desynchronise the array
/// from the page numbers the handle is used with.
fn page_sizes(session: &Session) -> Vec<serde_json::Value> {
    (0..session.len())
        .map(|i| match session.page_size(i) {
            Some((width, height)) => serde_json::json!({ "width": width, "height": height }),
            None => serde_json::json!({ "width": 0.0, "height": 0.0 }),
        })
        .collect()
}

/// A stale or unknown document handle.
fn bad_handle() -> Error {
    err!(
        Code::BindingBadHandle,
        during = "wasm-worker",
        detail = "unknown document handle"
    )
}

/// Zero-based page index for the engine's `usize` page numbers.
fn page_index(page: u32) -> usize {
    usize::try_from(page).unwrap_or(usize::MAX)
}

// ---------------------------------------------------------------------------
// The navigation wire shapes (SL-3.DOC-NAV)
// ---------------------------------------------------------------------------
//
// Each is a *mapping*, not a translation: the engine's model already names
// every action class and every destination, so the guest's job is to hand that
// over with its own spelling intact. Two rules hold across all of them:
//
//  * an action class crosses as its own name, with the UI's `LinkActionKind`
//    vocabulary as `kind` and the document's verbatim `/S` as `rawName`, so a
//    class this build does not model is still distinguishable from a missing
//    action; and
//  * a target the document wrote but this engine cannot resolve crosses as
//    `{"kind":"unresolved"}` — never as an absent `target`, which would read as
//    "this link has no destination" rather than "this link's destination is
//    missing".

/// One outline item, with its subtree.
fn outline_item_value(item: &selis_pdf_doc::OutlineItem) -> serde_json::Value {
    serde_json::json!({
        "title": item.title,
        "target": item.target.as_ref().map(target_value),
        "action": item.action.as_ref().map(action_value),
        // `/Count` verbatim. The sign is the spec's "starts collapsed"
        // convention (section 12.3.3) and the viewer owns the decision about
        // what to do with it, so it is not normalised here.
        "count": item.count,
        "children": item
            .children
            .iter()
            .map(outline_item_value)
            .collect::<Vec<serde_json::Value>>(),
    })
}

/// One action, by class.
fn action_value(action: &selis_pdf_doc::Action) -> serde_json::Value {
    serde_json::json!({
        "kind": link_action_kind(&action.kind),
        // The document's own `/S`, verbatim. This is the field that makes
        // "the viewer refused a /Launch" distinguishable from "the engine has
        // never heard of this action class" — the question ADR-P0020's split
        // exists to keep answerable.
        "rawName": String::from_utf8_lossy(action.kind.as_name()).to_string(),
        "uri": action.uri,
        "name": action.name,
        "target": action.target.as_ref().map(target_value),
        // Reported, never followed: a `/Next` chain is a second place a
        // `/Launch` can hide, and the viewer is the layer that decides whether
        // a chain is part of one activation.
        "hasNext": action.has_next,
    })
}

/// The UI's `LinkActionKind` vocabulary, mapped from the engine's classes.
///
/// This is a **naming** map and nothing else — no class is refused, reordered,
/// or dropped here. A class outside the UI's list maps to `"unknown"` while
/// `rawName` keeps the document's spelling, which is precisely the "report,
/// don't decide" contract: the viewer still sees `/Rendition` as `/Rendition`.
fn link_action_kind(kind: &selis_pdf_doc::ActionKind) -> &'static str {
    use selis_pdf_doc::ActionKind as K;
    match kind {
        K::GoTo => "goTo",
        K::Uri => "uri",
        K::Launch => "launch",
        K::GoToR => "goToR",
        K::SubmitForm => "submitForm",
        K::ImportData => "importData",
        K::JavaScript => "javascript",
        K::Named => "named",
        _ => "unknown",
    }
}

/// One destination.
fn target_value(target: &selis_pdf_doc::NavTarget) -> serde_json::Value {
    use selis_pdf_doc::NavTarget as T;
    match target {
        T::Page(page) => serde_json::json!({
            "kind": "page",
            "page": page.page,
            // The spec's own spelling. A class outside the viewer's list is
            // carried verbatim and the viewer maps it to a plain `xyz` hop,
            // which is a loss of fidelity in the viewer, not an error here.
            "destKind": String::from_utf8_lossy(page.kind.as_name()).to_string(),
            "left": page.left,
            "top": page.top,
            "zoom": page.zoom,
        }),
        T::Named(name) => serde_json::json!({ "kind": "named", "name": name }),
        T::Unresolved => serde_json::json!({ "kind": "unresolved" }),
    }
}

/// A `Bytes` value as JSON text rather than as a byte array.
///
/// serde renders `&[u8]` as `[114, 105]`, which is a shape no consumer of a
/// label string or a `/S` name can read. This is the one place the transport
/// converts, and it is lossy on purpose: a `/PageLabels` style is one of five
/// ASCII names and a prefix is a short display string, so a byte that is not
/// valid UTF-8 is a document defect worth showing as U+FFFD rather than a
/// reason to cross an unreadable shape.
fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// A link annotation's `/Rect`, verbatim and unnormalised.
///
/// An inverted rectangle is a document defect a viewer may usefully normalise
/// for hit-testing; the guest is not the layer that decides what "where" means,
/// so the four numbers cross as written and an absent `/Rect` crosses as
/// `null` rather than as a zero box that would silently swallow clicks in the
/// corner.
fn annot_rect_value(rect: Option<[f64; 4]>) -> serde_json::Value {
    match rect {
        Some([x0, y0, x1, y1]) => {
            serde_json::json!({ "x0": x0, "y0": y0, "x1": x1, "y1": y1 })
        }
        None => serde_json::Value::Null,
    }
}

/// Resolve the search page range: explicit `{from, to}` (from â‰¤ to, `to`
/// clamped to the last page) or the whole document.
fn search_range(opts: &SearchOpts, page_count: usize) -> Result<(u32, u32)> {
    let last = u32::try_from(page_count.saturating_sub(1)).unwrap_or(u32::MAX);
    match opts.pages {
        None => Ok((0, last)),
        Some(PageRange { from, to }) => {
            if from > to {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "wasm-worker",
                    detail = "search page range is empty"
                ));
            }
            Ok((from.min(last), to.min(last)))
        }
    }
}

/// The [`Budget`] a client profile names: the surface profile, with the
/// client's per-resource overrides applied on top.
///
/// The `bytes` override is the JS heap-cap channel (SL-4.WASM.04): the
/// shell derives it from `performance.memory.jsHeapSizeLimit` or its
/// configured tab budget. A claim past the 4 GiB wasm ceiling is hostile
/// input (`BINDING_BAD_ARGUMENT`, never a silent clamp); anything else is
/// enforced per-operation as `BUDGET_BYTES`.
///
/// # Errors
///
/// * `BINDING_BAD_ARGUMENT` for a wall override that cannot be expressed in
///   nanoseconds (overflow is treated as hostile input, not clamped).
/// * `BINDING_BAD_ARGUMENT` for a byte override past the wasm ceiling.
fn budget_from_profile(profile: &BudgetProfile) -> Result<Budget> {
    let surface = match profile.surface {
        SurfaceName::Thumbnail => Surface::Thumbnail,
        SurfaceName::Viewer => Surface::Viewer,
        SurfaceName::Editor => Surface::Editor,
        SurfaceName::Batch => Surface::Batch,
        SurfaceName::Server => Surface::Server,
    };
    let mut budget = Budget::profile(surface);
    if let Some(o) = &profile.overrides {
        apply_overrides(&mut budget, o)?;
    }
    if budget.bytes > WASM_MAX_BYTES {
        return Err(err!(
            Code::BindingBadArgument,
            during = "wasm-worker",
            detail = "bytes override exceeds the 4 GiB wasm32 ceiling"
        ));
    }
    Ok(budget)
}

fn apply_overrides(budget: &mut Budget, o: &BudgetOverrides) -> Result<()> {
    if let Some(bytes) = o.bytes {
        budget.bytes = bytes;
    }
    if let Some(wall_ms) = o.wall_ms {
        let nanos = wall_ms.checked_mul(1_000_000).ok_or_else(|| {
            err!(
                Code::BindingBadArgument,
                during = "wasm-worker",
                detail = "wall_ms override overflows"
            )
        })?;
        budget.wall = nanos;
    }
    if let Some(depth) = o.depth {
        budget.depth = depth;
    }
    if let Some(objects) = o.objects {
        budget.objects = objects;
    }
    if let Some(pixels) = o.pixels {
        budget.pixels = pixels;
    }
    Ok(())
}

/// Validate the mutation envelope's *shape* (schema tag + op list). The ops
/// themselves are opaque until `selis-pdf-edit` types them (Phase 5).
fn validate_mutation(mutation: &MutationEnvelope) -> Result<()> {
    if mutation.schema != "selis-mutate/1" {
        return Err(err!(
            Code::BindingBadArgument,
            during = "wasm-worker",
            detail = "unknown mutation envelope schema"
        ));
    }
    Ok(())
}

/// Copy a device-pixel tile out of a rendered RGBA8 canvas.
///
/// # Errors
///
/// `BINDING_BAD_ARGUMENT` when the tile does not fit (defence in depth â€”
/// the caller validates first); the copy never indexes out of bounds.
fn crop_tile(pixels: &[u8], canvas_w: u32, tile: crate::protocol::Tile) -> Result<Vec<u8>> {
    let stride = u64::from(canvas_w).saturating_mul(4);
    let row_bytes = u64::from(tile.w).saturating_mul(4);
    let mut out = Vec::new();
    for row in tile.y..tile.y.saturating_add(tile.h) {
        let start = u64::from(row)
            .saturating_mul(stride)
            .saturating_add(u64::from(tile.x).saturating_mul(4));
        let end = start.saturating_add(row_bytes);
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        let end = usize::try_from(end).unwrap_or(usize::MAX);
        let Some(row_src) = pixels.get(start..end) else {
            return Err(err!(
                Code::BindingBadArgument,
                during = "wasm-worker",
                detail = "tile does not fit the rendered canvas"
            ));
        };
        out.extend_from_slice(row_src);
    }
    Ok(out)
}

/// One page's assembled text: the ordered lines, their recovered texts, and
/// their per-run texts (each entry parallels one line's runs).
struct PageText {
    lines: Vec<TextLine>,
    line_texts: Vec<String>,
    run_texts: Vec<Vec<String>>,
    /// SL-3.TEXT.10: display-list text drew nothing readable (see the CLI's
    /// `page_lines` â€” same pipeline, same honesty rule).
    low_confidence: bool,
}

/// The assembled text lines, their recovered texts, and their per-run texts
/// for one page's display list (the same pipeline `selis extract` drives:
/// shared gather + Unicode-recovery helpers, so CLI and WASM cannot drift â€”
/// SL-3.TEXT.08/09/10).
fn page_text(
    session: &Session,
    page: usize,
    budget: &Budget,
    dl: &selis_pdf_content::display_list::DisplayList,
    mcid_order: Option<&[u32]>,
    g: &mut BudgetGuard<'_>,
) -> Result<PageText> {
    // Same visible-region gate as the CLI's `page_lines`, so search can never
    // surface a span the page clips away (SL-3.TEXT.26).
    let visible = session.page_visible_box(page);
    let mut glyphs = selis_pdf_text::gather_glyphs(dl, visible);
    let drew_text = !glyphs.is_empty();
    selis_pdf_text::apply_unicode_recovery(&mut glyphs, &mut |font, code| {
        session.text_unicode(page, font, code, budget, g)
    });
    let lines = selis_pdf_text::assemble(glyphs);
    let mcid_lines: Vec<LineWithMcid> = lines
        .into_iter()
        .map(|line| {
            let mcid = line
                .words
                .iter()
                .flat_map(|w| w.runs.iter())
                .find_map(|r| r.glyphs.first().and_then(|gl| gl.mcid));
            LineWithMcid { line, mcid }
        })
        .collect();
    let ordered = selis_pdf_text::order_lines(mcid_lines, mcid_order, g)?;
    let line_texts: Vec<String> = ordered.lines.iter().map(line_text).collect();
    let run_texts: Vec<Vec<String>> = ordered
        .lines
        .iter()
        .map(|line| {
            line.words
                .iter()
                .flat_map(|w| w.runs.iter())
                .map(|run| {
                    run.glyphs
                        .iter()
                        .filter_map(|gl| char::from_u32(u32::from(gl.code)))
                        .collect()
                })
                .collect()
        })
        .collect();
    let recovered = line_texts.iter().any(|t| !t.trim().is_empty());
    Ok(PageText {
        lines: ordered.lines,
        line_texts,
        run_texts,
        low_confidence: drew_text && !recovered,
    })
}

/// The advance width of every character code drawn on a page, in user space.
///
/// ## Why this table rather than a font lookup
///
/// `page_layer` asks for a code's advance and leaves font resolution to its
/// caller, because only the caller knows which font a glyph was drawn with.
/// This worker has that information - and it already paid for it: every
/// `TextGlyph` in the assembled lines carries `advance`, the pen step it
/// actually moved in user space, justified advance included. So the table is
/// read off the page's own glyphs rather than re-resolved from font
/// dictionaries, which is both cheaper and strictly more accurate: a
/// selection highlight belongs where the glyph was *drawn*, not where an
/// un-justified font metric says it should have been.
///
/// ## The limit, stated rather than hidden
///
/// The table is keyed by character code alone, because that is the only key
/// `page_layer` hands the callback. A page that draws the same code at two
/// sizes therefore gives both occurrences the first one's width, and the
/// second one's highlight runs short. Single-size pages - which is what the
/// conformance corpus is - are exact. A code that never appears yields a
/// zero-width quad, which highlights nothing rather than highlighting the
/// wrong place; both are visible, and neither is a silently correct-looking
/// selection.
fn advance_table(lines: &[TextLine]) -> std::collections::HashMap<u16, f64> {
    let mut widths = std::collections::HashMap::new();
    for line in lines {
        for word in &line.words {
            for run in &word.runs {
                for gl in &run.glyphs {
                    widths.entry(gl.code).or_insert(gl.advance);
                }
            }
        }
    }
    widths
}

/// A rectangle on the wire, in the shape `apps/ui`'s `Rect` already uses.
///
/// Named `x`/`y`/`width`/`height` rather than the `w`/`h` this crate's search
/// matches use: those are match rectangles for a results list, this is a
/// selection quad a viewer positions directly, and the host validates it
/// field by field. Two spellings of the same rectangle in one protocol is
/// how a rename gets applied to the wrong one.
fn rect_value(rect: &selis_geom::Rect) -> serde_json::Value {
    serde_json::json!({
        "x": rect.x0,
        "y": rect.y0,
        "width": rect.width(),
        "height": rect.height(),
    })
}
/// The recovered text of a line (code â†’ Unicode char), with a space between
/// words (the assembler strips space glyphs when splitting runs into words).
fn line_text(line: &TextLine) -> String {
    let mut out = String::new();
    for (wi, word) in line.words.iter().enumerate() {
        if wi > 0 {
            out.push(' ');
        }
        for run in &word.runs {
            for gl in &run.glyphs {
                if let Some(ch) = char::from_u32(u32::from(gl.code)) {
                    out.push(ch);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    // These tests drive the real `Worker::handle` entry over the real wire, so
    // the `expect`s below are the assertions rather than crash primitives
    // reached from untrusted input — the same scope-and-reasoning the sibling
    // loader tests use. The engine's production paths keep every deny lint, and
    // no production module in this file widens anything.
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    /// A real one-page document that sets `/Helvetica` with no embedded font
    /// program — the same fixture `selis-pdf-engine`'s `fallback_lazy` test
    /// uses, borrowed rather than duplicated so there is one copy of this class
    /// of file in the repo.
    const HELVETICA_NO_EMBED: &[u8] =
        include_bytes!("../../../apps/cli/tests/fixtures/text11_tm_scaled.pdf");

    /// The face that fixture substitutes to, as real bytes: the cache-poisoning
    /// guard digests them, so a constant would make the guard vacuous.
    const SANS: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");
    const SANS_NAME: &str = "LiberationSans-Regular";

    fn worker_env<'a>(
        clock: &'a selis_sandbox::FixedClock,
        cancel: &'a CancelToken,
    ) -> WorkerEnv<'a> {
        WorkerEnv {
            clock,
            cancel: cancel.clone(),
            progress: &NullProgress,
        }
    }

    /// Dispatch one request with no attachment and return its `value`.
    fn call(
        w: &mut Worker,
        clock: &selis_sandbox::FixedClock,
        cancel: &CancelToken,
        raw: &serde_json::Value,
    ) -> serde_json::Value {
        let e = worker_env(clock, cancel);
        let out = w.handle(&serde_json::to_vec(raw).expect("serialise"), &[], &e);
        assert!(
            out.response.code.is_none(),
            "unexpected error: {:?}",
            out.response
        );
        out.response.value.expect("a typed value")
    }

    /// The one claim every test opens the loader with: a real digest over real
    /// bytes, so nothing here is a constant that could make the guard vacuous.
    fn claim() -> serde_json::Value {
        serde_json::json!({
            "name": SANS_NAME,
            "sha256": crate::cjkchunk::digest_hex(SANS),
            "rawBytes": SANS.len(),
            "url": format!("fallback/{SANS_NAME}.ttf.br"),
        })
    }

    /// Open the fixture and answer with its handle.
    fn open_fixture(
        w: &mut Worker,
        clock: &selis_sandbox::FixedClock,
        cancel: &CancelToken,
    ) -> u64 {
        let raw = serde_json::json!({
            "v": PROTOCOL_VERSION,
            "id": 1,
            "op": "open",
            "src": { "kind": "bytes", "len": HELVETICA_NO_EMBED.len() },
        });
        let e = worker_env(clock, cancel);
        let out = w.handle(
            &serde_json::to_vec(&raw).expect("serialise"),
            HELVETICA_NO_EMBED,
            &e,
        );
        out.response
            .value
            .expect("an open reply")
            .get("doc")
            .and_then(serde_json::Value::as_u64)
            .expect("a document handle")
    }

    /// Open a fallback loader on `doc` holding one real claim.
    fn open_fallback(
        w: &mut Worker,
        clock: &selis_sandbox::FixedClock,
        cancel: &CancelToken,
        doc: u64,
    ) -> serde_json::Value {
        let raw = serde_json::json!({
            "v": PROTOCOL_VERSION,
            "id": 2,
            "op": "fallbackOpen",
            "doc": doc,
            "claims": [claim()],
        });
        call(w, clock, cancel, &raw)
    }

    /// Deliver the real face bytes as `fallbackFace`'s attachment.
    fn deliver_face(
        w: &mut Worker,
        clock: &selis_sandbox::FixedClock,
        cancel: &CancelToken,
        doc: u64,
        id: u64,
    ) -> serde_json::Value {
        let raw = serde_json::json!({
            "v": PROTOCOL_VERSION,
            "id": id,
            "op": "fallbackFace",
            "doc": doc,
            "face": SANS_NAME,
            "status": 200,
            "len": SANS.len(),
        });
        let e = worker_env(clock, cancel);
        let out = w.handle(&serde_json::to_vec(&raw).expect("serialise"), SANS, &e);
        out.response.value.expect("a delivery reply")
    }

    /// Send `fallbackClose` and answer with its value.
    fn close_face(
        w: &mut Worker,
        clock: &selis_sandbox::FixedClock,
        cancel: &CancelToken,
        doc: u64,
        face: &str,
    ) -> serde_json::Value {
        let raw = serde_json::json!({
            "v": PROTOCOL_VERSION,
            "id": 3,
            "op": "fallbackClose",
            "doc": doc,
            "face": face,
        });
        call(w, clock, cancel, &raw)
    }

    fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
        v.get(key)
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{key} is a list"))
            .iter()
            .filter_map(|e| e.as_str().map(str::to_owned))
            .collect()
    }

    fn number(v: &serde_json::Value, key: &str) -> u64 {
        v.get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("{key} is a number"))
    }

    /// The whole point of the op, over the wire: a delivered face goes back, and
    /// every field a shell reads says so.
    #[test]
    fn fallback_close_gives_the_face_back_and_reports_it() {
        let clock = selis_sandbox::FixedClock(0);
        let cancel = CancelToken::new();
        let mut w = Worker::new();
        let doc = open_fixture(&mut w, &clock, &cancel);
        open_fallback(&mut w, &clock, &cancel, doc);
        let held = deliver_face(&mut w, &clock, &cancel, doc, 2);
        assert_eq!(held.get("adopted"), Some(&serde_json::json!(true)));
        assert_eq!(strings(&held, "loaded"), vec![SANS_NAME.to_owned()]);
        assert!(number(&held, "residentBytes") > 0);

        let before = number(&held, "revision");
        let out = close_face(&mut w, &clock, &cancel, doc, SANS_NAME);
        assert_eq!(out.get("closed"), Some(&serde_json::json!(true)));
        assert_eq!(out.get("released"), Some(&serde_json::json!(SANS_NAME)));
        assert_eq!(
            out.get("releasedBytes").and_then(serde_json::Value::as_u64),
            Some(u64::try_from(SANS.len()).unwrap_or(u64::MAX))
        );
        // **The leak assertion.** Nothing resident, and reported that way.
        assert_eq!(number(&out, "residentBytes"), 0, "the bytes are gone");
        assert!(strings(&out, "loaded").is_empty(), "no face is resident");
        assert!(number(&out, "revision") > before, "a repaint is owed");

        // And the no-op answer, for a shell that polls.
        let noop = close_face(&mut w, &clock, &cancel, doc, SANS_NAME);
        assert_eq!(noop.get("closed"), Some(&serde_json::json!(false)));
        assert_eq!(
            number(&noop, "revision"),
            number(&out, "revision"),
            "still no repaint owed"
        );
    }

    /// **The stranding guard, over the wire.** After the close the same document
    /// must still be able to take the same face back with no second
    /// `fallbackOpen` — that is what "the loader stays installed" buys, and a
    /// teardown that detached it could not do this. The response also names the
    /// face in `needs`, so a shell cannot read "evicted" as "nothing wanted".
    #[test]
    fn a_closed_face_is_still_owed_and_the_loader_stays_installed() {
        let clock = selis_sandbox::FixedClock(0);
        let cancel = CancelToken::new();
        let mut w = Worker::new();
        let doc = open_fixture(&mut w, &clock, &cancel);
        open_fallback(&mut w, &clock, &cancel, doc);
        let _ = deliver_face(&mut w, &clock, &cancel, doc, 2);

        let out = close_face(&mut w, &clock, &cancel, doc, SANS_NAME);
        assert_eq!(
            strings(&out, "needs"),
            vec![SANS_NAME.to_owned()],
            "an evicted face is 'not here yet', and the shell is told so"
        );
        assert!(
            strings(&out, "unavailable").is_empty(),
            "and it is not 'never arrives' either"
        );

        // The same op delivers it again: no re-open, no new claim.
        let again = deliver_face(&mut w, &clock, &cancel, doc, 4);
        assert_eq!(
            again.get("adopted"),
            Some(&serde_json::json!(true)),
            "the loader is still armed: a closed face is re-acquirable"
        );
        assert_eq!(strings(&again, "loaded"), vec![SANS_NAME.to_owned()]);
        assert!(number(&again, "residentBytes") > 0);
    }

    /// A close with nothing delivered is a no-op, not an error: a shell evicting
    /// speculatively must not get a typed failure for evicting nothing.
    #[test]
    fn closing_before_anything_was_delivered_is_a_no_op() {
        let clock = selis_sandbox::FixedClock(0);
        let cancel = CancelToken::new();
        let mut w = Worker::new();
        let doc = open_fixture(&mut w, &clock, &cancel);
        let opened = open_fallback(&mut w, &clock, &cancel, doc);
        let out = close_face(&mut w, &clock, &cancel, doc, SANS_NAME);
        assert_eq!(out.get("closed"), Some(&serde_json::json!(false)));
        assert_eq!(number(&out, "residentBytes"), 0);
        assert_eq!(
            number(&out, "revision"),
            number(&opened, "revision"),
            "nothing changed, so nothing is owed"
        );
    }

    /// The two ways a close can be wrong about *where*, and both are typed.
    /// "That document is gone" and "you never called `fallbackOpen`" are
    /// different bugs, and a shell that confuses them shows the wrong message.
    #[test]
    fn a_close_against_a_stale_doc_or_a_loaderless_doc_is_typed() {
        let clock = selis_sandbox::FixedClock(0);
        let cancel = CancelToken::new();
        let mut w = Worker::new();
        let doc = open_fixture(&mut w, &clock, &cancel);
        let e = worker_env(&clock, &cancel);

        let stale = serde_json::json!({
            "v": PROTOCOL_VERSION, "id": 3, "op": "fallbackClose",
            "doc": doc + 999, "face": SANS_NAME,
        });
        let out = w.handle(&serde_json::to_vec(&stale).expect("serialise"), &[], &e);
        assert_eq!(
            out.response.code,
            Some(Code::BindingBadHandle.id()),
            "BINDING_BAD_HANDLE"
        );

        let loaderless = serde_json::json!({
            "v": PROTOCOL_VERSION, "id": 3, "op": "fallbackClose",
            "doc": doc, "face": SANS_NAME,
        });
        let out = w.handle(
            &serde_json::to_vec(&loaderless).expect("serialise"),
            &[],
            &e,
        );
        assert_eq!(
            out.response.code,
            Some(Code::BindingBadArgument.id()),
            "BINDING_BAD_ARGUMENT"
        );
        let detail = out.response.detail.clone().unwrap_or_default();
        assert!(
            detail.contains("fallbackOpen"),
            "the detail names the op that was never called; got {detail:?}"
        );
    }

    /// A face name no build knows is refused through the interning table rather
    /// than interned, so a close cannot be used to make the guest retain an
    /// arbitrary string for the life of the loader. It is a no-op, not an error,
    /// because it names nothing resident.
    #[test]
    fn closing_an_unknown_face_name_releases_nothing() {
        let clock = selis_sandbox::FixedClock(0);
        let cancel = CancelToken::new();
        let mut w = Worker::new();
        let doc = open_fixture(&mut w, &clock, &cancel);
        open_fallback(&mut w, &clock, &cancel, doc);
        let out = close_face(&mut w, &clock, &cancel, doc, "Comic Sans");
        assert_eq!(out.get("closed"), Some(&serde_json::json!(false)));
        assert!(strings(&out, "loaded").is_empty());
    }
}
