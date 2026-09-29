//! The Worker wire protocol, schema version 1 (SL-4.WASM.01).
//!
//! The JS shell (Worker) and the engine exchange JSON messages over
//! `postMessage`, with at most one binary attachment per message
//! (`24-BINDINGS-SPEC.md §2`, ADR-P0042). This module is the normative schema:
//! every message round-trips through [`serde_json`] in both directions, and
//! the same types compile for `wasm32` (the guest) and natively (the
//! conformance harness and the fuzz target).
//!
//! # The wire, in one paragraph
//!
//! A request is `{"v":1,"id":N,"op":...}` where `op` selects one of the
//! request bodies below (`open`, `close`, `page`, `render`, `text`,
//! `textLayer`, `search`, `outline`, `pageLabels`, `destinations`, `pageLinks`,
//! `rangeOpen`, `rangeChunk`, `rangeClose`, `mutate`, `save`, `cancel`,
//! `memoryStats`, `memoryPressure`). A response is `{"v":1,"id":N,...}` with
//! exactly one of: `ok:true` + `value` (the op's result object), `ok:false` +
//! `code` + `message` + `docState` (+ optional engine-owned `detail`), or
//! `progress` (`{fraction, stage}`, reserved for the threaded shell path —
//! the single-threaded guest reports progress through the exported progress
//! slot instead). Field names are camelCase; error codes are the numeric ids
//! from `selis-error`'s registry (`xtask/codes.toml` — the bindings never
//! invent a taxonomy), and `docState` uses the registry's `doc_state` values
//! so the UI always knows whether the user's work survived.
//!
//! # Binary attachments
//!
//! Document bytes (`open` with a `bytes` source) arrive as the request's
//! attachment; range responses (`rangeChunk`, SL-4.WASM.06) arrive the same
//! way, and the body is the *only* thing that op's attachment can carry —
//! there is no field for a request body, which is how "never upload the
//! document" (ADR-P0016) holds across the wire and not just in the guest.
//! Rendered pixels and extracted text leave as the response's
//! attachment. The attachment is declared in the message body (`len` on
//! `bytes` and on `rangeChunk`, `format` + dimensions on render, `length` on text) and validated
//! before use. Over the cdylib ABI the shell's glue copies both buffers into
//! guest linear memory (`selis_input_alloc`) and hands over their guest
//! addresses; the wire format itself never contains a pointer.
//!
//! # Versioning
//!
//! `v` is on every message. A guest that cannot interpret a request answers
//! with `BINDING_UNSUPPORTED_OP` (6017) rather than guessing, so a newer
//! shell against an older engine degrades into typed errors, not silent
//! misbehaviour. The Phase 5 edit surface extends `mutate`/`save` bodies
//! behind their own envelope schemas (`selis-mutate/1`); this module pins
//! those envelopes now so the schema does not move when the ops arrive.

use serde::{Deserialize, Serialize};

/// The protocol schema version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// An opaque document handle (`24-BINDINGS-SPEC.md §2`: handles, not
/// structures). The engine mints them on `open`; the shell treats them as
/// unforgeable ids and the engine answers every stale or unknown one with
/// `BINDING_BAD_HANDLE`. Over the wire it is a plain number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocHandle {
    /// The raw handle value (opaque; never 0).
    pub raw: u64,
}

/// Upper bound on search matches returned in one response (the response is
/// JSON; beyond this the UI pages or narrows the query).
pub const MAX_SEARCH_MATCHES: u32 = 10_000;

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// One client → engine request: envelope + op body (see the module docs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestMessage {
    /// The schema version; must be [`PROTOCOL_VERSION`].
    pub v: u32,
    /// Correlation id. Responses repeat it; `cancel` targets it.
    pub id: u64,
    /// The operation body, tagged by the `"op"` field.
    #[serde(flatten)]
    pub op: RequestOp,
}

/// The operation bodies, tagged `op` on the wire (`24-BINDINGS-SPEC.md §2`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum RequestOp {
    /// Open a document. The `bytes` source carries the document as the
    /// request's binary attachment; the other descriptors name a source the
    /// shell owns (an opaque id, never bytes).
    #[serde(rename_all = "camelCase")]
    Open {
        /// Where the document bytes come from.
        src: SourceDescriptor,
        /// Resource limits for this document's operations.
        #[serde(skip_serializing_if = "Option::is_none")]
        budget: Option<BudgetProfile>,
    },
    /// Release a document and its guest memory.
    #[serde(rename_all = "camelCase")]
    Close {
        /// The handle to release.
        doc: DocHandle,
    },
    /// One page's metadata (effective page size in points).
    #[serde(rename_all = "camelCase")]
    Page {
        /// The document handle.
        doc: DocHandle,
        /// Zero-based page number.
        page: u32,
    },
    /// Render a page (or a tile of one) into the response's attachment.
    #[serde(rename_all = "camelCase")]
    Render {
        /// The document handle.
        doc: DocHandle,
        /// Zero-based page number.
        page: u32,
        /// Canvas and tile selection.
        params: RenderParams,
    },
    /// One page's text layer: per-character selection geometry (SL-4.UI.04).
    ///
    /// A separate op from `Text` because the payloads are different
    /// things, not different encodings: `Text` answers with a UTF-8
    /// attachment, while a layer is rectangles and a per-character index
    /// that has to line up with the text. Folding it in as a `TextFormat`
    /// would have made one op answer two shapes, and the shells that
    /// validate replies field by field (the MV3 extension, SL-4.EXT.03)
    /// would then have to guess which arrived.
    #[serde(rename = "textLayer")]
    TextLayer {
        /// The document handle.
        doc: DocHandle,
        /// Zero-based page number.
        page: u32,
    },
    /// Extract one page's text. The text leaves as the response's attachment.
    #[serde(rename_all = "camelCase")]
    Text {
        /// The document handle.
        doc: DocHandle,
        /// Zero-based page number.
        page: u32,
        /// Output format; defaults to [`TextFormat::Text`].
        #[serde(skip_serializing_if = "Option::is_none")]
        format: Option<TextFormat>,
    },
    /// Search page text across a page range.
    #[serde(rename_all = "camelCase")]
    Search {
        /// The document handle.
        doc: DocHandle,
        /// The query (matched after Unicode normalisation, case-insensitive
        /// unless [`SearchOpts::match_case`]).
        query: String,
        /// Search options.
        #[serde(skip_serializing_if = "Option::is_none")]
        opts: Option<SearchOpts>,
    },
    /// The document's outline (bookmark) tree, in document order
    /// (SL-3.DOC-NAV).
    ///
    /// One op per structure rather than a `navigation` op with a `what`
    /// selector, for the reason the `Text`/`TextLayer` split already states:
    /// an op that answers two shapes forces every shell that validates replies
    /// field by field to guess which arrived.
    ///
    /// The response separates three states the viewer must not confuse:
    /// `present:false` (this document has no outline), `truncated:true` (it has
    /// one this engine could not read in full), and a complete tree. A refusal
    /// — budget or cancellation — is a typed error response, not an empty list.
    #[serde(rename_all = "camelCase")]
    Outline {
        /// The document handle.
        doc: DocHandle,
    },
    /// The document's `/PageLabels` ranges, in document order (SL-3.DOC.08).
    #[serde(rename_all = "camelCase")]
    PageLabels {
        /// The document handle.
        doc: DocHandle,
    },
    /// The document's `/Dests` name tree (SL-3.DOC-NAV), used to resolve the
    /// named destinations outline items and `/GoTo` actions may carry.
    #[serde(rename_all = "camelCase")]
    Destinations {
        /// The document handle.
        doc: DocHandle,
    },
    /// One page's link annotations, in `/Annots` order (SL-3.DOC-NAV).
    ///
    /// **Every action class the document wrote crosses this boundary by name**,
    /// including the classes ADR-P0020 disables and any class the engine does
    /// not model. The engine reports; the viewer decides. An op that filtered
    /// them would make "is the viewer refusing this, or does the engine not
    /// know about it?" unanswerable from the outside — which is the question
    /// `apps/ui/src/viewer/links.test.ts` exists to answer.
    ///
    /// An out-of-range page answers `PAGE_OUT_OF_RANGE`, never an empty list:
    /// "page 9000 of a 3-page document has no links" and "this host cannot read
    /// links" must not look alike.
    #[serde(rename_all = "camelCase")]
    PageLinks {
        /// The document handle.
        doc: DocHandle,
        /// Zero-based page number.
        page: u32,
    },
    /// Begin a range fetch for a remote document (SL-4.WASM.06).
    ///
    /// The response carries the first `RangeRequest` the engine wants; the
    /// shell answers with `rangeChunk`. The engine plans and judges, the shell
    /// only moves bytes — see `crate::httprange` for why the split falls there
    /// and what each hostile origin does.
    ///
    /// Cancellation: the `cancel` message whose `target` is a not-yet-arrived
    /// `rangeChunk` answers `CANCELLED` without executing (the pre-cancel
    /// channel a single-threaded Worker can actually deliver), an in-flight
    /// fetch is aborted by the shell and reported as `rangeChunk` with
    /// `status: 0` — which the engine observes through the cancel token at its
    /// next tick — and `rangeClose` releases a transfer outright.
    #[serde(rename_all = "camelCase")]
    RangeOpen {
        /// The document URL. The engine never interprets it and never sends
        /// anything to it: the only requests it can name are bodyless `GET`s
        /// for byte ranges.
        url: String,
        /// The document length when the caller knows one. A claim, not a fact:
        /// the origin confirms or contradicts it in the first response.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        /// The working unit of one range request, in bytes. Absent means the
        /// engine's 1 MiB default; a non-positive value is hostile input and
        /// falls back to it rather than producing an empty request.
        #[serde(skip_serializing_if = "Option::is_none")]
        chunk: Option<u64>,
        /// Resource limits for this transfer (ADR-P0006: the caller chooses,
        /// the engine never picks its own).
        #[serde(skip_serializing_if = "Option::is_none")]
        budget: Option<BudgetProfile>,
    },
    /// Deliver one range response, as the request's binary attachment.
    ///
    /// The attachment is the response body and nothing else; there is no field
    /// to put a request body in, which is how the no-upload invariant
    /// (ADR-P0016) holds across the wire and not only in the guest.
    ///
    /// # Malformed Input
    ///
    /// `len` must match the attachment exactly, `status` is bounded to a `u16`,
    /// and the header strings are parsed defensively — a stale `transfer`, a
    /// mismatched `len` and an unparseable `contentRange` each answer a typed
    /// error, release the transfer, and leave the document registry untouched.
    #[serde(rename_all = "camelCase")]
    RangeChunk {
        /// The transfer this answers.
        transfer: u64,
        /// The offset the engine asked for; the engine rejects a response
        /// whose `Content-Range` names a different one.
        start: u64,
        /// The HTTP status, or `0` for "no response arrived" — a network
        /// error, a CORS refusal or an abort, which the host cannot tell apart.
        status: u16,
        /// The raw `Content-Range` response header, if the origin sent one and
        /// CORS let the shell read it.
        #[serde(skip_serializing_if = "Option::is_none")]
        content_range: Option<String>,
        /// The raw `Content-Length` response header, if readable.
        #[serde(skip_serializing_if = "Option::is_none")]
        content_length: Option<u64>,
        /// The attachment's length in bytes; must match it exactly.
        len: u64,
    },
    /// Release a range transfer and every byte it has assembled.
    ///
    /// This is also the abort path: a shell whose `AbortSignal` fires sends it
    /// rather than abandoning the transfer silently, so the guest's memory is
    /// reclaimed instead of waiting for a tab cap.
    #[serde(rename_all = "camelCase")]
    RangeClose {
        /// The transfer to release.
        transfer: u64,
    },
    /// Open the lazy CJK chunk loader and name the first chunk it wants
    /// (SL-4.WASM.07).
    ///
    /// The `claims` are the manifest's record of what each chunk file *is* —
    /// id, SHA-256, raw length, published path. The guest checks them, keeps
    /// them, and answers with the first [`CjkRequestBody`] it wants (or none, if
    /// the document has needed nothing yet). The guest plans and judges; the
    /// shell only moves bytes (ADR-P0043 §3) — see `crate::cjkchunk`.
    ///
    /// `core` is the subsetted core's bytes, carried as the request's binary
    /// attachment; it is loaded once and is never evictable. `unserved` are the
    /// ranges the payload has **no file** for (`served_by: null`), so the guest
    /// never asks for them and can tell a shell to say "this payload has no
    /// Korean" rather than "still loading".
    #[serde(rename_all = "camelCase")]
    CjkOpen {
        /// The document whose renders consult the set.
        doc: DocHandle,
        /// The manifest's per-chunk claims.
        claims: Vec<CjkClaimBody>,
        /// Ranges the payload cannot serve at all.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unserved: Vec<String>,
        /// The core subset's length in bytes; must match the attachment.
        core_len: u64,
        /// Resource limits for this loader (ADR-P0006: the caller chooses).
        #[serde(skip_serializing_if = "Option::is_none")]
        budget: Option<BudgetProfile>,
    },
    /// Deliver one chunk file, as the request's binary attachment.
    ///
    /// The attachment is the file and nothing else — there is no field to put a
    /// request body in, which is how the no-upload invariant (ADR-P0016) holds
    /// across the wire here too. The guest verifies the bytes against the claim
    /// it holds, adopts or refuses them, and answers with the next request it
    /// wants or with `done: true`.
    ///
    /// # Malformed Input
    ///
    /// A stale or unknown `doc`, a `len` that does not match the attachment, a
    /// `chunk` the loader never claimed, and an unrecognised `status` are each a
    /// typed error that leaves the loader untouched. A chunk whose bytes
    /// contradict the manifest's digest is **not** an error — it is a counted
    /// failed attempt, so the attempt bound (not the shell) decides when to stop
    /// asking.
    #[serde(rename_all = "camelCase")]
    CjkChunk {
        /// The document whose loader this answers.
        doc: DocHandle,
        /// The chunk id being delivered.
        chunk: String,
        /// The HTTP status, or `0` for "no response arrived" (network error, CORS
        /// refusal, or an abort — the host cannot tell those apart).
        status: u16,
        /// The attachment's length in bytes; must match it exactly.
        len: u64,
    },
    /// Release one resident CJK chunk's bytes (SL-4.WASM.07).
    ///
    /// The FONT.10-F1 lever: the shell decides its own storage pressure and
    /// gives bytes back through here, rather than discovering the ceiling as a
    /// quota exception. The response reports the new resident total and the set's
    /// revision — **a revision change is the repaint signal** (ADR-P0043 §3). An
    /// id that names nothing resident answers `closed: false` and does *not* move
    /// the revision, so a polling shell never repaints for a no-op.
    #[serde(rename_all = "camelCase")]
    CjkClose {
        /// The document whose loader releases the chunk.
        doc: DocHandle,
        /// The chunk id to release.
        chunk: String,
    },
    /// Install the lazy-fallback face loader and answer with the first face the
    /// document wants (SL-3.FONT.12).
    ///
    /// The `claims` are the `selis-fallback/1` manifest's record of what each
    /// face file *is* - name, SHA-256, decompressed length, published path. The
    /// guest checks them and keeps them; the shell only moves bytes.
    ///
    /// **There is no core attachment here, unlike `cjkOpen`.** The two faces the
    /// web module embeds are already inside the wasm binary, so there are no
    /// built-in bytes to hand over - the loader starts empty and everything it
    /// holds was fetched and digest-checked.
    #[serde(rename_all = "camelCase")]
    FallbackOpen {
        /// The document whose renders consult the set.
        doc: DocHandle,
        /// The manifest's per-face claims.
        claims: Vec<FaceClaimBody>,
        /// Faces the payload carries no file for, so the guest never asks.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unavailable: Vec<String>,
        /// Resource limits for this loader (ADR-P0006: the caller chooses).
        #[serde(skip_serializing_if = "Option::is_none")]
        budget: Option<BudgetProfile>,
    },
    /// Deliver one fallback face, as the request's binary attachment.
    ///
    /// The attachment is the font and nothing else - there is no field to put a
    /// request body in, which is how the no-upload invariant (ADR-P0016) holds
    /// across the wire here too. The guest verifies the bytes against the claim
    /// it holds, adopts or refuses them, and answers with the next face it wants
    /// or with `done: true`.
    ///
    /// # Malformed Input
    ///
    /// A stale or unknown `doc`, a `len` that does not match the attachment, a
    /// face the loader never claimed, and an unrecognised `status` are each a
    /// typed error that leaves the loader untouched. A face whose bytes
    /// contradict the manifest's digest is **not** an error - it is a counted
    /// failed attempt, so the attempt bound (not the shell) decides when to stop
    /// asking.
    #[serde(rename_all = "camelCase")]
    FallbackFace {
        /// The document whose loader this answers.
        doc: DocHandle,
        /// The face name being delivered.
        face: String,
        /// The HTTP status, or `0` for "no response arrived".
        status: u16,
        /// The attachment's length in bytes; must match it exactly.
        len: u64,
    },
    /// Apply a mutation journal (Phase 5). The envelope schema is versioned
    /// now; v1 engines validate the envelope and answer
    /// `BINDING_UNSUPPORTED_OP` for every body they cannot execute.
    #[serde(rename_all = "camelCase")]
    Mutate {
        /// The document handle.
        doc: DocHandle,
        /// The versioned mutation envelope.
        mutation: MutationEnvelope,
    },
    /// Save the document (Phase 5): an incremental update appended to the
    /// original bytes, or an explicit rewrite.
    #[serde(rename_all = "camelCase")]
    Save {
        /// The document handle.
        doc: DocHandle,
        /// Incremental append or full rewrite.
        mode: SaveMode,
    },
    /// Cancel a request by id. If the target has not run yet it answers
    /// `CANCELLED` without executing; if it is already in flight the host
    /// must reach the exported cancel slot (shared memory) — a single-threaded
    /// Worker cannot process a message mid-op.
    #[serde(rename_all = "camelCase")]
    Cancel {
        /// The request id to cancel.
        target: u64,
    },
    /// Report guest memory accounting (SL-4.WASM.04): live/peak bytes, the
    /// 4 GiB wasm ceiling, and the tab cap the guest enforces against. The
    /// shell polls this to evict its own caches before the browser kills
    /// the tab.
    #[serde(rename_all = "camelCase")]
    MemoryStats,
    /// Memory-pressure signal from the shell (SL-4.WASM.04): the guest drops
    /// what it can (the pre-cancel queue today, tile/display-list caches when
    /// they land) and reports what remains. `level` is `0` low, `1`
    /// moderate, `2` critical (clamped — higher levels evict at least as
    /// much). The out-of-band twin is the `selis_memory_pressure` export for
    /// threaded hosts; single-threaded shells send this message between ops.
    #[serde(rename_all = "camelCase")]
    MemoryPressure {
        /// Pressure level (`0` low, `1` moderate, `2` critical).
        level: u32,
    },
}

/// One chunk file's manifest claim, as it crosses the wire (SL-4.WASM.07).
///
/// The wire form of [`crate::cjkchunk::ChunkClaim`]. `id` is a `String` here
/// rather than the static table's `&'static str` because it is *untrusted*: the
/// loader resolves it against the table and refuses anything it does not carry,
/// which is the only way a manifest can be checked rather than believed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CjkClaimBody {
    /// The chunk id (`cjk/<id>.ttf` in the manifest).
    pub id: String,
    /// Lowercase-hex SHA-256 of the raw file bytes.
    pub sha256: String,
    /// The raw file length in bytes.
    pub raw_bytes: u64,
    /// The path the manifest published the file at.
    pub url: String,
}

/// One fallback face's manifest claim, as it crosses the wire (SL-3.FONT.12).
///
/// The wire form of [`crate::fallbackchunk::FaceClaim`]. `name` is a `String`
/// because it is *untrusted*: the loader resolves it against its closed table
/// of twelve and refuses anything else, which is what makes a claim checkable
/// rather than merely carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceClaimBody {
    /// The face name (`LiberationSans-Regular`).
    pub name: String,
    /// Lowercase-hex SHA-256 of the decompressed font bytes.
    pub sha256: String,
    /// The decompressed length in bytes.
    pub raw_bytes: u64,
    /// The path the manifest published the file at.
    pub url: String,
}

/// Where a document's bytes come from (`24-BINDINGS-SPEC.md §2`).
///
/// Handles, not structures: every non-inline descriptor names something the
/// shell owns (a `Blob`, an OPFS path, a File System Access handle, a URL)
/// and the main thread never hands over document bytes it has parsed. The
/// Worker-side `DocSource` adapters land with SL-4.WASM.05 (blob/opfs/fsa) and
/// SL-4.WASM.06 (http-range).
///
/// `http-range` is the odd one out, and deliberately so: a remote document
/// has no bytes to hand over yet, so it cannot be named by `open` at all. It
/// is fetched by the `rangeOpen` / `rangeChunk` / `rangeClose` exchange, and a
/// v1 engine answers `open` with this descriptor `BINDING_UNSUPPORTED_OP`
/// pointing there — `open` promises a document handle, and there is nothing to
/// mint one from until the bytes exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SourceDescriptor {
    /// Inline document bytes, carried as the request's binary attachment.
    Bytes {
        /// The attachment's length in bytes; must match it exactly.
        len: u64,
    },
    /// A `File`/`Blob` the shell picked up (picker, drop, paste).
    #[serde(rename_all = "camelCase")]
    Blob {
        /// The shell's opaque id for the blob.
        source_id: String,
    },
    /// An Origin Private File System path.
    Opfs {
        /// The OPFS path to open.
        path: String,
    },
    /// A File System Access handle (save-in-place, Phase 5).
    #[serde(rename_all = "camelCase")]
    Fsa {
        /// The shell's opaque id for the handle.
        handle_id: String,
    },
    /// A remote document fetched by byte ranges.
    #[serde(rename_all = "camelCase")]
    HttpRange {
        /// The document URL.
        url: String,
        /// The content length when the server reports one.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
}

/// The resource limits a client chooses for a document (ADR-P0006: callers
/// choose budgets; the engine never picks its own).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BudgetProfile {
    /// The named surface shape; its profile is the base.
    pub surface: SurfaceName,
    /// Per-resource overrides applied on top of the surface profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<BudgetOverrides>,
}

/// The surface shapes the wire exposes (the `selis-sandbox` profiles).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SurfaceName {
    /// Thumbnails and first-paint probes.
    Thumbnail,
    /// The viewer default.
    #[default]
    Viewer,
    /// Editing operations (Phase 5).
    Editor,
    /// Offline batch runs.
    Batch,
    /// Multi-tenant server workloads.
    Server,
}

/// Per-resource overrides over a surface profile. Absent fields inherit the
/// surface profile's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BudgetOverrides {
    /// Peak allocation, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Deadline, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    /// Maximum nesting depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u16>,
    /// Maximum indirect objects resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects: Option<u32>,
    /// Maximum rasterised samples.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixels: Option<u64>,
}

/// Canvas and tile selection for a render (`ADR-P0011`'s tile path: the UI
/// requests the tile it needs; the engine owns the decomposition).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RenderParams {
    /// Render resolution in dots per inch (72 = 1 pt per px). Default 72.
    #[serde(default = "default_dpi")]
    pub dpi: f64,
    /// Override the page-to-device matrix (a, b, c, d, e, f). When absent
    /// the engine derives it from the page view at `dpi` (honouring
    /// `/Rotate`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix: Option<[f64; 6]>,
    /// When present, only this device-pixel rectangle of the rendered canvas
    /// is returned (the attachment is the tile, `w`×`h`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tile: Option<Tile>,
}

fn default_dpi() -> f64 {
    72.0
}

/// A device-pixel tile rectangle (x, y, w, h).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tile {
    /// Left edge in device pixels.
    pub x: u32,
    /// Top edge in device pixels.
    pub y: u32,
    /// Tile width in device pixels.
    pub w: u32,
    /// Tile height in device pixels.
    pub h: u32,
}

/// Text output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextFormat {
    /// Plain text (the `selis extract` default).
    #[default]
    Text,
    /// Structured JSON (`selis-extract/1`).
    Json,
    /// Markdown.
    Markdown,
}

/// Search options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchOpts {
    /// Match case-sensitively (default: insensitive).
    #[serde(default)]
    pub match_case: bool,
    /// Inclusive page range `{from, to}`; the whole document when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<PageRange>,
    /// Return at most this many matches (bounded by
    /// [`MAX_SEARCH_MATCHES`]); `truncated` in the response says when the
    /// list was cut.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_matches: Option<u32>,
}

/// An inclusive page range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRange {
    /// First page (zero-based, inclusive).
    pub from: u32,
    /// Last page (zero-based, inclusive).
    pub to: u32,
}

/// The versioned mutation envelope (Phase 5 fills the op set; the schema is
/// locked here so the wire does not move when they arrive).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutationEnvelope {
    /// The mutation schema tag, e.g. `"selis-mutate/1"`. An engine that does
    /// not recognise the schema answers `BINDING_UNSUPPORTED_OP`.
    pub schema: String,
    /// The mutation ops, opaque at this layer (typed and validated by
    /// `selis-pdf-edit` when the surface lands).
    pub ops: Vec<serde_json::Value>,
}

/// Save mode (ADR-P0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaveMode {
    /// Append an incremental update; the original bytes stay untouched.
    Incremental,
    /// Explicit full rewrite with render-verification.
    Rewrite,
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// One engine → client response (see the module docs for the wire shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseMessage {
    /// The schema version.
    pub v: u32,
    /// The request id this answers (or a progress report about).
    pub id: u64,
    /// `true` for `value` responses, `false` for typed failures, absent on
    /// `progress` reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// The op's result object (`ok: true` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// The registry error code (`ok: false` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<u32>,
    /// The registry's English user message (`ok: false` only; the shell
    /// localises by `code` — ADR-P0034).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Engine-owned, content-free failure context (never document bytes,
    /// ADR-P0017).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What happened to the user's document (`ok: false` only; the registry's
    /// `doc_state` value).
    #[serde(rename = "docState", skip_serializing_if = "Option::is_none")]
    pub doc_state: Option<String>,
    /// Mid-operation progress (`{fraction, stage}`); reserved for the
    /// threaded shell path — the single-threaded guest reports progress
    /// through the exported slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressBody>,
}

/// A progress report body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressBody {
    /// Completion in basis points, 0..=10_000.
    pub fraction: u32,
    /// The stage identifier (`open` | `render` | `text` | `search`).
    pub stage: String,
}

/// A `progress` report the shell can post for a long op (the threaded
/// path); the v1 guest never emits it over the wire.
#[must_use]
pub fn progress_response(v: u32, id: u64, body: ProgressBody) -> ResponseMessage {
    ResponseMessage {
        v,
        id,
        ok: None,
        value: None,
        code: None,
        message: None,
        detail: None,
        doc_state: None,
        progress: Some(body),
    }
}

impl ResponseMessage {
    /// An `ok: true` response carrying the op's result object.
    #[must_use]
    pub fn ok(v: u32, id: u64, value: serde_json::Value) -> Self {
        Self {
            v,
            id,
            ok: Some(true),
            value: Some(value),
            code: None,
            message: None,
            detail: None,
            doc_state: None,
            progress: None,
        }
    }

    /// An `ok: false` response carrying the typed error (`code` from the
    /// registry, `message` the registry's English string, `docState` the
    /// registry's document outcome).
    #[must_use]
    pub fn error(v: u32, id: u64, e: &selis_error::Error) -> Self {
        let code = e.code();
        Self {
            v,
            id,
            ok: Some(false),
            value: None,
            code: Some(code.id()),
            message: Some(
                selis_error::user_message(&selis_error::EnglishMessages, code).into_owned(),
            ),
            detail: e.ctx().detail.clone(),
            doc_state: Some(doc_state_str(code.doc_state()).to_owned()),
            progress: None,
        }
    }

    /// Validate the one-of shape: exactly one of `value` / (`code`,
    /// `message`, `doc_state`) / `progress` is set, matching `ok`.
    ///
    /// # Errors
    ///
    /// A static-message protocol error describing the violated shape; the
    /// caller answers it as `BINDING_BAD_ARGUMENT`.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.v != PROTOCOL_VERSION {
            return Err("unsupported schema version");
        }
        if let Some(p) = &self.progress {
            if self.ok.is_some() || self.value.is_some() || self.code.is_some() {
                return Err("progress response carries neither ok nor value nor code");
            }
            if p.fraction > 10_000 {
                return Err("progress fraction out of range");
            }
            return Ok(());
        }
        match self.ok {
            Some(true) => {
                if self.value.is_none() || self.code.is_some() || self.message.is_some() {
                    return Err("ok response must carry value and no error fields");
                }
                Ok(())
            }
            Some(false) => {
                if self.value.is_some() || self.progress.is_some() {
                    return Err("error response must not carry value or progress");
                }
                if self.code.is_none() || self.message.is_none() || self.doc_state.is_none() {
                    return Err("error response must carry code, message and docState");
                }
                Ok(())
            }
            None => Err("response must set ok or progress"),
        }
    }
}

/// The registry's `doc_state` spelling for a document outcome (the wire uses
/// the PascalCase registry values, not the log kebab-case).
///
/// The wildcard covers future registry states: when `selis-error` grows a
/// `DocState` variant, this mapping must name it (the shell localises by the
/// exact string).
#[must_use]
pub fn doc_state_str(state: selis_error::DocState) -> &'static str {
    match state {
        selis_error::DocState::NotLoaded => "NotLoaded",
        selis_error::DocState::Loaded => "Loaded",
        selis_error::DocState::PartiallyLoaded => "PartiallyLoaded",
        selis_error::DocState::Unchanged => "Unchanged",
        selis_error::DocState::Modified => "Modified",
        _ => "Loaded",
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unwrap_in_result,
        clippy::panic
    )]
    use super::*;

    /// Every message round-trips through serialize → deserialize unchanged
    /// (the DoD's round-trip discipline, at the schema level).
    #[test]
    fn every_request_round_trips() {
        let msgs = vec![
            r#"{"v":1,"id":7,"op":"open","src":{"kind":"bytes","len":10},"budget":{"surface":"viewer"}}"#,
            r#"{"v":1,"id":8,"op":"open","src":{"kind":"blob","sourceId":"b1"},"budget":{"surface":"editor","overrides":{"bytes":1000,"wallMs":500,"depth":8,"objects":10,"pixels":100}}}"#,
            r#"{"v":1,"id":9,"op":"open","src":{"kind":"opfs","path":"/doc.pdf"}}"#,
            r#"{"v":1,"id":10,"op":"open","src":{"kind":"fsa","handleId":"h1"}}"#,
            r#"{"v":1,"id":11,"op":"open","src":{"kind":"http-range","url":"https://x/y.pdf","size":5}}"#,
            r#"{"v":1,"id":12,"op":"close","doc":3}"#,
            r#"{"v":1,"id":13,"op":"page","doc":3,"page":0}"#,
            r#"{"v":1,"id":14,"op":"render","doc":3,"page":0,"params":{"dpi":150,"matrix":[1,0,0,1,0,0],"tile":{"x":0,"y":0,"w":10,"h":10}}}"#,
            r#"{"v":1,"id":15,"op":"render","doc":3,"page":1,"params":{}}"#,
            r#"{"v":1,"id":16,"op":"text","doc":3,"page":0,"format":"json"}"#,
            r#"{"v":1,"id":17,"op":"search","doc":3,"query":"hi","opts":{"matchCase":true,"pages":{"from":0,"to":3},"maxMatches":5}}"#,
            r#"{"v":1,"id":18,"op":"mutate","doc":3,"mutation":{"schema":"selis-mutate/1","ops":[]}}"#,
            r#"{"v":1,"id":19,"op":"save","doc":3,"mode":"incremental"}"#,
            r#"{"v":1,"id":20,"op":"save","doc":3,"mode":"rewrite"}"#,
            r#"{"v":1,"id":21,"op":"cancel","target":17}"#,
            r#"{"v":1,"id":22,"op":"memoryStats"}"#,
            r#"{"v":1,"id":23,"op":"memoryPressure","level":1}"#,
            // SL-4.WASM.06: the range exchange. `rangeChunk` is the only op
            // whose attachment is a network response body, and it is the only
            // op with no field a document could be uploaded through.
            r#"{"v":1,"id":24,"op":"rangeOpen","url":"https://example.test/a.pdf"}"#,
            r#"{"v":1,"id":25,"op":"rangeOpen","url":"https://example.test/b.pdf","size":423,"chunk":65536,"budget":{"surface":"viewer"}}"#,
            r#"{"v":1,"id":26,"op":"rangeChunk","transfer":1,"start":0,"status":206,"contentRange":"bytes 0-422/423","contentLength":423,"len":423}"#,
            r#"{"v":1,"id":27,"op":"rangeChunk","transfer":1,"start":0,"status":0,"len":0}"#,
            r#"{"v":1,"id":28,"op":"rangeClose","transfer":1}"#,
        ];
        for s in msgs {
            let msg: RequestMessage =
                serde_json::from_str(s).unwrap_or_else(|e| panic!("deserialise {s}: {e}"));
            assert_eq!(msg.v, PROTOCOL_VERSION);
            let out = serde_json::to_string(&msg).expect("serialise");
            let back: RequestMessage = serde_json::from_str(&out).expect("re-deserialise");
            assert_eq!(msg, back, "round-trip drift for {s}");
        }
    }

    #[test]
    fn render_defaults_dpi_and_optional_fields() {
        let msg: RequestMessage =
            serde_json::from_str(r#"{"v":1,"id":1,"op":"render","doc":2,"page":0,"params":{}}"#)
                .expect("parses");
        match msg.op {
            RequestOp::Render { params, .. } => {
                assert_eq!(params.dpi, 72.0);
                assert_eq!(params.matrix, None);
                assert_eq!(params.tile, None);
            }
            other => panic!("wrong op: {other:?}"),
        }
    }

    #[test]
    fn text_format_defaults_to_plain_text() {
        let msg: RequestMessage =
            serde_json::from_str(r#"{"v":1,"id":1,"op":"text","doc":2,"page":0}"#).expect("parses");
        match msg.op {
            RequestOp::Text { format, .. } => assert_eq!(format, None),
            other => panic!("wrong op: {other:?}"),
        }
    }

    #[test]
    fn responses_round_trip_and_validate() {
        let ok = ResponseMessage::ok(
            PROTOCOL_VERSION,
            1,
            serde_json::json!({ "doc": 1, "pages": 3 }),
        );
        assert!(ok.validate().is_ok());
        let e = selis_error::Error::new(selis_error::Code::PageOutOfRange);
        let err = ResponseMessage::error(PROTOCOL_VERSION, 2, &e);
        assert!(err.validate().is_ok());
        assert_eq!(err.code, Some(selis_error::Code::PageOutOfRange.id()));
        assert_eq!(err.doc_state.as_deref(), Some("Loaded"));
        let prog = progress_response(
            PROTOCOL_VERSION,
            3,
            ProgressBody {
                fraction: 5_000,
                stage: "render".to_owned(),
            },
        );
        assert!(prog.validate().is_ok());
        for msg in [ok, err, prog] {
            let s = serde_json::to_string(&msg).expect("serialise");
            let back: ResponseMessage = serde_json::from_str(&s).expect("re-deserialise");
            assert_eq!(msg, back);
            assert!(back.validate().is_ok());
        }
    }

    #[test]
    fn malformed_response_shapes_are_rejected_by_validate() {
        let mut bad = ResponseMessage::ok(PROTOCOL_VERSION, 1, serde_json::json!(null));
        bad.value = None;
        assert_eq!(
            bad.validate(),
            Err("ok response must carry value and no error fields")
        );
        let empty: ResponseMessage =
            serde_json::from_str(r#"{"v":1,"id":1,"ok":false}"#).expect("parses");
        assert_eq!(
            empty.validate(),
            Err("error response must carry code, message and docState")
        );
    }
}
