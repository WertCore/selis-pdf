//! CJK fallback chunk model (SL-3.FONT.10).
//!
//! Noto CJK is far too large to bundle in the 3 MB WASM budget, so the web
//! shell ships a subsetted core and **lazy-loads additional ranges as
//! separate chunks**. This module owns the font-side of that split: the
//! Unicode range → chunk table. The shell asks [`chunk_for`] when a code
//! point has no glyph in the resident set; the chunk URL is
//! `cjk/<chunk-id>.woff2`, fetched on demand and cached (renders `notdef`,
//! then repaints — the viewer never blocks on the fetch).
//!
//! The ranges follow the Unicode CJK block boundaries; the main Ideographs
//! block (U+4E00–U+9FFF) is split into quarters for incremental downloads.

/// A lazy-loaded CJK font chunk: a contiguous Unicode range within the
/// subsetted Noto CJK core set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CjkChunk {
    /// The chunk identifier — the shell's load key (`cjk/<id>.woff2`).
    pub id: &'static str,
    /// The first code point (inclusive).
    pub first: u32,
    /// The last code point (inclusive).
    pub last: u32,
}

/// The CJK chunk covering a code point, or `None` for non-CJK scripts.
///
/// Deterministic (ADR-P0012): the same code point always maps to the same
/// chunk.
#[must_use]
pub fn chunk_for(code: u32) -> Option<&'static CjkChunk> {
    CHUNKS.iter().find(|c| code >= c.first && code <= c.last)
}

/// Whether a code point is inside the CJK set (any chunk).
#[must_use]
pub fn is_cjk(code: u32) -> bool {
    chunk_for(code).is_some()
}

/// The full chunk table, in code-point order.
///
/// The main Ideographs block is quartered (U+4E00–U+51FF, U+5200–U+57FF,
/// U+5800–U+5EFF, U+5F00–U+9FFF) so a document that only uses a slice of the
/// block loads a fraction of it.
pub const CHUNKS: &[CjkChunk] = &[
    CjkChunk {
        id: "punct-kana",
        first: 0x3000,
        last: 0x30FF,
    }, // CJK punct + hiragana/katakana
    CjkChunk {
        id: "compat-punct",
        first: 0x31C0,
        last: 0x31EF,
    }, // CJK strokes
    CjkChunk {
        id: "ext-a",
        first: 0x3400,
        last: 0x4DBF,
    },
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
        id: "compat-ideographs",
        first: 0xF900,
        last: 0xFAFF,
    },
    CjkChunk {
        id: "halfwidth",
        first: 0xFF00,
        last: 0xFFEF,
    },
    CjkChunk {
        id: "ext-b",
        first: 0x20000,
        last: 0x2A6DF,
    },
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
];

#[cfg(test)]
mod tests {
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
    }

    #[test]
    fn non_cjk_is_none() {
        assert_eq!(chunk_for(0x41), None); // 'A'
        assert_eq!(chunk_for(0x0), None);
        assert_eq!(chunk_for(0x10FFFF), None);
    }

    #[test]
    fn is_cjk_matches_chunk_for() {
        assert!(is_cjk(0x4E00));
        assert!(is_cjk(0x3042));
        assert!(!is_cjk(0x41));
    }

    #[test]
    fn chunks_are_sorted_and_contiguous() {
        let mut prev_last = 0u32;
        for c in CHUNKS {
            assert!(c.first <= c.last, "{} out of order", c.id);
            assert!(c.first > prev_last, "{} overlaps", c.id);
            prev_last = c.last;
        }
    }
}
