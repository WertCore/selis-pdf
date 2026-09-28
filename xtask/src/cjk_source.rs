//! The pinned CJK source fonts `cjk-fetch` may download (SL-3.FONT.10).
//!
//! ## Why a pinned table rather than a URL argument
//!
//! The payload is a release artifact built from **one** source font, and every
//! byte of it is a subset of that font. If the source could move, two builds a
//! week apart would produce two different payloads under the same version, and
//! a cached chunk from one would be served to a reader of the other — the
//! manifest's SHA-256 would not match and the shell would (correctly) refuse
//! it. So the URL is pinned to an immutable commit *and* the bytes are pinned
//! to a SHA-256 the download is checked against. Both, not either: the commit
//! makes the URL immutable, the digest makes the *content* immutable even if a
//! mirror is compromised.
//!
//! This is the release pipeline's input, not the shipped product. Nothing here
//! is fetched by the extension (which holds no host permission and ships no
//! font — `apps/extension/src/size-budget.ts` fails the build on a `.ttf` in
//! the package) or by the viewer, which only ever sees the subset files this
//! pipeline produces.
//!
//! ## Why Noto Sans SC and not "Noto Sans CJK"
//!
//! ADR-P0043 says "a Noto Sans CJK static TTF". That file does not exist in a
//! form this toolchain can subset: the `notofonts/noto-cjk` releases are CFF
//! (`.otf`/OTC), and the FONT.11 subsetter subsets `glyf` only, refusing CFF
//! loudly rather than emitting an empty payload. The Google Fonts builds of
//! the Noto CJK family are TrueType-flavoured and carry 30 890 code points
//! each, so `noto-sans-sc` is the pinned production source.
//!
//! The cost of that choice is measured and published rather than assumed: a
//! Simplified Chinese source has **no Hangul syllables at all**, so the
//! `hangul-1`…`hangul-4` ranges come out `served_by: null` in the manifest and
//! a Korean document renders `.notdef` with a refusal the shell can name.
//! Japanese kana *are* covered (`punct-kana`). A Korean or Japanese payload
//! needs its own pinned source and its own manifest — one row here per
//! language, never a union, because ADR-P0043 requires every file in a payload
//! to be a subset of one source so a glyph's outlines cannot depend on which
//! file answered for it.
//!
//! ## The source is variable
//!
//! `noto-sans-sc` is the Google Fonts variable build (`[wght]`, wght 100–900).
//! The subsetter emits static SFNT and drops `fvar`/`gvar`, so a payload built
//! from it carries the **default instance's** outlines (wght 400) and has no
//! weight axis at all: bold CJK text is synthesised by the raster walk, the
//! same way the standard-14 substitution already handles it. That is a
//! deliberate, stated loss, not an accident — and it is why a future
//! multi-weight payload is a FONT.11 task, not a `cjk-build` flag.

/// A source font `cjk-fetch` knows how to obtain and verify.
#[derive(Debug, Clone, Copy)]
pub struct PinnedSource {
    /// Stable id (`--source-id`), recorded in the manifest's `source.id`.
    pub id: &'static str,
    /// The upstream family name, for the manifest and the provenance file.
    pub family: &'static str,
    /// The file name to write locally.
    pub file: &'static str,
    /// The download URL — pinned to an immutable commit, never a branch.
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the file's bytes.
    pub sha256: &'static str,
    /// The file's size in bytes, checked alongside the digest.
    pub bytes: u64,
    /// The licence the subsets inherit. OFL 1.1 permits embedding and
    /// redistribution; its reserved-name clause is why the subsets are renamed
    /// (see `selis_font::SubsetName`).
    pub license: &'static str,
    /// The OFL's Reserved Font Name for this family, if it has one. `Noto Sans
    /// SC` is a Google Fonts build of Adobe's Source Han Sans, whose OFL
    /// reserves "Source" — so a modified version may not be distributed under
    /// that name, and the payload's files are published as `Selis CJK`.
    pub reserved_font_name: Option<&'static str>,
}

impl PinnedSource {
    /// One line describing coverage, for error messages and the provenance
    /// file. Deliberately a claim about the *font*, not about a payload: what
    /// a build of it covers is measured per build and written to the manifest.
    #[must_use]
    pub fn coverage(&self) -> &'static str {
        match self.id {
            "noto-sans-sc" => {
                "Simplified Chinese, Latin, kana, CJK punctuation; no Hangul syllables"
            }
            _ => "unpinned",
        }
    }
}

/// Every source this toolchain may fetch.
///
/// Adding a row here is a supply-chain decision (ADR-P0021): the file, the
/// commit it came from, the digest, and the licence. Nothing is fetched by
/// name at runtime, and no row may point at a moving ref.
pub const PINNED_SOURCES: &[PinnedSource] = &[PinnedSource {
    id: "noto-sans-sc",
    family: "Noto Sans SC",
    file: "NotoSansSC-wght.ttf",
    url: "https://raw.githubusercontent.com/google/fonts/a85815a42757630ce188fdad368c2dfc444d4773/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf",
    sha256: "a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da",
    bytes: 17_772_300,
    license: "SIL Open Font License 1.1",
    reserved_font_name: Some("Source"),
}];

/// The pinned source with this id, or `None`.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static PinnedSource> {
    PINNED_SOURCES.iter().find(|s| s.id == id)
}

/// Every pinned source id, for error messages.
#[must_use]
pub fn ids() -> Vec<&'static str> {
    PINNED_SOURCES.iter().map(|s| s.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_is_pinned_by_commit_and_digest() {
        for s in PINNED_SOURCES {
            assert!(
                !s.id.is_empty() && s.id.bytes().all(|c| c.is_ascii_lowercase() || c == b'-'),
                "{}: unsafe id",
                s.id
            );
            assert_eq!(s.sha256.len(), 64, "{}: full-length hex digest", s.id);
            assert!(
                s.sha256
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{}: lowercase hex digest",
                s.id
            );
            assert!(s.bytes > 0, "{}: the pinned size is a second check", s.id);
            assert!(
                s.license.contains("License") || s.license.contains("OFL"),
                "{}: the licence must be named, not implied",
                s.id
            );
            if let Some(rfn) = s.reserved_font_name {
                assert!(!rfn.is_empty(), "{}: an empty RFN reserves nothing", s.id);
            }
        }
    }

    #[test]
    fn no_source_url_points_at_a_moving_ref() {
        for s in PINNED_SOURCES {
            assert!(
                !s.url.contains("/main/") && !s.url.contains("/master/"),
                "{}: a branch is not a pin",
                s.id
            );
            assert!(s.url.starts_with("https://"), "{}: https only", s.id);
            // The google/fonts pin reads .../<40-hex commit>/<path>.
            let commit = s
                .url
                .split('/')
                .find(|seg| seg.len() == 40 && seg.bytes().all(|c| c.is_ascii_hexdigit()));
            assert!(commit.is_some(), "{}: no commit sha in {}", s.id, s.url);
        }
    }

    #[test]
    fn ids_are_unique_and_lookup_works() {
        let mut seen = std::collections::BTreeSet::new();
        for s in PINNED_SOURCES {
            assert!(seen.insert(s.id), "duplicate id {}", s.id);
            assert_eq!(by_id(s.id).map(|p| p.url), Some(s.url));
        }
        assert!(by_id("not-a-pinned-source").is_none());
        assert_eq!(ids().len(), PINNED_SOURCES.len());
    }

    #[test]
    fn coverage_prose_is_per_id_and_says_what_is_missing() {
        for s in PINNED_SOURCES {
            assert!(!s.coverage().is_empty());
        }
        assert!(by_id("noto-sans-sc")
            .expect("pinned")
            .coverage()
            .contains("no Hangul"));
    }
}
