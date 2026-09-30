//! The `selis-fallback/2` manifest: the lazily-fetched fallback faces.
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
//! ## Why every face carries its own `url`
//!
//! `selis-fallback/1` named each face and pinned its digest, but said nothing
//! about *where the bytes are*. The shell reconstructed `fallback/<name>.ttf.br`
//! from a convention duplicated in TypeScript, which meant a build that published
//! the payload anywhere else - under a CDN prefix, a version segment, a different
//! extension - fetched nothing and failed silently, while nothing detected the
//! generator and the reader disagreeing. The `url` field makes the generator's
//! actual path the one thing both sides read.
//!
//! It is **relative**, resolved by the caller against the base the manifest
//! itself was served from. An absolute URL baked into a build artifact would
//! break the moment the same artifact was promoted from staging to production,
//! and would let a manifest point the fetcher at another origin; both are
//! reasons the field is a path and not a location.
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
///
/// **2** added the per-face `url`. A 1 document has no `url`, so it fails to
/// deserialise - it is refused, never read with a reconstructed path.
pub const MANIFEST_VERSION: u32 = 2;

/// The manifest's media type, as served.
pub const MANIFEST_MIME: &str = "application/vnd.selis.fallback+json";

/// The suffix every `url` must end in, because it says how the bytes are
/// encoded: brotli-compressed TrueType.
///
/// The generator writes this and this validator requires it, so a manifest
/// cannot claim a payload the transport would decode wrongly. Pinned in
/// `fallback-manifest.ts` for the same reason.
pub const FACE_SUFFIX: &str = ".ttf.br";

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
    /// Where the compressed file is published, relative to the base the
    /// manifest itself was served from - e.g. `fallback/LiberationSans-Regular.ttf.br`.
    ///
    /// Relative, never absolute: the same artifact is promoted from staging to
    /// production, and an absolute URL would break on the move and would let a
    /// manifest send the fetcher to another origin. Validated on parse, before
    /// anything here is used to fetch anything.
    pub url: String,
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
        let m: Self = serde_json::from_slice(raw)
            // The hint is on the *parse* failure because that is where a
            // document from an older format lands: it deserialises into
            // everything except the field that format did not have. "missing
            // field `url`" is technically true and practically useless, so the
            // version this build reads is named alongside it.
            .map_err(|e| {
                ManifestError::Parse(format!(
                    "{e} (this build reads selis-fallback/{MANIFEST_VERSION})"
                ))
            })?;
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
            // Last, so the cheaper "this face is not what it says it is" rules
            // still report first for a face that is wrong in several ways.
            if let Some(why) = url_problem(&f.url) {
                return Err(ManifestError::Url(f.name.clone(), why));
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
    /// A face's `url` was not a plain relative path to a compressed face.
    /// Carries the face and the reason, because "the url is wrong" is not
    /// actionable and "the url for `X` is absolute" is.
    Url(String, &'static str),
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
            Self::Url(n, why) => {
                write!(f, "fallback manifest url for {n} is unusable: {why}")
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Why a `url` cannot be used, or `None` if it can.
///
/// The rule is one sentence: a face's `url` is a **plain relative path** to a
/// brotli-compressed font. Everything refused here is a way of being something
/// other than that.
///
/// ## Why the string is checked raw, not decoded
///
/// A `..` check that ran on the *decoded* path would be defeated by the same
/// path written `%2e%2e`, which is what an attacker writes when a raw check is
/// in the way - the URL layer decodes it and the fetcher walks up a directory.
/// So the traversal test decodes the one escape that can spell a dot and asks
/// what the segment would *become*, while the empty-segment and encoded-
/// separator tests look at the characters actually present. Checking only one
/// of the two forms is how a validator ends up rejecting `%2e%2e` and admitting
/// `..`, or the reverse.
fn url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() {
        return Some("it is empty");
    }
    // A backslash is a separator to some URL layers and a literal character to
    // others, so a path containing one means two different things depending on
    // who reads it. There is no legitimate face name that needs it.
    if url.contains('\\') {
        return Some("it contains a backslash");
    }
    // `//host/path` is a *network-path reference*: it inherits the scheme and
    // points at another host, which is the one thing a relative field must not
    // be able to do.
    if url.starts_with("//") {
        return Some("it is protocol-relative and would leave the payload origin");
    }
    if has_scheme(url) {
        return Some("it is absolute rather than relative to the manifest base");
    }
    if !url.ends_with(FACE_SUFFIX) {
        return Some("it does not end in the suffix the transport decodes");
    }
    for seg in url.split('/') {
        if seg.is_empty() {
            return Some("it has an empty path segment");
        }
        if is_dot_segment(seg) {
            return Some("it has a `.` or `..` path segment");
        }
        if has_encoded_separator(seg) {
            return Some("it hides a path separator inside a percent-escape");
        }
    }
    None
}

/// Does this path start with something that reads as a URL scheme?
///
/// `fallback/x.ttf.br` must not be mistaken for one: the first character is a
/// letter, but the run stops at the `/` without reaching a `:`, so it is a
/// relative path. Written as a scan rather than a split so the answer does not
/// depend on a library's URL parser and its idea of what is special.
fn has_scheme(url: &str) -> bool {
    let mut chars = url.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
            return false;
        }
    }
    false
}

/// Is this segment `.` or `..`, written either plainly or percent-encoded?
fn is_dot_segment(seg: &str) -> bool {
    let decoded = decode_dot_escapes(seg);
    decoded == "." || decoded == ".."
}

/// Percent-decode **only** the escape that can spell a dot (`%2e`, either
/// case), leaving every other escape exactly as written.
///
/// Deliberately not a general decoder: decoding `%25` first would let
/// `%252e%252e` become `%2e%2e` and then be mistaken for a real dot, and a
/// decoder that handled the full grammar would be a second URL parser to keep
/// in step with the first. Face names are ASCII identifiers, so the only
/// escapes that can appear are the ones a generator would never write - which
/// is exactly why they have to be understood to be refused.
fn decode_dot_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let escape: String = it.clone().take(2).collect();
        if escape.len() == 2 && escape.eq_ignore_ascii_case("2e") {
            out.push('.');
            for _ in escape.chars() {
                let _ = it.next();
            }
        } else {
            out.push('%');
        }
    }
    out
}

/// Does this segment percent-encode a `/` or a `\`, the two separators?
///
/// A `%2f` inside a segment is a separator the segment check cannot see, which
/// is the same smuggling trick as `%2e%2e` one level up.
fn has_encoded_separator(seg: &str) -> bool {
    let lower = seg.to_ascii_lowercase();
    lower.contains("%2f") || lower.contains("%5c")
}

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
            url: format!("fallback/{name}{FACE_SUFFIX}"),
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

    /// A `url` is only useful if it is a plain relative path. Every other shape
    /// is refused, and the reason is carried alongside the face so the failure
    /// says which entry was wrong and how.
    ///
    /// The list is deliberately exhaustive over the ways a relative path can
    /// stop being one: it can name another origin (absolute, protocol-relative),
    /// it can leave the payload directory (`..`), it can mean two different
    /// things to two readers (backslash, encoded separator), and it can name a
    /// file the transport would decode wrongly (wrong suffix).
    #[test]
    fn a_url_that_is_not_a_plain_relative_path_is_refused() {
        for (bad, why) in [
            // Absolute, in every spelling a generator or an attacker might use.
            ("https://cdn.example.test/fallback/x.ttf.br", "absolute"),
            ("http://cdn.example.test/x.ttf.br", "absolute"),
            ("file:///etc/passwd.ttf.br", "absolute"),
            ("HTTPS://CDN.EXAMPLE.TEST/x.ttf.br", "absolute"),
            ("data:font/ttf;base64,AAAA.ttf.br", "absolute"),
            // Inherits the scheme and names a host: the same escape, quieter.
            ("//cdn.example.test/fallback/x.ttf.br", "protocol-relative"),
            // Traversal, plainly and percent-encoded.
            ("../x.ttf.br", "`..`"),
            ("fallback/../../x.ttf.br", "`..`"),
            ("fallback/%2e%2e/x.ttf.br", "`..`"),
            ("fallback/%2E%2E/x.ttf.br", "`..`"),
            ("fallback/%2e./x.ttf.br", "`..`"),
            ("./x.ttf.br", "`..`"),
            // A backslash is a separator to some URL layers and a literal to
            // others, so the path means two different files depending on who
            // fetches it.
            ("fallback\\x.ttf.br", "backslash"),
            ("..\\..\\x.ttf.br", "backslash"),
            // Empty segments: a leading slash, a doubled one, a trailing one.
            ("/fallback/x.ttf.br", "empty path segment"),
            ("fallback//x.ttf.br", "empty path segment"),
            ("fallback/", "suffix"),
            // A separator smuggled through an escape the segment split cannot
            // see - the same trick as `%2e%2e`, one level up.
            ("fallback%2fx.ttf.br", "percent-escape"),
            ("fallback%2Fx.ttf.br", "percent-escape"),
            ("fallback%5cx.ttf.br", "percent-escape"),
            // The transport decodes brotli-compressed TrueType; anything else is
            // a claim about bytes nobody published that way.
            ("fallback/x.ttf", "suffix"),
            ("fallback/x.TTF.BR", "suffix"),
            ("fallback/x.ttf.gz", "suffix"),
            ("", "empty"),
        ] {
            let mut m = manifest();
            first(&mut m).url = bad.to_string();
            let got = FallbackManifest::parse(&json(&m));
            assert!(
                matches!(&got, Err(ManifestError::Url(_, reason)) if reason.contains(why)),
                "url {bad:?} should be refused as {why:?}, got {got:?}"
            );
        }
    }

    /// The list of refused shapes must not quietly become the list of *accepted*
    /// ones: a validator that refuses everything passes the test above. These
    /// are the shapes a generator may legitimately emit, including a layout that
    /// is not the one the shell used to reconstruct.
    #[test]
    fn a_plain_relative_url_is_accepted_whatever_the_layout() {
        for good in [
            "fallback/LiberationSans-Regular.ttf.br",
            "LiberationSans-Regular.ttf.br",
            "v2/fallback/LiberationSans-Regular.ttf.br",
            "custom/place/face.ttf.br",
            // A percent-escape that cannot spell a dot or a separator is left
            // alone rather than refused: this reader is not a second URL parser.
            "fallback/Liberation%20Sans.ttf.br",
        ] {
            let mut m = manifest();
            first(&mut m).url = good.to_string();
            assert!(
                FallbackManifest::parse(&json(&m)).is_ok(),
                "url {good:?} should be accepted"
            );
        }
    }

    /// A `selis-fallback/1` document has no `url`, and this build must refuse it
    /// rather than fall back to reconstructing the path it used to guess - a
    /// reader that quietly filled the field in would reintroduce exactly the
    /// silent-404 hole the field was added to close.
    #[test]
    fn an_old_format_manifest_is_refused_not_reconstructed() {
        let v1 = br#"{
          "version": 1,
          "faces": [
            {
              "name": "LiberationSans-Regular",
              "transfer_size": 100,
              "raw_size": 200,
              "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
          ]
        }"#;
        let err = FallbackManifest::parse(v1).expect_err("a v1 document is not this format");
        assert!(
            matches!(err, ManifestError::Parse(_)),
            "expected a parse refusal, got {err:?}"
        );
        // The message has to name the version, because "missing field `url`" is
        // what serde says and it explains nothing about which formats are
        // readable.
        let text = err.to_string();
        assert!(
            text.contains("url") && text.contains(&MANIFEST_VERSION.to_string()),
            "the refusal should name the missing field and the format, got: {text}"
        );
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
            b"{\"version\":2,\"faces\":\"not an array\"}",
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
