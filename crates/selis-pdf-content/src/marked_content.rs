//! Marked content and the MCID map (SL-2.CONT.06).
//!
//! `BMC`/`BDC`/`EMC` maintain the marked-content stack; the page-level MCID
//! map records which content ranges belong to which marked-content identifier.
//! Required by `selis-pdf-text` for structure-driven reading order and by
//! `selis-pdf-redact` to find what a region contains. Per ADR-P0031 this is
//! Phase 2, not optional.

use std::collections::BTreeMap;

/// A marked-content entry: the tag and the content range it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkedContent {
    /// The tag (e.g. `/P`, `/Span`, `/Artifact`).
    pub tag: selis_bytes::Bytes,
    /// The marked-content identifier (MCID), when present (from `BDC`).
    pub mcid: Option<u32>,
    /// The byte range of the content stream this entry covers.
    pub range: (usize, usize),
}

/// The marked-content stack (`BMC`/`BDC` push, `EMC` pops).
#[derive(Debug, Default)]
pub struct McStack {
    /// The open entries, innermost last.
    stack: Vec<OpenEntry>,
    /// The next MCID to assign.
    next_mcid: u32,
}

#[derive(Debug, Clone)]
struct OpenEntry {
    tag: selis_bytes::Bytes,
    mcid: Option<u32>,
    start: usize,
}

impl McStack {
    /// A new empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `BMC tag` — push a marked-content point (no MCID).
    pub fn begin_no_mcid(&mut self, tag: selis_bytes::Bytes, pos: usize) {
        self.stack.push(OpenEntry {
            tag,
            mcid: None,
            start: pos,
        });
    }

    /// `BDC tag props` — push a marked-content point with an MCID.
    pub fn begin_with_mcid(&mut self, tag: selis_bytes::Bytes, pos: usize) {
        let mcid = self.next_mcid;
        self.next_mcid = self.next_mcid.saturating_add(1);
        self.stack.push(OpenEntry {
            tag,
            mcid: Some(mcid),
            start: pos,
        });
    }

    /// `EMC` — pop the innermost entry, returning it and the byte range.
    ///
    /// # Errors
    ///
    /// `OBJ_UNEXPECTED` on `EMC` underflow (a malformed content stream).
    pub fn end(&mut self, pos: usize) -> Result<MarkedContent, selis_error::Error> {
        let entry = self.stack.pop().ok_or_else(|| {
            selis_error::err!(
                selis_error::Code::ObjUnexpected,
                during = "marked-content",
                detail = "EMC without matching BMC/BDC"
            )
        })?;
        Ok(MarkedContent {
            tag: entry.tag,
            mcid: entry.mcid,
            range: (entry.start, pos),
        })
    }

    /// The current nesting depth.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// Whether the stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

/// The page-level MCID map: MCID → content range.
#[derive(Debug, Default)]
pub struct McidMap {
    /// The ranges, keyed by MCID, in content order.
    map: BTreeMap<u32, (usize, usize)>,
}

impl McidMap {
    /// A new empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a completed marked-content entry.
    pub fn record(&mut self, mc: MarkedContent) {
        if let Some(mcid) = mc.mcid {
            self.map.insert(mcid, mc.range);
        }
    }

    /// The content range for an MCID.
    #[must_use]
    pub fn range_for(&self, mcid: u32) -> Option<(usize, usize)> {
        self.map.get(&mcid).copied()
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// All (MCID, range) pairs in content order.
    #[must_use]
    pub fn entries(&self) -> impl Iterator<Item = (u32, (usize, usize))> + '_ {
        self.map.iter().map(|(&k, &v)| (k, v))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    fn tag(s: &[u8]) -> selis_bytes::Bytes {
        selis_bytes::Bytes::copy_from_slice(s)
    }

    #[test]
    fn bmc_bdc_emc_roundtrip() {
        let mut stack = McStack::new();
        stack.begin_no_mcid(tag(b"Artifact"), 0);
        stack.begin_with_mcid(tag(b"P"), 10);
        let inner = stack.end(50).expect("end");
        assert_eq!(inner.mcid, Some(0));
        assert_eq!(inner.range, (10, 50));
        let outer = stack.end(60).expect("end");
        assert_eq!(outer.mcid, None);
        assert_eq!(outer.range, (0, 60));
        assert!(stack.is_empty());
    }

    #[test]
    fn emc_underflow_is_a_typed_error() {
        let mut stack = McStack::new();
        let e = stack.end(5).expect_err("underflow");
        assert_eq!(e.code(), selis_error::Code::ObjUnexpected);
    }

    #[test]
    fn mcid_map_records_ranges() {
        let mut stack = McStack::new();
        let mut map = McidMap::new();
        stack.begin_with_mcid(tag(b"P"), 10);
        map.record(stack.end(50).expect("end"));
        assert_eq!(map.range_for(0), Some((10, 50)));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn mcids_are_sequential() {
        let mut stack = McStack::new();
        stack.begin_with_mcid(tag(b"P"), 0);
        stack.begin_with_mcid(tag(b"Span"), 1);
        // The stack is LIFO: the inner (Span, mcid 1) pops first.
        let inner = stack.end(2).expect("inner");
        let outer = stack.end(3).expect("outer");
        assert_eq!(inner.mcid, Some(1));
        assert_eq!(outer.mcid, Some(0));
        assert_eq!(inner.tag.as_slice(), b"Span");
        assert_eq!(outer.tag.as_slice(), b"P");
    }
}
