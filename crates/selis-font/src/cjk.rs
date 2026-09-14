//! CJK fallback chunk model (SL-3.FONT.10).
//!
//! Noto CJK is far too large to bundle in the 3 MB WASM budget, so the web
//! shell ships a **subsetted core** and **lazy-loads additional ranges as
//! separate chunk files**, fetched on demand and cached. This module owns the
//! font side of that split:
//!
//! * [`CHUNKS`] — the static Unicode range → chunk table (ADR-P0012: selection
//!   is a pure function of the code point, never of installed system fonts).
//! * [`CjkFontSet`] (module [`set`]) — the resident set: core bytes plus
//!   loaded chunk fonts, a pending-request queue, and a revision counter the
//!   shell uses to invalidate (repaint) after a chunk is provided. The engine
//!   never fetches: chunk bytes arrive through [`CjkFontSet::provide`] (an
//!   injected [`CjkChunkSource`] drains the queue synchronously off the render
//!   path). A render walk reads one immutable [`CjkSnapshot`](set::CjkSnapshot),
//!   so a chunk arriving mid-walk cannot perturb its pixels.
//! * [`build`](build) — the production of core + chunk files: `subset_ttf`
//!   (SL-3.FONT.11) run over a source font, once per chunk range plus the
//!   core coverage set. Chunk files are plain subset TrueType (SFNT), served
//!   as `cjk/<chunk-id>.ttf` with transport-level compression (ADR-P0043).
//!
//! The viewer never blocks on a font fetch: a code point whose chunk is not
//! yet resident renders a `.notdef` box on this pass (the engine marks the
//! chunk in the queue), and repaints once the shell provides it — the
//! notdef-then-repaint sequence the DoD describes.
//!
//! The ranges follow Unicode CJK block boundaries; the main Ideographs block
//! (U+4E00–U+9FFF) and the Hangul syllables block (U+AC00–U+D7A3) are
//! quartered, so a document that only uses a slice of a block downloads a
//! fraction of it.

pub mod build;
pub mod set;

/// A lazy-loaded CJK font chunk: a contiguous Unicode range addressed by one
/// subset font file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CjkChunk {
    /// The chunk identifier — the shell's load key (`cjk/<id>.ttf`) and
    /// cache key. `"core"` is reserved for the bundled core set.
    pub id: &'static str,
    /// The first code point (inclusive).
    pub first: u32,
    /// The last code point (inclusive).
    pub last: u32,
}

/// The reserved id of the bundled core set (never a chunk-table id).
pub const CORE_ID: &str = "core";

/// The CJK chunk covering a code point, or `None` for non-CJK scripts.
///
/// Deterministic (ADR-P0012): the same code point always maps to the same
/// chunk.
#[must_use]
pub fn chunk_for(code: u32) -> Option<&'static CjkChunk> {
    CHUNKS.iter().find(|c| code >= c.first && code <= c.last)
}

/// The chunk with a known id (`"core"` is a valid key too), or `None`.
#[must_use]
pub fn chunk_by_id(id: &str) -> Option<&'static CjkChunk> {
    CHUNKS.iter().find(|c| c.id == id)
}

/// Whether a code point is inside the addressable CJK set (any chunk).
#[must_use]
pub fn is_cjk(code: u32) -> bool {
    chunk_for(code).is_some()
}

/// The full chunk table, in code-point order.
///
/// The main Ideographs block is quartered (U+4E00–U+51FF, U+5200–U+57FF,
/// U+5800–U+5EFF, U+5F00–U+9FFF) and the modern Hangul syllables block is
/// quartered likewise; every other range is a whole Unicode block. The core
/// coverage (subset at build time from [`CORE_STATIC_IDS`] plus a frequency
/// list) overlaps this table: coverage is cmap-driven at runtime, *addressing*
/// is table-driven.
pub const CHUNKS: &[CjkChunk] = &[
    CjkChunk {
        id: "jamo",
        first: 0x1100,
        last: 0x11FF,
    }, // Hangul Jamo
    CjkChunk {
        id: "radicals",
        first: 0x2E80,
        last: 0x2EFF,
    }, // CJK Radicals Supplement
    CjkChunk {
        id: "kangxi",
        first: 0x2F00,
        last: 0x2FDF,
    }, // Kangxi Radicals
    CjkChunk {
        id: "punct-kana",
        first: 0x3000,
        last: 0x30FF,
    }, // CJK punctuation + hiragana/katakana
    CjkChunk {
        id: "hangul-compat-jamo",
        first: 0x3130,
        last: 0x318F,
    }, // Hangul Compatibility Jamo
    CjkChunk {
        id: "strokes",
        first: 0x31C0,
        last: 0x31EF,
    }, // CJK Strokes
    CjkChunk {
        id: "enclosed",
        first: 0x3200,
        last: 0x32FF,
    }, // Enclosed CJK Letters and Months
    CjkChunk {
        id: "compat",
        first: 0x3300,
        last: 0x33FF,
    }, // CJK Compatibility
    CjkChunk {
        id: "ext-a",
        first: 0x3400,
        last: 0x4DBF,
    }, // Ideographs Extension A
    CjkChunk {
        id: "ideographs-1",
        first: 0x4E00,
        last: 0x51FF,
    },
    CjkChunk {
        id: "ideographs-2",
        first: 0x5200,
        last: 0x57FF,
    },
    CjkChunk {
        id: "ideographs-3",
        first: 0x5800,
        last: 0x5EFF,
    },
    CjkChunk {
        id: "ideographs-4",
        first: 0x5F00,
        last: 0x9FFF,
    },
    CjkChunk {
        id: "jamo-ext-a",
        first: 0xA960,
        last: 0xA97C,
    }, // Hangul Jamo Extended-A
    CjkChunk {
        id: "hangul-1",
        first: 0xAC00,
        last: 0xB7FF,
    },
    CjkChunk {
        id: "hangul-2",
        first: 0xB800,
        last: 0xC3FF,
    },
    CjkChunk {
        id: "hangul-3",
        first: 0xC400,
        last: 0xCFFF,
    },
    CjkChunk {
        id: "hangul-4",
        first: 0xD000,
        last: 0xD7A3,
    },
    CjkChunk {
        id: "jamo-ext-b",
        first: 0xD7B0,
        last: 0xD7FF,
    }, // Hangul Jamo Extended-B
    CjkChunk {
        id: "compat-ideographs",
        first: 0xF900,
        last: 0xFAFF,
    }, // CJK Compatibility Ideographs
    CjkChunk {
        id: "vertical",
        first: 0xFE10,
        last: 0xFE1F,
    }, // Vertical Forms
    CjkChunk {
        id: "compat-forms",
        first: 0xFE30,
        last: 0xFE4F,
    }, // CJK Compatibility Forms
    CjkChunk {
        id: "small-forms",
        first: 0xFE50,
        last: 0xFE6F,
    }, // Small Form Variants
    CjkChunk {
        id: "halfwidth",
        first: 0xFF00,
        last: 0xFFEF,
    }, // Halfwidth and Fullwidth Forms
    CjkChunk {
        id: "ext-b",
        first: 0x20000,
        last: 0x2A6DF,
    }, // Ideographs Extension B
    CjkChunk {
        id: "ext-c",
        first: 0x2A700,
        last: 0x2B73F,
    },
    CjkChunk {
        id: "ext-d",
        first: 0x2B740,
        last: 0x2B81F,
    },
    CjkChunk {
        id: "ext-e",
        first: 0x2B820,
        last: 0x2CEAF,
    },
    CjkChunk {
        id: "ext-f",
        first: 0x2CEB0,
        last: 0x2EBEF,
    },
    CjkChunk {
        id: "ext-g",
        first: 0x2EBF0,
        last: 0x2EE5F,
    },
    CjkChunk {
        id: "ext-sup",
        first: 0x2F800,
        last: 0x2FA1F,
    }, // Ideographs Supplement
];

/// The small CJK-adjacent ranges that ship **in the core subset** on every
/// platform (chunk ids resolved through the table; the frequency list adds
/// the common ideographs on top). Keeping these in the core costs little and
/// removes a round trip for the vast majority of Japanese/Korean punctuation
/// and kana traffic.
pub const CORE_STATIC_IDS: &[&str] = &[
    "radicals",
    "kangxi",
    "punct-kana",
    "hangul-compat-jamo",
    "strokes",
    "enclosed",
    "compat",
    "vertical",
    "compat-forms",
    "small-forms",
    "halfwidth",
];

pub use set::{CjkChunkSource, CjkFontSet, CjkGlyph, CjkSlot, CjkSnapshot};

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, clippy::integer_division)]
    use super::*;

    #[test]
    fn main_ideographs_map_to_the_quartered_chunks() {
        assert_eq!(chunk_for(0x4E00).expect("chunk").id, "ideographs-1");
        assert_eq!(chunk_for(0x51FF).expect("chunk").id, "ideographs-1");
        assert_eq!(chunk_for(0x5200).expect("chunk").id, "ideographs-2");
        assert_eq!(chunk_for(0x5800).expect("chunk").id, "ideographs-3");
        assert_eq!(chunk_for(0x5F00).expect("chunk").id, "ideographs-4");
        assert_eq!(chunk_for(0x9FFF).expect("chunk").id, "ideographs-4");
    }

    #[test]
    fn kana_and_extension_blocks() {
        assert_eq!(chunk_for(0x3042).expect("chunk").id, "punct-kana"); // あ
        assert_eq!(chunk_for(0x4E8C).expect("chunk").id, "ideographs-1"); // 二
        assert_eq!(chunk_for(0x20000).expect("chunk").id, "ext-b");
        assert_eq!(chunk_for(0x2EBEF).expect("chunk").id, "ext-f");
        assert_eq!(chunk_for(0x2F999).expect("chunk").id, "ext-sup");
    }

    #[test]
    fn hangul_ranges_are_addressable() {
        assert_eq!(chunk_for(0x1100).expect("chunk").id, "jamo"); // ᄀ
        assert_eq!(chunk_for(0x3131).expect("chunk").id, "hangul-compat-jamo"); // ㄱ
        assert_eq!(chunk_for(0xAC00).expect("chunk").id, "hangul-1"); // 가
        assert_eq!(chunk_for(0xC3FF).expect("chunk").id, "hangul-2");
        assert_eq!(chunk_for(0xD7A3).expect("chunk").id, "hangul-4"); // 힣
        assert_eq!(chunk_for(0xA960).expect("chunk").id, "jamo-ext-a");
        assert_eq!(chunk_for(0xD7B0).expect("chunk").id, "jamo-ext-b");
    }

    #[test]
    fn small_cjk_blocks_are_addressable() {
        assert_eq!(chunk_for(0x2E80).expect("chunk").id, "radicals");
        assert_eq!(chunk_for(0x2F00).expect("chunk").id, "kangxi");
        assert_eq!(chunk_for(0x3000).expect("chunk").id, "punct-kana");
        assert_eq!(chunk_for(0x3200).expect("chunk").id, "enclosed");
        assert_eq!(chunk_for(0x3300).expect("chunk").id, "compat");
        assert_eq!(chunk_for(0xFE10).expect("chunk").id, "vertical");
        assert_eq!(chunk_for(0xFE50).expect("chunk").id, "small-forms");
        assert_eq!(chunk_for(0xFF01).expect("chunk").id, "halfwidth");
    }

    #[test]
    fn non_cjk_is_none() {
        assert_eq!(chunk_for(0x41), None); // 'A'
        assert_eq!(chunk_for(0x0), None);
        assert_eq!(chunk_for(0x10FFFF), None);
        assert_eq!(chunk_for(0x600), None); // Arabic
        assert_eq!(chunk_for(0x3100), None); // unpadded Latin gap
        assert_eq!(chunk_for(0xD7A4), None); // just past the syllables
    }

    #[test]
    fn is_cjk_matches_chunk_for() {
        assert!(is_cjk(0x4E00));
        assert!(is_cjk(0x3042));
        assert!(is_cjk(0xAC00));
        assert!(!is_cjk(0x41));
    }

    #[test]
    fn chunks_are_sorted_and_disjoint() {
        let mut prev_last = 0u32;
        for c in CHUNKS {
            assert!(c.first <= c.last, "{} out of order", c.id);
            assert!(c.first > prev_last, "{} overlaps", c.id);
            prev_last = c.last;
        }
    }

    #[test]
    fn chunk_ids_are_unique_and_lookup_works() {
        let mut seen = std::collections::BTreeSet::new();
        for c in CHUNKS {
            assert!(seen.insert(c.id), "duplicate id {}", c.id);
            assert_eq!(chunk_by_id(c.id), Some(c));
        }
        assert_eq!(chunk_by_id("no-such-chunk"), None);
        assert_eq!(chunk_by_id(CORE_ID), None, "core is a reserved id");
    }

    #[test]
    fn core_static_ids_are_known_chunks() {
        for id in CORE_STATIC_IDS {
            assert!(chunk_by_id(id).is_some(), "unknown core id {id}");
        }
    }
}
