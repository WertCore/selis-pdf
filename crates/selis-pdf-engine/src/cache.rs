//! The engine's caches (SL-2.PERF.01, ADR-P0025).
//!
//! `selis-pdf-engine` owns every cache the shells would otherwise each
//! reinvent: parsed pages, display lists, and rendered tiles. One
//! memory-budgeted LRU serves all three, keyed by the four-part identity the
//! ADR fixes:
//!
//! ```text
//! (page, matrix, params, revision)
//! ```
//!
//! * **LRU under a memory budget** — the oldest entries are evicted when the
//!   configured byte budget is exceeded;
//! * **exact invalidation** — an edit invalidates only the affected page's
//!   entries, never the whole cache.
//!
//! The value is generic; callers insert with an explicit byte size (the
//! display-list heap estimate, a tile's pixel buffer, a parsed page's bytes).

use std::collections::HashMap;

use selis_geom::Matrix;

/// A stable id for a [`Matrix`], for cache keying.
///
/// FNV-1a over the IEEE bits of the six elements, so the id is byte-stable
/// across platforms (ADR-P0012).
#[must_use]
pub fn matrix_id(m: Matrix) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
        for b in v.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// The four-part cache key: `(page, matrix, params, revision)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// The page number (0-based).
    pub page: u32,
    /// A stable id of the page-to-device matrix.
    pub matrix_id: u64,
    /// A stable id of the render parameters (`RenderParams::cache_id()` from
    /// `selis-raster`, or any stable id the caller supplies).
    pub params_id: u64,
    /// The document revision; increments on every edit.
    pub revision: u64,
}

impl CacheKey {
    /// A cache key from a page, a matrix, a render-params id, and the current
    /// document revision.
    #[must_use]
    pub fn new(page: u32, matrix: Matrix, params_id: u64, revision: u64) -> Self {
        Self {
            page,
            matrix_id: matrix_id(matrix),
            params_id,
            revision,
        }
    }
}

/// A memory-budgeted LRU cache (ADR-P0025).
#[derive(Debug)]
pub struct LruCache<V> {
    /// The eviction threshold, in bytes.
    budget_bytes: u64,
    /// The current total size of the stored values, in bytes.
    used_bytes: u64,
    /// A monotonic access clock for LRU ordering.
    clock: u64,
    /// The entries, keyed by identity.
    map: HashMap<CacheKey, Entry<V>>,
}

#[derive(Debug)]
struct Entry<V> {
    value: V,
    size_bytes: u64,
    /// The last access stamp (higher = more recently used).
    stamp: u64,
}

impl<V> LruCache<V> {
    /// An empty cache with the given byte budget.
    #[must_use]
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            budget_bytes,
            used_bytes: 0,
            clock: 0,
            map: HashMap::new(),
        }
    }

    /// The configured byte budget.
    #[must_use]
    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// The current total stored bytes.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The value for `key`, marking it most-recently-used.
    #[must_use]
    pub fn get(&mut self, key: &CacheKey) -> Option<&V> {
        self.clock = self.clock.saturating_add(1);
        self.map.get_mut(key).map(|e| {
            e.stamp = self.clock;
            &e.value
        })
    }

    /// Insert or replace `value` under `key`, charging `size_bytes` to the
    /// budget and evicting the least-recently-used entries while over budget.
    pub fn insert(&mut self, key: CacheKey, value: V, size_bytes: u64) {
        self.clock = self.clock.saturating_add(1);
        if let Some(existing) = self.map.get_mut(&key) {
            self.used_bytes = self
                .used_bytes
                .saturating_sub(existing.size_bytes)
                .saturating_add(size_bytes);
            existing.value = value;
            existing.size_bytes = size_bytes;
            existing.stamp = self.clock;
        } else {
            self.map.insert(
                key,
                Entry {
                    value,
                    size_bytes,
                    stamp: self.clock,
                },
            );
            self.used_bytes = self.used_bytes.saturating_add(size_bytes);
        }
        self.evict();
    }

    /// Remove the entry for `key` exactly (no collateral eviction).
    pub fn invalidate(&mut self, key: &CacheKey) {
        if let Some(entry) = self.map.remove(key) {
            self.used_bytes = self.used_bytes.saturating_sub(entry.size_bytes);
        }
    }

    /// Remove every entry for `page`, leaving all other pages untouched —
    /// the "exact invalidation on edit" contract.
    pub fn invalidate_page(&mut self, page: u32) {
        let keys: Vec<CacheKey> = self
            .map
            .keys()
            .filter(|k| k.page == page)
            .copied()
            .collect();
        for key in keys {
            self.invalidate(&key);
        }
    }

    /// Clear the cache entirely.
    pub fn clear(&mut self) {
        self.map.clear();
        self.used_bytes = 0;
    }

    /// Evict the least-recently-used entries until within the budget.
    fn evict(&mut self) {
        while self.used_bytes > self.budget_bytes {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.stamp)
                .map(|(k, _)| *k);
            match oldest {
                Some(key) => self.invalidate(&key),
                None => break, // empty cache over budget: nothing to evict
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    fn key(page: u32) -> CacheKey {
        CacheKey::new(page, Matrix::IDENTITY, 7, 1)
    }

    /// A zero-filled buffer through the sanctioned allocator.
    fn zeros(n: usize) -> Vec<u8> {
        let mut g = selis_sandbox::Budget::unlimited().guard();
        selis_sandbox::alloc::vec_filled(&mut g, n, 0u8).expect("unlimited budget")
    }

    #[test]
    fn insert_get_roundtrip() {
        let mut cache = LruCache::new(1000);
        cache.insert(key(0), vec![1u8, 2, 3], 3);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.used_bytes(), 3);
        assert_eq!(cache.get(&key(0)), Some(&vec![1u8, 2, 3]));
    }

    #[test]
    fn lru_evicts_the_oldest_under_budget() {
        // Budget 10 bytes; inserting a third 4-byte value forces eviction.
        let mut cache = LruCache::new(10);
        cache.insert(key(0), zeros(4), 4);
        cache.insert(key(1), zeros(4), 4);
        // Touch page 0 so page 1 is the oldest (the discarded `Option` is
        // the point: the touch, not the value, drives LRU recency).
        let _ = cache.get(&key(0));
        cache.insert(key(2), zeros(4), 4); // would be 12 > 10
                                           // Page 1 (oldest) is gone; pages 0 and 2 remain.
        assert!(cache.get(&key(1)).is_none());
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.get(&key(2)).is_some());
        assert!(cache.used_bytes() <= 10);
    }

    #[test]
    fn replacing_a_value_adjusts_the_budget() {
        let mut cache = LruCache::new(100);
        cache.insert(key(0), zeros(4), 4);
        cache.insert(key(0), zeros(8), 8); // replace
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.used_bytes(), 8);
    }

    /// DoD: a mutation invalidates only the affected page's entries.
    #[test]
    fn invalidate_page_is_exact() {
        let mut cache = LruCache::new(1000);
        cache.insert(CacheKey::new(0, Matrix::IDENTITY, 7, 1), zeros(4), 4);
        cache.insert(CacheKey::new(1, Matrix::IDENTITY, 7, 1), zeros(4), 4);
        cache.insert(CacheKey::new(1, Matrix::IDENTITY, 8, 1), zeros(4), 4);
        // Edit touches page 1.
        cache.invalidate_page(1);
        // Page 0 survives; both page-1 entries (any params) are gone.
        assert!(cache
            .get(&CacheKey::new(0, Matrix::IDENTITY, 7, 1))
            .is_some());
        assert!(cache
            .get(&CacheKey::new(1, Matrix::IDENTITY, 7, 1))
            .is_none());
        assert!(cache
            .get(&CacheKey::new(1, Matrix::IDENTITY, 8, 1))
            .is_none());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn revision_distinguishes_stale_entries() {
        let mut cache = LruCache::new(1000);
        cache.insert(CacheKey::new(0, Matrix::IDENTITY, 7, 1), zeros(4), 4);
        // After an edit, the engine renders with revision 2.
        cache.insert(CacheKey::new(0, Matrix::IDENTITY, 7, 2), zeros(4), 4);
        // The stale revision-1 entry is not the current one.
        assert!(cache
            .get(&CacheKey::new(0, Matrix::IDENTITY, 7, 2))
            .is_some());
        assert_eq!(cache.len(), 2); // both retained until evicted or invalidated
    }

    #[test]
    fn different_matrix_ids_are_distinct_keys() {
        let mut cache = LruCache::new(1000);
        cache.insert(CacheKey::new(0, Matrix::IDENTITY, 7, 1), zeros(4), 4);
        cache.insert(CacheKey::new(0, Matrix::scale(2.0, 2.0), 7, 1), zeros(4), 4);
        assert_eq!(cache.len(), 2);
        assert_ne!(
            matrix_id(Matrix::IDENTITY),
            matrix_id(Matrix::scale(2.0, 2.0))
        );
    }
}
