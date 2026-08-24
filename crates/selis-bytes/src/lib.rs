//! Immutable, refcounted byte slices — the mechanism behind ADR-P0007.
//!
//! [`Bytes`] is a cheap-to-clone handle onto a shared buffer. There is **no API
//! on this crate that yields a mutable view of a [`Bytes`]**, and that absence is
//! the point: invariant I1 of `23-EDIT-MODEL-SPEC.md` ("the bytes of the opened
//! document are never modified") is enforced by the type system rather than by
//! discipline.
//!
//! [`BytesMut`] is the *build* side: you write into it, then [`BytesMut::freeze`]
//! turns it into a `Bytes` and consumes it. The two directions never coexist for
//! the same allocation.
//!
//! # Slicing is free
//!
//! A subrange of a `Bytes` shares the same allocation:
//!
//! ```
//! use selis_bytes::Bytes;
//! let all = Bytes::from_vec(b"%PDF-1.7\nbody".to_vec());
//! let header = all.slice(0..8).expect("in range");
//! assert_eq!(header.as_slice(), b"%PDF-1.7");
//! ```

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

mod rope;

pub use rope::{Rope, RopeChunk};

/// An immutable, refcounted byte range.
///
/// Cloning is an atomic increment. Slicing is a bounds check. Neither copies.
#[derive(Clone)]
pub struct Bytes {
    buf: Arc<Vec<u8>>,
    /// Invariant: `start <= end <= buf.len()`. Upheld by every constructor.
    start: usize,
    end: usize,
}

impl Bytes {
    /// An empty `Bytes`.
    #[must_use]
    pub fn new() -> Self {
        Self::from_vec(Vec::new())
    }

    /// Take ownership of a `Vec` without copying.
    #[must_use]
    pub fn from_vec(v: Vec<u8>) -> Self {
        let end = v.len();
        Self {
            buf: Arc::new(v),
            start: 0,
            end,
        }
    }

    /// Copy a slice into a new buffer.
    #[must_use]
    pub fn copy_from_slice(s: &[u8]) -> Self {
        Self::from_vec(s.to_vec())
    }

    /// The bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // Cannot panic: the start <= end <= buf.len() invariant makes this range
        // always valid, and `get` returns Option rather than indexing anyway.
        self.buf.get(self.start..self.end).unwrap_or(&[])
    }

    /// The length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the range is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A subrange sharing this allocation.
    ///
    /// Returns `None` if the range is reversed or out of bounds. Document-derived
    /// offsets go through here precisely so that an out-of-range value is a
    /// `None` to handle rather than a panic.
    #[must_use]
    pub fn slice(&self, range: Range<usize>) -> Option<Self> {
        if range.start > range.end || range.end > self.len() {
            return None;
        }
        let start = self.start.checked_add(range.start)?;
        let end = self.start.checked_add(range.end)?;
        Some(Self {
            buf: Arc::clone(&self.buf),
            start,
            end,
        })
    }

    /// A subrange clamped to the available bytes.
    ///
    /// For the many places in a PDF parser where a length field is a *hint*: a
    /// declared `/Length` running past end-of-file yields the bytes that exist,
    /// and the caller records a deviation.
    #[must_use]
    pub fn slice_clamped(&self, range: Range<usize>) -> Self {
        let len = self.len();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        // Cannot fail: start <= end <= len by construction above.
        self.slice(start..end).unwrap_or_else(Bytes::new)
    }

    /// The byte at `i`, or `None`.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<u8> {
        self.as_slice().get(i).copied()
    }

    /// Whether the range starts with `prefix`.
    #[must_use]
    pub fn starts_with(&self, prefix: &[u8]) -> bool {
        self.as_slice().starts_with(prefix)
    }

    /// The offset of the first occurrence of `needle`, searching forwards.
    #[must_use]
    pub fn find(&self, needle: &[u8]) -> Option<usize> {
        find(self.as_slice(), needle)
    }

    /// The offset of the last occurrence of `needle`, searching backwards.
    #[must_use]
    pub fn rfind(&self, needle: &[u8]) -> Option<usize> {
        rfind(self.as_slice(), needle)
    }

    /// Copy out into a fresh `Vec`.
    #[must_use]
    pub fn to_vec(&self) -> Vec<u8> {
        self.as_slice().to_vec()
    }
}

impl Default for Bytes {
    fn default() -> Self {
        Self::new()
    }
}

impl core::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for Bytes {}

impl PartialEq<[u8]> for Bytes {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_slice() == other
    }
}

impl core::hash::Hash for Bytes {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Self {
        Self::from_vec(v)
    }
}

impl From<&[u8]> for Bytes {
    fn from(s: &[u8]) -> Self {
        Self::copy_from_slice(s)
    }
}

/// Debug prints the length and a short hex preview — never the whole buffer,
/// which would put document content into logs (ADR-P0017).
impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes(len={}", self.len())?;
        if !self.is_empty() {
            f.write_str(", ")?;
            for b in self.as_slice().iter().take(8) {
                write!(f, "{b:02x}")?;
            }
            if self.len() > 8 {
                f.write_str("…")?;
            }
        }
        f.write_str(")")
    }
}

/// The mutable build-side buffer.
///
/// Write into it; [`BytesMut::freeze`] consumes it and hands back an immutable
/// [`Bytes`]. There is no way back.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BytesMut {
    v: Vec<u8>,
}

impl BytesMut {
    /// An empty buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self { v: Vec::new() }
    }

    /// An empty buffer with reserved capacity.
    ///
    /// # Panics
    ///
    /// Never for engine use: capacity here is always engine-chosen. A
    /// *document-derived* length must go through `selis_sandbox::alloc`, which
    /// charges the budget first and returns `Result` (ADR-P0006).
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            v: Vec::with_capacity(cap),
        }
    }

    /// Append a slice.
    pub fn extend_from_slice(&mut self, s: &[u8]) {
        self.v.extend_from_slice(s);
    }

    /// Append one byte.
    pub fn push(&mut self, b: u8) {
        self.v.push(b);
    }

    /// The bytes written so far.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.v
    }

    /// A mutable view of the bytes written so far.
    ///
    /// Legitimate on the build side (e.g. back-patching a `/Length` placeholder
    /// in a stream we are emitting). Not reachable from a [`Bytes`].
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.v
    }

    /// The number of bytes written.
    #[must_use]
    pub fn len(&self) -> usize {
        self.v.len()
    }

    /// Whether nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }

    /// Discard the contents, keeping the allocation.
    pub fn clear(&mut self) {
        self.v.clear();
    }

    /// Shorten to `len` bytes, dropping the tail.
    pub fn truncate(&mut self, len: usize) {
        self.v.truncate(len);
    }

    /// Convert to an immutable [`Bytes`] without copying.
    #[must_use]
    pub fn freeze(self) -> Bytes {
        Bytes::from_vec(self.v)
    }

    /// Take the underlying `Vec`.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.v
    }
}

impl From<Vec<u8>> for BytesMut {
    fn from(v: Vec<u8>) -> Self {
        Self { v }
    }
}

/// Forward substring search.
///
/// A plain two-pointer scan. PDF needles (`obj`, `endstream`, `startxref`) are
/// short and the haystacks are bounded by the budget, so a sublinear algorithm
/// would add code without adding speed.
#[must_use]
pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len().checked_sub(needle.len())?;
    for i in 0..=last {
        if haystack.get(i..i.checked_add(needle.len())?) == Some(needle) {
            return Some(i);
        }
    }
    None
}

/// Backward substring search. Returns the offset of the *last* match.
///
/// This is how `startxref` is found: from the tail, because a PDF's index lives
/// at the end and an incrementally-updated document has several.
#[must_use]
pub fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(haystack.len());
    }
    if needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len().checked_sub(needle.len())?;
    for i in (0..=last).rev() {
        if haystack.get(i..i.checked_add(needle.len())?) == Some(needle) {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_shares_the_allocation() {
        let a = Bytes::from_vec(b"0123456789".to_vec());
        let b = a.slice(2..5).expect("in range");
        assert_eq!(b.as_slice(), b"234");
        assert_eq!(b.len(), 3);
        // Slicing a slice composes correctly.
        let c = b.slice(1..3).expect("in range");
        assert_eq!(c.as_slice(), b"34");
    }

    #[test]
    fn out_of_range_slice_is_none_not_a_panic() {
        let a = Bytes::from_vec(b"abc".to_vec());
        assert!(a.slice(0..4).is_none());
        assert!(a.slice(2..1).is_none());
        assert!(a.slice(4..5).is_none());
        assert_eq!(a.slice(3..3).map(|b| b.len()), Some(0));
    }

    #[test]
    fn slice_clamped_tolerates_a_lying_length() {
        // The real-world case: /Length says 9999, the file ends at 3.
        let a = Bytes::from_vec(b"abc".to_vec());
        assert_eq!(a.slice_clamped(0..9999).as_slice(), b"abc");
        assert_eq!(a.slice_clamped(2..9999).as_slice(), b"c");
        assert_eq!(a.slice_clamped(9999..99999).as_slice(), b"");
    }

    #[test]
    fn find_and_rfind() {
        let h = b"aXbXc";
        assert_eq!(find(h, b"X"), Some(1));
        assert_eq!(rfind(h, b"X"), Some(3));
        assert_eq!(find(h, b"zz"), None);
        assert_eq!(find(h, b""), Some(0));
        assert_eq!(find(b"", b"x"), None);
        assert_eq!(rfind(b"aaa", b"aaaa"), None);
        assert_eq!(find(b"abc", b"abc"), Some(0));
        assert_eq!(rfind(b"abc", b"abc"), Some(0));
    }

    #[test]
    fn freeze_then_no_way_back() {
        let mut m = BytesMut::new();
        m.extend_from_slice(b"%PDF-");
        m.push(b'2');
        let frozen = m.freeze();
        assert_eq!(frozen.as_slice(), b"%PDF-2");
        // There is deliberately no `frozen.as_mut_slice()`; if this compiled,
        // ADR-P0007's I1 would be unenforceable.
    }

    #[test]
    fn debug_does_not_dump_content() {
        let b = Bytes::from_vec(b"SECRETSECRETSECRET".to_vec());
        let s = alloc::format!("{b:?}");
        assert!(!s.contains("SECRET"));
        assert!(s.contains("len=18"));
    }

    #[test]
    fn equality_ignores_provenance() {
        let a = Bytes::from_vec(b"xxabcxx".to_vec())
            .slice(2..5)
            .expect("in range");
        let b = Bytes::copy_from_slice(b"abc");
        assert_eq!(a, b);
    }
}
