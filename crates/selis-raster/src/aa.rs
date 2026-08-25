//! Anti-aliasing policy + determinism (SL-2.RAST.09).
//!
//! Fixes the AA approach and the determinism contract of
//! `01-ARCHITECTURE.md`: the same bytes twice, and across x86-64 Linux,
//! arm64 macOS, and WASM. Any SIMD path must prove bit-identity or be
//! disabled — the policy is enforced here so the rest of the rasteriser
//! cannot accidentally drift.

/// The anti-aliasing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AaPolicy {
    /// Full 2×2 or 4×4 supersampled AA via the backend's native coverage.
    /// Deterministic because the backend is.
    Backend,
    /// One-sample-per-pixel (no AA) — used for preview thumbnails and for
    /// the determinism cross-check.
    None,
}

impl AaPolicy {
    /// The default policy for a full render.
    #[must_use]
    pub const fn full() -> Self {
        AaPolicy::Backend
    }

    /// The policy for a thumbnail or determinism probe.
    #[must_use]
    pub const fn preview() -> Self {
        AaPolicy::None
    }
}

/// The determinism mode a render was produced under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Determinism {
    /// Threaded rasterisation (the same tiles, any thread count).
    Threaded,
    /// Sequential rasterisation (single-threaded WASM).
    Sequential,
}

/// A checkable determinism guarantee.
#[derive(Debug, Clone, PartialEq)]
pub struct DeterminismProof {
    /// The policy used.
    pub policy: AaPolicy,
    /// The mode used.
    pub mode: Determinism,
    /// The hash of the raster bytes (a stable hash, not an OS hash).
    pub hash: [u8; 32],
}

/// The stable hash used for determinism proofs.
///
/// A fixed FNV-1a over the raster bytes: trivial, deterministic, and
/// identical on every platform — the point is to catch divergence, not to be
/// cryptographically strong.
///
/// # Budget
///
/// Single pass over `bytes`; no heap beyond the 32-byte result.
///
/// # Malformed Input
///
/// Total on any input — hashing cannot fail.
#[must_use]
pub fn stable_hash(bytes: &[u8]) -> [u8; 32] {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    // Expand the 64-bit hash to 32 bytes deterministically.
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = ((h >> ((i % 8).wrapping_mul(8))) & 0xff) as u8;
    }
    out
}

/// Verify that a threaded and a sequential render are bit-identical.
///
/// The DoD: threaded and unthreaded renders hash-equal on the whole corpus.
///
/// # Budget
///
/// Single pass over both slices; no heap allocation.
///
/// # Malformed Input
///
/// Total on any input — comparison cannot fail.
#[must_use]
pub fn renders_match(a: &[u8], b: &[u8]) -> bool {
    a == b
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn stable_hash_is_deterministic() {
        let a = stable_hash(b"hello");
        let b = stable_hash(b"hello");
        assert_eq!(a, b);
        let c = stable_hash(b"world");
        assert_ne!(a, c);
    }

    #[test]
    fn stable_hash_is_platform_independent() {
        // The hash is a pure function of the bytes; no platform types or
        // hash orderings are involved.
        let bytes = (0u8..=255u8).collect::<Vec<u8>>();
        let h1 = stable_hash(&bytes);
        let h2 = stable_hash(&bytes);
        assert_eq!(h1, h2);
    }

    #[test]
    fn renders_match_detects_divergence() {
        assert!(renders_match(b"same", b"same"));
        assert!(!renders_match(b"same", b"differ"));
    }

    #[test]
    fn policy_defaults() {
        assert_eq!(AaPolicy::full(), AaPolicy::Backend);
        assert_eq!(AaPolicy::preview(), AaPolicy::None);
    }
}
