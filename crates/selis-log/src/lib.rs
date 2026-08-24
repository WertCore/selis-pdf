//! Two logs that must never be merged (01-ARCHITECTURE.md §11).
//!
//! * [`log`] — **structured logging for engineers.** Off in release except at
//!   `warn`. Never contains document content (ADR-P0017). The sink is injected,
//!   because no crate below L4 may name a filesystem or a socket.
//!
//! * [`oplog`] — **the document operation log.** A user-facing, per-document
//!   record of every mutation: what changed, when, and which revision it
//!   produced. It powers undo history, "compare versions", the enterprise audit
//!   trail, and the answer to *"what did this tool do to my file?"*.
//!
//! Merging them would put document content into engineer logs, which is exactly
//! the failure ADR-P0017 exists to prevent.

#![forbid(unsafe_code)]

extern crate alloc;

pub mod log;
pub mod oplog;

pub use log::{CaptureSink, Level, LogSink, NullSink, Record, Value};
pub use oplog::{ActorId, OpLog, OpRecord, Outcome};
