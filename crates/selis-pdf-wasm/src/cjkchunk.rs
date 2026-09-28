//! The lazy CJK chunk loader (SL-4.WASM.07) — the shell's transport contract.
//!
//! SL-3.FONT.10 built the payload (a subsetted core plus one immutable SFNT
//! file per Unicode range) and the engine-side resident set. This module is the
//! half that was left: **how a chunk is actually asked for and handed over**, and
//! what a missing or failed one does. ADR-P0043 §3 settles the shape —
//!
//! > the engine never fetches (L0–L3 purity, no fs/net/async). Resident bytes
//! > are injected […] The shell owns transport, HTTP/Cache-API caching, and
//! > quota; cache keys pair the chunk id with the manifest's SHA-256.
//!
//! so the split is the same one [`crate::httprange`] argues for, and for the same
//! reason. The guest has no `fetch` import at all and cannot block on the
//! network; a request goes host → guest and exactly one response comes back. A
//! guest that pulled would deadlock its only channel.
//!
//! # The exchange
//!
//! Three wire ops, mirroring `rangeOpen` / `rangeChunk` / `rangeClose`:
//!
//! | op | what the guest does | what the shell does |
//! |---|---|---|
//! | `cjkOpen` | records the manifest's **claims** (id, SHA-256, length, url) and answers with the first [`CjkRequest`] it wants | — |
//! | `cjkChunk` | verifies the attachment against the claim, `provide`s it, answers with the next request or `null` | fetches the bytes and hands them over |
//! | `cjkClose` | `evict`s one resident chunk and reports the new resident total | frees its cache entry |
//!
//! **The guest names the chunk; the host only moves bytes.** The engine cannot
//! be handed a font it did not ask for, and — the reason the exchange is worth
//! its weight — a shell cannot hand over bytes that do not match what the
//! manifest promised without the guest refusing them.
//!
//! # What a missing or failed chunk does
//!
//! Four distinct outcomes, none of them a blank box:
//!
//! | situation | detected by | outcome |
//! |---|---|---|
//! | the manifest has **no file** for the range (`served_by: null`) | no claim at `cjkOpen` | `mark_unavailable`: never requested, `.notdef` forever, reported as `unavailable` so the shell can say "this payload has no Korean" rather than "still loading" |
//! | the fetch **failed** (no response, CORS refusal, abort, `5xx`) | `status == 0` or `>= 500` | retried up to [`MAX_ATTEMPTS_PER_CHUNK`], then the id is *exhausted*: `.notdef`, reported as `exhausted`, and **not asked for again this session** |
//! | the bytes **do not match the manifest's SHA-256** | digest comparison | refused — never adopted (the cache-poisoning guard), counted as an attempt, exhausted at the same bound |
//! | the bytes are **not a font** | the set's own SFNT parse in `provide` | refused the same way, counted as an attempt |
//!
//! Every one of those leaves the code painting `.notdef` — deterministically, on
//! every platform — and every one of them is *named* in the response, so a shell
//! can distinguish "your network is down" from "this font has no Korean" from
//! "the cache is corrupt". The alternative — silently retrying forever — is what
//! [`MAX_ATTEMPTS_PER_CHUNK`] exists to prevent, and what
//! `exhausted_ids_never_loop` pins.
//!
//! # FONT.10-F1: what lazy loading does and does not fix
//!
//! The whole payload is **11 162 268 B raw**, over the extension store's 8 MiB
//! budget *and* over Chrome's 10 MiB `storage.local` quota (see
//! `assets/cjk/PROVENANCE.md`). **Lazy loading does not make that total go
//! away** — it makes the *common* case small: a document needs the core plus the
//! one or two chunks its characters fall in, which is 1.3–3.7 MB. So:
//!
//! * the common case is solved, and measurably (`xtask cjk-measure`);
//! * a document that genuinely ranges across the whole payload is **not**, and
//!   no amount of laziness changes that — 11 MB resident is 11 MB resident;
//! * which is why this module also carries the *other* half of the answer: the
//!   shell can [`CjkChunkLoader::close`] a chunk to give the bytes back, and
//!   [`resident_bytes`](CjkChunkLoader::resident_bytes) is the number it measures
//!   against its own budget. Without that lever "lazy" would only postpone the
//!   ceiling; with it, the ceiling is a policy the shell chooses rather than a
//!   crash it discovers.
//!
//! # Trust boundary, stated honestly
//!
//! The digest check makes the guest judge the bytes it is given; it does not
//! make the manifest authoritative. The claims arrive in the same message era
//! as the bytes, from a shell that fetched them over the same transport. What
//! the check *does* catch, and what is worth catching, is a truncated or
//! corrupt cache read, a file served under the wrong content-addressed key, and
//! a shell bug. It is a cache-integrity guard, not a supply-chain one.
//!
//! # Budget
//!
//! Every attachment is charged against the caller's [`BudgetGuard`] *before* it
//! is kept, and against the loader's own transfer ceiling, so neither one
//! oversized chunk nor many small ones can exceed the chosen budget. Attempts
//! are bounded by [`MAX_ATTEMPTS_PER_CHUNK`] per id and [`MAX_CHUNK_REQUESTS`]
//! per loader, so a shell that answers every request with the same failure is
//! stopped rather than looped.
//!
//! # Malformed Input
//!
//! The claims are untrusted (the shell is the source, and the shell is not the
//! engine). An unknown chunk id, a malformed digest, a zero-length or absurd
//! claim, an unknown `cjkChunk` id, a mismatched `len`, and an unrecognised
//! status are each a typed error that leaves the loader untouched — never a
//! panic, never a partially-adopted chunk.

use std::collections::{BTreeMap, BTreeSet};

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_font::cjk::{chunk_by_id, CjkFontSet, CHUNKS};
use selis_sandbox::{Budget, BudgetGuard, Resource};
use sha2::{Digest, Sha256};

/// Maximum fetch attempts for **one** chunk id before it is declared
/// exhausted.
///
/// Two failures for one id is a broken origin, a corrupt cache, or a shell that
/// cannot reach the network — not a flaky link. The bound is what turns "the
/// chunk is missing" from an infinite retry into a reported state, and it is the
/// reason the failure modes above can be told apart at all.
pub const MAX_ATTEMPTS_PER_CHUNK: u32 = 3;

/// Hard ceiling on chunk requests one loader may plan.
///
/// The chunk table has 31 ranges; a document that legitimately needs most of
/// them is legitimate. This bound is past that, and exists so a loader driven by
/// a render loop that re-queues forever cannot issue requests without limit.
pub const MAX_CHUNK_REQUESTS: u32 = 64;

/// Absolute ceiling on a single chunk file's claimed length, in bytes.
///
/// 4 MiB is roughly three times the largest file the committed payload contains
/// (`ext-a`, 2 449 132 B raw), so it rejects a manifest claiming a
/// multi-gigabyte font without being tuned per release. It is a *hostile-input*
/// bound on the claim, checked before any bytes arrive; the caller's own
/// [`Budget`] is the separate, tighter ceiling enforced at accept time.
pub const MAX_CLAIMED_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// The manifest's claim about one chunk file.
///
/// A claim is what the shell says the bytes *will* be. It is checked, not
/// trusted: the loader holds a claim per id and refuses anything that does not
/// match (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkClaim {
    /// The chunk id, resolved against [`selis_font::cjk::CHUNKS`].
    pub id: &'static str,
    /// Lowercase-hex SHA-256 of the raw (decompressed) file bytes.
    pub sha256: String,
    /// The raw file length in bytes.
    pub raw_bytes: u64,
    /// The path the manifest published the file at (`cjk/<id>.ttf`).
    pub url: String,
}

/// One bodyless request for a chunk file.
///
/// Deliberately impoverished in the same way [`crate::httprange::RangeRequest`]
/// is: an id and a path, no method and no body. A shell can only fetch these
/// bytes; it cannot be induced to send anything anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CjkRequest {
    /// The chunk id to fetch.
    pub id: &'static str,
    /// The path the manifest published, resolved by the shell against its base.
    pub url: String,
    /// The digest the shell's cache key must pair with the id (ADR-P0043 §3).
    pub sha256: String,
    /// The length the shell should expect.
    pub raw_bytes: u64,
}

/// What the host observed for one chunk request.
///
/// # Malformed Input
///
/// Every field is untrusted: the id is a string the host chose and the length is
/// a number it declared. The body is copied on acceptance and never aliased, so
/// a caller may reuse or drop its buffer immediately.
#[derive(Debug, Clone)]
pub struct ChunkReport {
    /// The id the loader asked for.
    pub id: String,
    /// The HTTP status, or `0` when no response arrived (network error, CORS
    /// refusal, or an abort — the host cannot tell those apart).
    pub status: u16,
    /// The attachment's length in bytes; must match it exactly.
    pub len: u64,
    /// The file bytes.
    pub body: Vec<u8>,
}

/// What the loader decided to do with a [`ChunkReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkStep {
    /// Fetch this chunk next.
    Request(CjkRequest),
    /// Nothing usable arrived: ask the same chunk again, bounded by
    /// [`MAX_ATTEMPTS_PER_CHUNK`]. No bytes were kept.
    Retry(CjkRequest),
    /// The chunk was adopted. `revision` is the set's invalidation counter after
    /// the adoption — **a change is the repaint signal** (ADR-P0043 §3).
    Adopted {
        /// The chunk that landed.
        id: &'static str,
        /// The set's revision after adoption.
        revision: u64,
    },
    /// Nothing is wanted any more: every queued id is resident, unavailable, or
    /// exhausted. `revision` is the set's current counter.
    Done {
        /// The set's revision.
        revision: u64,
    },
}

/// Parse a lowercase-hex SHA-256 into its 32 bytes.
///
/// `None` for anything that is not exactly 64 lowercase hex characters. A claim
/// with an unparseable digest is a claim the loader cannot check, and a claim it
/// cannot check is one it refuses — which is the whole point of checking.
fn parse_digest(hex: &str) -> Option<[u8; 32]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        let hi = hex_val(pair.first()?)?;
        let lo = hex_val(pair.get(1)?)?;
        let slot = out.get_mut(i)?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

/// One hex digit's value, or `None` if it is not one.
fn hex_val(c: &u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(c.wrapping_sub(b'a').wrapping_add(10)),
        _ => None,
    }
}

/// The SHA-256 of `bytes`, lowercase hex.
fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        // `write!` into a `String` is infallible; two hex digits per byte, always.
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{b:02x}"));
    }
    out
}

/// The given chunk ids in the static table's order.
///
/// The chunk table is a compatibility surface (ADR-P0043 consequences) and its
/// order is what every `needs` list is reported in, so an id list that came from
/// anywhere is normalised through this rather than left in whatever order the
/// collection that produced it happened to have.
fn table_order(ids: &[&'static str]) -> Vec<&'static str> {
    CHUNKS
        .iter()
        .map(|c| c.id)
        .filter(|id| ids.contains(id))
        .collect()
}


/// The guest-side CJK chunk loader: the resident set plus the claims, attempts
/// and exhaustion state that turn a render's `needs` list into fetches.
///
/// One loader is one CJK payload for one session. It owns **no** I/O: it plans
/// what it wants, judges what it is given, and reports what happened. See the
/// module docs for the exchange and the failure table.
#[derive(Debug, Clone)]
pub struct CjkChunkLoader {
    set: CjkFontSet,
    claims: BTreeMap<&'static str, ChunkClaim>,
    attempts: BTreeMap<&'static str, u32>,
    exhausted: BTreeSet<&'static str>,
    /// Ids handed out by [`plan`](Self::plan) and not yet delivered.
    ///
    /// Without this, planning twice before delivering would name the same chunk
    /// twice: two fetches of one file, two attempt charges, and two shell loops
    /// racing each other for the same bytes. It is a *set* because a shell may
    /// legitimately have several chunks outstanding at once — a document that
    /// spans three ranges is fetched by three parallel requests, and a single
    /// slot would silently re-offer the first one the moment the second was
    /// planned.
    in_flight: BTreeSet<&'static str>,
    requests: u32,
    budget: Budget,
}

impl CjkChunkLoader {
    /// A loader over an already-loaded core, with the manifest's claims.
    ///
    /// Claims naming an id the chunk table does not carry are **refused**
    /// (`BINDING_BAD_ARGUMENT`) rather than ignored: a manifest that invents a
    /// range is a manifest this build cannot serve, and quietly dropping the row
    /// would leave the shell believing it had installed something.
    ///
    /// # Malformed Input
    ///
    /// `claims` is untrusted. An unknown id, a `sha256` that is not 64 lowercase
    /// hex characters, a zero or absurd `raw_bytes`, and an empty `url` are each
    /// `BINDING_BAD_ARGUMENT` and leave nothing behind.
    ///
    /// # Budget
    ///
    /// `budget` bounds the whole loader: every attachment is charged against it,
    /// so a session cannot accumulate more chunk bytes than the caller allows
    /// however the requests are spread over time. The core was already resident
    /// and is accounted by the caller that loaded it.
    ///
    /// # Errors
    ///
    /// `BINDING_BAD_ARGUMENT` for a claim this build cannot check or serve.
    pub fn new(core: Bytes, claims: Vec<ChunkClaim>, budget: Budget) -> Result<Self> {
        let mut resolved: BTreeMap<&'static str, ChunkClaim> = BTreeMap::new();
        for claim in claims {
            let Some(chunk) = chunk_by_id(claim.id) else {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "cjk-chunk",
                    detail = "claim names a range the chunk table does not carry"
                ));
            };
            if parse_digest(&claim.sha256).is_none() {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "cjk-chunk",
                    detail = "claim digest is not 64 lowercase hex characters"
                ));
            }
            if claim.raw_bytes == 0 || claim.raw_bytes > MAX_CLAIMED_CHUNK_BYTES {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "cjk-chunk",
                    detail = "claim length is zero or past the hostile-input bound"
                ));
            }
            if claim.url.is_empty() {
                return Err(err!(
                    Code::BindingBadArgument,
                    during = "cjk-chunk",
                    detail = "claim carries no url"
                ));
            }
            let _ = resolved.insert(chunk.id, claim);
        }
        Ok(Self {
            set: CjkFontSet::new(core),
            claims: resolved,
            attempts: BTreeMap::new(),
            exhausted: BTreeSet::new(),
            in_flight: BTreeSet::new(),
            requests: 0,
            budget,
        })
    }

    /// The resident set, for a render walk to snapshot.
    ///
    /// The engine half of the contract: a render reads an immutable snapshot and
    /// never blocks. Everything the loader does happens on the other side of a
    /// dispatch, never inside a walk.
    #[must_use]
    pub const fn set(&self) -> &CjkFontSet {
        &self.set
    }

    /// Mutable access, for a caller that renders through the set directly (the
    /// engine's `render_page_cjk` takes `&mut CjkFontSet`).
    pub const fn set_mut(&mut self) -> &mut CjkFontSet {
        &mut self.set
    }

    /// The set's invalidation revision. A change is the repaint signal.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.set.revision()
    }

    /// Raw bytes resident: the core plus every loaded chunk (FONT.10-F1).
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.set.resident_bytes()
    }

    /// The ids this build will not fetch, because the payload has no file for
    /// them. A shell reports these as "this payload has no Korean", not as
    /// "still loading".
    #[must_use]
    pub fn unavailable_ids(&self) -> Vec<&'static str> {
        self.set.unavailable_ids()
    }

    /// The ids whose fetch failed past [`MAX_ATTEMPTS_PER_CHUNK`]. They paint
    /// `.notdef` and are not asked for again this session.
    #[must_use]
    pub fn exhausted_ids(&self) -> Vec<&'static str> {
        let ids: Vec<&'static str> = self.exhausted.iter().copied().collect();
        table_order(&ids)
    }

    /// The ids loaded and resident, in chunk-table order.
    #[must_use]
    pub fn loaded_ids(&self) -> Vec<&'static str> {
        self.set.loaded_ids()
    }

    /// Mark the ranges the payload has no file for (`served_by: null`).
    ///
    /// Called once at `cjkOpen` from the manifest's `unserved` list. Marking is
    /// not a pixel change, so it does not bump the revision — it only stops the
    /// queue asking for a fetch that cannot succeed.
    pub fn mark_unavailable(&mut self, ids: &[&str]) {
        self.set.mark_unavailable(ids);
    }


    /// The next chunk to fetch, or `None` when nothing is wanted.
    ///
    /// The order is the set's own sticky `needs` queue in chunk-table order, so
    /// the request sequence is a pure function of the resident state — the same
    /// document asks for the same chunks in the same order on every platform.
    ///
    /// # Budget
    ///
    /// Planning is where the request bound is enforced, because every plan is a
    /// request the host will really issue: past [`MAX_CHUNK_REQUESTS`] in total
    /// the plan is `BUDGET_BYTES`, and past [`MAX_ATTEMPTS_PER_CHUNK`] on one id
    /// the id is *exhausted* and no further request is planned for it.
    ///
    /// # Errors
    ///
    /// `BUDGET_BYTES` when the request bound is exhausted.
    pub fn plan(&mut self) -> Result<Option<CjkRequest>> {
        let queued = self.set.queued();
        for id in queued {
            if self.set.chunk(id).is_some() {
                continue; // already resident
            }
            if self.exhausted.contains(id) {
                continue; // failed past the bound: not asked again
            }
            if self.in_flight.contains(id) {
                continue; // already handed out and not yet delivered
            }
            let Some(claim) = self.claims.get(id).cloned() else {
                // No claim means the payload has no file for this range. Marking
                // it here (rather than only at `cjkOpen`) is what makes the "no
                // such range" outcome reachable even when the shell opened the
                // loader without the manifest's `unserved` list.
                self.set.mark_unavailable(&[id]);
                continue;
            };
            if self.requests >= MAX_CHUNK_REQUESTS {
                return Err(err!(
                    Code::BudgetBytes,
                    during = "cjk-chunk",
                    detail = "chunk request bound exhausted for this loader"
                ));
            }
            self.requests = self.requests.saturating_add(1);
            let spent = self.attempts.entry(id).or_insert(0);
            *spent = spent.saturating_add(1);
            let _ = self.in_flight.insert(id);
            return Ok(Some(CjkRequest {
                id,
                url: claim.url,
                sha256: claim.sha256,
                raw_bytes: claim.raw_bytes,
            }));
        }
        Ok(None)
    }


    /// Judge one delivered chunk and advance the loader.
    ///
    /// # Budget
    ///
    /// The attachment is charged against `guard` and against the loader's own
    /// `budget.bytes` **before** the bytes are kept, so a shell cannot make the
    /// guest allocate first and account afterwards.
    ///
    /// # Malformed Input
    ///
    /// `report.id` must name a chunk the loader holds a claim for, and
    /// `report.len` must equal the attachment. `status` `0` and `>= 500` are
    /// retries; anything else outside `200`/`206` is `IO_READ_FAILED`. Bytes
    /// whose digest or length contradicts the claim are refused, never adopted.
    /// None of these panic, and none leaves a half-adopted chunk.
    pub fn accept(
        &mut self,
        report: &ChunkReport,
        guard: &mut BudgetGuard<'_>,
    ) -> Result<ChunkStep> {
        guard.tick()?;
        let arrived = u64::try_from(report.body.len()).unwrap_or(u64::MAX);
        if arrived != report.len {
            return Err(err!(
                Code::BindingBadArgument,
                during = "cjk-chunk",
                detail = "chunk length does not match its attachment"
            ));
        }
        if self.set.resident_bytes().saturating_add(arrived) > self.budget.bytes {
            return Err(err!(
                Code::BudgetBytes,
                during = "cjk-chunk",
                detail = "resident CJK set would exceed the loader budget"
            ));
        }
        guard.charge(Resource::Bytes, arrived)?;

        let Some(chunk) = chunk_by_id(&report.id) else {
            return Err(err!(
                Code::BindingBadArgument,
                during = "cjk-chunk",
                detail = "delivery names a range the chunk table does not carry"
            ));
        };
        let id = chunk.id;
        // This delivery answers whatever was in flight for `id`; it is answered
        // now, so a later `plan` is free to name it again if the set still wants
        // it. Other ids stay in flight — their fetches are still outstanding.
        let _ = self.in_flight.remove(id);

        // A delivery for an id this loader has no claim for is not something to
        // adopt: there is no digest to check it against.
        let Some(claim) = self.claims.get(id).cloned() else {
            return Err(err!(
                Code::BindingBadArgument,
                during = "cjk-chunk",
                detail = "delivery names a range the manifest never claimed"
            ));
        };

        if report.status == 0 || report.status >= 500 {
            return Ok(self.retry_or_give_up(id, &claim));
        }
        if report.status != 200 && report.status != 206 {
            return Err(err!(
                Code::IoReadFailed,
                during = "cjk-chunk",
                detail = "chunk fetch returned an unexpected status"
            ));
        }

        // The cache-poisoning guard. A digest that does not match is refused
        // whole: a truncated read, a file served under the wrong key, and a
        // tampered cache entry all land here, and none of them is adopted.
        let expected = parse_digest(&claim.sha256);
        let actual = parse_digest(&digest_hex(&report.body));
        if expected != actual || arrived != claim.raw_bytes {
            return Ok(self.retry_or_give_up(id, &claim));
        }

        // `provide` does the last gate itself: the id must be a real chunk and
        // the bytes must parse as an SFNT. A rejected chunk is counted as an
        // attempt like any other failure, so it cannot loop.
        if !self.set.provide(id, Bytes::copy_from_slice(&report.body)) {
            return Ok(self.retry_or_give_up(id, &claim));
        }
        let _ = self.attempts.remove(id);
        let _ = self.exhausted.remove(id);
        Ok(ChunkStep::Adopted {
            id,
            revision: self.set.revision(),
        })
    }


    /// One failure for `id`: retry while the attempt bound allows, then exhaust.
    ///
    /// The exhaustion is what makes the failure *reportable* rather than a loop,
    /// and it is deliberately not the same thing as
    /// [`mark_unavailable`](Self::mark_unavailable): a chunk that failed to
    /// download may well be available on the next session, while a range the
    /// payload has no file for never will be.
    fn retry_or_give_up(&mut self, id: &'static str, claim: &ChunkClaim) -> ChunkStep {
        let spent = self.attempts.entry(id).or_insert(0);
        // `plan` already charged one attempt for this request, so a failure
        // with the spent count at the bound has nothing left to try.
        if *spent >= MAX_ATTEMPTS_PER_CHUNK {
            let _ = self.exhausted.insert(id);
            return ChunkStep::Done {
                revision: self.set.revision(),
            };
        }
        ChunkStep::Retry(CjkRequest {
            id,
            url: claim.url.clone(),
            sha256: claim.sha256.clone(),
            raw_bytes: claim.raw_bytes,
        })
    }

    /// Release one resident chunk's bytes and report the new resident total.
    ///
    /// The FONT.10-F1 lever: the shell decides its own storage pressure and
    /// gives bytes back through here. Returns `None` when the id names nothing
    /// resident, which is **not** a repaint (the set's revision is unchanged) —
    /// a shell polling this must not repaint the world because an eviction was a
    /// no-op.
    pub fn close(&mut self, id: &str) -> Option<CjkCloseReport> {
        if !self.set.evict(id) {
            return None;
        }
        Some(CjkCloseReport {
            id: id.to_owned(),
            released: self.claims.get(id).map_or(0, |c| c.raw_bytes),
            resident_bytes: self.set.resident_bytes(),
            revision: self.set.revision(),
        })
    }
}

/// What [`CjkChunkLoader::close`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CjkCloseReport {
    /// The chunk that was released.
    pub id: String,
    /// The manifest's claimed size for it — what the shell's cache should drop.
    pub released: u64,
    /// Raw bytes still resident across the whole set.
    pub resident_bytes: u64,
    /// The set's revision after the eviction. **A change is the repaint signal.**
    pub revision: u64,
}


#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, clippy::integer_division)]
    use super::*;
    use selis_font::add_glyph;
    use selis_font::subset::{subset_ttf, GlyphSet};
    use selis_font::OutlineCmd;

    // The FONT.11 subsetter's own fixture, reached across crates: this loader's
    // tests build real subset SFNTs so the digest and length claims are real
    // rather than a constant that would make the cache-poisoning guard vacuous.
    const MINI: &[u8] = include_bytes!("../../selis-font/tests/fixtures/mini.ttf");

    fn guard() -> selis_sandbox::BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    /// A budget with room for several chunk-sized payloads.
    fn budget() -> Budget {
        Budget {
            bytes: 8 * 1024 * 1024,
            ..Budget::unlimited()
        }
    }

    fn rect() -> Vec<OutlineCmd> {
        vec![
            OutlineCmd::Move { x: 100.0, y: 100.0 },
            OutlineCmd::Line { x: 900.0, y: 100.0 },
            OutlineCmd::Line { x: 900.0, y: 900.0 },
            OutlineCmd::Line { x: 100.0, y: 900.0 },
            OutlineCmd::Close,
        ]
    }

    /// A stand-in payload file covering `codes` (the FONT.11 subset path over
    /// the `mini.ttf` fixture), so the digest and length claims are real.
    fn payload(codes: &[u32]) -> Bytes {
        let mut g = guard();
        let mut font = Bytes::copy_from_slice(MINI);
        for &code in codes {
            font = add_glyph(&font, code, &rect(), 1000, &mut g)
                .expect("add ok")
                .expect("font merged");
        }
        let keep: Vec<u16> = codes
            .iter()
            .filter_map(|&c| selis_font::glyph_id_for_char(&font, c))
            .collect();
        subset_ttf(&font, &GlyphSet { keep }, &mut g)
            .expect("subset ok")
            .expect("ttf subset")
    }

    fn claim(id: &'static str, bytes: &Bytes) -> ChunkClaim {
        ChunkClaim {
            id,
            sha256: digest_hex(bytes.as_slice()),
            raw_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            url: format!("cjk/{id}.ttf"),
        }
    }

    /// A loader whose only claim is `ideographs-4` (U+6F00), plus that chunk's
    /// bytes so a test can deliver exactly what the manifest promised.
    fn loader_with_chunk() -> (CjkChunkLoader, Bytes) {
        let chunk = payload(&[0x6F00]);
        let c = claim("ideographs-4", &chunk);
        let loader = CjkChunkLoader::new(Bytes::new(), vec![c], budget()).expect("claims parse");
        (loader, chunk)
    }

    fn report(id: &str, status: u16, body: Vec<u8>) -> ChunkReport {
        let len = u64::try_from(body.len()).unwrap_or(u64::MAX);
        ChunkReport {
            id: id.to_owned(),
            status,
            len,
            body,
        }
    }

    #[test]
    fn digest_helpers_round_trip_and_refuse_junk() {
        let d = digest_hex(b"selis");
        assert_eq!(d.len(), 64);
        assert_eq!(parse_digest(&d), parse_digest(&digest_hex(b"selis")));
        for bad in [
            "",
            "zz",
            "A".repeat(64).as_str(), // uppercase is not the pinned spelling
            "0".repeat(63).as_str(),
            "0".repeat(65).as_str(),
        ] {
            assert!(parse_digest(bad).is_none(), "must refuse {bad:?}");
        }
    }

    #[test]
    fn a_claim_naming_an_unknown_range_is_refused() {
        let bytes = payload(&[0x6F00]);
        let bad = ChunkClaim {
            id: "no-such-chunk",
            ..claim("ideographs-4", &bytes)
        };
        let e = CjkChunkLoader::new(Bytes::new(), vec![bad], budget())
            .err()
            .expect("refused");
        assert_eq!(e.code(), Code::BindingBadArgument);
    }

    #[test]
    fn an_uncheckable_claim_is_refused() {
        let bytes = payload(&[0x6F00]);
        for bad in [
            ChunkClaim {
                sha256: "not-a-digest".to_owned(),
                ..claim("ideographs-4", &bytes)
            },
            ChunkClaim {
                raw_bytes: 0,
                ..claim("ideographs-4", &bytes)
            },
            ChunkClaim {
                raw_bytes: MAX_CLAIMED_CHUNK_BYTES.saturating_add(1),
                ..claim("ideographs-4", &bytes)
            },
            ChunkClaim {
                url: String::new(),
                ..claim("ideographs-4", &bytes)
            },
        ] {
            assert!(
                CjkChunkLoader::new(Bytes::new(), vec![bad], budget()).is_err(),
                "an uncheckable claim must not be accepted"
            );
        }
    }


    /// The happy path: a render queues the chunk, the loader plans it, the
    /// shell delivers exactly what the manifest promised, the set adopts it, and
    /// the revision is the repaint signal.
    #[test]
    fn a_planned_chunk_that_arrives_intact_is_adopted_and_bumps_the_revision() {
        let (mut loader, chunk) = loader_with_chunk();
        assert_eq!(loader.set_mut().request(0x6F00), Some("ideographs-4"));
        let req = loader.plan().expect("bounded").expect("planned");
        assert_eq!(req.id, "ideographs-4");
        assert_eq!(req.url, "cjk/ideographs-4.ttf");
        assert_eq!(req.raw_bytes, u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        assert_eq!(req.sha256, digest_hex(chunk.as_slice()));

        let mut g = guard();
        let step = loader
            .accept(&report("ideographs-4", 200, chunk.to_vec()), &mut g)
            .expect("adopt");
        assert_eq!(
            step,
            ChunkStep::Adopted {
                id: "ideographs-4",
                revision: 1
            }
        );
        assert!(loader.set().covers(0x6F00));
        assert!(loader.set().queued().is_empty(), "the queue cleared");
        assert_eq!(loader.plan().expect("bounded"), None, "nothing to fetch");
        assert_eq!(loader.loaded_ids(), vec!["ideographs-4"]);
    }

    /// Bytes that contradict the manifest are refused — the cache-poisoning
    /// guard — and the chunk is never adopted however often it is offered.
    #[test]
    fn bytes_that_contradict_the_manifest_are_never_adopted() {
        let (mut loader, chunk) = loader_with_chunk();
        let wrong = payload(&[0x6F01]); // a different, perfectly valid font
        assert_ne!(
            digest_hex(wrong.as_slice()),
            digest_hex(chunk.as_slice()),
            "the two payloads must differ for this test to mean anything"
        );
        assert_eq!(loader.set_mut().request(0x6F00), Some("ideographs-4"));

        let mut g = guard();
        for _ in 0..MAX_ATTEMPTS_PER_CHUNK {
            assert!(loader.plan().expect("bounded").is_some());
            let step = loader
                .accept(&report("ideographs-4", 200, wrong.to_vec()), &mut g)
                .expect("typed, not a panic");
            assert!(
                matches!(step, ChunkStep::Retry(_) | ChunkStep::Done { .. }),
                "a wrong-digest chunk is never adopted: {step:?}"
            );
        }
        assert!(
            !loader.set().covers(0x6F00),
            "the code stays notdef after every attempt"
        );
        assert!(loader.exhausted_ids().contains(&"ideographs-4"));
        assert_eq!(loader.resident_bytes(), 0, "and no bytes were kept");
    }

    /// The denial-of-service bound: a shell that never answers is stopped, not
    /// looped. This is the leg that would notice if the bound were only on paper.
    #[test]
    fn exhausted_ids_never_loop() {
        let (mut loader, _) = loader_with_chunk();
        assert_eq!(loader.set_mut().request(0x6F00), Some("ideographs-4"));
        let mut g = guard();
        let mut retries = 0usize;
        // The set's queue is sticky, so a shell that keeps answering "no
        // response" keeps being planned — until the bound stops it.
        for _ in 0..(MAX_ATTEMPTS_PER_CHUNK as usize * 4) {
            if loader.plan().expect("bounded").is_none() {
                break;
            }
            let step = loader
                .accept(&report("ideographs-4", 0, Vec::new()), &mut g)
                .expect("typed");
            if matches!(step, ChunkStep::Retry(_)) {
                retries = retries.saturating_add(1);
            }
        }
        assert!(
            retries < MAX_ATTEMPTS_PER_CHUNK as usize,
            "retries are bounded, got {retries}"
        );
        assert!(
            loader.exhausted_ids().contains(&"ideographs-4"),
            "and the id is reported as exhausted, not left asking"
        );
        // A further plan asks for nothing, ever.
        assert_eq!(loader.plan().expect("bounded"), None);
        assert_eq!(loader.plan().expect("bounded"), None);
    }


    /// A range the payload has no file for is never requested and never retried
    /// — the "this payload has no Korean" outcome, distinct from a failure.
    #[test]
    fn a_range_with_no_claim_is_never_fetched() {
        let mut loader = CjkChunkLoader::new(Bytes::new(), Vec::new(), budget()).expect("empty");
        assert_eq!(loader.set_mut().request(0xAC00), Some("hangul-1"));
        assert_eq!(loader.plan().expect("bounded"), None, "nothing to fetch");
        assert_eq!(loader.unavailable_ids(), vec!["hangul-1"]);
        assert!(loader.exhausted_ids().is_empty(), "not a failure");
        assert!(!loader.set().covers(0xAC00), "and it stays notdef");

        // Marking up front reaches the same state without a render at all.
        let mut pre = CjkChunkLoader::new(Bytes::new(), Vec::new(), budget()).expect("empty");
        pre.mark_unavailable(&["hangul-1"]);
        assert_eq!(pre.set_mut().request(0xAC00), None, "not even queued");
        assert_eq!(pre.unavailable_ids(), vec!["hangul-1"]);
    }

    /// Hostile deliveries are typed errors that leave the loader untouched —
    /// nothing resident changes, no revision moves.
    #[test]
    fn hostile_deliveries_are_typed_and_change_nothing() {
        let (mut loader, chunk) = loader_with_chunk();
        assert_eq!(loader.set_mut().request(0x6F00), Some("ideographs-4"));
        let _ = loader.plan().expect("planned");
        let before_rev = loader.revision();
        let before_bytes = loader.resident_bytes();
        let mut g = guard();

        for bad in [
            report("no-such-chunk", 200, chunk.to_vec()),
            report("ideographs-1", 200, chunk.to_vec()), // real id, no claim
            ChunkReport {
                id: "ideographs-4".to_owned(),
                status: 200,
                len: 999, // disagrees with the attachment
                body: chunk.to_vec(),
            },
            report("ideographs-4", 418, chunk.to_vec()), // unexpected status
        ] {
            let e = loader.accept(&bad, &mut g).err().expect("typed refusal");
            assert!(
                matches!(
                    e.code(),
                    Code::BindingBadArgument | Code::IoReadFailed
                ),
                "unexpected code for {bad:?}: {e:?}"
            );
        }
        assert_eq!(loader.revision(), before_rev, "no pixel changed");
        assert_eq!(loader.resident_bytes(), before_bytes, "no bytes kept");
    }


    /// Eviction is the FONT.10-F1 lever, and it reports the new resident total
    /// so the shell can measure against its own budget.
    #[test]
    fn closing_a_chunk_releases_bytes_and_reports_the_new_total() {
        let (mut loader, chunk) = loader_with_chunk();
        assert_eq!(loader.set_mut().request(0x6F00), Some("ideographs-4"));
        let _ = loader.plan().expect("planned");
        let mut g = guard();
        let _ = loader
            .accept(&report("ideographs-4", 200, chunk.to_vec()), &mut g)
            .expect("adopt");
        let held = loader.resident_bytes();
        assert_eq!(held, u64::try_from(chunk.len()).unwrap_or(u64::MAX));

        let out = loader.close("ideographs-4").expect("bytes were held");
        assert_eq!(out.id, "ideographs-4");
        assert_eq!(out.released, u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        assert_eq!(out.resident_bytes, 0, "the chunk's bytes are gone");
        assert_eq!(out.revision, 2, "adopt then evict: two pixel changes");
        assert!(!loader.set().covers(0x6F00), "back to notdef");

        // Idempotent: closing what is not resident is not a repaint.
        assert!(loader.close("ideographs-4").is_none());
        assert!(loader.close("no-such-chunk").is_none());
        assert!(loader.close("core").is_none(), "the core is not a chunk");
        assert_eq!(loader.revision(), 2, "still no repaint owed");
    }

    /// A chunk the loader's byte ceiling cannot hold is refused, and refused
    /// *before* it is kept — a shell cannot make the guest allocate first and
    /// account afterwards.
    #[test]
    fn an_oversized_attachment_is_refused_before_it_is_kept() {
        let chunk = payload(&[0x6F00]);
        let mut loader = CjkChunkLoader::new(
            Bytes::new(),
            vec![claim("ideographs-4", &chunk)],
            Budget {
                // Room for the claim, not for the file.
                bytes: 64,
                ..Budget::unlimited()
            },
        )
        .expect("the claim itself is within the hostile bound");
        assert_eq!(loader.set_mut().request(0x6F00), Some("ideographs-4"));
        let _ = loader.plan().expect("planned");
        let mut g = guard();
        let e = loader
            .accept(&report("ideographs-4", 200, chunk.to_vec()), &mut g)
            .err()
            .expect("refused");
        assert_eq!(e.code(), Code::BudgetBytes);
        assert!(!loader.set().covers(0x6F00));
        assert_eq!(loader.resident_bytes(), 0, "nothing was kept");
    }

    /// The request sequence is a pure function of the resident state: the same
    /// queued ids, planned in chunk-table order, every time (ADR-P0012).
    #[test]
    fn the_request_order_is_deterministic_and_table_ordered() {
        let a = payload(&[0x4E00]);
        let b = payload(&[0x6F00]);
        let claims = vec![claim("ideographs-4", &b), claim("ideographs-1", &a)];
        let mut loader =
            CjkChunkLoader::new(Bytes::new(), claims, budget()).expect("claims parse");
        // Queue both, in the wrong order on purpose.
        let _ = loader.set_mut().request(0x6F00);
        let _ = loader.set_mut().request(0x4E00);
        let first = loader.plan().expect("one").expect("planned");
        let second = loader.plan().expect("two").expect("planned");
        assert_eq!(first.id, "ideographs-1", "table order, not request order");
        assert_eq!(second.id, "ideographs-4");
        assert_eq!(loader.plan().expect("bounded"), None, "and no repeats");
    }

    /// Planning twice before delivering must not name the same chunk twice: two
    /// fetches of one file, two attempt charges, and two shell loops racing for
    /// the same bytes.
    #[test]
    fn planning_never_names_a_chunk_that_is_already_in_flight() {
        let (mut loader, chunk) = loader_with_chunk();
        assert_eq!(loader.set_mut().request(0x6F00), Some("ideographs-4"));
        assert!(loader.plan().expect("bounded").is_some());
        assert_eq!(
            loader.plan().expect("bounded"),
            None,
            "the one wanted chunk is already out"
        );
        // The delivery clears it, and the attempt charge was paid once.
        let mut g = guard();
        let _ = loader
            .accept(&report("ideographs-4", 200, chunk.to_vec()), &mut g)
            .expect("adopt");
        assert_eq!(loader.plan().expect("bounded"), None, "and now it is resident");
    }
}

