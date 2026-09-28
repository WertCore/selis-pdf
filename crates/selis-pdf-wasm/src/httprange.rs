//! The `HttpRangeSource` fetch driver (SL-4.WASM.06).
//!
//! # Where the driver lives, and why
//!
//! The obvious place for "fetch a PDF over HTTP" is the host: the shell owns
//! `fetch`, `AbortController` and CORS. The Do's text ("the JS side of range
//! fetching") says where the *socket* is, not where the *decisions* belong, and
//! this module is the part that has to be in the guest:
//!
//! * **ADR-P0005 / `check-purity`.** No crate below L4 may name a network API,
//!   and the guest has no `fetch` import at all — the WASM.01 ABI is
//!   `postMessage` plus exported memory. A driver that issued requests would be
//!   a driver that cannot be built.
//! * **The Worker protocol's direction of travel.** A request goes host →
//!   guest and exactly one response comes back. A guest that blocked on the
//!   network would deadlock the only channel it has, so the guest cannot pull.
//! * **What must not be shell-implemented.** Which ranges to ask for, whether
//!   a response is acceptable, when to degrade, when to stop asking, how many
//!   bytes may be spent — these are the hostile-input decisions, and a shell
//!   that got them wrong would hand the engine a spliced document with no way
//!   for the engine to tell. Putting them in the guest makes them testable
//!   without a browser, deterministic, and unbypassable: the guest *emits the
//!   request*, so a shell can only obey it.
//!
//! So the split is: **the guest plans and judges, the host only moves bytes.**
//! The wire ops are `rangeOpen` (mint a transfer, receive the first range to
//! ask for), `rangeChunk` (deliver one response, receive either the next range
//! or nothing) and `rangeClose` (release; this is also how a shell's
//! `AbortSignal` reaches the engine).
//!
//! # No upload, ever
//!
//! [`RangeRequest`] has no body field and no method field. The only thing a
//! shell can do with one is issue a bodyless `GET` for a named byte range, so
//! "never send the document anywhere" (WEB.03's invariant, ADR-P0016) is
//! structural here rather than a promise in a comment: there is no value to
//! put a document in. A shell that wanted to upload would have to smuggle the
//! bytes through a field the type does not have — and
//! `no_request_carries_a_body` in the tests is the standing check.
//!
//! # The hostile-server decision table
//!
//! A range source is a promise the origin may break. Each break has one
//! documented outcome; none of them is "carry on and hope".
//!
//! | What the origin did | How it is detected | Outcome |
//! |---|---|---|
//! | Serves the range (`206` + matching `Content-Range`) | status, parsed header | bytes accepted, next range planned |
//! | **Ignores `Range`**, answers `200` with the whole body | status `200` on the first response | the documented fallback: the whole body is the document, `degraded: true`, one request, no more asking |
//! | Answers `200` *after* ranges were already accepted | status `200` with `received > 0` | `SOURCE_CHANGED` — appending a full body to a partial prefix yields a document that is neither the old one nor the new one |
//! | `206` with **no / unparseable `Content-Range`** | header absent or malformed | `IO_READ_FAILED`, detail `origin-refused-ranges`. The fallback is the host's plain full `GET`; the engine cannot invent an offset |
//! | `206` whose `Content-Range` **start is not the offset requested** | parsed header vs. request | `SOURCE_CHANGED` |
//! | `Content-Range` **total changes** mid-transfer | parsed header vs. what was already learned | `SOURCE_CHANGED` — the origin changed its mind |
//! | **Truncated body** (shorter than the range, not at end of file) | body length vs. `Content-Range` span | `IO_READ_FAILED` — a hole in the middle is not a document |
//! | Short body that reaches exactly the total | `start + len >= total` | accepted: a legitimate short final range |
//! | Body that contradicts `Content-Length` | header vs. delivered length | `IO_READ_FAILED` |
//! | `416` | status | `IO_READ_FAILED`, detail `range-not-satisfiable` |
//! | No HTTP response at all — network error, **CORS refusal**, abort | status `0` | retryable, bounded (below) |
//! | `5xx` | status | retryable, bounded |
//! | Empty body on a `206` | length `0` | `IO_READ_FAILED` — accepting it would spin forever |
//!
//! # The budget (ADR-P0006)
//!
//! A driver that retries a hostile origin without a bound is a
//! denial-of-service vector pointed at the user's own browser, so the bound
//! lives in the guest, which is the only party that cannot be talked out of it:
//!
//! * **bytes** — each accepted body is charged to `Resource::Bytes` against the
//!   caller's chosen [`Budget`] *before* it is copied, and the transfer's
//!   running total is checked against the same limit, so a long transfer cannot
//!   spend a per-dispatch allowance in slices;
//! * **requests** — [`MAX_REQUESTS`] is a hard ceiling on how many range
//!   requests one transfer may plan, and [`MAX_ATTEMPTS_PER_RANGE`] on how many
//!   may target the same offset. Exceeding either is `IO_READ_FAILED` naming
//!   the bound, not a hang;
//! * **wall / cancellation** — every response ticks the injected guard, so the
//!   deadline and the [`CancelToken`](selis_sandbox::CancelToken) are observed
//!   at each round trip. A cancelled transfer is dropped whole.
//!
//! # A partially-assembled document
//!
//! There isn't one, by decision. The driver holds a contiguous prefix, but the
//! bytes only leave once the prefix is the whole document, so a caller sees
//! exactly two outcomes — a complete `Vec<u8>`, or a typed error. The
//! "degraded but complete" full-body case is reported as *complete and
//! degraded*, which is a property of a whole document, not a partial one. The
//! alternative (hand back the prefix and let the parser try) would mean a PDF
//! header with no body parses as a broken document instead of reporting why it
//! is broken.
//!
//! # ADR-P0007
//!
//! The driver never writes to bytes it was given. [`HttpRangeDriver::accept`]
//! copies the response body into its own assembly buffer under the budget and
//! leaves the caller's bytes alone — the same rule the incremental writer
//! follows for the original document. `accept_never_mutates_the_caller_buffer`
//! in the tests is the standing check.
//!
//! # Budget
//!
//! [`HttpRangeDriver::new`] takes the caller's [`Budget`] and every later
//! growth of the assembly buffer is charged against it before it happens, so
//! the driver's memory is inside the same ceiling as the engine's.
//!
//! # Malformed Input
//!
//! Status codes, `Content-Range` / `Content-Length` header text and body
//! lengths all arrive from the network. Each is bounded and total: an
//! unparseable header, an inverted range, a body that disagrees with its
//! headers, a hostile `u64` total and a stale transfer id are each a typed
//! error that leaves the driver (and the document registry) untouched — never
//! a panic, never a spliced document.

use std::sync::{Arc, Mutex};

use selis_error::{err, Code, Result};
use selis_io::{Availability, DocSource, FetchFn, HttpRangeSource, RangeSet};
use selis_sandbox::{Budget, BudgetGuard, Resource};

/// The working unit of one range request: 1 MiB, matching the shell's
/// `DEFAULT_RANGE_CHUNK` (`apps/web/host/src/handoff-flow.ts`). A request
/// larger than this spends a slow link on bytes nobody has asked for yet; a
/// smaller one multiplies the request count against the [`MAX_REQUESTS`] bound.
pub const DEFAULT_CHUNK_BYTES: u64 = 1024 * 1024;

/// Hard ceiling on range requests one transfer may plan (ADR-P0006: the
/// denial-of-service bound). 512 requests of 1 MiB is 512 MiB of transfer, far
/// past the protocol's 64 MiB source bound, so a cooperative origin never
/// reaches it; an origin that answers every request with one byte reaches it
/// and is stopped there rather than looping.
pub const MAX_REQUESTS: u32 = 512;

/// Hard ceiling on range requests that may target the *same* offset. Two
/// failures for one offset is a broken origin, not a flaky network.
pub const MAX_ATTEMPTS_PER_RANGE: u32 = 3;

/// One bodyless `GET` for a half-open byte range `[start, end)`.
///
/// The type is deliberately impoverished: no method, no headers, no body. A
/// shell can only fetch bytes from a URL, and the engine is the party that
/// names which ones — see the module docs on the no-upload invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeRequest {
    /// First byte to ask for.
    pub start: u64,
    /// One past the last byte to ask for (clamped to the document length).
    pub end: u64,
}

impl RangeRequest {
    /// The `Range` header value for this request (`bytes=first-last`).
    #[must_use]
    pub fn header_value(self) -> String {
        let last = self.end.saturating_sub(1);
        format!("bytes={}-{last}", self.start)
    }

    /// The number of bytes this request asks for.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether this request asks for no bytes at all.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

/// What the host observed for one range request, as the driver needs to judge
/// it.
///
/// # Malformed Input
///
/// Every field is untrusted: the numbers come off the wire and the headers are
/// text the origin chose. The body is copied on acceptance and never aliased,
/// so a caller may reuse or drop its buffer immediately.
#[derive(Debug, Clone)]
pub struct ChunkReport {
    /// The offset the driver asked for.
    pub start: u64,
    /// The HTTP status, or `0` when no response arrived at all (network error,
    /// CORS refusal, or an aborted request — the host cannot tell those apart
    /// and neither can the driver, so they share one bounded-retry path).
    pub status: u16,
    /// The raw `Content-Range` response header, if the origin sent one *and*
    /// the shell was allowed to read it. A cross-origin origin that omits
    /// `Access-Control-Expose-Headers` makes this `None` even though the
    /// header was on the wire — which is exactly the CORS fallback trigger.
    pub content_range: Option<String>,
    /// The raw `Content-Length` response header, if readable.
    pub content_length: Option<u64>,
    /// The response body.
    pub body: Vec<u8>,
}

/// What the driver decided to do with a [`ChunkReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkStep {
    /// Ask for this range next.
    Request(RangeRequest),
    /// Nothing arrived (network error, CORS refusal, `5xx`): ask the same range
    /// again, bounded by [`MAX_ATTEMPTS_PER_RANGE`]. No bytes were kept.
    Retry(RangeRequest),
    /// The document is whole. `degraded` marks the full-body fallback, where
    /// the origin ignored `Range` and sent everything in one response — and
    /// which therefore is not randomly addressable.
    Complete {
        /// The origin did not honour ranges.
        degraded: bool,
    },
}

/// A parsed `Content-Range: bytes <start>-<end>/<total>` response header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentRange {
    /// First byte of the served range.
    start: u64,
    /// Last byte of the served range, inclusive.
    end: u64,
    /// The document length, or `None` for the `*` form.
    total: Option<u64>,
}

/// Parse a `Content-Range` header. `None` on any failure — a header is
/// untrusted text and a malformed one must not be a panic (ADR-P0013).
fn parse_content_range(raw: &str) -> Option<ContentRange> {
    let rest = raw.trim().strip_prefix("bytes ")?;
    let (span, total) = rest.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        n => Some(n.parse::<u64>().ok()?),
    };
    Some(ContentRange { start, end, total })
}

/// The range-fetch driver for one remote document.
///
/// Construct with [`HttpRangeDriver::new`], then alternate
/// [`HttpRangeDriver::plan`] and [`HttpRangeDriver::accept`] until the driver
/// answers [`ChunkStep::Complete`] or an error. The driver owns the request
/// budget, so a shell cannot talk it into asking for more.
#[derive(Debug)]
pub struct HttpRangeDriver {
    /// The document URL, carried for diagnostics and the shell's request log.
    /// The driver never interprets it: no upload means there is no second
    /// destination for anything to travel to.
    url: String,
    /// The length the origin has stated, once it has stated one. Seeded from
    /// the caller's `size` hint so the very first request can be clamped.
    total: Option<u64>,
    /// The next offset to request.
    next: u64,
    /// The working unit of one request.
    chunk: u64,
    /// The contiguous prefix assembled so far.
    assembled: Vec<u8>,
    /// Range requests planned so far.
    requests: u32,
    /// Requests planned for the offset currently being worked on.
    attempts: u32,
    /// Bytes accepted so far, checked against `budget.bytes` per transfer.
    received: u64,
    /// Whether the origin honoured ranges. `false` after the full-body
    /// fallback, which is what `is_random_access` reports downstream.
    supports_ranges: bool,
    /// The budget the caller chose.
    budget: Budget,
}

impl HttpRangeDriver {
    /// Start a transfer for `url`.
    ///
    /// `announced` is the length the caller believes the document has (the
    /// `size` of a `http-range` source descriptor); it is a claim to be
    /// confirmed, not a fact. `chunk` is the working unit and `None` means
    /// [`DEFAULT_CHUNK_BYTES`]; a zero `chunk` is hostile input and falls back
    /// to the default rather than producing an empty request that would spin.
    ///
    /// # Budget
    ///
    /// The driver's ceiling is `budget` from the first request. Nothing
    /// document-sized is allocated here: the assembly buffer grows as bytes are
    /// accepted, and each growth is charged before it happens.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        announced: Option<u64>,
        chunk: Option<u64>,
        budget: Budget,
    ) -> Self {
        let chunk = chunk.filter(|c| *c > 0).unwrap_or(DEFAULT_CHUNK_BYTES);
        Self {
            url: url.into(),
            total: announced,
            next: 0,
            chunk,
            assembled: Vec::new(),
            requests: 0,
            attempts: 0,
            received: 0,
            supports_ranges: true,
            budget,
        }
    }

    /// The document URL this transfer is fetching.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Bytes accepted so far.
    #[must_use]
    pub fn received(&self) -> u64 {
        self.received
    }

    /// The document length, once the origin (or the caller) has stated one.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// Whether the origin honoured byte ranges. `false` after the documented
    /// full-body fallback.
    #[must_use]
    pub fn supports_ranges(&self) -> bool {
        self.supports_ranges
    }

    /// Whether the whole document is assembled.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.total.is_some_and(|t| self.received >= t)
    }

    /// How many range requests have been planned.
    #[must_use]
    pub fn requests(&self) -> u32 {
        self.requests
    }

    /// The budget this transfer runs under — the one the caller chose at
    /// `rangeOpen` (ADR-P0006: the caller picks, the engine never does).
    #[must_use]
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    /// The end of the next window, clamped to a known total.
    fn window_end(&self) -> u64 {
        match self.total {
            Some(t) => self.next.saturating_add(self.chunk).min(t),
            None => self.next.saturating_add(self.chunk),
        }
    }

    /// The next range to ask for, or `None` when the document is already whole.
    ///
    /// # Budget
    ///
    /// Planning is where the denial-of-service bound is enforced, because every
    /// plan is a request the host will really issue: past [`MAX_REQUESTS`] in
    /// total, or past [`MAX_ATTEMPTS_PER_RANGE`] on one offset, this is
    /// `IO_READ_FAILED` rather than another request.
    ///
    /// # Errors
    ///
    /// `IO_READ_FAILED` when a request bound is exhausted.
    pub fn plan(&mut self) -> Result<Option<RangeRequest>> {
        if self.is_complete() {
            return Ok(None);
        }
        if self.requests >= MAX_REQUESTS {
            return Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "range request bound exhausted; the origin is not making progress"
            ));
        }
        if self.attempts >= MAX_ATTEMPTS_PER_RANGE {
            return Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "range attempt bound exhausted for this offset"
            ));
        }
        let start = self.next;
        let end = self.window_end();
        if end <= start {
            return Ok(None);
        }
        self.requests += 1;
        self.attempts += 1;
        Ok(Some(RangeRequest { start, end }))
    }
}

impl HttpRangeDriver {
    /// Judge one response and advance the transfer.
    ///
    /// `guard` is the caller's per-dispatch budget guard: it is ticked (wall
    /// deadline, cancellation) before anything is copied, and charged with the
    /// body's length before it is copied, so a hostile origin cannot make the
    /// guest allocate first and account afterwards.
    ///
    /// # Budget
    ///
    /// Bytes are charged twice, deliberately: `guard` bounds this dispatch and
    /// `self.budget` bounds the transfer, so neither one oversized response nor
    /// many small ones can exceed the caller's chosen ceiling.
    ///
    /// # Malformed Input
    ///
    /// A `200` after ranges were accepted, a `Content-Range` naming a
    /// different offset, a total that changed mid-transfer, a truncated body
    /// with a hole in the middle, an empty `206`, a body that contradicts
    /// `Content-Length`, an unexpected status and a `416` are each a typed
    /// error (see the module's decision table). None of them panics, and none
    /// of them splices accepted bytes into a document that is not whole: the
    /// assembled prefix only ever leaves through
    /// [`HttpRangeDriver::take_document`], which requires completeness.
    pub fn accept(
        &mut self,
        report: &ChunkReport,
        guard: &mut BudgetGuard<'_>,
    ) -> Result<ChunkStep> {
        guard.tick()?;
        let body_len = u64::try_from(report.body.len()).unwrap_or(u64::MAX);
        if self.received.saturating_add(body_len) > self.budget.bytes {
            return Err(err!(
                Code::BudgetBytes,
                during = "http-range",
                detail = "range transfer exceeded the document byte budget"
            ));
        }
        guard.charge(Resource::Bytes, body_len)?;

        if report.status == 0 || report.status >= 500 {
            // No response, or a server-side failure. Nothing is kept and the
            // same offset is retried; `plan` is what bounds that, so the
            // attempt counter is *not* advanced here — re-planning is the only
            // way to spend an attempt.
            return Ok(ChunkStep::Retry(RangeRequest {
                start: self.next,
                end: self.window_end(),
            }));
        }
        match report.status {
            416 => Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "range-not-satisfiable"
            )),
            206 => self.accept_partial(report),
            200 => self.accept_full_body(report),
            _ => Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "unexpected response status for a range request"
            )),
        }
    }

    /// `200`: the origin ignored `Range` and sent the whole document.
    ///
    /// This is the documented fallback, and it is only safe on the *first*
    /// response. Once a prefix has been accepted, a `200` means the origin
    /// changed its mind about what it serves, and appending it would produce a
    /// document that is neither the old one nor the new one.
    fn accept_full_body(&mut self, report: &ChunkReport) -> Result<ChunkStep> {
        if self.received > 0 {
            return Err(err!(
                Code::SourceChanged,
                during = "http-range",
                detail = "origin stopped honouring Range mid-transfer"
            ));
        }
        let len = u64::try_from(report.body.len()).unwrap_or(u64::MAX);
        if len == 0 {
            return Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "origin-refused-ranges"
            ));
        }
        if self.total.is_some_and(|announced| announced != len) {
            return Err(err!(
                Code::SourceChanged,
                during = "http-range",
                detail = "origin document length changed mid-transfer"
            ));
        }
        self.total = Some(len);
        self.received = len;
        self.supports_ranges = false;
        self.assembled.clone_from(&report.body);
        Ok(ChunkStep::Complete { degraded: true })
    }
}

impl HttpRangeDriver {
    /// `206`: the origin honoured the range. Judge the headers, then the body.
    fn accept_partial(&mut self, report: &ChunkReport) -> Result<ChunkStep> {
        let Some(range) = report.content_range.as_deref().and_then(parse_content_range) else {
            // `Access-Control-Expose-Headers` did not name `Content-Range`, or
            // the origin never sent it, or it was not the documented shape. The
            // bytes cannot be placed, and an engine that guessed the offset
            // would splice a document out of parts it cannot identify. The
            // fallback is the host's plain full `GET`; this typed error is the
            // signal that it is needed.
            return Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "origin-refused-ranges"
            ));
        };
        if range.start != report.start {
            return Err(err!(
                Code::SourceChanged,
                during = "http-range",
                detail = "origin served a different byte range than the one requested"
            ));
        }
        if let Some(total) = range.total {
            if self.total.is_some_and(|known| known != total) {
                return Err(err!(
                    Code::SourceChanged,
                    during = "http-range",
                    detail = "origin document length changed mid-transfer"
                ));
            }
            self.total = Some(total);
        }
        let body_len = u64::try_from(report.body.len()).unwrap_or(u64::MAX);
        if body_len == 0 {
            return Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "empty partial response; refusing to retry indefinitely"
            ));
        }
        let served = range.end.saturating_sub(range.start).saturating_add(1);
        if body_len != served {
            // A short body is only legitimate at the very end of the document;
            // anywhere else it is a hole, and a hole is not a document.
            let reaches_end = range
                .total
                .is_some_and(|t| range.start.saturating_add(body_len) >= t);
            if !reaches_end {
                return Err(err!(
                    Code::IoReadFailed,
                    during = "http-range",
                    detail = "truncated partial response"
                ));
            }
        }
        if report.content_length.is_some_and(|l| l != body_len) {
            return Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "response body does not match its Content-Length"
            ));
        }
        // Copied, never aliased: the caller's buffer is not ours to keep, and
        // the assembly buffer is ours to own (ADR-P0007).
        self.assembled.extend_from_slice(&report.body);
        self.received = self.received.saturating_add(body_len);
        self.next = self.received;
        // Progress was made, so the per-offset attempt allowance resets.
        self.attempts = 0;
        Ok(match self.is_complete() {
            true => ChunkStep::Complete { degraded: false },
            false => ChunkStep::Request(RangeRequest {
                start: self.next,
                end: self.window_end(),
            }),
        })
    }

    /// Take the assembled document, if it is whole.
    ///
    /// `None` for a partial transfer: there is no such thing as a
    /// partially-assembled document here (see the module docs).
    #[must_use]
    pub fn take_document(&mut self) -> Option<Vec<u8>> {
        let assembled = u64::try_from(self.assembled.len()).unwrap_or(u64::MAX);
        if self.is_complete() && self.received == assembled {
            Some(core::mem::take(&mut self.assembled))
        } else {
            None
        }
    }
}

impl HttpRangeDriver {
    /// Hand the assembled document to the IO layer's own [`HttpRangeSource`]
    /// and read it back out through `read_at`, so the bytes the engine finally
    /// parses travel the same `DocSource` path every other web adapter takes
    /// (SL-4.WASM.05) instead of a private shortcut.
    ///
    /// The source is *driven*, not bypassed: its fetch callback is what
    /// populates residency. `HttpRangeSource` coalesces with a read-ahead
    /// window whose size is its own business, so the walk below asks for one
    /// range at a time and steps to whatever window the callback was handed,
    /// rather than hard-coding a size that belongs to another crate. The walk
    /// is bounded by construction — each step strictly advances, and the
    /// document length is the ceiling.
    ///
    /// # Budget
    ///
    /// The buffer here is sized from the established total, and
    /// `take_document` (which requires the assembled prefix to *be* the
    /// document) runs first, so a hostile `Content-Range` total is rejected
    /// before anything document-sized is allocated.
    ///
    /// # Malformed Input
    ///
    /// An unestablished length, an assembled prefix that disagrees with the
    /// total, a source that will not make the prefix resident and a `Pending`
    /// hole are each `IO_READ_FAILED` — never a partially-filled buffer handed
    /// onward.
    ///
    /// # Errors
    ///
    /// `IO_READ_FAILED` when the document length was never established, when
    /// the assembled length contradicts it, when the range source refuses to
    /// serve the prefix, or when it reports a hole; `selis_io`'s own typed
    /// errors otherwise.
    pub fn drain_through_source(&mut self) -> Result<Vec<u8>> {
        let Some(total) = self.total else {
            return Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "document length was never established"
            ));
        };
        let Some(bytes) = self.take_document() else {
            return Err(err!(
                Code::IoReadFailed,
                during = "http-range",
                detail = "range transfer is not complete"
            ));
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != total {
            return Err(err!(
                Code::SourceChanged,
                during = "http-range",
                detail = "assembled length does not match the origin's total"
            ));
        }

        let data = Arc::new(bytes);
        // The end of the last window the source asked for, so the walk below
        // can step to it without knowing the read-ahead size.
        let window: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
        let sink = Arc::clone(&window);
        let payload = Arc::clone(&data);
        let fetch: Arc<FetchFn> = Arc::new(move |want: &RangeSet, out: &mut Vec<u8>| {
            let Some(start) = want.start() else {
                return Err("range source asked for no bytes".to_owned());
            };
            let end = want.end().unwrap_or(start);
            if let Ok(mut slot) = sink.lock() {
                *slot = end;
            }
            let begin = usize::try_from(start).map_err(|_| "offset out of range".to_owned())?;
            let stop = usize::try_from(end).map_err(|_| "offset out of range".to_owned())?;
            match payload.get(begin..stop) {
                Some(slice) => {
                    out.extend_from_slice(slice);
                    Ok(())
                }
                None => Err("requested range is not assembled".to_owned()),
            }
        });
        let source = HttpRangeSource::new(total, fetch);
        source.set_supports_ranges(self.supports_ranges);

        let mut offset: u64 = 0;
        while offset < total {
            if let Ok(mut slot) = window.lock() {
                *slot = 0;
            }
            source.request(&RangeSet::one(offset, offset.saturating_add(1)));
            let upto = window.lock().map(|g| *g).unwrap_or(0);
            if upto <= offset {
                // The source asked for nothing new: either the rest is already
                // resident, or it is stuck. The drain below tells the two
                // apart, so this never spins.
                break;
            }
            offset = upto;
        }

        let mut out = vec![0u8; usize::try_from(total).unwrap_or(usize::MAX)];
        let mut read: u64 = 0;
        while read < total {
            let at = usize::try_from(read).unwrap_or(usize::MAX);
            let buf = out.get_mut(at..).ok_or_else(|| {
                err!(
                    Code::IoReadFailed,
                    during = "http-range",
                    detail = "read offset outside the drained buffer"
                )
            })?;
            match source.read_at(read, buf)? {
                Availability::Filled(n) if n > 0 => {
                    read = read.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
                }
                Availability::Filled(_) => {
                    return Err(err!(
                        Code::IoReadFailed,
                        during = "http-range",
                        detail = "range source returned an empty fill"
                    ));
                }
                Availability::Pending { .. } => {
                    return Err(err!(
                        Code::IoReadFailed,
                        during = "http-range",
                        detail = "range source left a hole in the document"
                    ));
                }
                Availability::Eof => break,
            }
        }
        out.truncate(usize::try_from(read).unwrap_or(out.len()));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::arithmetic_side_effects
    )]

    use super::*;
    use selis_sandbox::{CancelToken, FixedClock, Surface};

    fn budget() -> Budget {
        Budget::profile(Surface::Viewer)
    }

    /// A driver over a `total`-byte document with a 4-byte working unit.
    fn driver(total: u64) -> HttpRangeDriver {
        HttpRangeDriver::new(
            "https://example.test/doc.pdf",
            Some(total),
            Some(4),
            budget(),
        )
    }

    /// The chunk a cooperative origin would send for `[start, end)`.
    fn cooperative(doc: &[u8], start: u64, end: u64) -> ChunkReport {
        let len = u64::try_from(doc.len()).unwrap_or(0);
        let stop = usize::try_from(end.min(len)).unwrap_or(0);
        let begin = usize::try_from(start).unwrap_or(0);
        let slice = doc.get(begin..stop).unwrap_or(&[]);
        let served = u64::try_from(slice.len()).unwrap_or(0);
        let last = start.saturating_add(served).saturating_sub(1);
        ChunkReport {
            start,
            status: 206,
            content_range: Some(format!("bytes {start}-{last}/{len}")),
            content_length: Some(served),
            body: slice.to_vec(),
        }
    }

    /// Drive a whole transfer against a cooperative origin, in place. The
    /// assembled bytes are left in the driver for the caller to take.
    fn fetch_all(d: &mut HttpRangeDriver, doc: &[u8]) {
        let clock = FixedClock(0);
        let cancel = CancelToken::new();
        let mut guard = d.budget.guard_with(&clock, cancel);
        let mut steps = 0u32;
        while let Some(req) = d.plan().expect("plan") {
            steps += 1;
            assert!(steps < 64, "the driver asked for more than 64 ranges");
            let report = cooperative(doc, req.start, req.end);
            let step = d.accept(&report, &mut guard).expect("accept");
            if let ChunkStep::Complete { .. } = step {
                return;
            }
        }
    }

    #[test]
    fn no_request_carries_a_body() {
        // The no-upload invariant, structurally: the only things a shell can
        // build from a `RangeRequest` are a header value and an offset pair.
        let req = RangeRequest {
            start: 0,
            end: 1024,
        };
        assert_eq!(req.header_value(), "bytes=0-1023");
        assert_eq!(req.len(), 1024);
        assert!(!req.is_empty());
        let json = serde_json::to_string(&serde_json::json!({
            "start": req.start,
            "end": req.end,
            "header": req.header_value(),
        }))
        .expect("serialise");
        for forbidden in ["\"body\"", "\"method\"", "\"upload\"", "\"post\"", "\"formData\""] {
            assert!(
                !json.contains(forbidden),
                "the request shape grew a {forbidden} field"
            );
        }
    }

    #[test]
    fn cooperative_server_assembles_the_document() {
        let doc = b"%PDF-1.7\n0123456789\n%%EOF".to_vec();
        let mut d = driver(u64::try_from(doc.len()).unwrap_or(0));
        fetch_all(&mut d, &doc);
        assert_eq!(d.take_document().expect("a complete document"), doc);
    }

    #[test]
    fn assembled_bytes_leave_through_the_io_layer_source() {
        // The driver does not hand the engine its own buffer: the bytes travel
        // the same `DocSource` path as every WASM.05 adapter.
        let doc = b"%PDF-1.7\n0123456789\n%%EOF".to_vec();
        let mut d = driver(u64::try_from(doc.len()).unwrap_or(0));
        let _ = fetch_all(&mut d, &doc);
        assert_eq!(d.drain_through_source().expect("drain"), doc);
    }

    #[test]
    fn server_ignoring_range_degrades_to_the_whole_body() {
        let doc = b"%PDF-1.7 whole body".to_vec();
        let mut d = HttpRangeDriver::new("https://example.test/d.pdf", None, Some(4), budget());
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let req = d.plan().expect("plan").expect("a first range");
        let report = ChunkReport {
            start: req.start,
            status: 200,
            content_range: None,
            content_length: Some(u64::try_from(doc.len()).unwrap_or(0)),
            body: doc.clone(),
        };
        assert_eq!(
            d.accept(&report, &mut guard).expect("accept"),
            ChunkStep::Complete { degraded: true }
        );
        assert!(!d.supports_ranges());
        assert_eq!(d.take_document().expect("document"), doc);
        assert!(
            d.plan().expect("plan").is_none(),
            "a degraded transfer asks for nothing more"
        );
    }

    #[test]
    fn range_ignored_mid_transfer_is_source_changed() {
        let doc = b"%PDF-1.7\n0123456789\n%%EOF".to_vec();
        let mut d = driver(u64::try_from(doc.len()).unwrap_or(0));
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let req = d.plan().expect("plan").expect("a first range");
        let report = cooperative(&doc, req.start, req.end);
        d.accept(&report, &mut guard).expect("accept");
        let late = ChunkReport {
            start: d.received(),
            status: 200,
            content_range: None,
            content_length: Some(u64::try_from(doc.len()).unwrap_or(0)),
            body: doc.clone(),
        };
        let e = d.accept(&late, &mut guard).expect_err("source changed");
        assert_eq!(e.code(), Code::SourceChanged);
        assert!(
            d.take_document().is_none(),
            "a failed transfer yields no document at all"
        );
    }

    #[test]
    fn missing_content_range_is_the_cors_fallback_signal() {
        let mut d = driver(20);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let req = d.plan().expect("plan").expect("a first range");
        let report = ChunkReport {
            start: req.start,
            status: 206,
            content_range: None,
            content_length: Some(4),
            body: b"%PDF".to_vec(),
        };
        let e = d.accept(&report, &mut guard).expect_err("no placement");
        assert_eq!(e.code(), Code::IoReadFailed);
        assert_eq!(e.ctx().detail.as_deref(), Some("origin-refused-ranges"));
    }

    #[test]
    fn unparseable_content_range_is_contained() {
        let mut d = driver(20);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let req = d.plan().expect("plan").expect("a first range");
        for raw in [
            "bytes=--/",
            "bytes=x-y/10",
            "bytes 0-3",
            "",
            "bytes 9-2/10",
            "bytes 0-3/",
            "bytes 0-3/18446744073709551616",
        ] {
            let report = ChunkReport {
                start: req.start,
                status: 206,
                content_range: Some(raw.to_owned()),
                content_length: Some(4),
                body: b"%PDF".to_vec(),
            };
            let e = d.accept(&report, &mut guard).expect_err("malformed header");
            assert_eq!(e.code(), Code::IoReadFailed, "raw = {raw:?}");
        }
    }

    #[test]
    fn content_range_for_another_offset_is_source_changed() {
        let mut d = driver(20);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let _ = d.plan().expect("plan").expect("a first range");
        let report = ChunkReport {
            start: 0,
            status: 206,
            content_range: Some("bytes 8-11/20".to_owned()),
            content_length: Some(4),
            body: b"%PDF".to_vec(),
        };
        assert_eq!(
            d.accept(&report, &mut guard)
                .expect_err("wrong offset")
                .code(),
            Code::SourceChanged
        );
    }

    #[test]
    fn a_total_that_changes_mid_transfer_is_source_changed() {
        let doc = b"%PDF-1.7\n0123456789\n%%EOF".to_vec();
        let mut d = driver(u64::try_from(doc.len()).unwrap_or(0));
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let req = d.plan().expect("plan").expect("a first range");
        let report = cooperative(&doc, req.start, req.end);
        d.accept(&report, &mut guard).expect("accept");
        let at = d.received();
        let report = ChunkReport {
            start: at,
            status: 206,
            content_range: Some(format!("bytes {at}-4095/4096")),
            content_length: Some(4),
            body: b"tail".to_vec(),
        };
        assert_eq!(
            d.accept(&report, &mut guard)
                .expect_err("changed its mind")
                .code(),
            Code::SourceChanged
        );
    }

    #[test]
    fn a_hole_in_the_middle_is_not_a_document() {
        let mut d = driver(64);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let req = d.plan().expect("plan").expect("a first range");
        // Claims bytes 0-15, delivers 4, and the total is nowhere near.
        let report = ChunkReport {
            start: req.start,
            status: 206,
            content_range: Some("bytes 0-15/64".to_owned()),
            content_length: Some(16),
            body: b"%PDF".to_vec(),
        };
        let e = d.accept(&report, &mut guard).expect_err("truncated");
        assert_eq!(e.code(), Code::IoReadFailed);
        assert_eq!(
            e.ctx().detail.as_deref(),
            Some("truncated partial response")
        );
        assert!(d.take_document().is_none());
    }

    #[test]
    fn a_short_final_range_is_accepted() {
        let doc = b"%PDF-1.7\n0123456789\n%%EOF".to_vec();
        let total = u64::try_from(doc.len()).unwrap_or(0);
        let mut d = driver(total);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let _ = d.plan().expect("plan").expect("a first range");
        let report = ChunkReport {
            start: 0,
            status: 206,
            content_range: Some(format!("bytes 0-{}/{total}", total.saturating_sub(1))),
            content_length: Some(total),
            body: doc.clone(),
        };
        let step = d.accept(&report, &mut guard).expect("accept");
        assert!(matches!(step, ChunkStep::Complete { degraded: false }));
        assert_eq!(d.take_document().expect("document"), doc);
    }

    #[test]
    fn empty_and_unsatisfiable_responses_are_typed() {
        let mut d = driver(64);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let _ = d.plan().expect("plan").expect("a first range");
        let empty = ChunkReport {
            start: 0,
            status: 206,
            content_range: Some("bytes 0-3/64".to_owned()),
            content_length: Some(0),
            body: Vec::new(),
        };
        assert_eq!(
            d.accept(&empty, &mut guard).expect_err("empty").code(),
            Code::IoReadFailed
        );
        let unsat = ChunkReport {
            start: 0,
            status: 416,
            content_range: None,
            content_length: None,
            body: Vec::new(),
        };
        let e = d.accept(&unsat, &mut guard).expect_err("416");
        assert_eq!(e.ctx().detail.as_deref(), Some("range-not-satisfiable"));
        let redirected = ChunkReport {
            start: 0,
            status: 302,
            content_range: None,
            content_length: None,
            body: Vec::new(),
        };
        assert_eq!(
            d.accept(&redirected, &mut guard)
                .expect_err("unexpected status")
                .code(),
            Code::IoReadFailed
        );
    }

    #[test]
    fn a_body_that_contradicts_content_length_is_rejected() {
        let mut d = driver(64);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let _ = d.plan().expect("plan").expect("a first range");
        let report = ChunkReport {
            start: 0,
            status: 206,
            content_range: Some("bytes 0-3/64".to_owned()),
            content_length: Some(99),
            body: b"%PDF".to_vec(),
        };
        let e = d.accept(&report, &mut guard).expect_err("length mismatch");
        assert_eq!(e.code(), Code::IoReadFailed);
    }

    #[test]
    fn a_hostile_origin_cannot_make_the_driver_ask_forever() {
        // An origin that answers every request with a single byte, forever:
        // the request bounds stop it, and the transfer fails typed rather than
        // spinning. This is the denial-of-service bound (ADR-P0006).
        let total = 1u64 << 20;
        let mut d = driver(total);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let mut plans = 0u32;
        let outcome = loop {
            plans += 1;
            assert!(
                plans <= MAX_REQUESTS + 2,
                "plan looped past the request bound"
            );
            let req = match d.plan() {
                Ok(Some(req)) => req,
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            };
            let report = ChunkReport {
                start: req.start,
                status: 206,
                content_range: Some(format!("bytes {}-{}/{total}", req.start, req.start)),
                content_length: Some(1),
                body: vec![b'x'],
            };
            if let Err(e) = d.accept(&report, &mut guard) {
                break Err(e);
            }
        };
        let e = outcome.expect_err("the request bound must stop this");
        assert_eq!(e.code(), Code::IoReadFailed);
        assert!(d.requests() <= MAX_REQUESTS);
        assert!(d.take_document().is_none());
    }

    #[test]
    fn attempts_on_one_offset_are_bounded() {
        // A range that keeps failing at the same offset: bounded retries, then
        // typed. The bound is per-offset, not merely per-transfer.
        let mut d = driver(64);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        for planned in 0..MAX_ATTEMPTS_PER_RANGE {
            let req = d.plan().expect("plan").expect("a range");
            let report = ChunkReport {
                start: req.start,
                status: 0,
                content_range: None,
                content_length: None,
                body: Vec::new(),
            };
            let step = d.accept(&report, &mut guard).expect("a bounded retry");
            assert_eq!(step, ChunkStep::Retry(req), "same offset, no bytes kept");
            assert_eq!(d.received(), 0);
            assert_eq!(d.requests(), planned + 1, "one request spent per attempt");
        }
        let e = d.plan().expect_err("attempt bound");
        assert_eq!(e.code(), Code::IoReadFailed);
        assert_eq!(
            e.ctx().detail.as_deref(),
            Some("range attempt bound exhausted for this offset")
        );
    }

    #[test]
    fn a_server_error_is_retried_within_the_bound() {
        let mut d = driver(64);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let req = d.plan().expect("plan").expect("a range");
        for status in [500u16, 502, 503] {
            let report = ChunkReport {
                start: req.start,
                status,
                content_range: None,
                content_length: None,
                body: Vec::new(),
            };
            assert_eq!(
                d.accept(&report, &mut guard).expect("retryable"),
                ChunkStep::Retry(req)
            );
        }
        assert_eq!(d.received(), 0, "a 5xx never contributes bytes");
    }

    #[test]
    fn the_byte_budget_stops_a_lying_origin() {
        // A 1 MiB document against a 64 KiB budget: the transfer's running
        // total is checked against the caller's ceiling before a byte is
        // copied, so the guest never grows past it.
        let mut d = HttpRangeDriver::new(
            "https://example.test/big.pdf",
            Some(1 << 20),
            Some(1024),
            Budget {
                bytes: 512,
                ..budget()
            },
        );
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let req = d.plan().expect("plan").expect("a first range");
        assert_eq!(req.len(), 1024, "the request is larger than the budget");
        let report = cooperative(&b"%PDF".repeat(4096), req.start, req.end);
        assert_eq!(
            d.accept(&report, &mut guard)
                .expect_err("over budget")
                .code(),
            Code::BudgetBytes
        );
        assert_eq!(d.received(), 0, "nothing is kept from a refused chunk");
        assert!(d.take_document().is_none());
    }

    #[test]
    fn cancellation_is_observed_at_the_next_chunk() {
        let mut d = driver(64);
        let clock = FixedClock(0);
        let cancel = CancelToken::new();
        let mut guard = d.budget.guard_with(&clock, cancel.clone());
        let req = d.plan().expect("plan").expect("a first range");
        cancel.cancel();
        let report = cooperative(b"%PDF-1.7", req.start, req.end);
        assert_eq!(
            d.accept(&report, &mut guard).expect_err("cancelled").code(),
            Code::Cancelled
        );
        assert!(d.take_document().is_none());
    }

    #[test]
    fn accept_never_mutates_the_caller_buffer() {
        // ADR-P0007: the driver's input is borrowed and left alone.
        let body = b"%PDF-1.7\n0123456789\n%%EOF".to_vec();
        let original = body.clone();
        let len = u64::try_from(body.len()).unwrap_or(0);
        let mut d = driver(len);
        let clock = FixedClock(0);
        let mut guard = d.budget.guard_with(&clock, CancelToken::new());
        let report = ChunkReport {
            start: 0,
            status: 206,
            content_range: Some(format!("bytes 0-{}/{len}", len.saturating_sub(1))),
            content_length: Some(len),
            body,
        };
        d.accept(&report, &mut guard).expect("accept");
        assert_eq!(report.body, original, "the caller's buffer was modified");
    }

    #[test]
    fn a_zero_chunk_falls_back_to_the_default() {
        let d = HttpRangeDriver::new("https://example.test/d.pdf", Some(4096), Some(0), budget());
        assert_eq!(d.chunk, DEFAULT_CHUNK_BYTES);
        assert_eq!(d.url(), "https://example.test/d.pdf");
        assert!(d.supports_ranges());
        assert!(!d.is_complete());
    }

    #[test]
    fn an_unknown_total_is_learned_from_the_first_range() {
        let doc = b"%PDF-1.7\n0123456789\n%%EOF".to_vec();
        let mut d = HttpRangeDriver::new("https://example.test/d.pdf", None, Some(4), budget());
        fetch_all(&mut d, &doc);
        assert_eq!(
            d.total(),
            Some(u64::try_from(doc.len()).unwrap_or(0)),
            "the first Content-Range establishes the length"
        );
        assert_eq!(d.take_document().expect("document"), doc);
    }

    #[test]
    fn draining_without_a_known_length_is_typed() {
        let mut d = HttpRangeDriver::new("https://example.test/d.pdf", None, Some(4), budget());
        let e = d.drain_through_source().expect_err("no length");
        assert_eq!(e.code(), Code::IoReadFailed);
    }

    #[test]
    fn content_range_parsing_is_total() {
        assert_eq!(
            parse_content_range("bytes 0-9/10"),
            Some(ContentRange {
                start: 0,
                end: 9,
                total: Some(10)
            })
        );
        assert_eq!(
            parse_content_range(" bytes 5-5/* "),
            Some(ContentRange {
                start: 5,
                end: 5,
                total: None
            })
        );
        for bad in ["", "bytes", "bytes 0-9", "bytes 0-9/", "bytes 0-9/x", "bytes 9-0/10"] {
            assert_eq!(parse_content_range(bad), None, "bad = {bad:?}");
        }
    }
}
