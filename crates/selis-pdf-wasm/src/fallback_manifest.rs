//! The `selis-fallback/1` manifest: the lazily-fetched fallback faces.
//!
//! **A font from the network is not a font from the build.** A TTF is a
//! structured binary that a parser reads, and a parser that reads attacker-
//! chosen bytes is a memory-safety surface, not just a correctness one. So a
//! face is only ever handed to the engine after its SHA-256 matches the digest
//! recorded here — the same discipline the CJK payload already uses, and the
//! reason a subresource-integrity gap in the hosting page does not become a
//! font-parsing exploit.
//!
//! The digests are pinned at build time from the real files, so this is a
//! build artifact, not a hand-maintained list. `cargo xtask fallback-assets`
//! regenerates it; it also regenerates the subsets, so a font update cannot
//! leave the digests describing bytes that are no longer shipped.
//!
//! ## Why the manifest is not enough on its own
//!
//! A matching digest means the bytes are the ones we shipped. It does not mean
//! they were safe to *ship*. A compromised upstream Liberation release would be
//! faithfully reproduced by a matching digest, so the digests are a transport
//! check, not a supply-chain review. Recording that here so nobody later reads
//! "digest verified" as "trusted".

//! ## Why this lives in the wasm crate and not in `selis-font`
//!
//! The manifest is a delivery artifact, not font logic: it describes bytes in
//! transit, and the crate that fetches them owns the trust boundary. Putting it
//! in `selis-font` would have pulled `serde` into a parser crate that
//! deliberately has no serialization dependency - against the grain of the
//! sizing work that created the need for lazy faces in the first place. The
//! font-side counterpart is `selis_font::fallback_set::FallbackFontSet`, which
//! holds the verified bytes and stays dependency-free.

use serde::{Deserialize, Serialize};

/// Format version. Bump on any incompatible manifest change; readers reject
/// versions they do not know rather than guessing at a partial parse.
pub const MANIFEST_VERSION: u32 = 1;

/// The manifest's media type, as served.
pub const MANIFEST_MIME: &str = "application/vnd.selis.fallback+json";

/// One lazily-fetched fallback face.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackFace {
    /// The `family + style` name `fallback::substitute` produces, e.g.
    /// `LiberationSerif-Italic`. This is the key the engine looks up, and it
    /// is spelled the same way as in `fallback.rs` so the two can be checked
    /// against each other.
    pub name: String,
    /// Size of the transferred (post-compression) payload, for budgeting.
    pub transfer_size: u32,
    /// Size of the decompressed font, which is what the engine actually holds.
    pub raw_size: u32,
    /// Lowercase hex SHA-256 of the *decompressed* font bytes.
    ///
    /// Deliberately the raw bytes, not the transfer bytes: the digest is what
    /// the parser consumes, so that is what must be verified. The compression
    /// layer is covered by whatever protects the transfer itself.
    pub sha256: String,
}

/// The whole lazy payload: every fallback face this build does not embed, with
/// the sizes and digests needed to fetch and verify them.
///
/// Rejecting a bad manifest whole rather than skipping the bad entries is
/// deliberate. A manifest naming one face with a malformed digest is evidence
/// that something is wrong with the manifest, and serving the other nine would
/// mean rendering a document with nine of its ten fallbacks while looking
/// healthy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackManifest {
    /// Format version, checked against MANIFEST_VERSION on parse.
    pub version: u32,
    /// The faces on offer, sorted by name by the generator.
    pub faces: Vec<FallbackFace>,
}

impl FallbackManifest {
    /// A manifest, checking the invariants a reader is entitled to assume.
    ///
    /// Returns `Err` rather than a partially-valid manifest, because every
    /// caller of a bad manifest has the same reaction: stop and load nothing.
    pub fn parse(raw: &[u8]) -> Result<Self, ManifestError> {
        let m: Self =
            serde_json::from_slice(raw).map_err(|e| ManifestError::Parse(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    /// The invariants a manifest must hold to be usable.
    fn validate(&self) -> Result<(), ManifestError> {
        if self.version != MANIFEST_VERSION {
            return Err(ManifestError::Version(self.version));
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in &self.faces {
            if !seen.insert(f.name.as_str()) {
                // A duplicate is not a harmless redundancy: the reader would
                // have to pick a winner, and two different digests for one
                // name means the two faces disagree about what the font is.
                return Err(ManifestError::Duplicate(f.name.clone()));
            }
            // **Lowercase only**, matching what the guest will actually accept.
            // `is_ascii_hexdigit` also admits A-F, so an uppercase digest would
            // pass this validator and then be refused at `fallbackOpen`, where
            // `parse_digest` matches only `0-9a-f`. A build-time manifest that
            // validates and then fails at load is the worst of both: the error
            // surfaces far from its cause, in the browser, with no way to fix
            // it from the build log.
            if f.sha256.len() != 64
                || !f
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err(ManifestError::Digest(f.name.clone()));
            }
            if f.raw_size == 0 || f.transfer_size == 0 {
                return Err(ManifestError::Size(f.name.clone()));
            }
            if f.transfer_size > f.raw_size {
                // Compression made it bigger. Not fatal, but it means the
                // face was probably never compressed and the budget above is
                // fiction.
                return Err(ManifestError::TransferLarger(f.name.clone()));
            }
        }
        Ok(())
    }

    /// The entry for a face, if the manifest carries one.
    #[must_use]
    pub fn face(&self, name: &str) -> Option<&FallbackFace> {
        self.faces.iter().find(|f| f.name == name)
    }
}

/// Why a manifest was refused. Every variant is a reason to load nothing, and
/// each names the face involved so the failure is attributable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The bytes were not valid JSON for this format.
    Parse(String),
    /// A version this build does not know how to read.
    Version(u32),
    /// The same face name appeared more than once.
    Duplicate(String),
    /// A digest was not 64 hex characters.
    Digest(String),
    /// A face declared a zero size.
    Size(String),
    /// A face claimed to transfer larger than it stores, so its stated budget
    /// is not describing anything real.
    TransferLarger(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "fallback manifest is not valid JSON: {e}"),
            Self::Version(v) => {
                write!(
                    f,
                    "fallback manifest version {v} is not supported (expected {MANIFEST_VERSION})"
                )
            }
            Self::Duplicate(n) => write!(f, "fallback manifest lists {n} twice"),
            Self::Digest(n) => write!(f, "fallback manifest digest for {n} is not 64 hex chars"),
            Self::Size(n) => write!(f, "fallback manifest has a zero size for {n}"),
            Self::TransferLarger(n) => {
                write!(
                    f,
                    "fallback manifest says {n} transfers larger than it stores"
                )
            }
        }
    }
}

impl std::error::Error for ManifestError {}

#[cfg(test)]
mod tests {
    // Fixture code only, and the same scoping the `cjkchunk` tests use: an
    // `expect` here is the assertion, not a crash primitive reached from
    // untrusted input. These tests build the manifest in memory precisely so
    // the production validator - the code that does handle untrusted bytes -
    // needs no such escape hatch. Deliberately scoped to this module.
    #![allow(clippy::expect_used)]
    use super::*;

    fn good_digest() -> String {
        "a".repeat(64)
    }

    /// The first face, mutably. Indexes safely so these tests need no
    /// `indexing_slicing` allow.
    fn first(m: &mut FallbackManifest) -> &mut FallbackFace {
        m.faces
            .first_mut()
            .expect("the fixture always has at least one face")
    }

    fn face(name: &str) -> FallbackFace {
        FallbackFace {
            name: name.to_string(),
            transfer_size: 100,
            raw_size: 200,
            sha256: good_digest(),
        }
    }

    fn manifest() -> FallbackManifest {
        FallbackManifest {
            version: MANIFEST_VERSION,
            faces: vec![
                face("LiberationSans-Regular"),
                face("LiberationSerif-Italic"),
            ],
        }
    }

    fn json(m: &FallbackManifest) -> Vec<u8> {
        serde_json::to_vec(m).unwrap()
    }

    /// The happy path, and the thing a future format bump would break.
    #[test]
    fn a_well_formed_manifest_parses() {
        let m = FallbackManifest::parse(&json(&manifest())).expect("valid");
        assert_eq!(m.faces.len(), 2);
        assert!(m.face("LiberationSans-Regular").is_some());
        assert!(m.face("LiberationMono-Bold").is_none());
    }

    /// A version this build does not know must be refused, not partially read.
    /// Guessing at an unknown shape is how a reader ends up trusting a digest
    /// it actually ignored.
    #[test]
    fn an_unknown_version_is_refused() {
        let mut m = manifest();
        m.version = MANIFEST_VERSION + 1;
        assert_eq!(
            FallbackManifest::parse(&json(&m)),
            Err(ManifestError::Version(MANIFEST_VERSION + 1))
        );
    }

    /// Two entries for one name means two claims about what the font is. The
    /// reader would have to pick a winner, so neither is served.
    #[test]
    fn a_duplicate_face_is_refused() {
        let mut m = manifest();
        m.faces.push(face("LiberationSans-Regular"));
        assert_eq!(
            FallbackManifest::parse(&json(&m)),
            Err(ManifestError::Duplicate(
                "LiberationSans-Regular".to_string()
            ))
        );
    }

    /// A digest that is not 64 hex characters cannot be a SHA-256, and
    /// accepting one would mean the verification was skipped rather than
    /// passed.
    #[test]
    fn a_malformed_digest_is_refused() {
        // Uppercase is included deliberately: the guest's digest parser accepts
        // only `0-9a-f`, so a manifest that admitted A-F would build cleanly and
        // then be refused when the browser opened it.
        for bad in [
            "",
            "abc",
            &"z".repeat(64),
            &"a".repeat(63),
            &"a".repeat(65),
            &"A".repeat(64),
        ] {
            let mut m = manifest();
            first(&mut m).sha256 = bad.to_string();
            assert!(
                matches!(
                    FallbackManifest::parse(&json(&m)),
                    Err(ManifestError::Digest(_))
                ),
                "digest {bad:?} should be refused"
            );
        }
    }

    /// A zero size means the budget for that face is fiction, so the entry is
    /// rejected rather than served as a free download.
    #[test]
    fn a_zero_size_is_refused() {
        let mut m = manifest();
        first(&mut m).raw_size = 0;
        assert!(matches!(
            FallbackManifest::parse(&json(&m)),
            Err(ManifestError::Size(_))
        ));
    }

    /// Compressed-larger-than-raw means the payload was not compressed, so the
    /// transfer budget does not describe the transfer.
    #[test]
    fn a_transfer_larger_than_its_source_is_refused() {
        let mut m = manifest();
        first(&mut m).transfer_size = 201;
        assert!(matches!(
            FallbackManifest::parse(&json(&m)),
            Err(ManifestError::TransferLarger(_))
        ));
    }

    /// Garbage in, `Err` out — never a panic, because this parses bytes from
    /// the network and a panic there is a denial of service.
    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        for bad in [
            &b""[..],
            b"not json",
            b"[]",
            b"{}",
            b"{\"version\":1,\"faces\":\"not an array\"}",
        ] {
            assert!(
                FallbackManifest::parse(bad).is_err(),
                "{bad:?} should be an error"
            );
        }
    }

    /// The committed manifest must satisfy its own validator. This is the test
    /// that catches a hand-edited or stale committed manifest, which no
    /// amount of runtime validation would notice, because the runtime is what
    /// trusts it.
    #[test]
    fn the_committed_manifest_is_valid() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/payload/fallback/manifest.json"
        );
        let raw = match std::fs::read(path) {
            Ok(r) => r,
            // The payload is a build artifact and may not exist in every
            // checkout; `cargo xtask fallback-assets --check` is the gate that
            // requires it.
            Err(_) => return,
        };
        let m = FallbackManifest::parse(&raw).expect("committed manifest is valid");
        assert!(!m.faces.is_empty(), "committed manifest carries no faces");
    }
}
