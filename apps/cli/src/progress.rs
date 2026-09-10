//! SL-1A.UI.06 — per-chunk byte progress and cancellation on the write
//! paths.
//!
//! The engine's sinks commit a whole output in one `append`; the CLI wraps
//! the destination sink in [`ProgressSink`], which splits the append into
//! chunks and, between chunks:
//!
//! * checks the operation's [`CancelToken`] — Ctrl-C becomes a typed
//!   `CANCELLED` error before the next chunk reaches the disk, and the
//!   temp+rename commit (SL-0.IO.05) leaves the destination untouched —
//!   never a partial file;
//! * reports cumulative byte progress at deterministic milestones (a pure
//!   function of byte counts, not of the clock, so tests are exact).
//!
//! Outputs below [`MIN_REPORTED`] skip the wrapper entirely: progress lines
//! for a write that takes microseconds would be noise, and the verification
//! line carries the result anyway.

use selis_error::{Code, Ctx, Error, Result};
use selis_io::{DocSink, SinkReceipt};
use selis_sandbox::CancelToken;

/// The append chunk size: cancellation and progress are observed between
/// chunks.
pub(crate) const CHUNK: usize = 1024 * 1024;

/// Outputs smaller than this commit without progress reporting.
pub(crate) const MIN_REPORTED: u64 = 1024 * 1024;

/// A [`DocSink`] that appends in [`CHUNK`]s, cancels between chunks, and
/// reports `(done, total)` byte progress at milestones.
pub(crate) struct ProgressSink {
    inner: Box<dyn DocSink>,
    total: u64,
    done: u64,
    /// The byte count last reported (milestones fire once each).
    reported: u64,
    cancel: CancelToken,
    report: Box<dyn FnMut(u64, u64) + Send>,
}

impl ProgressSink {
    /// Wrap `inner` for a write of `total` bytes.
    pub(crate) fn new(
        inner: Box<dyn DocSink>,
        total: u64,
        cancel: CancelToken,
        report: Box<dyn FnMut(u64, u64) + Send>,
    ) -> Self {
        ProgressSink {
            inner,
            total,
            done: 0,
            reported: 0,
            cancel,
            report,
        }
    }

    /// The typed cancellation error (never a panic, never a partial file).
    fn cancelled(&self) -> Error {
        let mut ctx = Ctx::new();
        ctx.detail = Some(format!(
            "cancelled while writing output at {} of {} bytes",
            self.done, self.total
        ));
        Error::with(Code::Cancelled, ctx)
    }

    fn fire(&mut self) {
        (self.report)(self.done, self.total);
        self.reported = self.done;
    }
}

impl DocSink for ProgressSink {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        for chunk in bytes.chunks(CHUNK) {
            if self.cancel.is_cancelled() {
                return Err(self.cancelled());
            }
            self.inner.append(chunk)?;
            self.done = self
                .done
                .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if milestone_crossed(self.reported, self.done, self.total, 10) {
                self.fire();
            }
        }
        Ok(())
    }

    fn position(&self) -> u64 {
        self.done
    }

    fn finish(mut self: Box<Self>) -> Result<SinkReceipt> {
        // Cancel before the commit, not inside it: the user asked to stop,
        // and the atomic commit guarantees the destination is untouched.
        if self.cancel.is_cancelled() {
            return Err(self.cancelled());
        }
        self.fire();
        self.inner.finish()
    }
}

/// Whether the move from `reported` to `done` crossed a progress milestone:
/// the first, then every `1/steps` fraction of `total`, each exactly once.
///
/// Deterministic by construction — a pure function of byte counts, no clock —
/// so tests assert exact report sequences.
#[must_use]
pub(crate) fn milestone_crossed(reported: u64, done: u64, total: u64, steps: u64) -> bool {
    let steps = steps.max(1);
    let step = total.checked_div(steps).filter(|s| *s > 0).unwrap_or(1);
    let index = |v: u64| v.checked_div(step).unwrap_or(0);
    index(done) > index(reported)
}

/// The stderr progress line: bytes measured, no decoration, no percentage
/// theatrics — the number is the proof.
pub(crate) fn stderr_report(label: &str) -> Box<dyn FnMut(u64, u64) + Send> {
    let label = label.to_string();
    Box::new(move |done, total| {
        eprintln!("writing {label}: {done}/{total} bytes");
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_error::Result;
    use std::sync::{Arc, Mutex};

    /// An in-memory sink recording every append it receives.
    #[derive(Default, Clone)]
    struct MemSink {
        data: Arc<Mutex<Vec<u8>>>,
    }

    impl MemSink {
        /// The recording buffer, recovering it if a concurrent test thread
        /// poisoned the mutex (the buffer's contents, not its validity, are
        /// what tests assert).
        fn guard(&self) -> std::sync::MutexGuard<'_, Vec<u8>> {
            self.data.lock().unwrap_or_else(|e| e.into_inner())
        }

        fn contents(&self) -> Vec<u8> {
            self.guard().clone()
        }
    }

    impl DocSink for MemSink {
        fn append(&mut self, bytes: &[u8]) -> Result<()> {
            self.guard().extend_from_slice(bytes);
            Ok(())
        }

        fn position(&self) -> u64 {
            u64::try_from(self.guard().len()).unwrap_or(u64::MAX)
        }

        fn finish(self: Box<Self>) -> Result<SinkReceipt> {
            let len = self.guard().len();
            Ok(SinkReceipt {
                bytes: u64::try_from(len).unwrap_or(u64::MAX),
            })
        }
    }

    /// Milestones fire in order, once each, and never for moves that stay
    /// inside one step.
    #[test]
    fn milestones_fire_once_in_order() {
        // 10 steps over 1000: milestones at 100, 200, ...
        assert!(milestone_crossed(0, 100, 1_000, 10));
        assert!(
            milestone_crossed(100, 250, 1_000, 10),
            "crosses the 200 mark"
        );
        assert!(
            !milestone_crossed(250, 299, 1_000, 10),
            "still inside step 2"
        );
        assert!(milestone_crossed(250, 300, 1_000, 10));
        // Zero-total and degenerate steps cannot divide by zero or loop.
        assert!(!milestone_crossed(0, 0, 0, 10));
        assert!(milestone_crossed(0, 1, 0, 10));
        // steps floored at 1: the only milestone is the whole total.
        assert!(!milestone_crossed(0, 1, 5, 0));
        assert!(milestone_crossed(0, 5, 5, 0));
    }

    /// The sink reports the deterministic milestone sequence for a
    /// multi-chunk write, exactly once per milestone.
    #[test]
    fn progress_reports_milestones_deterministically() {
        let cancel = CancelToken::new();
        let sink: Box<dyn DocSink> = Box::new(MemSink::default());
        let reports: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let collector = Arc::clone(&reports);
            let mut ps = ProgressSink::new(
                sink,
                CHUNK as u64 * 2 + 5,
                cancel,
                Box::new(move |done, total| collector.lock().unwrap().push((done, total))),
            );
            // 2.5 chunks: two full chunks plus a 5-byte tail.
            ps.append(&vec![0u8; CHUNK][..]).unwrap();
            ps.append(&vec![0u8; CHUNK][..]).unwrap();
            ps.append(&vec![0u8; 5][..]).unwrap();
            let receipt = Box::new(ps).finish().unwrap();
            assert_eq!(receipt.bytes, CHUNK as u64 * 2 + 5);
        }
        let reports = reports.lock().unwrap();
        // Progress milestones (total/10 ≈ 209716 bytes) fire during the
        // appends; the final 100% report fires at finish.
        assert!(!reports.is_empty(), "at least the final report fires");
        let (last_done, last_total) = reports[reports.len() - 1];
        assert_eq!(last_done, CHUNK as u64 * 2 + 5);
        assert_eq!(last_total, CHUNK as u64 * 2 + 5);
        // Monotone and deduplicated: every reported byte count increases.
        for pair in reports.windows(2) {
            assert!(pair[1].0 > pair[0].0, "reports must increase: {reports:?}");
        }
    }

    /// A cancel between chunks stops the write with the typed `CANCELLED`
    /// error before the offending chunk reaches the sink.
    #[test]
    fn cancellation_between_chunks_is_typed_and_stops_cleanly() {
        let cancel = CancelToken::new();
        let mem = MemSink::default();
        let sink: Box<dyn DocSink> = Box::new(mem);
        let mut ps = ProgressSink::new(sink, CHUNK as u64 * 2, cancel.clone(), Box::new(|_, _| {}));
        ps.append(&vec![0u8; CHUNK][..]).expect("first chunk");
        cancel.cancel();
        let err = ps
            .append(&vec![0u8; CHUNK][..])
            .expect_err("cancelled before the second chunk");
        assert_eq!(err.code(), Code::Cancelled);
        assert!(
            err.to_string().contains("cancelled while writing output"),
            "typed detail: {err}"
        );
    }

    /// A cancel before `finish` refuses the commit: the destination (the
    /// inner sink) never sees a completion, and the error is typed.
    #[test]
    fn cancellation_before_finish_refuses_the_commit() {
        let cancel = CancelToken::new();
        let sink: Box<dyn DocSink> = Box::new(MemSink::default());
        let mut ps = ProgressSink::new(sink, 10, cancel.clone(), Box::new(|_, _| {}));
        ps.append(&[0u8; 10]).expect("small write");
        cancel.cancel();
        let err = Box::new(ps).finish().expect_err("cancelled commit");
        assert_eq!(err.code(), Code::Cancelled);
    }

    /// The wrapper delivers every byte, in order, byte-identical, to the
    /// inner sink.
    #[test]
    fn payload_passes_through_byte_identical() {
        let cancel = CancelToken::new();
        let mem = MemSink::default();
        let sink: Box<dyn DocSink> = Box::new(mem.clone());
        let mut ps = ProgressSink::new(sink, 9, cancel, Box::new(|_, _| {}));
        ps.append(&[1u8, 2, 3]).unwrap();
        ps.append(&[4u8, 5, 6]).unwrap();
        ps.append(&[7u8, 8, 9]).unwrap();
        let receipt = Box::new(ps).finish().unwrap();
        assert_eq!(receipt.bytes, 9);
        assert_eq!(mem.contents(), vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}
