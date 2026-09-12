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
//! 3. **Route** the op to the engine — every engine call runs under the
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
//!   (shared memory — a SAB watcher on the web, another thread natively);
//!   the op observes it at its next budget tick. The harness proves the
//!   slot→token→tick→`CANCELLED` path both natively (deterministic, via the
//!   injected clock) and over the guest ABI.
//!
//! # Progress
//!
//! Every stage boundary writes through the injected [`ProgressSink`] — the
//! guest binds that to the exported progress slot (pollable out-of-band by
//! threaded hosts); native tests capture it directly. The wire `progress`
//! response shape is reserved for the threaded shell path (WASM.03).
//!
//! # What v1 does not do (yet, honestly)
//!
//! * Source adapters other than inline `bytes` answer
//!   `BINDING_UNSUPPORTED_OP` until SL-4.WASM.05/06.
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
use selis_pdf_content::text::TextGlyph;
use selis_pdf_engine::{Session, TinySkiaBackend};
use selis_pdf_text::{LineWithMcid, TextLine};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, Clock, Resource, Surface};

use crate::protocol::{
    BudgetOverrides, BudgetProfile, DocHandle, MutationEnvelope, PageRange, RenderParams,
    RequestMessage, RequestOp, ResponseMessage, SaveMode, SearchOpts, SourceDescriptor,
    SurfaceName, TextFormat, MAX_SEARCH_MATCHES, PROTOCOL_VERSION,
};

/// Hard ceiling on one inline document (the same hostile-input bound the
/// raw render ABI applies; larger sources arrive as streaming adapters).
pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Hard ceiling on any canvas dimension in device pixels (a hostile media
/// box is rejected before the pixel buffer exists).
pub const MAX_CANVAS_DIM: u32 = 16_384;

/// Maximum simultaneously open documents (the guest registry bound; further
/// opens answer `BUDGET_BYTES` until one is closed).
pub const MAX_OPEN_DOCS: u64 = 64;

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
    /// Raw RGBA8 pixels, `width`×`height`×4 bytes, row-major, top-left
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
pub struct Worker {
    docs: BTreeMap<u64, OpenDoc>,
    next_handle: u64,
    pre_cancelled: Vec<u64>,
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

impl Worker {
    /// A fresh, empty worker.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            docs: BTreeMap::new(),
            next_handle: 1,
            pre_cancelled: Vec::new(),
        }
    }

    /// The number of currently open documents.
    #[must_use]
    pub fn open_docs(&self) -> usize {
        self.docs.len()
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
        // trampoline — a bug below the boundary is a typed INTERNAL_PANIC
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
            RequestOp::Search { doc, query, opts } => self.op_search(id, doc, query, opts, env),
            RequestOp::Mutate { doc, mutation } => self.op_mutate(doc, mutation),
            RequestOp::Save { doc, mode } => self.op_save(doc, mode),
            RequestOp::Cancel { target } => self.op_cancel(id, target),
        }
    }

    // -- open / close ------------------------------------------------------

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
                let arrived = u64::try_from(payload.len()).unwrap_or(u64::MAX);
                if arrived != len {
                    return Err(err!(
                        Code::BindingBadArgument,
                        during = "wasm-worker",
                        detail = "payload length does not match the bytes descriptor"
                    ));
                }
                if len > MAX_INPUT_BYTES {
                    return Err(err!(
                        Code::SourceTooLarge,
                        during = "wasm-worker",
                        detail = "inline source exceeds the 64 MiB protocol bound"
                    ));
                }
                if self.docs.len() as u64 >= MAX_OPEN_DOCS {
                    return Err(err!(
                        Code::BudgetBytes,
                        during = "wasm-worker",
                        detail = "document registry full; close a document first"
                    ));
                }
                env.progress.progress(id, Stage::Open, 0);
                let session = Session::open(payload.to_vec(), &budget, env.clock)?;
                let pages = u32::try_from(session.len()).unwrap_or(u32::MAX);
                let handle = self.mint_handle()?;
                self.docs.insert(handle.raw, OpenDoc { session, budget });
                Ok(Outgoing::ok(
                    id,
                    serde_json::json!({ "doc": handle.raw, "pages": pages }),
                ))
            }
            SourceDescriptor::Blob { .. }
            | SourceDescriptor::Opfs { .. }
            | SourceDescriptor::Fsa { .. }
            | SourceDescriptor::HttpRange { .. } => Err(err!(
                Code::BindingUnsupportedOp,
                during = "wasm-worker",
                detail =
                    "source adapter lands with SL-4.WASM.05/06; only inline bytes are accepted"
            )),
        }
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
        self.docs.remove(&doc.raw).ok_or_else(bad_handle)?;
        Ok(Outgoing::ok(id, serde_json::json!({ "closed": true })))
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
        let opened = self.docs.get(&doc.raw).ok_or_else(bad_handle)?;
        let idx = page_index(page);
        if idx >= opened.session.len() {
            return Err(err!(Code::PageOutOfRange, during = "wasm-worker"));
        }
        let view = opened
            .session
            .page_view(idx, params.dpi)
            .ok_or_else(|| err!(Code::PageOutOfRange, during = "wasm-worker"))?;
        let (w, h) = (view.width, view.height);
        if w == 0 || h == 0 || w > MAX_CANVAS_DIM || h > MAX_CANVAS_DIM {
            return Err(err!(
                Code::BudgetPixels,
                during = "wasm-worker",
                detail = "canvas exceeds the protocol dimension cap"
            ));
        }
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
        // allocation exists (ADR-P0006).
        let canvas_pixels = u64::from(w).saturating_mul(u64::from(h));
        g.charge(Resource::Pixels, canvas_pixels)?;
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
        opened
            .session
            .render_page(idx, &mut backend, ctm, &budget, &mut g)?;
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
        let value = if tile_json.is_null() {
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
        let dl = opened.session.page_display_list(idx, &budget, &mut g)?;
        let mcid = opened
            .session
            .mcid_order(&budget, &mut g)
            .ok()
            .filter(|v| !v.is_empty());
        let assembled = page_text(&dl, mcid.as_deref(), &mut g)?;
        let (lines, line_texts) = (&assembled.lines, &assembled.line_texts);
        let fmt = format.unwrap_or_default();
        let out = match fmt {
            TextFormat::Text => selis_pdf_text::to_text(lines, line_texts),
            TextFormat::Json => {
                let structured =
                    selis_pdf_text::structured(lines, line_texts, &assembled.run_texts);
                selis_pdf_text::to_json(&structured)
            }
            TextFormat::Markdown => selis_pdf_text::to_markdown(lines, line_texts),
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
        let span = u64::from(to)
            .saturating_sub(u64::from(from))
            .saturating_add(1);
        for p in from..=to {
            // Cancellation and the deadline land between pages (and inside
            // the engine, at its own ticks).
            g.tick()?;
            let idx = page_index(p);
            let dl = opened.session.page_display_list(idx, &budget, &mut g)?;
            let mcid = opened
                .session
                .mcid_order(&budget, &mut g)
                .ok()
                .filter(|v| !v.is_empty());
            let assembled = page_text(&dl, mcid.as_deref(), &mut g)?;
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
        // Record the target unless it is already answered (dispatched) — the
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

/// Resolve the search page range: explicit `{from, to}` (from ≤ to, `to`
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
/// # Errors
///
/// `BINDING_BAD_ARGUMENT` for a wall override that cannot be expressed in
/// nanoseconds (overflow is treated as hostile input, not clamped).
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
/// `BINDING_BAD_ARGUMENT` when the tile does not fit (defence in depth —
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
}

/// The assembled text lines, their recovered texts, and their per-run texts
/// for one page's display list (the same pipeline `selis extract` drives).
fn page_text(
    dl: &selis_pdf_content::display_list::DisplayList,
    mcid_order: Option<&[u32]>,
    g: &mut BudgetGuard<'_>,
) -> Result<PageText> {
    let mut glyphs: Vec<TextGlyph> = Vec::new();
    for op in &dl.ops {
        if let selis_pdf_content::display_list::Op::Text { at, state, runs } = op {
            let mcid = state.mcid;
            for run in runs {
                for &code in &run.glyphs {
                    glyphs.push(TextGlyph {
                        code,
                        at: *at,
                        font: run.font.clone(),
                        size: run.size,
                        mcid,
                    });
                }
            }
        }
    }
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
    Ok(PageText {
        lines: ordered.lines,
        line_texts,
        run_texts,
    })
}

/// The recovered text of a line (code → Unicode char), with a space between
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
