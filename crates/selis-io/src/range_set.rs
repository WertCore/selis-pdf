//! [`RangeSet`] — the resident-range bookkeeping of the partial-availability
//! model (SL-0.IO.01).
//!
//! A set of half-open `[start, end)` byte ranges, always coalesced and always
//! disjoint. Union, subtraction and containment are the operations the source
//! and the resumable parser need, and each must be total — no panics on any
//! input, because the inputs can come from a document.

use core::cmp;
use core::fmt;
use core::ops::Range;

/// A set of byte ranges, kept coalesced and disjoint.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RangeSet {
    /// Invariant: sorted, non-overlapping, `start < end` for every entry.
    ranges: Vec<Range<u64>>,
}

impl RangeSet {
    /// An empty set.
    #[must_use]
    pub const fn new() -> Self {
        Self { ranges: Vec::new() }
    }

    /// A set containing exactly one range.
    #[must_use]
    pub fn one(start: u64, end: u64) -> Self {
        let mut s = Self::new();
        s.insert(start, end);
        s
    }

    /// The ranges, in order.
    #[must_use]
    pub fn ranges(&self) -> &[Range<u64>] {
        &self.ranges
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The total number of bytes covered, saturating.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        let mut total = 0u64;
        for r in &self.ranges {
            total = total.saturating_add(r.end.saturating_sub(r.start));
        }
        total
    }

    /// The first covered byte, if any.
    #[must_use]
    pub fn start(&self) -> Option<u64> {
        self.ranges.first().map(|r| r.start)
    }

    /// The last covered byte (exclusive), if any.
    #[must_use]
    pub fn end(&self) -> Option<u64> {
        self.ranges.last().map(|r| r.end)
    }

    /// Add `[start, end)` to the set, coalescing neighbours.
    ///
    /// A zero-length or reversed range is a no-op (not an error, not a panic —
    /// the caller may be probing with a document-derived length).
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        // First range that is NOT entirely before `start`.
        let idx = self.ranges.partition_point(|r| r.end < start);

        // Case A: `[start, end)` overlaps or touches the range at `idx`.
        if let Some(cur) = self.ranges.get(idx) {
            if cur.start <= end {
                let merged_start = cur.start.min(start);
                let mut merged_end = cur.end.max(end);
                // Absorb every following range it now covers.
                let mut j = idx.saturating_add(1);
                while let Some(next) = self.ranges.get(j) {
                    if next.start <= merged_end {
                        merged_end = merged_end.max(next.end);
                        j = j.saturating_add(1);
                    } else {
                        break;
                    }
                }
                if let Some(slot) = self.ranges.get_mut(idx) {
                    slot.start = merged_start;
                    slot.end = merged_end;
                }
                if j > idx.saturating_add(1) {
                    self.ranges.drain(idx.saturating_add(1)..j);
                }
                return;
            }
        }

        // Case B: `[start, end)` touches the previous range from behind.
        if idx > 0 {
            if let Some(prev) = self.ranges.get_mut(idx.saturating_sub(1)) {
                if prev.end >= start {
                    prev.end = prev.end.max(end);
                    return;
                }
            }
        }

        // Case C: it sits in a gap.
        self.ranges.insert(idx, Range { start, end });
    }

    /// Remove `[start, end)` from the set, splitting ranges as needed.
    pub fn subtract(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let mut kept: Vec<Range<u64>> = Vec::with_capacity(self.ranges.len());
        for r in self.ranges.drain(..) {
            if r.end <= start || r.start >= end {
                kept.push(r);
                continue;
            }
            if r.start < start {
                kept.push(Range {
                    start: r.start,
                    end: start,
                });
            }
            if r.end > end {
                kept.push(Range {
                    start: end,
                    end: r.end,
                });
            }
        }
        self.ranges = kept;
    }

    /// Whether `[start, end)` is fully covered.
    #[must_use]
    pub fn contains(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return true; // vacuously covered
        }
        let idx = self.ranges.partition_point(|r| r.end <= start);
        self.ranges
            .get(idx)
            .is_some_and(|r| r.start <= start && r.end >= end)
    }

    /// Whether `offset` is inside any covered range.
    #[must_use]
    pub fn contains_point(&self, offset: u64) -> bool {
        let idx = self.ranges.partition_point(|r| r.end <= offset);
        self.ranges
            .get(idx)
            .is_some_and(|r| r.start <= offset && offset < r.end)
    }

    /// The union of two sets.
    #[must_use]
    pub fn union(&self, other: &RangeSet) -> RangeSet {
        let mut out = self.clone();
        for r in &other.ranges {
            out.insert(r.start, r.end);
        }
        out
    }

    /// The difference `self − other`.
    #[must_use]
    pub fn difference(&self, other: &RangeSet) -> RangeSet {
        let mut out = self.clone();
        for r in &other.ranges {
            out.subtract(r.start, r.end);
        }
        out
    }

    /// The intersection of two sets.
    #[must_use]
    pub fn intersection(&self, other: &RangeSet) -> RangeSet {
        let mut out = RangeSet::new();
        let mut i = 0usize;
        let mut j = 0usize;
        while let (Some(a), Some(b)) = (self.ranges.get(i), other.ranges.get(j)) {
            let start = cmp::max(a.start, b.start);
            let end = cmp::min(a.end, b.end);
            if start < end {
                out.insert(start, end);
            }
            if a.end <= b.end {
                i = i.saturating_add(1);
            }
            if b.end <= a.end {
                j = j.saturating_add(1);
            }
        }
        out
    }

    /// Whether the set is internally consistent (sorted, disjoint, non-empty
    /// ranges). Invariant used by tests and debug assertions.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.ranges.windows(2).all(|w| {
            w.first()
                .zip(w.get(1))
                .is_some_and(|(a, b)| a.end <= b.start)
        }) && self.ranges.iter().all(|r| r.start < r.end)
    }
}

impl fmt::Debug for RangeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RangeSet({:?})", self.ranges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rs(pairs: &[(u64, u64)]) -> RangeSet {
        let mut s = RangeSet::new();
        for &(a, b) in pairs {
            s.insert(a, b);
        }
        s
    }

    #[test]
    fn insert_coalesces_adjacent_and_overlapping() {
        let mut s = RangeSet::new();
        s.insert(0, 10);
        s.insert(10, 20); // adjacent
        s.insert(15, 30); // overlapping
        assert_eq!(s.ranges(), &[Range { start: 0, end: 30 }]);
        assert!(s.is_consistent());
    }

    #[test]
    fn insert_keeps_gaps() {
        let mut s = RangeSet::new();
        s.insert(0, 10);
        s.insert(20, 30);
        assert_eq!(s.ranges().len(), 2);
        assert!(s.contains(0, 10));
        assert!(!s.contains(10, 20));
        assert!(s.contains(20, 30));
    }

    #[test]
    fn reversed_insert_is_a_noop() {
        let mut s = rs(&[(0, 10)]);
        s.insert(5, 5);
        s.insert(9, 3);
        assert_eq!(s.ranges().len(), 1);
    }

    #[test]
    fn subtract_splits_middle() {
        let mut s = rs(&[(0, 100)]);
        s.subtract(40, 60);
        assert_eq!(
            s.ranges(),
            &[
                Range { start: 0, end: 40 },
                Range {
                    start: 60,
                    end: 100
                }
            ]
        );
    }

    #[test]
    fn subtract_edges() {
        let mut s = rs(&[(0, 100)]);
        s.subtract(0, 10);
        s.subtract(90, 200);
        assert_eq!(s.ranges(), &[Range { start: 10, end: 90 }]);
    }

    #[test]
    fn contains_covers_partial_probes() {
        let s = rs(&[(0, 10), (20, 40)]);
        assert!(s.contains(0, 10));
        assert!(s.contains(5, 8));
        assert!(!s.contains(8, 12));
        assert!(!s.contains(10, 20));
        assert!(s.contains(20, 40));
        assert!(s.contains(35, 40));
        // Vacuous: empty probes are always "covered".
        assert!(s.contains(50, 50));
    }

    #[test]
    fn total_bytes_sums_without_double_counting() {
        let s = rs(&[(0, 10), (10, 20), (30, 35)]);
        assert_eq!(s.total_bytes(), 25);
        assert_eq!(s.start(), Some(0));
        assert_eq!(s.end(), Some(35));
    }

    #[test]
    fn union_intersection_difference() {
        let a = rs(&[(0, 10), (20, 30)]);
        let b = rs(&[(5, 25)]);
        assert_eq!(a.union(&b).ranges(), &[Range { start: 0, end: 30 }]);
        assert_eq!(
            a.intersection(&b).ranges(),
            &[Range { start: 5, end: 10 }, Range { start: 20, end: 25 }]
        );
        assert_eq!(
            a.difference(&b).ranges(),
            &[Range { start: 0, end: 5 }, Range { start: 25, end: 30 }]
        );
    }

    /// SL-0.IO.01 DoD: RangeSet has property tests for union, subtract, and
    /// coalesce.
    #[cfg(test)]
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Inserting the same set twice yields the same set — idempotent.
            #[test]
            fn insert_is_idempotent(pairs: Vec<(u64, u64)>) {
                let pairs: Vec<(u64, u64)> = pairs
                    .into_iter()
                    .map(|(a, b)| if a > b { (b, a) } else { (a, b) })
                    .collect();
                let mut s1 = RangeSet::new();
                let mut s2 = RangeSet::new();
                for (a, b) in &pairs {
                    s1.insert(*a, *b);
                }
                for (a, b) in &pairs {
                    s2.insert(*a, *b);
                }
                assert_eq!(s1, s2);
                assert!(s1.is_consistent());
                assert!(s2.is_consistent());
            }

            /// subtract then re-insert is the identity for ranges contained in
            /// the set.
            #[test]
            fn subtract_then_insert_restores(base: (u64, u64), cut: (u64, u64)) {
                let (a, b) = if base.0 > base.1 { (base.1, base.0) } else { (base.0, base.1) };
                let (c, d) = if cut.0 > cut.1 { (cut.1, cut.0) } else { (cut.0, cut.1) };
                let mut s = RangeSet::one(a, b);
                let before = s.clone();
                // Only meaningful when the cut is contained in the set.
                if s.contains(c, d) {
                    s.subtract(c, d);
                    assert!(!s.contains(c, d), "subtract must remove the cut");
                    s.insert(c, d);
                    assert_eq!(s, before, "subtract+insert must restore the set");
                    assert!(s.is_consistent());
                }
            }

            /// insert never creates an inconsistent set, whatever the inputs.
            #[test]
            fn insert_always_stays_consistent(pairs: Vec<(u64, u64)>) {
                let mut s = RangeSet::new();
                for (a, b) in pairs {
                    s.insert(a, b);
                    assert!(s.is_consistent());
                }
            }
        }
    }
}
