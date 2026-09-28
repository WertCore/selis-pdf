//! The resident CJK set: core + loaded chunks + pending-fetch queue
//! (SL-3.FONT.10).
//!
//! The [`CjkFontSet`] owns the bytes the shell has supplied so far. It never
//! fetches anything itself (L0–L3 purity: no fs, no net, no async): chunk
//! bytes arrive through [`CjkFontSet::provide`], and the shell drains the
//! pending queue either synchronously via [`CjkFontSet::drain_requested`]
//! (native asset lookup, tests) or with its own async loader (the web WASM
//! shell, SL-4.WASM.07).
//!
//! Determinism (ADR-P0012): a render walk consumes an immutable
//! [`CjkSnapshot`] captured at walk start, so bytes provided while a page is
//! painting cannot change that page's pixels; the same snapshot yields the
//! same pixels on every platform. Chunk *selection* is a pure function of
//! the code point through [`chunk_for`](super::chunk_for) — installed system
//! fonts never participate.

use std::collections::{BTreeMap, BTreeSet};

use selis_bytes::Bytes;

use super::{chunk_by_id, chunk_for, CORE_ID};
use crate::outline::glyph_id_for_char;

/// Where in the resident CJK set a glyph came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CjkSlot {
    /// The bundled core subset.
    Core,
    /// A lazy-loaded chunk subset.
    Chunk(&'static str),
}

impl CjkSlot {
    /// The stable cache key for this slot: [`CORE_ID`] or the chunk id.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Core => CORE_ID,
            Self::Chunk(id) => id,
        }
    }
}

/// A glyph resolved from the resident CJK set: the font program to extract
/// the outline from plus the glyph id within it.
#[derive(Debug, Clone)]
pub struct CjkGlyph {
    /// The slot the glyph came from.
    pub slot: CjkSlot,
    /// The font program bytes (an `Arc`-backed clone — cheap).
    pub bytes: Bytes,
    /// The glyph id within `bytes`.
    pub gid: u16,
}

/// An immutable view of a [`CjkFontSet`] for one render walk.
///
/// The snapshot shares the set's `Bytes` handles (no payload copy). Walks
/// take snapshots, never the set itself: the set is mutable (`provide` may
/// land between two walks) while a snapshot is not — that is what keeps a
/// walk deterministic.
#[derive(Debug, Clone)]
pub struct CjkSnapshot {
    core: Bytes,
    chunks: BTreeMap<&'static str, Bytes>,
}

impl CjkSnapshot {
    /// A snapshot carrying only the core set.
    #[must_use]
    pub fn core_only(core: Bytes) -> Self {
        Self {
            core,
            chunks: BTreeMap::new(),
        }
    }

    /// The code point's glyph in the resident set, or `None` when nothing
    /// loaded covers it.
    ///
    /// The chunk font covering the code point wins over the core when both
    /// carry it; both are subsets of the same source font, so the outlines
    /// — and therefore the pixels — are identical either way.
    #[must_use]
    pub fn resolve(&self, code: u32) -> Option<CjkGlyph> {
        if let Some(chunk) = chunk_for(code) {
            if let Some(bytes) = self.chunks.get(chunk.id) {
                if let Some(gid) = glyph_id_for_char(bytes, code) {
                    return Some(CjkGlyph {
                        slot: CjkSlot::Chunk(chunk.id),
                        bytes: bytes.clone(),
                        gid,
                    });
                }
            }
        }
        let gid = glyph_id_for_char(&self.core, code)?;
        Some(CjkGlyph {
            slot: CjkSlot::Core,
            bytes: self.core.clone(),
            gid,
        })
    }

    /// Whether the chunk file for `id` is resident in this view.
    #[must_use]
    pub fn has_chunk(&self, id: &str) -> bool {
        self.chunks.contains_key(id)
    }

    /// Whether the resident set covers a code point (the pre-walk answer the
    /// shell may use to decide what to prefetch).
    #[must_use]
    pub fn covers(&self, code: u32) -> bool {
        self.resolve(code).is_some()
    }
}

/// The injected fetch boundary for chunk bytes.
///
/// The engine calls this only off the render path
/// ([`CjkFontSet::drain_requested`]); the web shell instead drives
/// [`CjkFontSet::provide`] from its own async loader (ADR-P0043 fetch/caching
/// contract). Implementations may read a cache, bundled assets, or an
/// in-memory map; the render walk itself never blocks here.
pub trait CjkChunkSource {
    /// The chunk bytes for a known chunk id, or `None` when unavailable
    /// (the request stays pending for the next drain).
    fn fetch(&mut self, chunk_id: &str) -> Option<Bytes>;
}

/// The resident CJK fallback set: the subsetted core, the chunks loaded so
/// far, the chunks a render asked for but did not find, the ranges the payload
/// provably cannot serve, and an invalidation [`revision`](Self::revision).
#[derive(Debug, Clone)]
pub struct CjkFontSet {
    resident: CjkSnapshot,
    requested: BTreeSet<&'static str>,
    unavailable: BTreeSet<&'static str>,
    revision: u64,
}

impl CjkFontSet {
    /// A set holding only the subsetted core (no chunks loaded yet).
    #[must_use]
    pub fn new(core: Bytes) -> Self {
        Self {
            resident: CjkSnapshot::core_only(core),
            requested: BTreeSet::new(),
            unavailable: BTreeSet::new(),
            revision: 0,
        }
    }

    /// The bundled core bytes.
    #[must_use]
    pub fn core(&self) -> &Bytes {
        &self.resident.core
    }

    /// The chunk currently resident for an id, if provided and validated.
    #[must_use]
    pub fn chunk(&self, id: &str) -> Option<Bytes> {
        self.resident.chunks.get(id).cloned()
    }

    /// The ids of all loaded chunks, in table order.
    #[must_use]
    pub fn loaded_ids(&self) -> Vec<&'static str> {
        super::CHUNKS
            .iter()
            .filter(|c| self.resident.chunks.contains_key(c.id))
            .map(|c| c.id)
            .collect()
    }

    /// The invalidation revision: bumped every time provided bytes change
    /// the resident set. The shell repaints when it changes — this is the
    /// hook the notdef-to-repaint sequence drives (SL-4.WASM.07).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Adopt a downloaded chunk file.
    ///
    /// `id` must name a chunk in [`CHUNKS`](super::CHUNKS) and `bytes` must
    /// parse as an SFNT font (the cache-poisoning guard: a mis-named or
    /// corrupt chunk is rejected, never adopted). Returns whether the
    /// resident set changed (`true` bumps [`revision`](Self::revision) and
    /// clears the id's pending request). Re-providing identical bytes is a
    /// no-op. Validation is a header parse, not a decode — glyphs are
    /// outline-extracted under the render budget as always.
    ///
    /// Not budget-gated: the bytes were already allocated by the caller (the
    /// shell owns the download/cache quota).
    ///
    /// # Malformed Input
    ///
    /// An unknown id or unparseable font returns `false` and leaves the set
    /// unchanged — never a panic (hostile chunk files are expected).
    pub fn provide(&mut self, id: &str, bytes: Bytes) -> bool {
        let Some(chunk) = chunk_by_id(id) else {
            return false;
        };
        if self
            .resident
            .chunks
            .get(chunk.id)
            .is_some_and(|held| *held == bytes)
        {
            self.requested.remove(chunk.id);
            return false;
        }
        let parses =
            crate::contain(|| skrifa::FontRef::new(bytes.as_slice()).is_ok()).unwrap_or(false);
        if !parses {
            return false;
        }
        self.resident.chunks.insert(chunk.id, bytes);
        self.requested.remove(chunk.id);
        // Bytes beat a stale manifest: a payload that did ship this file is
        // authoritative over one that said it could not.
        self.unavailable.remove(chunk.id);
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// The immutable view one render walk consumes.
    #[must_use]
    pub fn snapshot(&self) -> CjkSnapshot {
        self.resident.clone()
    }

    /// Resolve a code point against the resident set (walk-free convenience;
    /// a walk uses [`CjkSnapshot::resolve`]).
    #[must_use]
    pub fn resolve(&self, code: u32) -> Option<CjkGlyph> {
        self.resident.resolve(code)
    }

    /// Whether the resident set covers a code point.
    #[must_use]
    pub fn covers(&self, code: u32) -> bool {
        self.resident.covers(code)
    }

    /// Record that a render needed the chunk addressing `code` but did not
    /// find it resident. Returns the chunk id, or `None` when the code is
    /// non-CJK, already covered (the core answered it, or some set member
    /// does), its covering chunk is already resident, or the payload was marked
    /// unable to serve that range. That last rule is the fetch-loop guard for
    /// a range the source font does not cover at all (Noto Sans SC has no
    /// Hangul): the code renders `.notdef` with no repeated download, and
    /// [`unavailable_ids`](Self::unavailable_ids) is what the shell reports to
    /// the user as "this payload has no Korean" rather than "still loading".
    ///
    /// The rule before it is the fetch-loop guard for a *hole inside* a range
    /// the source does cover (Noto's extensions have holes): a code the
    /// resident chunk font genuinely lacks renders `.notdef` with no repeated
    /// download either.
    pub fn request(&mut self, code: u32) -> Option<&'static str> {
        let chunk = chunk_for(code)?;
        if self.resident.covers(code) {
            return None;
        }
        if self.resident.chunks.contains_key(chunk.id) {
            return None;
        }
        if self.unavailable.contains(chunk.id) {
            return None; // no file exists for this range: nothing to ask for
        }
        self.requested.insert(chunk.id);
        Some(chunk.id)
    }

    /// Record that the payload this set is fed from has **no file** for these
    /// ranges — the manifest's `served_by: null` rows, which the build
    /// reports as [`unserved`](super::build::CjkSetBuild::unserved).
    ///
    /// This is the defined behaviour for "a CJK font the system does not
    /// have": a Noto Sans SC payload has no Hangul syllable, so a Korean
    /// document's 가 (U+AC00) paints `.notdef` — permanently, deterministically,
    /// and *without* the shell being told to fetch `hangul-1` on every
    /// repaint. Without this the set would queue an id that can never be
    /// satisfied, and a loader would retry a 404 forever.
    ///
    /// Unknown ids are ignored; a marked-unavailable id that is later
    /// [`provide`](Self::provide)d is un-marked (bytes beat a stale manifest).
    /// Marking does not bump [`revision`](Self::revision): it changes no
    /// pixels, only what is worth asking for.
    pub fn mark_unavailable(&mut self, ids: &[&str]) {
        for id in ids {
            if let Some(chunk) = chunk_by_id(id) {
                let _ = self.unavailable.insert(chunk.id);
                // A queue entry for a range that cannot exist is not a fetch.
                let _ = self.requested.remove(chunk.id);
            }
        }
    }

    /// The ranges this set was told the payload cannot serve, in table order.
    #[must_use]
    pub fn unavailable_ids(&self) -> Vec<&'static str> {
        super::CHUNKS
            .iter()
            .filter(|c| self.unavailable.contains(c.id))
            .map(|c| c.id)
            .collect()
    }

    /// Record a needed chunk by id (from a walk's collected pending set).
    /// Unknown, already-resident, or unavailable ids are ignored.
    pub fn request_chunk(&mut self, id: &str) {
        let Some(chunk) = chunk_by_id(id) else {
            return;
        };
        if self.unavailable.contains(chunk.id) {
            return; // the payload has no such file: never queue a fetch
        }
        if !self.resident.chunks.contains_key(chunk.id) {
            self.requested.insert(chunk.id);
        }
    }

    /// The pending queue's contents in deterministic **table order**,
    /// without consuming it: renders report this mirror (sticky across
    /// repaints until the set's state changes), [`drain_requested`](Self::drain_requested)
    /// consumes it.
    #[must_use]
    pub fn queued(&self) -> Vec<&'static str> {
        super::CHUNKS
            .iter()
            .filter(|c| self.requested.contains(c.id))
            .map(|c| c.id)
            .collect()
    }

    /// The pending chunk ids in deterministic **table order**, clearing the
    /// queue. A chunk whose fetch failed is re-marked by the next render, so
    /// draining is safe to interleave with repaints.
    pub fn take_requested(&mut self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for chunk in super::CHUNKS {
            if self.requested.remove(chunk.id) {
                out.push(chunk.id);
            }
        }
        self.requested.clear(); // ids are table-checked on insert; defensive
        out
    }

    /// Drain the pending queue through an injected source, providing every
    /// chunk the source returns. Returns the number adopted; ids the source
    /// could not serve go back on the queue (the next fetch cycle asks
    /// again).
    ///
    /// This is the synchronous face of the fetch contract: native shells and
    /// tests call it on the *repaint edge*, never during a render walk.
    /// Consuming the queue here (unlike the sticky [`queued`](Self::queued)
    /// mirror renders report) means a drain always makes forward progress.
    pub fn drain_requested(&mut self, source: &mut dyn CjkChunkSource) -> usize {
        let mut adopted = 0usize;
        for id in self.take_requested() {
            if let Some(bytes) = source.fetch(id) {
                // A `false` here means identical-already-resident or a
                // rejected corrupt chunk; the next render's walk re-marks
                // the latter, so a failed fetch cannot loop on its own.
                if self.provide(id, bytes) {
                    adopted = adopted.saturating_add(1);
                }
            } else {
                self.requested.insert(id);
            }
        }
        adopted
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, clippy::integer_division)]
    use super::*;
    use crate::outline::OutlineCmd;
    use crate::subset::{add_glyph, subset_ttf, GlyphSet};
    use selis_sandbox::Budget;

    const MINI: &[u8] = include_bytes!("../../tests/fixtures/mini.ttf");

    fn guard() -> selis_sandbox::BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    /// A rectangle outline spanning the em box (font units, upm as in the
    /// fixture).
    fn rect_outline(x0: f64, x1: f64) -> Vec<OutlineCmd> {
        vec![
            OutlineCmd::Move { x: x0, y: 100.0 },
            OutlineCmd::Line { x: x1, y: 100.0 },
            OutlineCmd::Line { x: x1, y: 900.0 },
            OutlineCmd::Line { x: x0, y: 900.0 },
            OutlineCmd::Close,
        ]
    }

    /// A tiny stand-in "Noto CJK source": `mini.ttf` with a rectangle glyph
    /// merged in at each code point (the FONT.11 add_glyph path).
    fn source_font(codes: &[u32]) -> Bytes {
        let mut g = guard();
        let mut font = Bytes::copy_from_slice(MINI);
        for &code in codes {
            font = add_glyph(&font, code, &rect_outline(100.0, 900.0), 1000, &mut g)
                .expect("add ok")
                .expect("font merged");
        }
        font
    }

    /// The chunk/core-style subset of `source` covering exactly `codes`.
    fn subset_of(source: &Bytes, codes: &[u32]) -> Bytes {
        let mut g = guard();
        let mut keep = Vec::new();
        for &code in codes {
            if let Some(gid) = glyph_id_for_char(source, code) {
                keep.push(gid);
            }
        }
        subset_ttf(source, &GlyphSet { keep }, &mut g)
            .expect("subset ok")
            .expect("ttf subset")
    }

    #[test]
    fn core_only_set_resolves_core_codes_only() {
        let source = source_font(&[0x3042, 0x4E00, 0x6F00]);
        let core = subset_of(&source, &[0x3042, 0x4E00]);
        let set = CjkFontSet::new(core.clone());
        let g = set.resolve(0x4E00).expect("core hit");
        assert_eq!(g.slot, CjkSlot::Core);
        assert_eq!(g.slot.key(), CORE_ID);
        assert_eq!(
            g.gid,
            glyph_id_for_char(&core, 0x4E00).expect("core maps 一")
        );
        assert!(set.covers(0x3042));
        assert!(!set.covers(0x6F00));
    }

    #[test]
    fn provide_validates_rejects_and_bumps_revision() {
        let chunk = subset_of(&source_font(&[0x6F00]), &[0x6F00]);
        let other = subset_of(&source_font(&[0x6F00, 0x6F01]), &[0x6F00, 0x6F01]);
        let mut set = CjkFontSet::new(Bytes::new());
        assert_eq!(set.revision(), 0);
        // Unknown id: rejected.
        assert!(!set.provide("no-such-chunk", chunk.clone()));
        // Garbage bytes: rejected.
        assert!(!set.provide(
            "ideographs-4",
            Bytes::copy_from_slice(b"not a font at all!!")
        ));
        assert_eq!(set.revision(), 0);
        // Valid chunk: adopted, revision bumps.
        assert!(set.provide("ideographs-4", chunk.clone()));
        assert_eq!(set.revision(), 1);
        assert!(set.covers(0x6F00));
        // Identical re-provide: no-op, no revision bump.
        assert!(!set.provide("ideographs-4", chunk));
        assert_eq!(set.revision(), 1);
        // Changed bytes for the id: adopted, revision bumps again.
        assert!(set.provide("ideographs-4", other));
        assert_eq!(set.revision(), 2);
        assert_eq!(set.loaded_ids(), vec!["ideographs-4"]);
    }

    #[test]
    fn request_queue_is_table_ordered_and_deduped() {
        let mut set = CjkFontSet::new(Bytes::new());
        assert_eq!(set.request(0x41), None, "non-CJK asks for nothing");
        assert_eq!(set.request(0xAC00), Some("hangul-1"));
        assert_eq!(set.request(0x6F00), Some("ideographs-4"));
        assert_eq!(set.request(0x6F02), Some("ideographs-4"), "dedupes");
        assert_eq!(
            set.queued(),
            vec!["ideographs-4", "hangul-1"],
            "peek mirrors the queue in table order without consuming"
        );
        // take in table order, not insertion order.
        assert_eq!(set.take_requested(), vec!["ideographs-4", "hangul-1"]);
        assert!(set.take_requested().is_empty(), "queue drained");
        set.request_chunk("ext-b");
        set.request_chunk("no-such-chunk"); // ignored
        assert_eq!(set.take_requested(), vec!["ext-b"]);
    }

    struct MapSource {
        map: BTreeMap<&'static str, Bytes>,
        fetches: usize,
    }

    impl CjkChunkSource for MapSource {
        fn fetch(&mut self, chunk_id: &str) -> Option<Bytes> {
            self.fetches += 1;
            self.map.get(chunk_id).cloned()
        }
    }

    #[test]
    fn drain_adopts_served_and_keeps_unserved_pending() {
        let source = source_font(&[0x6F00, 0xAC00]);
        let han = subset_of(&source, &[0x6F00]);
        let mut set = CjkFontSet::new(Bytes::new());
        set.request(0x6F00);
        set.request(0xAC00);
        let mut src = MapSource {
            map: BTreeMap::from([("ideographs-4", han)]),
            fetches: 0,
        };
        assert_eq!(set.drain_requested(&mut src), 1);
        assert_eq!(src.fetches, 2, "both pending ids were asked once");
        assert!(set.covers(0x6F00));
        assert!(!set.covers(0xAC00));
        assert_eq!(
            set.take_requested(),
            vec!["hangul-1"],
            "unserved stays pending"
        );
        assert_eq!(set.revision(), 1);
    }

    #[test]
    fn loaded_but_uncovered_codes_stop_reserving_forever() {
        // ideographs-4 resident without the code: request() must say "nothing
        // to fetch" so a missing-in-source character cannot loop downloads.
        let chunk = subset_of(&source_font(&[0x6F00]), &[0x6F00]);
        let mut set = CjkFontSet::new(Bytes::new());
        set.provide("ideographs-4", chunk);
        assert!(!set.covers(0x6F02));
        assert_eq!(set.request(0x6F02), None);
    }

    /// "The payload has no Korean" — the defined, tested behaviour for a CJK
    /// range the source font does not cover at all (SL-3.FONT.10's measured
    /// Noto Sans SC result: zero Hangul syllables, so `hangul-1` has no file).
    ///
    /// Without the mark, `request` would queue an id no fetch can satisfy and
    /// a shell would retry a 404 on every repaint; with it, the queue stays
    /// empty, the code stays `.notdef`, and the shell can *name* the reason.
    #[test]
    fn an_unavailable_range_is_never_requested_and_stays_notdef() {
        let mut set = CjkFontSet::new(Bytes::new());
        set.mark_unavailable(&["hangul-1", "no-such-chunk"]);
        assert_eq!(set.unavailable_ids(), vec!["hangul-1"], "unknown ids ignored");
        assert_eq!(set.request(0xAC00), None, "가: nothing to fetch");
        assert!(set.queued().is_empty(), "and nothing is queued");
        set.request_chunk("hangul-1");
        assert!(set.queued().is_empty(), "nor by id");
        assert!(!set.covers(0xAC00), "the glyph is still notdef");
        // Marking is not a pixel change: no repaint is owed.
        assert_eq!(set.revision(), 0);
        // A payload that did ship the file overrides the manifest.
        let chunk = subset_of(&source_font(&[0xAC00]), &[0xAC00]);
        assert!(set.provide("hangul-1", chunk));
        assert!(set.covers(0xAC00));
        assert!(set.unavailable_ids().is_empty());
    }

    /// An id already queued before the mark is dropped: the manifest is read
    /// once, and a stale queue must not outlive it.
    #[test]
    fn marking_drops_an_already_queued_id() {
        let mut set = CjkFontSet::new(Bytes::new());
        assert_eq!(set.request(0xAC00), Some("hangul-1"));
        assert_eq!(set.queued(), vec!["hangul-1"]);
        set.mark_unavailable(&["hangul-1"]);
        assert_eq!(
            set.take_requested(),
            Vec::<&'static str>::new(),
            "a queue entry for an impossible chunk is not a fetch"
        );
    }

    #[test]
    fn chunk_font_wins_over_core() {
        let source = source_font(&[0x4E00]);
        let core = subset_of(&source, &[0x4E00]);
        let chunk = subset_of(&source, &[0x4E00]);
        let mut set = CjkFontSet::new(core);
        assert_eq!(set.request(0x4E00), None, "the core already answers it");
        set.provide("ideographs-1", chunk.clone());
        assert_eq!(set.request(0x4E00), None, "already resident");
        let g = set.resolve(0x4E00).expect("covered");
        assert_eq!(g.slot, CjkSlot::Chunk("ideographs-1"));
        assert_eq!(g.bytes.as_slice(), chunk.as_slice());
    }

    #[test]
    fn snapshot_is_immutable_across_provides() {
        let chunk = subset_of(&source_font(&[0x6F00]), &[0x6F00]);
        let mut set = CjkFontSet::new(Bytes::new());
        let walk_view = set.snapshot();
        assert!(!walk_view.covers(0x6F00));
        set.provide("ideographs-4", chunk);
        // The walk's snapshot keeps pre-provide pixels…
        assert!(!walk_view.covers(0x6F00));
        // …while the next walk's snapshot sees the chunk.
        assert!(set.snapshot().covers(0x6F00));
    }
}
