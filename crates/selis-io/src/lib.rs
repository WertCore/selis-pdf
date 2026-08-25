//! The injected I/O abstraction (01-ARCHITECTURE.md §5, SL-0.IO.01).
//!
//! # Why I/O is a trait and not `std::fs`
//!
//! ADR-P0005 forbids every crate below L4 from naming a filesystem or network
//! API. The engine is fed by a [`DocSource`] and writes through a [`DocSink`];
//! the shells supply the concrete implementations (`MemSource`, `FileSource`,
//! `HttpRangeSource`, …). This is what makes the same engine run in a browser
//! worker, a sandboxed child process, and a CLI without a single `#[cfg]`.
//!
//! # The partial-availability model
//!
//! [`DocSource::read_at`] never blocks on the network. A source that cannot
//! satisfy a read immediately returns [`Availability::Pending`] with the ranges
//! it *does* have, and the caller re-drives the parse when the data arrives.
//! `selis-pdf-cos` is written to tolerate `Pending` at every read (SL-1.COS.07);
//! this is how a linearised 200 MB PDF opens before the first megabyte has
//! arrived.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod fault;
mod file;
mod http;
mod mem;
mod range_set;
mod sink;

pub use fault::{FaultConfig, FaultSource};
pub use file::FileSource;
pub use http::{FetchFn, HttpRangeSource};
pub use mem::MemSource;
pub use range_set::RangeSet;
pub use sink::AppendSink;

use selis_error::Result;

/// Random-access, possibly-partial, read-only access to document bytes.
///
/// # Contract
///
/// * Implementations must not block longer than the caller's deadline.
///   `read_at` on an unavailable range returns [`Availability::Pending`] rather
///   than blocking on the network.
/// * `len()` returns `None` only for stream-only sources whose length is not
///   yet known.
/// * Reads never modify the source's observable state except its notion of what
///   is resident ([`DocSource::available`] may grow after a fetch).
///
/// # Budget
///
/// Implementations must charge their own allocation and I/O budget; the caller
/// is separately budgeted for parsing.
pub trait DocSource: Send + Sync {
    /// The total length in bytes, or `None` while unknown (streaming).
    fn len(&self) -> Option<u64>;

    /// Fill `buf` with bytes starting at `off`.
    ///
    /// Returns:
    /// * `Filled(n)` — the first `n` bytes of `buf` are resident and were
    ///   copied. `n < buf.len()` is normal and means "this is all that is
    ///   available right now".
    /// * `Pending { hint }` — the requested range is not resident. `hint`
    ///   lists the ranges the caller should supply and then re-drive the parse.
    /// * `Eof` — `off >= len`; there are no more bytes.
    ///
    /// # Malformed Input
    ///
    /// A source must never return `Filled(n)` with `n` exceeding `buf.len()`.
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<Availability>;

    /// The set of byte ranges currently resident.
    fn available(&self) -> &RangeSet;

    /// Hint that these ranges are wanted soon. A source may prefetch or ignore.
    fn request(&self, ranges: &RangeSet);

    /// Whether random access is supported. `false` = must be read start-to-end.
    fn is_random_access(&self) -> bool;
}

/// The outcome of a [`DocSource::read_at`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// `n` bytes were copied into the buffer (the first `n` of it).
    Filled(usize),
    /// The range is not resident; `hint` says what to supply next.
    Pending {
        /// The byte ranges the caller should make resident before retrying.
        hint: RangeSet,
    },
    /// `off` is at or beyond the end of the source.
    Eof,
}

/// Append-or-write destination. Deliberately minimal, deliberately not
/// Seek-by-default: the incremental writer only ever appends (ADR-P0007).
pub trait DocSink: Send {
    /// Append bytes. The sink may buffer; durability is `finish`'s job.
    ///
    /// # Errors
    ///
    /// `SINK_FINISHED` if the sink was already finished. Transport errors are
    /// the implementation's choice of code.
    fn append(&mut self, bytes: &[u8]) -> Result<()>;

    /// The current append position in bytes.
    fn position(&self) -> u64;

    /// Commit and flush. The `SinkReceipt` proves the write happened.
    ///
    /// # Errors
    ///
    /// `WRITE_FAILED` when the destination could not be committed.
    fn finish(self: Box<Self>) -> Result<SinkReceipt>;
}

/// Proof that a sink was finished and its contents committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkReceipt {
    /// The total number of bytes committed.
    pub bytes: u64,
}
