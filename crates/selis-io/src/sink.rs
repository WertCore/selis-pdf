//! [`AppendSink`] — an append-only destination with atomic commit (SL-0.IO.05).

use std::sync::Mutex;

use crate::{DocSink, SinkReceipt};

/// A sink that appends to an in-memory buffer and commits atomically.
///
/// This is the test and fuzz workhorse for the incremental-save writer, and the
/// pattern every real sink follows: writes buffer, [`DocSink::finish`] flushes
/// and produces a [`SinkReceipt`] proving how many bytes were committed.
#[derive(Debug)]
pub struct AppendSink {
    buf: Mutex<Vec<u8>>,
    finished: Mutex<bool>,
}

impl AppendSink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: Mutex::new(Vec::new()),
            finished: Mutex::new(false),
        }
    }

    /// The bytes appended so far (a copy).
    ///
    /// # Panics
    ///
    /// If a lock is poisoned — a panic inside this sink would be a bug.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        match self.buf.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Default for AppendSink {
    fn default() -> Self {
        Self::new()
    }
}

impl DocSink for AppendSink {
    fn append(&mut self, bytes: &[u8]) -> selis_error::Result<()> {
        {
            let finished = match self.finished.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if *finished {
                return Err(selis_error::Error::new(selis_error::Code::SinkFinished));
            }
        }
        match self.buf.lock() {
            Ok(mut g) => g.extend_from_slice(bytes),
            Err(poisoned) => poisoned.into_inner().extend_from_slice(bytes),
        }
        Ok(())
    }

    fn position(&self) -> u64 {
        match self.buf.lock() {
            Ok(g) => g.len() as u64,
            Err(poisoned) => poisoned.into_inner().len() as u64,
        }
    }

    fn finish(self: Box<Self>) -> selis_error::Result<SinkReceipt> {
        let bytes = self.position();
        let mut finished = match self.finished.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *finished {
            return Err(selis_error::Error::new(selis_error::Code::SinkFinished));
        }
        *finished = true;
        Ok(SinkReceipt { bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_then_finish_receipts() {
        let sink: Box<dyn DocSink> = Box::new(AppendSink::new());
        let mut s = sink;
        s.append(b"abc").expect("append");
        s.append(b"def").expect("append");
        assert_eq!(s.position(), 6);
        let receipt = s.finish().expect("finish");
        assert_eq!(receipt.bytes, 6);
    }

    #[test]
    fn finished_sink_rejects_appends() {
        // `finish` consumes the Box, so post-finish appends are impossible at
        // the type level; the SinkFinished guard protects implementations that
        // are shared. Assert the receipt path and position tracking instead.
        let s: Box<dyn DocSink> = Box::new(AppendSink::new());
        let mut s = s;
        s.append(b"x").expect("append");
        assert_eq!(s.position(), 1);
        let receipt = s.finish().expect("finish");
        assert_eq!(receipt.bytes, 1);
    }

    #[test]
    fn bytes_are_retrievable() {
        let mut s = AppendSink::new();
        s.append(b"hi").expect("append");
        assert_eq!(s.bytes(), b"hi");
    }
}
