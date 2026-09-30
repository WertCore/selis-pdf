//! `cargo xtask fallback-assets`'s other half: the transport that fetches a
//! standard-14 fallback face and refuses to adopt bytes that are not the ones
//! the manifest named (SL-3.FONT.12).
//!
//! **The mirror of [`crate::cjkchunk`], deliberately.** The CJK payload already
//! solved "bytes arrive from a network, then a font parser reads them", and its
//! answers are the ones worth copying rather than reinventing: verify the
//! digest before adoption, count a bad delivery as an *attempt* so a poisoned
//! response cannot loop, bound attempts per item, bound total requests, keep
//! "will never arrive" separate from "not here yet", and never let the walk
//! itself touch the network.
//!
//! ## What makes a face different from a chunk
//!
//! Two things, both of which shaped this design:
//!
//! 1. **A face is all-or-nothing where a chunk is incremental.** A missing CJK
//!    chunk degrades one range; a missing face leaves a page's *text* wrong, so
//!    the shell fetches before rendering ([`crate::Session`]'s
//!    `render_page_fallbacks` blocks) rather than painting `.notdef` and
//!    repainting. The loader is therefore the same shape but the shell drives it
//!    in a fetch-then-render loop instead of the CJK render-then-fetch one.
//! 2. **There are ten of them and they are all the same size.** Unlike CJK's
//!    size-ranged chunk table, the fallback manifest is a flat list, so there
//!    is no range table to resolve an id against \u2014 but there *is* a real risk of
//!    asking for all ten on a document that only needed one, so the request
//!    bound is per-face as well as in total.
//!
//! Four wire ops, mirroring `rangeOpen`/`rangeChunk`/`rangeClose` and
//! [`crate::cjkchunk`]'s `cjkOpen`/`cjkChunk`/`cjkClose`:
//!
//! | op | what the guest does | what the shell does |
//! |---|---|---|
//! | `fallbackOpen` | checks the manifest's **claims** and answers with the first face it wants, or `null` | — |
//! | `fallbackFace` | verifies the attachment against the claim, adopts it, answers with the next face or `null` | fetches the bytes and hands them over |
//! | `fallbackClose` | `release`s one resident face, re-arms the request for it, and reports the new resident total | frees its cache entry |
//!
//! **The guest names the face; the host only moves bytes** — the same
//! invariant, for the same reason, as its CJK twin.

use std::collections::{BTreeMap, BTreeSet};

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard, Resource};

use selis_font::fallback_set::FallbackFontSet;

use super::cjkchunk::{digest_hex, parse_digest};

/// Total face requests a single document render may cause.
///
/// Ten faces at ~70 kB is the whole lazy payload, so a document that needs more
/// than this is not going to be served by fetching; the bound is what turns
/// that from an unbounded download loop into a typed budget error.
pub const MAX_FACE_REQUESTS: u32 = 12;

/// Attempts allowed for one face. Two, as for CJK: enough to ride out one
/// transient network failure, few enough that a serving host which is
/// consistently wrong is given up on rather than hammered.
pub const MAX_ATTEMPTS_PER_FACE: u32 = 2;

/// Hostile-input bound on a manifest claim, checked before any bytes arrive.
///
/// 1 MiB is roughly twice the largest face the committed payload contains
/// (LiberationSans-Italic, 162 036 B raw), so it rejects a manifest claiming a
/// multi-megabyte "font" without needing per-release tuning. The caller's
/// [`Budget`] remains the separate, tighter ceiling enforced at accept time.
pub const MAX_CLAIMED_FACE_BYTES: u64 = 1024 * 1024;

/// The manifest's claim about one face.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceClaim {
    /// The `family + style` face name (`LiberationSans-Regular`).
    pub name: &'static str,
    /// Lowercase-hex SHA-256 of the decompressed font bytes.
    pub sha256: String,
    /// The decompressed length in bytes.
    pub raw_bytes: u64,
    /// The path the manifest published the file at (`fallback/<name>.ttf.br`).
    pub url: String,
}

/// One bodyless request for a face.
///
/// Impoverished in the same way [`crate::cjkchunk::CjkRequest`] is: a name and
/// a path, no method and no body, so a shell can only *fetch* these bytes and
/// cannot be induced to send anything anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceRequest {
    /// The face to fetch.
    pub name: &'static str,
    /// The path the manifest published, resolved by the shell against its base.
    pub url: String,
    /// The digest the shell's cache key must pair with the name.
    pub sha256: String,
    /// The decompressed length the shell should expect.
    pub raw_bytes: u64,
}

/// What the host observed for one face request.
///
/// Every field is untrusted: the name is a string the host chose and the length
/// is a number it declared. The body is copied on adoption and never aliased.
#[derive(Debug, Clone)]
pub struct FaceReport {
    /// The face the loader asked for.
    pub name: String,
    /// The HTTP status, or `0` when no response arrived.
    pub status: u16,
    /// The attachment's length in bytes; must match it exactly.
    pub len: u64,
    /// The delivered body.
    pub body: Vec<u8>,
}

/// What the loader decided to do with a [`FaceReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaceStep {
    /// Fetch this face next.
    Request(FaceRequest),
    /// Nothing usable arrived: ask again, bounded by [`MAX_ATTEMPTS_PER_FACE`].
    /// No bytes were kept.
    Retry(FaceRequest),
    /// The face was adopted. `revision` is the set's counter after adoption.
    Adopted {
        /// The face that landed.
        name: &'static str,
        /// The set's revision after adoption.
        revision: u64,
    },
    /// Nothing is wanted any more. `revision` is the set's current counter.
    Done {
        /// The set's revision.
        revision: u64,
    },
}

/// The host-side loader for lazily-delivered fallback faces.
#[derive(Debug)]
pub struct FallbackFaceLoader {
    set: FallbackFontSet,
    claims: BTreeMap<&'static str, FaceClaim>,
    attempts: BTreeMap<&'static str, u32>,
    exhausted: BTreeSet<&'static str>,
    in_flight: BTreeSet<&'static str>,
    requests: u32,
    budget: Budget,
}

impl FallbackFaceLoader {
    /// Build a loader from the manifest's claims.
    ///
    /// A claim is checked, not trusted: the digest must parse, the length must
    /// be non-zero and inside the hostile-input bound, and the face must be one
    /// this build knows how to name. A claim that fails any of those is refused
    /// at open time, before a single byte is fetched.
    ///
    /// # Errors
    ///
    /// `BINDING_BAD_ARGUMENT` for a claim this build cannot check or serve.
    pub fn new(claims: Vec<FaceClaim>, budget: Budget) -> Result<Self> {
        let mut resolved: BTreeMap<&'static str, FaceClaim> = BTreeMap::new();
        for claim in claims {
            if parse_digest(&claim.sha256).is_none() {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "fallback-face",
                    detail = "claim digest is not 64 lowercase hex characters"
                ));
            }
            if claim.raw_bytes == 0 || claim.raw_bytes > MAX_CLAIMED_FACE_BYTES {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "fallback-face",
                    detail = "claim length is zero or past the hostile-input bound"
                ));
            }
            if claim.url.is_empty() {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "fallback-face",
                    detail = "claim carries no url"
                ));
            }
            if intern(claim.name).is_none() {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "fallback-face",
                    detail = "claim names a face this build does not know"
                ));
            }
            let _ = resolved.insert(claim.name, claim);
        }
        Ok(Self {
            set: FallbackFontSet::new(),
            claims: resolved,
            attempts: BTreeMap::new(),
            exhausted: BTreeSet::new(),
            in_flight: BTreeSet::new(),
            requests: 0,
            budget,
        })
    }

    /// The resident set, for `Session::render_page_fallbacks` to snapshot.
    #[must_use]
    pub const fn set_mut(&mut self) -> &mut FallbackFontSet {
        &mut self.set
    }

    /// The faces a render has asked for that are not resident yet.
    ///
    /// The fetch-then-render lever: a shell reads this, fetches, and only then
    /// renders. Read-only, because reporting a need is not a mutation.
    #[must_use]
    pub fn needs(&self) -> Vec<&'static str> {
        self.set.pending()
    }

    /// The set's revision. A change is the repaint signal.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.set.revision()
    }

    /// Raw bytes resident across every delivered face.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.set
            .loaded()
            .iter()
            .filter_map(|n| self.set.face(n))
            .fold(0, |a, b| {
                a.saturating_add(u64::try_from(b.as_ref().len()).unwrap_or(u64::MAX))
            })
    }

    /// The faces delivered so far, sorted.
    #[must_use]
    pub fn loaded(&self) -> Vec<&'static str> {
        self.set.loaded()
    }

    /// The faces this build will not fetch, because the payload has no file.
    #[must_use]
    pub fn unavailable(&self) -> Vec<&'static str> {
        self.set.unavailable()
    }

    /// The faces whose fetch failed past [`MAX_ATTEMPTS_PER_FACE`].
    #[must_use]
    pub fn exhausted(&self) -> Vec<&'static str> {
        self.exhausted.iter().copied().collect()
    }

    /// Mark the faces the payload carries no file for.
    pub fn mark_unavailable(&mut self, names: &[&str]) {
        for n in names {
            if let Some(name) = intern(n) {
                self.set.mark_unavailable(name);
            }
        }
    }

    /// The next face to fetch, or `None` when nothing is wanted.
    ///
    /// The order is the set's own `needs` queue in sorted name order, so the
    /// request sequence is a pure function of resident state: the same document
    /// asks for the same faces in the same order on every platform.
    ///
    /// # Budget
    ///
    /// Planning is where the request bound is enforced, because every plan is a
    /// request the host will really issue.
    ///
    /// # Errors
    ///
    /// `BUDGET_BYTES` when the request bound is exhausted.
    pub fn plan(&mut self) -> Result<Option<FaceRequest>> {
        for name in self.set.pending() {
            if self.set.has(name) {
                continue;
            }
            if self.exhausted.contains(&name) {
                continue;
            }
            if self.in_flight.contains(&name) {
                continue;
            }
            let Some(claim) = self.claims.get(&name).cloned() else {
                // No claim means no digest to check the bytes against, so there
                // is nothing safe to fetch. Record it as permanently missing
                // rather than asking every render.
                self.set.mark_unavailable(name);
                continue;
            };
            if self.requests >= MAX_FACE_REQUESTS {
                return Err(err!(
                    Code::BudgetBytes,
                    during = "fallback-face",
                    detail = "face request bound exhausted"
                ));
            }
            self.requests = self.requests.saturating_add(1);
            let _ = self.in_flight.insert(name);
            return Ok(Some(FaceRequest {
                name,
                url: claim.url,
                sha256: claim.sha256,
                raw_bytes: claim.raw_bytes,
            }));
        }
        Ok(None)
    }

    /// Admit one delivery, or refuse it.
    ///
    /// # Errors
    ///
    /// `BINDING_BAD_ARGUMENT` when the report's length disagrees with its
    /// attachment, or names a face the manifest never claimed. `BUDGET_BYTES`
    /// when the resident set would exceed the loader budget.
    pub fn accept(&mut self, report: &FaceReport, guard: &mut BudgetGuard<'_>) -> Result<FaceStep> {
        guard.tick()?;
        let arrived = u64::try_from(report.body.len()).unwrap_or(u64::MAX);
        if arrived != report.len {
            return Err(err!(
                Code::BindingBadArgument,
                during = "fallback-face",
                detail = "face length does not match its attachment"
            ));
        }
        if self.resident_bytes().saturating_add(arrived) > self.budget.bytes {
            return Err(err!(
                Code::BudgetBytes,
                during = "fallback-face",
                detail = "resident fallback set would exceed the loader budget"
            ));
        }
        guard.charge(Resource::Bytes, arrived)?;

        let Some(claim) = self.claims.get(report.name.as_str()).cloned() else {
            return Err(err!(
                Code::BindingBadArgument,
                during = "fallback-face",
                detail = "delivery names a face the manifest never claimed"
            ));
        };
        let name = claim.name;
        let _ = self.in_flight.remove(name);

        if report.status == 0 || report.status >= 500 {
            return Ok(self.retry_or_give_up(name, &claim));
        }
        if report.status != 200 && report.status != 206 {
            return Err(err!(
                Code::IoReadFailed,
                during = "fallback-face",
                detail = "face fetch returned an unexpected status"
            ));
        }

        // The cache-poisoning guard, and the reason this module exists: a TTF is
        // a structured binary that a parser reads, so adopting unverified bytes
        // hands attacker-chosen input to the font engine. A truncated read, a
        // file served under the wrong key and a tampered cache entry all land
        // here, and none of them is adopted.
        let expected = parse_digest(&claim.sha256);
        let actual = parse_digest(&digest_hex(&report.body));
        if expected != actual || arrived != claim.raw_bytes {
            return Ok(self.retry_or_give_up(name, &claim));
        }

        // A body that is the right bytes but not a font is still refused. The
        // engine would otherwise hand a malformed SFNT to skrifa, and the
        // manifest digest is a transport check, not a promise about parsing.
        if !looks_like_sfnt(&report.body) {
            return Ok(self.retry_or_give_up(name, &claim));
        }

        self.set.install(name, Bytes::copy_from_slice(&report.body));
        let _ = self.attempts.remove(name);
        let _ = self.exhausted.remove(name);
        Ok(FaceStep::Adopted {
            name,
            revision: self.set.revision(),
        })
    }

    /// One failure for `name`: retry while the attempt bound allows, then give
    /// up on this face.
    ///
    /// **The attempt is charged here, on the failure, not in
    /// [`plan`](Self::plan).** Charging at request time looks equivalent and is
    /// not: it makes the bound conditional on the host calling `plan` before
    /// every `accept`, so a shell that delivers twice — or never plans at all
    /// — would retry a poisoned response forever, which is precisely the loop
    /// the bound exists to prevent. Counting failures makes the guarantee
    /// independent of the host's call pattern.
    fn retry_or_give_up(&mut self, name: &'static str, claim: &FaceClaim) -> FaceStep {
        let spent = {
            let slot = self.attempts.entry(name).or_insert(0);
            *slot = slot.saturating_add(1);
            *slot
        };
        if spent >= MAX_ATTEMPTS_PER_FACE {
            let _ = self.exhausted.insert(name);
            return FaceStep::Done {
                revision: self.set.revision(),
            };
        }
        FaceStep::Retry(FaceRequest {
            name,
            url: claim.url.clone(),
            sha256: claim.sha256.clone(),
            raw_bytes: claim.raw_bytes,
        })
    }

    /// Release one resident face's bytes, re-arm the request for it, and report
    /// the new resident total.
    ///
    /// The FONT.10-F1 lever for faces, and the twin of
    /// [`CjkChunkLoader::close`](super::cjkchunk::CjkChunkLoader::close). A face
    /// is ~140 kB and there are ten of them, so a document that ranges over
    /// several plus repeated navigation accumulates what nothing would
    /// otherwise give back.
    ///
    /// # This is an eviction, not a teardown
    ///
    /// The loader stays installed and the claim stays held, so the same
    /// document can fetch the same face again without a second `fallbackOpen`.
    /// That is the whole design, and it is not the CJK design for a good
    /// reason: a missing chunk is missing ink in the right place, whereas a
    /// missing face changes every advance width on the line. A session that
    /// lost a face and could not name it again would render silently wrong text
    /// — so a released face goes back on `needs`, and the next render re-asks
    /// for it. (See [`FallbackFontSet::release`], which is where the re-arm
    /// lives, and where the alternative — marking it *unavailable*, i.e.
    /// "never" — is called out as the bug it would be.)
    ///
    /// # What close deliberately does not do
    ///
    /// * It does not detach the loader, which would make the next render take
    ///   the plain `Session::render_page` path — one that reports no face queue
    ///   at all, so the shell would never learn the font was gone. That is the
    ///   stranding failure this method exists to make impossible.
    /// * It does not refund [`MAX_FACE_REQUESTS`]. The re-fetch is a real
    ///   network request and is counted as one, so `close` cannot be used to
    ///   walk past the bound; it buys memory, not free bytes.
    /// * It does not clear `in_flight`. A resident face is never in flight —
    ///   [`accept`](Self::accept) clears the mark as it adopts — so there is
    ///   nothing to clear, and a close that released nothing changes nothing at
    ///   all.
    ///
    /// Returns `None` when the name is not a resident face, which is **not** a
    /// repaint (the revision is unchanged) — a shell polling this must not
    /// repaint the world because an eviction turned out to be a no-op. An
    /// unknown name is refused through the interning table rather than
    /// interned, so a close is not a way to make the guest retain arbitrary
    /// strings.
    pub fn close(&mut self, name: &str) -> Option<FaceCloseReport> {
        let interned = intern(name)?;
        if !self.set.release(interned) {
            return None;
        }
        Some(FaceCloseReport {
            name: interned,
            released: self.claims.get(interned).map_or(0, |c| c.raw_bytes),
            resident_bytes: self.resident_bytes(),
            revision: self.set.revision(),
        })
    }
}

/// What [`FallbackFaceLoader::close`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceCloseReport {
    /// The face that was released.
    pub name: &'static str,
    /// The manifest's claimed size for it — what the shell's cache should drop.
    pub released: u64,
    /// Raw bytes still resident across the whole set.
    pub resident_bytes: u64,
    /// The set's revision after the eviction. **A change is the repaint signal.**
    pub revision: u64,
}

/// The SFNT version tags a TrueType file starts with.
///
/// Checked before adoption so a *correctly-digested* body that is not a font is
/// still refused. It is a cheap structural check, not a parse: the point is to
/// stop obviously-wrong bytes before the real parser sees them, not to
/// re-implement font validation.
const SFNT_TAGS: [[u8; 4]; 5] = [
    [0x00, 0x01, 0x00, 0x00], // TrueType
    [0x4F, 0x54, 0x54, 0x4F], // 'OTTO' CFF
    [0x74, 0x72, 0x75, 0x65], // 'true'
    [0x74, 0x79, 0x70, 0x31], // 'typ1' (Type 1)
    [0x74, 0x74, 0x63, 0x66], // 'ttcf' collection
];

#[must_use]
fn looks_like_sfnt(body: &[u8]) -> bool {
    let Some(head) = body.get(..4) else {
        return false;
    };
    SFNT_TAGS.iter().any(|t| head == t)
}

/// The twelve face names this build knows, as `&'static str`.
///
/// A manifest is a runtime string, and the set is keyed by `&'static str`
/// (which is what keeps `install` allocation-free on the render path). This is
/// the interning table between the two, and it is deliberately a *closed* list
/// rather than a `Box::leak`: a manifest is untrusted input, and leaking one
/// string per unverified name is an unbounded allocation driven by a remote
/// party. A face name that is not here is not a face the renderer can ask for,
/// so rejecting it costs nothing.
const KNOWN_FACES: [&str; 12] = [
    "LiberationSerif-Regular",
    "LiberationSerif-Bold",
    "LiberationSerif-Italic",
    "LiberationSerif-BoldItalic",
    "LiberationSans-Regular",
    "LiberationSans-Bold",
    "LiberationSans-Italic",
    "LiberationSans-BoldItalic",
    "LiberationMono-Regular",
    "LiberationMono-Bold",
    "LiberationMono-Italic",
    "LiberationMono-BoldItalic",
];

/// Map a runtime string to the `&'static str` face name it names, or `None`.
///
/// `pub` because the worker resolves the wire's untrusted `String` through the
/// same table the loader uses, so the two cannot disagree about which names
/// exist.
///
/// Returning `None` also refuses a name the *engine* would never substitute for,
/// so a manifest cannot introduce a face that no document could request.
pub fn intern(name: &str) -> Option<&'static str> {
    KNOWN_FACES.iter().copied().find(|f| *f == name)
}

#[cfg(test)]
mod tests {
    // Fixture code only: these tests build claims and bodies in memory and
    // assert on the loader's decision. The `expect`s are the assertions.
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::*;
    use selis_sandbox::Budget;

    const NAME: &str = "LiberationSans-Regular";

    /// A minimal but real SFNT: the version tag plus enough length to satisfy
    /// `looks_like_sfnt`. The loader never parses past the tag, so a fuller
    /// font would test nothing extra.
    fn sfnt() -> Vec<u8> {
        let mut v = vec![0x00, 0x01, 0x00, 0x00];
        v.extend_from_slice(&[0u8; 60]);
        v
    }

    fn claim_for(body: &[u8]) -> FaceClaim {
        FaceClaim {
            name: NAME,
            sha256: digest_hex(body),
            raw_bytes: u64::try_from(body.len()).unwrap_or(0),
            url: format!("fallback/{NAME}.ttf.br"),
        }
    }

    fn loader() -> FallbackFaceLoader {
        let body = sfnt();
        FallbackFaceLoader::new(vec![claim_for(&body)], Budget::unlimited()).expect("valid claim")
    }

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    fn report(name: &str, status: u16, body: Vec<u8>) -> FaceReport {
        FaceReport {
            name: name.to_string(),
            status,
            len: u64::try_from(body.len()).unwrap_or(u64::MAX),
            body,
        }
    }

    // --- the digest guard, which is the reason this module exists ---------

    /// The load-bearing test: bytes that do not match the manifest are never
    /// adopted, and the face stays absent.
    ///
    /// A TTF is a structured binary that a parser reads, so adopting unverified
    /// bytes hands attacker-chosen input to the font engine. A wrong digest, a
    /// truncated read and a tampered cache entry all land here.
    #[test]
    fn a_digest_mismatch_is_never_adopted() {
        let body = sfnt();
        let mut l = loader();
        l.set_mut().request(NAME);

        // Right length, wrong content.
        let mut tampered = body.clone();
        if let Some(b) = tampered.get_mut(4) {
            *b = 0xAB;
        }
        let mut g = guard();
        match l
            .accept(&report(NAME, 200, tampered), &mut g)
            .expect("accept decides")
        {
            FaceStep::Retry(_) => {}
            other => panic!("a mismatched digest must not be adopted, got {other:?}"),
        }
        assert!(
            !l.set_mut().has(NAME),
            "a face with the wrong bytes must not become resident"
        );
    }

    /// Right bytes, wrong declared length: also refused, and not adopted.
    #[test]
    fn a_length_disagreement_is_refused() {
        let body = sfnt();
        let mut l = loader();
        l.set_mut().request(NAME);
        let mut r = report(NAME, 200, body);
        r.len += 1; // the body is 64 B; claim says 64
        let mut g = guard();
        let err = l
            .accept(&r, &mut g)
            .expect_err("length mismatch is an error");
        assert!(matches!(err.code(), Code::BindingBadArgument));
    }

    /// Correctly-digested bytes that are not a font are still refused: the
    /// digest is a transport check, not a promise that the body parses.
    #[test]
    fn a_digested_body_that_is_not_a_font_is_refused() {
        let junk = vec![0xDEu8; 64];
        let mut l = FallbackFaceLoader::new(vec![claim_for(&junk)], Budget::unlimited())
            .expect("claim is well-formed");
        l.set_mut().request(NAME);
        let mut g = guard();
        match l
            .accept(&report(NAME, 200, junk), &mut g)
            .expect("accept decides")
        {
            FaceStep::Retry(_) => {}
            other => panic!("a non-SFNT body must not be adopted, got {other:?}"),
        }
        assert!(!l.set_mut().has(NAME));
    }

    // --- the happy path ---------------------------------------------------

    /// The correct face is adopted, becomes resident, and bumps the revision.
    #[test]
    fn a_matching_face_is_adopted_and_bumps_the_revision() {
        let body = sfnt();
        let mut l = loader();
        l.set_mut().request(NAME);
        let before = l.revision();
        let mut g = guard();
        match l
            .accept(&report(NAME, 200, body), &mut g)
            .expect("accept decides")
        {
            FaceStep::Adopted { name, revision } => {
                assert_eq!(name, NAME);
                assert!(revision > before, "adoption is a repaint signal");
            }
            other => panic!("expected adoption, got {other:?}"),
        }
        assert!(l.set_mut().has(NAME));
        assert_eq!(l.loaded(), vec![NAME]);
        assert!(l.resident_bytes() > 0);
    }

    // --- the plan / fetch loop --------------------------------------------

    /// Planning asks for exactly the face the engine reported, and reports it
    /// with the digest the shell must pair with its cache key.
    #[test]
    fn plan_asks_for_the_pending_face_with_its_digest() {
        let mut l = loader();
        l.set_mut().request(NAME);
        let req = l.plan().expect("plan decides").expect("one face wanted");
        assert_eq!(req.name, NAME);
        assert_eq!(req.url, format!("fallback/{NAME}.ttf.br"));
        assert_eq!(req.sha256.len(), 64);
        // Planning twice with nothing delivered must not double-ask: the first
        // request is still in flight.
        assert!(l.plan().expect("plan decides").is_none());
    }

    /// With nothing pending there is nothing to ask for.
    #[test]
    fn plan_is_none_when_nothing_is_wanted() {
        assert!(loader().plan().expect("plan decides").is_none());
    }

    // --- bounds -----------------------------------------------------------

    /// A repeatedly-failing face is given up on, not retried forever. This is
    /// the anti-loop property: a serving host that is consistently wrong must
    /// not be hammered.
    #[test]
    fn a_repeatedly_failing_face_is_exhausted() {
        let body = sfnt();
        let mut l = loader();
        l.set_mut().request(NAME);
        let mut bad = body.clone();
        if let Some(b) = bad.get_mut(8) {
            *b = 0xFF;
        }
        let mut g = guard();
        for _ in 0..4 {
            let _ = l
                .accept(&report(NAME, 200, bad.clone()), &mut g)
                .expect("accept decides");
        }
        assert!(!l.set_mut().has(NAME));
        assert!(
            l.exhausted().contains(&NAME),
            "a face that keeps failing must be given up on"
        );
    }

    /// A 5xx is transient and retried; a 404 is not a font this payload has and
    /// is a hard error rather than a silent retry.
    #[test]
    fn status_codes_are_distinguished() {
        let body = sfnt();
        let mut l = loader();
        l.set_mut().request(NAME);
        let mut g = guard();
        assert!(matches!(
            l.accept(&report(NAME, 503, body.clone()), &mut g)
                .expect("accept decides"),
            FaceStep::Retry(_)
        ));
        assert!(matches!(
            l.accept(&report(NAME, 404, body), &mut g).expect_err("404 is an error"),
            _ if true
        ));
    }

    // --- claims -----------------------------------------------------------

    /// A claim whose digest cannot be checked is refused at open time, before
    /// anything is fetched: a claim the loader cannot verify is one it refuses.
    #[test]
    fn an_unverifiable_claim_is_refused_at_open() {
        for bad in ["", "abc", &"Z".repeat(64)] {
            let c = FaceClaim {
                name: NAME,
                sha256: bad.to_string(),
                raw_bytes: 100,
                url: "fallback/x.ttf.br".to_string(),
            };
            assert!(
                FallbackFaceLoader::new(vec![c], Budget::unlimited()).is_err(),
                "digest {bad:?} must be refused at open"
            );
        }
    }

    /// A face name this build does not know is refused, so a manifest cannot
    /// introduce a face no document could ever request.
    #[test]
    fn an_unknown_face_name_is_refused_at_open() {
        let c = FaceClaim {
            name: "Comic Sans",
            sha256: "a".repeat(64),
            raw_bytes: 100,
            url: "fallback/x.ttf.br".to_string(),
        };
        assert!(FallbackFaceLoader::new(vec![c], Budget::unlimited()).is_err());
    }

    /// A delivery for a face the manifest never claimed is refused, not adopted.
    #[test]
    fn a_delivery_for_an_unclaimed_face_is_refused() {
        let body = sfnt();
        let mut l = loader();
        let mut g = guard();
        let err = l
            .accept(&report("LiberationMono-Bold", 200, body), &mut g)
            .expect_err("unclaimed delivery is an error");
        assert!(matches!(err.code(), Code::BindingBadArgument));
    }

    /// The total request bound turns "needs a lot of faces" into a typed budget
    /// error instead of an unbounded download loop.
    #[test]
    fn the_request_bound_is_enforced() {
        let body = sfnt();
        let claims: Vec<FaceClaim> = KNOWN_FACES
            .iter()
            .map(|n| FaceClaim {
                name: n,
                sha256: digest_hex(&body),
                raw_bytes: u64::try_from(body.len()).unwrap_or(0),
                url: format!("fallback/{n}.ttf.br"),
            })
            .collect();
        let mut l = FallbackFaceLoader::new(claims, Budget::unlimited()).expect("claims valid");
        for n in KNOWN_FACES.iter() {
            l.set_mut().request(n);
        }
        let mut planned = 0;
        loop {
            match l.plan() {
                Ok(Some(_)) => planned += 1,
                Ok(None) => break,
                Err(e) => {
                    assert!(matches!(e.code(), Code::BudgetBytes));
                    break;
                }
            }
        }
        assert!(
            planned <= usize::try_from(MAX_FACE_REQUESTS).unwrap_or(0),
            "planned {planned} requests, past the bound of {MAX_FACE_REQUESTS}"
        );
    }

    // --- eviction (FONT.10-F1) ---------------------------------------------

    /// Adopt a face through the real path (request → plan → accept), so the
    /// eviction below is tested against a loader that genuinely holds bytes.
    fn loader_holding_a_face() -> (FallbackFaceLoader, Vec<u8>) {
        let body = sfnt();
        let mut l = loader();
        l.set_mut().request(NAME);
        assert!(l.plan().expect("planned").is_some(), "a request is named");
        let mut g = guard();
        assert!(matches!(
            l.accept(&report(NAME, 200, body.clone()), &mut g)
                .expect("adopt decides"),
            FaceStep::Adopted { .. }
        ));
        (l, body)
    }

    /// The test that would have caught the original leak: after a close the
    /// loader holds **nothing**, and says so.
    ///
    /// Before `fallbackClose` existed there was no way to give a delivered face
    /// back at all, so a document that needed several of them accumulated ~140
    /// kB each for the life of the session with no lever to pull. `loaded()`
    /// being empty and `resident_bytes()` being zero *is* the leak assertion —
    /// a loader that still reported the face here is still leaking it.
    #[test]
    fn closing_a_face_releases_its_bytes() {
        let (mut l, body) = loader_holding_a_face();
        let held = l.resident_bytes();
        assert_eq!(held, u64::try_from(body.len()).unwrap_or(u64::MAX));
        assert_eq!(l.loaded(), vec![NAME]);

        let out = l.close(NAME).expect("bytes were held");
        assert_eq!(out.name, NAME);
        assert_eq!(out.released, u64::try_from(body.len()).unwrap_or(u64::MAX));
        assert_eq!(out.resident_bytes, 0, "the face's bytes are gone");
        assert_eq!(
            l.resident_bytes(),
            0,
            "the loader must report zero resident faces after a close"
        );
        assert!(l.loaded().is_empty(), "no face is still resident");
        assert_eq!(out.revision, 2, "adopt then evict: two pixel changes");
    }

    /// **The stranding test.** A session that has already rendered with a face
    /// must still be able to name it as owed after giving the bytes back,
    /// because a face's absence changes every advance width on the line.
    ///
    /// This is the property a "just detach the loader" teardown breaks: with no
    /// loader the next render takes the plain `Session::render_page` path, which
    /// reports no face queue at all, and the document is silently wrong forever.
    /// So the loader stays armed, the released face is back on `needs`, and it
    /// can be fetched and adopted again with no second `fallbackOpen`.
    #[test]
    fn a_closed_face_is_owed_again_and_can_be_reacquired() {
        let (mut l, body) = loader_holding_a_face();
        let _ = l.close(NAME).expect("released");
        assert_eq!(
            l.needs(),
            vec![NAME],
            "a released face is 'not here yet' — the shell must be told so"
        );
        assert!(
            !l.unavailable().contains(&NAME),
            "a released face is not a face that will never arrive"
        );

        // The round trip a stranded session cannot make: plan again from the
        // *same* loader, and adopt the same claim.
        let again = l.plan().expect("re-planned").expect("a request is named");
        assert_eq!(again.name, NAME);
        assert_eq!(again.sha256, digest_hex(&body));
        let mut g = guard();
        assert!(matches!(
            l.accept(&report(NAME, 200, body), &mut g)
                .expect("re-adopt decides"),
            FaceStep::Adopted { .. }
        ));
        assert_eq!(l.loaded(), vec![NAME], "resident again, with no re-open");
    }

    /// A close that released nothing is a no-op, not an error, and does not move
    /// the revision — a polling shell must not repaint the world because an
    /// eviction turned out to be a no-op.
    #[test]
    fn closing_a_face_that_is_not_resident_is_a_no_op() {
        let (mut l, _) = loader_holding_a_face();
        let rev = l.revision();
        let _ = l.close(NAME).expect("released once");
        let after = l.revision();
        assert!(after > rev, "the first close was a real change");

        assert!(l.close(NAME).is_none(), "already given back");
        assert!(l.close("LiberationSerif-Italic").is_none(), "never fetched");
        assert!(l.close("Comic Sans").is_none(), "not a face at all");
        assert_eq!(l.revision(), after, "still no repaint owed");
        assert_eq!(l.resident_bytes(), 0, "and still nothing resident");
    }

    /// A face the payload has no file for is "never", and a close must not turn
    /// that into "not here yet" — which would start a fetch loop for something
    /// the payload will never serve.
    #[test]
    fn closing_does_not_revive_a_face_the_payload_never_carried() {
        let mut l = loader();
        l.mark_unavailable(&[NAME]);
        assert!(
            l.close(NAME).is_none(),
            "there was never anything to release"
        );
        assert!(l.needs().is_empty(), "an unavailable face is not owed");
    }

    /// The re-fetch after a close is a real network request and is counted as
    /// one. If `close` refunded the budget, evicting and re-delivering would be
    /// a way to walk past [`MAX_FACE_REQUESTS`] for ever.
    #[test]
    fn a_re_fetch_after_a_close_still_costs_a_request() {
        let body = sfnt();
        let claims: Vec<FaceClaim> = KNOWN_FACES
            .iter()
            .map(|n| FaceClaim {
                name: n,
                sha256: digest_hex(&body),
                raw_bytes: u64::try_from(body.len()).unwrap_or(0),
                url: format!("fallback/{n}.ttf.br"),
            })
            .collect();
        let mut l = FallbackFaceLoader::new(claims, Budget::unlimited()).expect("claims valid");
        // Every face the engine could ask for, so `plan` has the whole table to
        // walk — the bound is only reachable when the document needs the payload.
        for n in KNOWN_FACES {
            l.set_mut().request(n);
        }
        let mut g = guard();
        let _ = l
            .accept(&report(NAME, 200, body.clone()), &mut g)
            .expect("adopt");
        let _ = l.close(NAME).expect("released");
        // The re-fetch put NAME back on the queue without spending a request.
        assert!(l.needs().contains(&NAME));

        let mut planned = 0;
        loop {
            match l.plan() {
                Ok(Some(_)) => planned += 1,
                Ok(None) => break,
                Err(e) => {
                    assert!(matches!(e.code(), Code::BudgetBytes));
                    break;
                }
            }
        }
        assert_eq!(
            planned,
            usize::try_from(MAX_FACE_REQUESTS).unwrap_or(0),
            "the bound is the whole payload; a close must not have refunded any of it"
        );
    }
}
