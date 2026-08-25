//! Bidi and RTL (SL-3.SHAPE.02) — UAX #9 via `unicode-bidi`.
//!
//! Resolves the paragraph direction (first-strong), computes the per-character
//! bidi levels, and reorders runs into visual order. The mirroring table
//! implements Rule L4 (the mirrored glyph pairs); `unicode-bidi` deliberately
//! leaves L4 to the engine.

use unicode_bidi::{BidiInfo, Level};

/// The resolved base direction of a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseDirection {
    /// Left-to-right.
    LeftToRight,
    /// Right-to-left.
    RightToLeft,
}

/// A visual run: a byte range of the logical text rendered at one level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisualRun {
    /// The byte offset of the run in the logical text.
    pub start: usize,
    /// The byte offset just past the run.
    pub end: usize,
    /// The resolved bidi level of the run (0 = LTR, odd = RTL).
    pub level: u8,
}

impl VisualRun {
    /// Whether the run is right-to-left.
    #[must_use]
    pub fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }
}

/// The bidi level for a base direction.
fn level_for(direction: BaseDirection) -> Level {
    match direction {
        BaseDirection::LeftToRight => Level::ltr(),
        BaseDirection::RightToLeft => Level::rtl(),
    }
}

/// Detect the paragraph direction with the first-strong heuristic (UAX #9
/// P2/P3): the first strong left-to-right or right-to-left character decides;
/// a paragraph with no strong character is left-to-right.
#[must_use]
pub fn paragraph_direction(text: &str) -> BaseDirection {
    for ch in text.chars() {
        match unicode_bidi::bidi_class(ch) {
            unicode_bidi::BidiClass::L => return BaseDirection::LeftToRight,
            unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL => {
                return BaseDirection::RightToLeft
            }
            _ => {}
        }
    }
    BaseDirection::LeftToRight
}

/// The resolved bidi level of every character (UAX #9), in logical order.
///
/// Level 0 is left-to-right, odd levels are right-to-left.
#[must_use]
pub fn bidi_levels(text: &str, direction: BaseDirection) -> Vec<u8> {
    let bidi = BidiInfo::new(text, Some(level_for(direction)));
    bidi.levels.iter().map(|l| l.number()).collect()
}

/// The visual-order runs of a paragraph (UAX #9 L1–L2).
///
/// Each run is a contiguous byte range of the *logical* text, in the order it
/// should be drawn.
#[must_use]
pub fn visual_runs(text: &str, direction: BaseDirection) -> Vec<VisualRun> {
    let bidi = BidiInfo::new(text, Some(level_for(direction)));
    // Build logical runs from the per-char levels.
    let mut logical: Vec<(u8, usize, usize)> = Vec::new(); // (level, start, end)
    let mut current: Option<(u8, usize, usize)> = None;
    let mut char_n = 0usize;
    let mut byte = 0usize;
    for ch in text.chars() {
        let level = bidi.levels.get(char_n).map(|l| l.number()).unwrap_or(0);
        let end = byte.saturating_add(ch.len_utf8());
        match current {
            Some((cur, s, _)) if cur == level => current = Some((cur, s, end)),
            Some((cur, s, _)) => {
                logical.push((cur, s, byte));
                current = Some((level, byte, end));
            }
            None => current = Some((level, byte, end)),
        }
        char_n = char_n.saturating_add(1);
        byte = end;
    }
    if let Some((cur, s, e)) = current {
        logical.push((cur, s, e));
    }
    if logical.is_empty() {
        return Vec::new();
    }

    // UAX #9 L2: reverse contiguous sequences of runs at level >= L, from the
    // highest level down to 1.
    let max_level = logical.iter().map(|r| r.0).max().unwrap_or(0);
    let mut runs = logical;
    let mut level = max_level;
    while level >= 1 {
        let mut i = 0usize;
        while i < runs.len() {
            if runs.get(i).is_some_and(|r| r.0 >= level) {
                let start = i;
                while i < runs.len() && runs.get(i).is_some_and(|r| r.0 >= level) {
                    i = i.saturating_add(1);
                }
                // Reverse the segment in place by pairwise swaps.
                let mut a = start;
                let mut b = i.saturating_sub(1);
                while a < b {
                    runs.swap(a, b);
                    a = a.saturating_add(1);
                    b = b.saturating_sub(1);
                }
            } else {
                i = i.saturating_add(1);
            }
        }
        level = level.saturating_sub(1);
    }
    runs.iter()
        .map(|&(level, start, end)| VisualRun { start, end, level })
        .collect()
}

/// The reordered (visual) text of a paragraph (UAX #9), for extraction.
#[must_use]
pub fn reorder_text(text: &str, direction: BaseDirection) -> String {
    let bidi = BidiInfo::new(text, Some(level_for(direction)));
    if let Some(para) = bidi.paragraphs.first() {
        let line = 0..text.len();
        bidi.reorder_line(para, line).into_owned()
    } else {
        text.to_string()
    }
}

/// The Unicode Bidi Mirroring glyph mapping (UAX #9 Rule L4).
///
/// Mirrored characters — brackets, angle brackets, relation signs — swap for
/// their mirror image in right-to-left context.
#[must_use]
pub fn mirrored_glyph(cp: u32) -> Option<u32> {
    const MIRRORS: &[(u32, u32)] = &[
        (0x0028, 0x0029), // (
        (0x0029, 0x0028),
        (0x003C, 0x003E), // <
        (0x003E, 0x003C),
        (0x005B, 0x005D), // [
        (0x005D, 0x005B),
        (0x007B, 0x007D), // {
        (0x007D, 0x007B),
        (0x00AB, 0x00BB), // «
        (0x00BB, 0x00AB),
        (0x2039, 0x203A), // ‹
        (0x203A, 0x2039),
        (0x2045, 0x2046), // ⁅
        (0x2046, 0x2045),
        (0x207D, 0x207E), // ⁽
        (0x207E, 0x207D),
        (0x2208, 0x220B), // ∈
        (0x220B, 0x2208),
        (0x2215, 0x29F5), // ∕
        (0x29F5, 0x2215),
        (0x223C, 0x223D), // ∼
        (0x223D, 0x223C),
        (0x2243, 0x22CD), // ≃
        (0x22CD, 0x2243),
        (0x2252, 0x2253), // ≒
        (0x2253, 0x2252),
        (0x2254, 0x2255), // ≔
        (0x2255, 0x2254),
        (0x2264, 0x2265), // ≤
        (0x2265, 0x2264),
        (0x2266, 0x2267), // ≦
        (0x2267, 0x2266),
        (0x2268, 0x2269), // ≨
        (0x2269, 0x2268),
        (0x226A, 0x226B), // ≪
        (0x226B, 0x226A),
        (0x226E, 0x226F), // ≮
        (0x226F, 0x226E),
        (0x2270, 0x2271), // ≰
        (0x2271, 0x2270),
        (0x2272, 0x2273), // ≲
        (0x2273, 0x2272),
        (0x2274, 0x2275), // ≴
        (0x2275, 0x2274),
        (0x2276, 0x2277), // ≶
        (0x2277, 0x2276),
        (0x2278, 0x2279), // ≸
        (0x2279, 0x2278),
        (0x227A, 0x227B), // ≺
        (0x227B, 0x227A),
        (0x227C, 0x227D), // ≼
        (0x227D, 0x227C),
        (0x227E, 0x227F), // ≾
        (0x227F, 0x227E),
        (0x2280, 0x2281), // ⊀
        (0x2281, 0x2280),
        (0x2282, 0x2283), // ⊂
        (0x2283, 0x2282),
        (0x2284, 0x2285), // ⊄
        (0x2285, 0x2284),
        (0x2286, 0x2287), // ⊆
        (0x2287, 0x2286),
        (0x2288, 0x2289), // ⊈
        (0x2289, 0x2288),
        (0x228A, 0x228B), // ⊊
        (0x228B, 0x228A),
        (0x228F, 0x2290), // ⊏
        (0x2290, 0x228F),
        (0x2291, 0x2292), // ⊑
        (0x2292, 0x2291),
        (0x2298, 0x29B8), // ⊘
        (0x29B8, 0x2298),
        (0x22A2, 0x22A3), // ⊢
        (0x22A3, 0x22A2),
        (0x22A6, 0x2ADE), // ⊦
        (0x2ADE, 0x22A6),
        (0x22A8, 0x2AE4), // ⊨
        (0x2AE4, 0x22A8),
        (0x22A9, 0x2AE3), // ⊩
        (0x2AE3, 0x22A9),
        (0x22AB, 0x2AE5), // ⊫
        (0x2AE5, 0x22AB),
        (0x22B0, 0x22B1), // ⊰
        (0x22B1, 0x22B0),
        (0x22B2, 0x22B3), // ⊲
        (0x22B3, 0x22B2),
        (0x22B4, 0x22B5), // ⊴
        (0x22B5, 0x22B4),
        (0x22B6, 0x22B7), // ⊶
        (0x22B7, 0x22B6),
        (0x22B8, 0x22B9), // ⊸
        (0x22B9, 0x22B8),
        (0x22C9, 0x22CA), // ⋉
        (0x22CA, 0x22C9),
        (0x22CB, 0x22CC), // ⋋
        (0x22CC, 0x22CB),
        (0x22D0, 0x22D1), // ⋐
        (0x22D1, 0x22D0),
        (0x22D6, 0x22D7), // ⋖
        (0x22D7, 0x22D6),
        (0x22D8, 0x22D9), // ⋘
        (0x22D9, 0x22D8),
        (0x22DA, 0x22DB), // ⋚
        (0x22DB, 0x22DA),
        (0x22DC, 0x22DD), // ⋜
        (0x22DD, 0x22DC),
        (0x22DE, 0x22DF), // ⋞
        (0x22DF, 0x22DE),
        (0x22E0, 0x22E1), // ⋠
        (0x22E1, 0x22E0),
        (0x22E2, 0x22E3), // ⋢
        (0x22E3, 0x22E2),
        (0x22E4, 0x22E5), // ⋤
        (0x22E5, 0x22E4),
        (0x22E6, 0x22E7), // ⋦
        (0x22E7, 0x22E6),
        (0x22E8, 0x22E9), // ⋨
        (0x22E9, 0x22E8),
        (0x22EA, 0x22EB), // ⋪
        (0x22EB, 0x22EA),
        (0x22EC, 0x22ED), // ⋬
        (0x22ED, 0x22EC),
        (0x22F0, 0x22F1), // ⋰
        (0x22F1, 0x22F0),
        (0x2308, 0x2309), // ⌈
        (0x2309, 0x2308),
        (0x230A, 0x230B), // ⌊
        (0x230B, 0x230A),
        (0x2329, 0x232A), // 〈
        (0x232A, 0x2329),
        (0x2768, 0x2769), // ❨
        (0x2769, 0x2768),
        (0x276A, 0x276B), // ❪
        (0x276B, 0x276A),
        (0x276C, 0x276D), // ❬
        (0x276D, 0x276C),
        (0x276E, 0x276F), // ❮
        (0x276F, 0x276E),
        (0x2770, 0x2771), // ❰
        (0x2771, 0x2770),
        (0x2772, 0x2773), // ❲
        (0x2773, 0x2772),
        (0x2774, 0x2775), // ❴
        (0x2775, 0x2774),
        (0x27C5, 0x27C6), // ⟅
        (0x27C6, 0x27C5),
        (0x27E6, 0x27E7), // ⟦
        (0x27E7, 0x27E6),
        (0x27E8, 0x27E9), // ⟨
        (0x27E9, 0x27E8),
        (0x27EA, 0x27EB), // ⟪
        (0x27EB, 0x27EA),
        (0x27EC, 0x27ED), // ⟬
        (0x27ED, 0x27EC),
        (0x27EE, 0x27EF), // ⟮
        (0x27EF, 0x27EE),
        (0x2983, 0x2984), // ⦃
        (0x2984, 0x2983),
        (0x2985, 0x2986), // ⦅
        (0x2986, 0x2985),
        (0x2987, 0x2988), // ⦇
        (0x2988, 0x2987),
        (0x2989, 0x298A), // ⦉
        (0x298A, 0x2989),
        (0x298B, 0x298C), // ⦋
        (0x298C, 0x298B),
        (0x298D, 0x2990), // ⦍
        (0x2990, 0x298D),
        (0x298F, 0x298E), // ⦏
        (0x298E, 0x298F),
        (0x2991, 0x2992), // ⦑
        (0x2992, 0x2991),
        (0x2993, 0x2994), // ⦓
        (0x2994, 0x2993),
        (0x2995, 0x2996), // ⦕
        (0x2996, 0x2995),
        (0x2997, 0x2998), // ⦗
        (0x2998, 0x2997),
        (0x29D8, 0x29D9), // ⧘
        (0x29D9, 0x29D8),
        (0x29DA, 0x29DB), // ⧚
        (0x29DB, 0x29DA),
        (0x29FC, 0x29FD), // ⧼
        (0x29FD, 0x29FC),
        (0x2A02, 0x2A01), // ⨂
        (0x2A01, 0x2A02),
        (0x2A06, 0x2A05), // ⨆
        (0x2A05, 0x2A06),
        (0x2A0C, 0x2A0D), // ⨌
        (0x2A0D, 0x2A0C),
        (0x2A2E, 0x2A2F), // ⨮
        (0x2A2F, 0x2A2E),
        (0x2A34, 0x2A35), // ⨴
        (0x2A35, 0x2A34),
        (0x2A3C, 0x2A3D), // ⨼
        (0x2A3D, 0x2A3C),
        (0x2A64, 0x2A65), // ⩤
        (0x2A65, 0x2A64),
        (0x2A79, 0x2A7A), // ⩹
        (0x2A7A, 0x2A79),
        (0x2A7D, 0x2A7E), // ⩽
        (0x2A7E, 0x2A7D),
        (0x2A7F, 0x2A80), // ⩿
        (0x2A80, 0x2A7F),
        (0x2A81, 0x2A82), // ⪁
        (0x2A82, 0x2A81),
        (0x2A83, 0x2A84), // ⪃
        (0x2A84, 0x2A83),
        (0x2A8B, 0x2A8C), // ⪋
        (0x2A8C, 0x2A8B),
        (0x2A91, 0x2A92), // ⪑
        (0x2A92, 0x2A91),
        (0x2A93, 0x2A94), // ⪓
        (0x2A94, 0x2A93),
        (0x2A97, 0x2A98), // ⪗
        (0x2A98, 0x2A97),
        (0x2A99, 0x2A9A), // ⪙
        (0x2A9A, 0x2A99),
        (0x2A9B, 0x2A9C), // ⪛
        (0x2A9C, 0x2A9B),
        (0x2AA1, 0x2AA2), // ⪡
        (0x2AA2, 0x2AA1),
        (0x2AA6, 0x2AA7), // ⪦
        (0x2AA7, 0x2AA6),
        (0x2AA8, 0x2AA9), // ⪨
        (0x2AA9, 0x2AA8),
        (0x2AAA, 0x2AAB), // ⪪
        (0x2AAB, 0x2AAA),
        (0x2AAC, 0x2AAD), // ⪬
        (0x2AAD, 0x2AAC),
        (0x2AAF, 0x2AB0), // ⪯
        (0x2AB0, 0x2AAF),
        (0x2AB3, 0x2AB4), // ⪳
        (0x2AB4, 0x2AB3),
        (0x2ABB, 0x2ABC), // ⪻
        (0x2ABC, 0x2ABB),
        (0x2ABD, 0x2ABE), // ⪽
        (0x2ABE, 0x2ABD),
        (0x2ABF, 0x2AC0), // ⪿
        (0x2AC0, 0x2ABF),
        (0x2AC1, 0x2AC2), // ⫁
        (0x2AC2, 0x2AC1),
        (0x2AC3, 0x2AC4), // ⫃
        (0x2AC4, 0x2AC3),
        (0x2AC5, 0x2AC6), // ⫅
        (0x2AC6, 0x2AC5),
        (0x2ACD, 0x2ACE), // ⫍
        (0x2ACE, 0x2ACD),
        (0x2ACF, 0x2AD0), // ⫏
        (0x2AD0, 0x2ACF),
        (0x2AD1, 0x2AD2), // ⫑
        (0x2AD2, 0x2AD1),
        (0x2AD3, 0x2AD4), // ⫓
        (0x2AD4, 0x2AD3),
        (0x2AD5, 0x2AD6), // ⫕
        (0x2AD6, 0x2AD5),
        (0x2AEC, 0x2AED), // ⫬
        (0x2AED, 0x2AEC),
        (0x2AF7, 0x2AF8), // ⫷
        (0x2AF8, 0x2AF7),
        (0x2AF9, 0x2AFA), // ⫹
        (0x2AFA, 0x2AF9),
        (0x2E02, 0x2E03), // ⸂
        (0x2E03, 0x2E02),
        (0x2E04, 0x2E05), // ⸄
        (0x2E05, 0x2E04),
        (0x2E09, 0x2E0A), // ⸉
        (0x2E0A, 0x2E09),
        (0x2E0C, 0x2E0D), // ⸌
        (0x2E0D, 0x2E0C),
        (0x2E1C, 0x2E1D), // ⸜
        (0x2E1D, 0x2E1C),
        (0x2E20, 0x2E21), // ⸠
        (0x2E21, 0x2E20),
        (0x2E22, 0x2E23), // ⸢
        (0x2E23, 0x2E22),
        (0x2E24, 0x2E25), // ⸤
        (0x2E25, 0x2E24),
        (0x2E26, 0x2E27), // ⸦
        (0x2E27, 0x2E26),
        (0x2E28, 0x2E29), // ⸨
        (0x2E29, 0x2E28),
        (0x3008, 0x3009), // 〈
        (0x3009, 0x3008),
        (0x300A, 0x300B), // 《
        (0x300B, 0x300A),
        (0x300C, 0x300D), // 「
        (0x300D, 0x300C),
        (0x300E, 0x300F), // 『
        (0x300F, 0x300E),
        (0x3010, 0x3011), // 【
        (0x3011, 0x3010),
        (0x3014, 0x3015), // 〔
        (0x3015, 0x3014),
        (0x3016, 0x3017), // 〖
        (0x3017, 0x3016),
        (0x3018, 0x3019), // 〘
        (0x3019, 0x3018),
        (0x301A, 0x301B), // 〚
        (0x301B, 0x301A),
        (0xFE59, 0xFE5A), // ﹙
        (0xFE5A, 0xFE59),
        (0xFE5B, 0xFE5C), // ﹛
        (0xFE5C, 0xFE5B),
        (0xFE5D, 0xFE5E), // ﹝
        (0xFE5E, 0xFE5D),
        (0xFF08, 0xFF09), // （
        (0xFF09, 0xFF08),
        (0xFF1C, 0xFF1E), // ＜
        (0xFF1E, 0xFF1C),
        (0xFF3B, 0xFF3D), // ［
        (0xFF3D, 0xFF3B),
        (0xFF5B, 0xFF5D), // ｛
        (0xFF5D, 0xFF5B),
        (0xFF5F, 0xFF60), // ｟
        (0xFF60, 0xFF5F),
        (0xFF62, 0xFF63), // ｢
        (0xFF63, 0xFF62),
    ];
    MIRRORS.iter().find(|(a, _)| *a == cp).map(|(_, b)| *b)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn paragraph_direction_detects_first_strong() {
        assert_eq!(paragraph_direction("Hello"), BaseDirection::LeftToRight);
        assert_eq!(paragraph_direction("שלום"), BaseDirection::RightToLeft);
        assert_eq!(paragraph_direction("مرحبا"), BaseDirection::RightToLeft);
        // No strong char → LTR.
        assert_eq!(paragraph_direction("123!?"), BaseDirection::LeftToRight);
        // Mixed: the first strong char decides.
        assert_eq!(paragraph_direction("abc שלום"), BaseDirection::LeftToRight);
        assert_eq!(paragraph_direction("שלום abc"), BaseDirection::RightToLeft);
    }

    #[test]
    fn rtl_text_is_reordered_into_visual_order() {
        // "ابج" (Arabic, RTL) in an RTL paragraph reverses to "جبا".
        assert_eq!(reorder_text("ابج", BaseDirection::RightToLeft), "جبا");
        // A pure-LTR run keeps its order (level 2, reversed twice per L2).
        assert_eq!(reorder_text("ABC", BaseDirection::RightToLeft), "ABC");
        // Pure LTR stays.
        assert_eq!(reorder_text("ABC", BaseDirection::LeftToRight), "ABC");
    }

    #[test]
    fn visual_runs_are_reversed_for_rtl() {
        let runs = visual_runs("ابج", BaseDirection::RightToLeft);
        // The single run is RTL (odd level) and spans the whole text.
        assert_eq!(runs.len(), 1);
        assert!(runs[0].is_rtl());
        assert_eq!(runs[0].start, 0);
        assert_eq!(runs[0].end, 6); // 3 chars × 2 UTF-8 bytes
    }

    #[test]
    fn mirroring_swaps_brackets() {
        assert_eq!(mirrored_glyph(0x0028), Some(0x0029)); // ( → )
        assert_eq!(mirrored_glyph(0x0029), Some(0x0028)); // ) → (
        assert_eq!(mirrored_glyph(0x003C), Some(0x003E)); // < → >
        assert_eq!(mirrored_glyph(0x3008), Some(0x3009)); // 〈 → 〉
        assert_eq!(mirrored_glyph(0x0041), None); // 'A' is not mirrored
    }
}
