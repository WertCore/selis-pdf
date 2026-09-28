//! Build the subsetted core + chunk files from a source CJK font
//! (SL-3.FONT.10, on top of the SL-3.FONT.11 subsetter).
//!
//! [`build_set`] turns one TrueType (glyf) CJK font — production builds use
//! a Noto Sans CJK static TTF — into exactly the payload the [`CjkFontSet`]
//! consumes: a **core** file covering [`CORE_STATIC_IDS`] plus a
//! code-point frequency list, and one **chunk** file per chunk-table range
//! the source actually covers (empties are skipped). Every file is a plain
//! subset SFNT — a strict glyph subset of the single source, so a glyph
//! present in several files (core and its range chunk) carries byte-identical
//! outlines, whichever the resolver answers with (ADR-P0012).
//!
//! The build is a pure bytes→bytes function (no fs, no net — L0–L3 purity):
//! `xtask cjk-build` wires it to the filesystem and pins the sizes in a
//! manifest; the engine tests run it against a synthetic source font.

use std::collections::{BTreeMap, BTreeSet};

use selis_bytes::Bytes;
use selis_error::Result;
use selis_sandbox::BudgetGuard;
use skrifa::{FontRef, MetadataProvider, Tag};

use super::set::CjkFontSet;
use super::{CjkChunk, CHUNKS, CORE_STATIC_IDS};
use crate::subset::{subset_ttf_named, GlyphSet, SubsetName};

/// Sample high-frequency Simplified Chinese core list (the opening of the
/// standard frequency order — 的 一 是 在 …). Production builds pass a
/// vetted, licensed frequency list through `core_extra` instead; this sample
/// keeps the offline default core useful for the common case and gives
/// `xtask cjk-build --core-list` a documented format (one char or `U+XXXX`
/// per whitespace-separated token).
pub const CORE_SAMPLE_HANZI: &[char] = &[
    '的', '一', '是', '在', '不', '了', '有', '和', '人', '这', '中', '大', '为', '上', '个', '国',
    '我', '以', '要', '他', '时', '来', '用', '们', '生', '到', '作', '地', '于', '出', '就', '分',
    '对', '成', '会', '可', '主', '发', '年',
];

/// One chunk file produced by [`build_set`].
#[derive(Debug, Clone)]
pub struct CjkBuiltChunk {
    /// The chunk-table entry this file serves.
    pub chunk: &'static CjkChunk,
    /// The number of source code points covered (not glyph count: shared
    /// glyphs are subsetted once).
    pub codes: usize,
    /// The subset font bytes (the `cjk/<id>.ttf` payload).
    pub data: Bytes,
}

impl CjkBuiltChunk {
    /// Adopt this chunk into a resident set (the shell's `provide` step).
    pub fn provide(&self, set: &mut CjkFontSet) -> bool {
        set.provide(self.chunk.id, self.data.clone())
    }
}

/// The complete built payload: bundled core plus the loadable chunks.
#[derive(Debug, Clone)]
pub struct CjkSetBuild {
    /// The subsetted core file (bundled on every platform).
    pub core: Bytes,
    /// Code points the core covers.
    pub core_codes: usize,
    /// The chunk files, in table order (table ranges the source misses are
    /// skipped: not every CJK font covers every block).
    pub chunks: Vec<CjkBuiltChunk>,
    /// The table ranges this source has **no** glyphs for at all, core-static
    /// ones included, in table order.
    ///
    /// This is the "what happens when the glyph is missing from the chosen
    /// source" answer, as data rather than as an absence: a payload built from
    /// Noto Sans SC has no Hangul syllable at all, so `hangul-1`…`hangul-4`
    /// are listed here and a Korean document's ᄀ U+AC00 paints `.notdef`
    /// forever. The manifest publishes this list, the shell feeds it to
    /// [`CjkFontSet::mark_unavailable`](super::set::CjkFontSet::mark_unavailable)
    /// so no chunk that cannot exist is ever requested, and a user is told
    /// "this payload has no Korean" rather than "still loading".
    pub unserved: Vec<&'static CjkChunk>,
}

impl CjkSetBuild {
    /// The resident set this build seeds (core only — chunks load lazily
    /// through their [`provide`](CjkBuiltChunk::provide) or a
    /// [`CjkChunkSource`](super::set::CjkChunkSource)).
    #[must_use]
    pub fn font_set(&self) -> CjkFontSet {
        CjkFontSet::new(self.core.clone())
    }
}

/// The source font's code point → glyph-id map, or `None` when it is not a
/// TrueType (glyf) font — CFF sources are a FONT.11 follow-up and reject
/// loudly rather than emit a silently-empty payload.
#[must_use]
pub fn source_codepoints(source: &Bytes) -> Option<BTreeMap<u32, u16>> {
    crate::contain(|| {
        let font = FontRef::new(source.as_slice()).ok()?;
        font.table_data(Tag::new(b"glyf"))?; // CFF subset deferred (SL-3.FONT.11 deviation)
        let mut map = BTreeMap::new();
        for (code, glyph) in font.charmap().mappings() {
            if let Ok(gid) = u16::try_from(glyph.to_u32()) {
                map.insert(code, gid);
            }
        }
        Some(map)
    })
    .flatten()
}

/// The name every CJK payload file is published under.
///
/// Not "Noto Sans SC": a subset is a modified version, and the OFL reserves
/// the source's family name (see [`SubsetName`]). The source's real identity
/// is pinned in the manifest's `source` row and in
/// `assets/cjk/PROVENANCE.md` — which is where a licence claim belongs.
pub const SELIS_CJK_NAME: SubsetName = SubsetName {
    family: "Selis CJK",
    subfamily: "Regular",
};

/// Build the core + chunk payload from a source font.
///
/// `core_extra` are additional code points forced into the core beyond
/// [`CORE_STATIC_IDS`] (the frequency list; pass
/// [`CORE_SAMPLE_HANZI`](self::CORE_SAMPLE_HANZI) codes for the offline
/// default). Every emitted file is named `name`.
///
/// # Budget
///
/// Every emitted file is a `subset_ttf` output and charges the guard its
/// bytes; the per-file input is bounded by the source.
///
/// # Malformed Input
///
/// A non-parsable or non-glyf source returns `Ok(None)` (like the
/// subsetter); a budget run mid-build propagates the typed `Err`.
pub fn build_set(
    source: &Bytes,
    core_extra: &[u32],
    name: SubsetName,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<CjkSetBuild>> {
    let Some(map) = source_codepoints(source) else {
        return Ok(None);
    };
    let mut core_gids: BTreeSet<u16> = BTreeSet::new();
    let mut core_codes = 0usize;
    let mut unserved: Vec<&'static CjkChunk> = Vec::new();
    for chunk in CHUNKS {
        let (gids, n) = range_gids(&map, chunk);
        if n == 0 {
            unserved.push(chunk);
        }
        if !CORE_STATIC_IDS.contains(&chunk.id) {
            continue;
        }
        core_codes = core_codes.saturating_add(n);
        core_gids.extend(gids);
    }
    for &code in core_extra {
        if let Some(&gid) = map.get(&code) {
            core_codes = core_codes.saturating_add(1);
            let _ = core_gids.insert(gid);
        }
    }
    let Some(core) = subset_gids(source, &core_gids, name, g)? else {
        return Ok(None);
    };
    let mut chunks = Vec::new();
    for chunk in CHUNKS {
        if CORE_STATIC_IDS.contains(&chunk.id) {
            continue; // core-only ranges: a chunk file would never be fetched
        }
        let (gids, n) = range_gids(&map, chunk);
        if gids.is_empty() {
            continue; // nothing in this range in this source: no file (reported
                      // in `unserved` — the range is addressable, just empty)
        }
        if let Some(data) = subset_gids(source, &gids, name, g)? {
            chunks.push(CjkBuiltChunk {
                chunk,
                codes: n,
                data,
            });
        } else {
            return Ok(None);
        }
    }
    Ok(Some(CjkSetBuild {
        core,
        core_codes,
        chunks,
        unserved,
    }))
}

/// The distinct surviving glyph ids for a chunk range, and how many code
/// points mapped to them.
fn range_gids(map: &BTreeMap<u32, u16>, range: &CjkChunk) -> (BTreeSet<u16>, usize) {
    let mut gids = BTreeSet::new();
    let mut codes = 0usize;
    for (_code, &gid) in map.range(range.first..=range.last) {
        codes = codes.saturating_add(1);
        let _ = gids.insert(gid);
    }
    (gids, codes)
}

fn subset_gids(
    source: &Bytes,
    gids: &BTreeSet<u16>,
    name: SubsetName,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Bytes>> {
    let keep = gids.iter().copied().collect();
    subset_ttf_named(source, &GlyphSet { keep }, Some(name), g)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, clippy::integer_division)]
    use super::*;
    use crate::outline::{glyph_id_for_char, units_per_em};

    fn guard() -> selis_sandbox::BudgetGuard<'static> {
        selis_sandbox::Budget::unlimited().guard()
    }

    fn rect_outline() -> Vec<crate::OutlineCmd> {
        use crate::OutlineCmd;
        vec![
            OutlineCmd::Move { x: 120.0, y: 80.0 },
            OutlineCmd::Line { x: 920.0, y: 80.0 },
            OutlineCmd::Line { x: 920.0, y: 920.0 },
            OutlineCmd::Line { x: 120.0, y: 920.0 },
            OutlineCmd::Close,
        ]
    }

    /// A synthetic Noto stand-in: `mini.ttf` plus one rectangle glyph at
    /// every code point that lands in a different table range — kana (core),
    /// a late ideograph (its own chunk), a common ideograph forced to core
    /// via the extra list, one Hangul quarter, an astral extension, and a
    /// Kangxi radical (core static).
    fn fixture_source() -> Bytes {
        let mut g = guard();
        let mut font = Bytes::copy_from_slice(include_bytes!("../../tests/fixtures/mini.ttf"));
        for &code in &[0x3042u32, 0x4E00, 0x6F00, 0xAC00, 0x20000, 0x2F81] {
            font = crate::subset::add_glyph(&font, code, &rect_outline(), 1000, &mut g)
                .expect("add ok")
                .expect("merged");
        }
        font
    }

    fn built() -> CjkSetBuild {
        let mut g = guard();
        let extra: Vec<u32> = ['一', '的'].iter().map(|c| u32::from(*c)).collect();
        build_set(&fixture_source(), &extra, SELIS_CJK_NAME, &mut g)
            .expect("budget ok")
            .expect("glyf source")
    }

    #[test]
    fn build_emits_core_covering_static_plus_frequency() {
        let b = built();
        assert!(units_per_em(&b.core).is_some(), "core parses");
        // 0x3042 is in the static kana range; 0x4E00 came from the extra
        // list; the ideographs-4 code must *not* be in the core.
        assert!(glyph_id_for_char(&b.core, 0x3042).is_some());
        assert!(glyph_id_for_char(&b.core, 0x4E00).is_some());
        assert!(
            glyph_id_for_char(&b.core, 0x2F81).is_some(),
            "kangxi is core-static"
        );
        assert!(glyph_id_for_char(&b.core, 0x6F00).is_none());
        assert!(b.core_codes >= 3);
    }

    #[test]
    fn build_emits_one_file_per_covered_range_in_table_order() {
        let b = built();
        let ids: Vec<&str> = b.chunks.iter().map(|c| c.chunk.id).collect();
        // Core-static ranges (kangxi, punct-kana) get no files; 0x4E00 is in
        // the core via the frequency list *and* in the range-pure
        // ideographs-1 chunk (the same outline bytes, either may answer).
        assert_eq!(
            ids,
            vec!["ideographs-1", "ideographs-4", "hangul-1", "ext-b"]
        );
        let four = b
            .chunks
            .iter()
            .find(|c| c.chunk.id == "ideographs-4")
            .expect("built");
        assert_eq!(four.codes, 1, "one source code point in range");
        assert!(glyph_id_for_char(&four.data, 0x6F00).is_some());
        assert!(
            glyph_id_for_char(&four.data, 0xAC00).is_none(),
            "chunk is range-scoped"
        );
    }

    #[test]
    fn build_is_deterministic_and_feeds_the_resident_set() {
        let a = built();
        let b = built();
        assert_eq!(a.core, b.core, "identical bytes across runs");
        assert_eq!(a.chunks.len(), b.chunks.len());
        for (x, y) in a.chunks.iter().zip(b.chunks.iter()) {
            assert_eq!(x.chunk.id, y.chunk.id);
            assert_eq!(x.data, y.data);
        }
        let mut set = a.font_set();
        assert_eq!(
            set.loaded_ids(),
            Vec::<&'static str>::new(),
            "chunks are not resident yet"
        );
        for c in &a.chunks {
            assert!(c.provide(&mut set), "chunk adopted for its own id");
        }
        // Core + chunks now cover every fixture code, each by its own rule.
        for code in [0x3042u32, 0x4E00, 0x6F00, 0xAC00, 0x2F81] {
            assert!(set.covers(code), "resident set covers {code:04X}");
        }
        assert_eq!(set.revision(), 4, "four chunk adoptions");
        assert_eq!(
            set.loaded_ids(),
            vec!["ideographs-1", "ideographs-4", "hangul-1", "ext-b"],
            "loaded ids in table order"
        );
    }

    #[test]
    fn cff_and_broken_sources_are_rejected_not_misbuilt() {
        let mut g = guard();
        let cff = Bytes::copy_from_slice(include_bytes!("../../tests/fixtures/cff.otf"));
        assert!(build_set(&cff, &[], SELIS_CJK_NAME, &mut g)
            .expect("budget ok")
            .is_none());
        let junk = Bytes::copy_from_slice(b"not a font");
        assert!(build_set(&junk, &[], SELIS_CJK_NAME, &mut g)
            .expect("budget ok")
            .is_none());
    }

    /// Every file the builder emits is named `SELIS_CJK_NAME`, never the
    /// source's family: a subset is an OFL modified version and the reserved
    /// name does not travel with the outlines.
    #[test]
    fn emitted_files_are_renamed_away_from_the_source_family() {
        let b = built();
        assert_eq!(
            name_ids(&b.core),
            vec![
                (1u16, "Selis CJK".to_string()),
                (2u16, "Regular".to_string()),
                (4u16, "Selis CJK Regular".to_string()),
                (6u16, "SelisCJK-Regular".to_string())
            ]
        );
        for c in &b.chunks {
            assert_eq!(
                name_ids(&c.data),
                vec![
                    (1u16, "Selis CJK".to_string()),
                    (2u16, "Regular".to_string()),
                    (4u16, "Selis CJK Regular".to_string()),
                    (6u16, "SelisCJK-Regular".to_string())
                ],
                "chunk {}",
                c.chunk.id
            );
        }
        // The source keeps its own records: the builder renames the output, it
        // does not rewrite its input.
        let source = name_ids(&fixture_source());
        assert!(
            source.iter().all(|(_, value)| value != "Selis CJK"),
            "the fixture source is not renamed in place"
        );
    }

    /// Decode a font's `name` table into `(nameID, value)` pairs (format 0,
    /// UTF-16BE) — the assertions above are about *bytes we wrote*, so they
    /// read the bytes rather than asking a library what it makes of them.
    fn name_ids(bytes: &Bytes) -> Vec<(u16, String)> {
        fn be16(data: &[u8], off: usize) -> u16 {
            let pair = data.get(off..off.saturating_add(2)).unwrap_or(&[0, 0]);
            let hi = pair.first().copied().unwrap_or(0);
            let lo = pair.get(1).copied().unwrap_or(0);
            u16::from_be_bytes([hi, lo])
        }
        let font = FontRef::new(bytes.as_slice()).expect("parses");
        let table = font
            .table_data(Tag::new(b"name"))
            .expect("has a name table");
        let data = table.as_bytes();
        let count = usize::from(be16(data, 2));
        let storage = usize::from(be16(data, 4));
        (0..count)
            .map(|i| {
                let rec = 6usize.saturating_add(i.saturating_mul(12));
                let id = be16(data, rec.saturating_add(6));
                let len = usize::from(be16(data, rec.saturating_add(8)));
                let off = usize::from(be16(data, rec.saturating_add(10)));
                let start = storage.saturating_add(off);
                let units: Vec<u16> = data
                    .get(start..start.saturating_add(len))
                    .unwrap_or_default()
                    .chunks_exact(2)
                    .map(|p| {
                        let hi = p.first().copied().unwrap_or(0);
                        let lo = p.get(1).copied().unwrap_or(0);
                        u16::from_be_bytes([hi, lo])
                    })
                    .collect();
                (id, String::from_utf16_lossy(&units))
            })
            .collect()
    }

    /// The ranges a source has nothing for are *reported*, not silently
    /// absent — that is the "no CJK font for this script" answer the manifest
    /// publishes and the shell turns into a refusal instead of a fetch loop.
    #[test]
    fn unserved_ranges_are_reported() {
        let b = built();
        let ids: Vec<&str> = b.unserved.iter().map(|c| c.id).collect();
        for absent in ["jamo", "ext-a", "compat-ideographs", "ext-sup"] {
            assert!(ids.contains(&absent), "{absent} has no fixture glyph");
        }
        for served in ["punct-kana", "ideographs-4", "hangul-1", "ext-b"] {
            assert!(!ids.contains(&served), "{served} is served");
        }
        // Every table range is classified exactly once: `unserved` (the source
        // has no glyph there) or served — by a chunk file, or by the core for
        // a core-static range. Never both, never neither: a range the build
        // cannot classify is a range a shell cannot answer for.
        for chunk in CHUNKS {
            let listed = b.unserved.iter().any(|c| c.id == chunk.id);
            let has_file = b.chunks.iter().any(|c| c.chunk.id == chunk.id);
            // A core-static range the source has nothing for is *unserved*,
            // not "served by the core": the core file carries no glyph for it.
            let served = has_file || (CORE_STATIC_IDS.contains(&chunk.id) && !listed);
            assert!(
                listed != served,
                "{} is {}",
                chunk.id,
                if listed {
                    "both unserved and served"
                } else {
                    "neither unserved nor served"
                }
            );
        }
    }
}
