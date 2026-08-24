//! A bounded rope for edit buffers.
//!
//! The incremental writer and the content-region editor both need to express
//! "the original bytes, except this range is replaced" without copying the
//! original. A [`Rope`] is exactly that: an ordered list of chunks, each either a
//! shared slice of the source or a freshly-built buffer.
//!
//! This is what makes `23-EDIT-MODEL-SPEC.md §4`'s re-emission rule cheap — "the
//! rest of the content stream is copied byte-identically" becomes two shared
//! slices rather than two memcpys.

use alloc::vec::Vec;

use crate::Bytes;

/// One segment of a [`Rope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RopeChunk {
    /// A shared slice of an existing buffer. Costs nothing to carry.
    Shared(Bytes),
    /// Bytes produced by the engine.
    Owned(Bytes),
}

impl RopeChunk {
    /// The bytes of this chunk.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            RopeChunk::Shared(b) | RopeChunk::Owned(b) => b.as_slice(),
        }
    }

    /// The length of this chunk.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Whether this chunk is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An ordered sequence of byte chunks with a bounded chunk count.
///
/// The bound matters: a content-region edit driven by a hostile document must not
/// be able to produce a rope with ten million one-byte chunks. Exceeding it is
/// reported to the caller, which converts it into a budget error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rope {
    chunks: Vec<RopeChunk>,
    len: usize,
    max_chunks: usize,
}

impl Rope {
    /// The default chunk ceiling. Generous for legitimate documents, finite for
    /// hostile ones.
    pub const DEFAULT_MAX_CHUNKS: usize = 65_536;

    /// An empty rope with [`Rope::DEFAULT_MAX_CHUNKS`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
            max_chunks: Self::DEFAULT_MAX_CHUNKS,
        }
    }

    /// An empty rope with an explicit chunk ceiling.
    #[must_use]
    pub const fn with_max_chunks(max_chunks: usize) -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
            max_chunks,
        }
    }

    /// Append a chunk. Returns `false` if the chunk ceiling would be exceeded.
    ///
    /// Empty chunks are dropped rather than counted, so a caller may append
    /// freely without checking for the degenerate case.
    #[must_use = "a false return means the rope is full and the caller must error"]
    pub fn push(&mut self, chunk: RopeChunk) -> bool {
        if chunk.is_empty() {
            return true;
        }
        if self.chunks.len() >= self.max_chunks {
            return false;
        }
        self.len = self.len.saturating_add(chunk.len());
        self.chunks.push(chunk);
        true
    }

    /// Append a shared slice.
    #[must_use = "a false return means the rope is full and the caller must error"]
    pub fn push_shared(&mut self, b: Bytes) -> bool {
        self.push(RopeChunk::Shared(b))
    }

    /// Append engine-produced bytes.
    #[must_use = "a false return means the rope is full and the caller must error"]
    pub fn push_owned(&mut self, b: Bytes) -> bool {
        self.push(RopeChunk::Owned(b))
    }

    /// Total length across all chunks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the rope holds no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of chunks.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// The chunks, in order.
    #[must_use]
    pub fn chunks(&self) -> &[RopeChunk] {
        &self.chunks
    }

    /// How many bytes are shared rather than owned.
    ///
    /// The incremental-save perf budget (03-CONVENTIONS.md §12) is really a claim
    /// about this ratio: rotating one page of a 200 MB file should share
    /// essentially all of it.
    #[must_use]
    pub fn shared_bytes(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| matches!(c, RopeChunk::Shared(_)))
            .fold(0usize, |acc, c| acc.saturating_add(c.len()))
    }

    /// Flatten into one contiguous buffer.
    ///
    /// Only for callers that genuinely need contiguity (a hash, a comparison). The
    /// write path streams [`Rope::chunks`] to the sink instead and never
    /// materialises the whole document.
    #[must_use]
    pub fn to_bytes(&self) -> Bytes {
        let mut out = crate::BytesMut::with_capacity(self.len);
        for c in &self.chunks {
            out.extend_from_slice(c.as_slice());
        }
        out.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_prefix_plus_owned_tail() {
        let source = Bytes::from_vec(b"ORIGINAL-BYTES".to_vec());
        let mut r = Rope::new();
        assert!(r.push_shared(source.clone()));
        assert!(r.push_owned(Bytes::copy_from_slice(b"+REVISION")));
        assert_eq!(r.to_bytes().as_slice(), b"ORIGINAL-BYTES+REVISION");
        assert_eq!(r.shared_bytes(), source.len());
        // Invariant I2 in miniature: the original is a byte-identical prefix.
        assert!(r.to_bytes().starts_with(source.as_slice()));
    }

    #[test]
    fn empty_chunks_are_free_and_uncounted() {
        let mut r = Rope::with_max_chunks(1);
        assert!(r.push_owned(Bytes::new()));
        assert!(r.push_owned(Bytes::new()));
        assert_eq!(r.chunk_count(), 0);
        assert!(r.push_owned(Bytes::copy_from_slice(b"x")));
        assert_eq!(r.chunk_count(), 1);
    }

    #[test]
    fn chunk_ceiling_is_reported_not_panicked() {
        let mut r = Rope::with_max_chunks(2);
        assert!(r.push_owned(Bytes::copy_from_slice(b"a")));
        assert!(r.push_owned(Bytes::copy_from_slice(b"b")));
        assert!(!r.push_owned(Bytes::copy_from_slice(b"c")));
        assert_eq!(r.len(), 2);
    }
}
