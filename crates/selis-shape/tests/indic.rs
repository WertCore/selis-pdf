//! Indic complex-script shaping validation (SL-3.SHAPE.04).
//!
//! Devanagari, Tamil, and Bengali through the [`SwashShaper`] with committed
//! OFL test fonts (`corpus/fixtures/fonts/NotoSans*-Regular.ttf` — unhinted
//! statics at 1000 upm, so shaping sees the same outlines every renderer
//! uses). Two layers:
//!
//! 1. **Headline structural tests** — one per construct (pre-base matra,
//!    conjunct, `reph`, two-part vowel, ligature, `nukta`): glyph order,
//!    merge behaviour, and cluster shape. Glyph identities are never
//!    hardcoded: expectations compare against the font's own cmap.
//! 2. **`indic_harfbuzz_parity`** — every fixture string the corpus
//!    generator emits, pinned as `(gid, cluster, advance, x/y offset)` and
//!    verified glyph-for-glyph against HarfBuzz 12.1 (maintainer-side
//!    `uharfbuzz` run, 2026-09; the harness cannot depend on HarfBuzz —
//!    `rustybuzz` is denied by the supply-chain gate, ADR-P0021 — so the
//!    table is committed data, and any drift fails loudly here instead of
//!    silently entering the corpus).
//!
//! Documented swash-vs-HarfBuzz divergences (all shaping-side only; the PDF
//! replay path consumes glyph ids either way):
//!
//! * **cluster merge targets** — swash merges a conjunct to the
//!   syllable-start byte (`स्त` → all cluster 6); HarfBuzz merges to the
//!   base consonant (`त` → cluster 12). Same glyphs, same positions.
//! * **Bengali/Devanagari pre-base matra variants** — swash keeps the
//!   isolated form (`ে` → 66, `ँ` → 101); HarfBuzz substitutes the
//!   contextual form (`ে` → 398, `ँ` → 34). Same advance; the raster impact
//!   is measured by the corpus sweep, not hidden.
//! * **Bengali subjoined-mark y offsets** — swash negates them relative to
//!   HarfBuzz (`-125` vs `+125` on `স্ব`-class marks). Same x; the raster
//!   impact is measured by the corpus sweep, not hidden.
//!
//! Tamil `ி` needs no glyph reordering: Noto uses the split-matra model
//! (the matra-form glyph carries the left extension), and HarfBuzz agrees
//! glyph-for-glyph (`கி` → `[க, matra-form]` in both).

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use selis_shape::{PositionedGlyph, Shaper, ShapingParams, SwashShaper};
use skrifa::{
    instance::{LocationRef, Size},
    FontRef, MetadataProvider,
};

const DEVA_TTF: &[u8] =
    include_bytes!("../../../corpus/fixtures/fonts/NotoSansDevanagari-Regular.ttf");
const TAML_TTF: &[u8] = include_bytes!("../../../corpus/fixtures/fonts/NotoSansTamil-Regular.ttf");
const BENG_TTF: &[u8] =
    include_bytes!("../../../corpus/fixtures/fonts/NotoSansBengali-Regular.ttf");

/// A shaper over one fixture font (1000 upm by construction, asserted).
struct Fixture {
    data: &'static [u8],
}

impl Fixture {
    fn deva() -> Self {
        Self::for_font(DEVA_TTF)
    }

    fn tamil() -> Self {
        Self::for_font(TAML_TTF)
    }

    fn bengali() -> Self {
        Self::for_font(BENG_TTF)
    }

    fn for_font(data: &'static [u8]) -> Self {
        let font = FontRef::new(data).expect("test font parses");
        let upem = font
            .metrics(Size::unscaled(), LocationRef::default())
            .units_per_em;
        // The committed fonts are 1000-upm by construction (verified at
        // vendoring); a different upm would silently rescale every shaping
        // expectation below, so pin it.
        assert_eq!(upem, 1000, "fixture font units-per-em");
        Self { data }
    }

    /// Shape `text` as one item (all fixture strings are single-script, one
    /// run — asserted, so the itemiser is under test here too).
    fn shape(&self, text: &str) -> Vec<PositionedGlyph> {
        let runs = selis_shape::script_runs(text);
        assert_eq!(
            runs.len(),
            1,
            "{text:?}: expected one script run, got {runs:?}"
        );
        let params = ShapingParams {
            text,
            font_data: self.data,
            font_size: 1000.0,
            script: runs[0].script,
            language: None,
            features: &[],
        };
        let shaped = SwashShaper.shape(&params).expect("shaping succeeds");
        assert!(
            !shaped.glyphs.is_empty(),
            "shaping {text:?} produced no glyphs"
        );
        shaped.glyphs
    }

    /// The font's glyph id for `ch` (0 when uncovered).
    fn gid(&self, ch: char) -> u16 {
        FontRef::new(self.data)
            .expect("font parses")
            .charmap()
            .map(ch)
            .map(|g| u16::try_from(g.to_u32()).unwrap_or(u16::MAX))
            .unwrap_or(0)
    }

    /// Structural sanity for any shaped string: no `.notdef`, clusters in
    /// range and monotonically non-decreasing (HarfBuzz merges clusters on
    /// reorder — swash must match, or the PDF backend's cluster bookkeeping
    /// diverges). (No glyph-count rule: fonts legitimately decompose one
    /// char into several glyphs, e.g. `ई` → base + mark.)
    fn assert_sane(&self, text: &str, shaped: &[PositionedGlyph]) {
        let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
        let mut prev = 0u32;
        for (n, g) in shaped.iter().enumerate() {
            assert_ne!(g.glyph_id, 0, "{text:?}: .notdef in output");
            assert!(g.cluster < len, "{text:?}: cluster out of range: {g:?}");
            assert!(
                g.cluster >= prev,
                "{text:?}: cluster went backwards at glyph {n} ({prev} -> {})",
                g.cluster
            );
            prev = g.cluster;
        }
    }

    /// The font covers `ch`: shaped in isolation (no cross-char merging),
    /// its own glyph (cluster 0) is present and not `.notdef`.
    fn assert_char_covered(&self, ch: char) {
        let text: String = [ch].iter().collect();
        let shaped = self.shape(&text);
        assert!(
            shaped.iter().any(|g| g.cluster == 0 && g.glyph_id != 0),
            "{text:?}: font lacks the char: {shaped:?}"
        );
    }
}

// ── Devanagari ──────────────────────────────────────────────────────────

/// `कि` (KA + vowel sign I): the matra reorders before the base. Both glyphs
/// share the syllable cluster; the base consonant keeps its cmap glyph,
/// second.
#[test]
fn deva_i_matra_reorders_before_base() {
    let f = Fixture::deva();
    let shaped = f.shape("कि");
    assert_eq!(shaped.len(), 2, "expected [matra, base], got {shaped:?}");
    assert_ne!(
        shaped[0].glyph_id,
        f.gid('क'),
        "matra renders first: {shaped:?}"
    );
    assert_eq!(shaped[1].glyph_id, f.gid('क'), "base keeps its glyph");
    assert_eq!((shaped[0].cluster, shaped[1].cluster), (0, 0));
    f.assert_sane("कि", &shaped);
}

/// `क्ष` (KA + VIRAMA + SSA): the halant sequence merges into one conjunct
/// glyph under one cluster.
#[test]
fn deva_conjunct_ksa_merges() {
    let f = Fixture::deva();
    let shaped = f.shape("क्ष");
    assert_eq!(shaped.len(), 1, "expected a conjunct, got {shaped:?}");
    assert_eq!(shaped[0].cluster, 0);
    f.assert_sane("क्ष", &shaped);
}

/// `र्क` (RA + VIRAMA + KA): `reph` — the base ka renders first and the ra
/// survives only as a zero-advance mark (no standalone RA glyph).
#[test]
fn deva_reph_consumes_ra() {
    let f = Fixture::deva();
    let shaped = f.shape("र्क");
    assert_eq!(shaped.len(), 2, "expected [base, reph], got {shaped:?}");
    assert_eq!(shaped[0].glyph_id, f.gid('क'), "base ka first");
    assert!(
        !shaped.iter().any(|g| g.glyph_id == f.gid('र')),
        "ra must not survive as its own glyph: {shaped:?}"
    );
    assert_eq!((shaped[0].cluster, shaped[1].cluster), (0, 0));
    f.assert_sane("र्क", &shaped);
}

/// `क़` (KA + NUKTA): the nukta folds into one precomposed glyph.
#[test]
fn deva_nukta_composes() {
    let f = Fixture::deva();
    let shaped = f.shape("क़");
    assert_eq!(shaped.len(), 1, "expected one glyph, got {shaped:?}");
    f.assert_sane("क़", &shaped);
}

/// Independent vowels, matras, and digits: every char covered, sane output.
#[test]
fn deva_vowels_and_matras_cover() {
    let f = Fixture::deva();
    for text in [
        "अ",
        "आ",
        "इ",
        "ई",
        "उ",
        "ऊ",
        "ए",
        "ऐ",
        "ओ",
        "औ",
        "अं",
        "अः",
        "कँ",
        "का",
        "की",
        "कु",
        "कू",
        "के",
        "कै",
        "को",
        "कौ",
        "कं",
        "कः",
        "१२३",
    ] {
        let shaped = f.shape(text);
        f.assert_sane(text, &shaped);
        for ch in text.chars() {
            f.assert_char_covered(ch);
        }
    }
}

/// Conjunct battery plus the `नमस्ते`/`दुनिया` sentences: sane output, full
/// coverage (exact sequences live in the parity table below).
#[test]
fn deva_conjunct_battery_merges() {
    let f = Fixture::deva();
    for text in ["क्त", "त्र", "ज्ञ", "स्व", "न्द", "ह्म", "स्त", "नमस्ते", "दुनिया"]
    {
        let shaped = f.shape(text);
        f.assert_sane(text, &shaped);
        for ch in text.chars() {
            f.assert_char_covered(ch);
        }
    }
}

// ── Tamil ───────────────────────────────────────────────────────────────

/// `கி` (KA + vowel sign I): the matra substitutes to its pre-base form
/// (glyph 166, not the cmap 42) but stays second — Noto Tamil carries the
/// left extension in the glyph itself (HarfBuzz agrees glyph-for-glyph).
#[test]
fn tamil_i_matra_keeps_split_model() {
    let f = Fixture::tamil();
    let shaped = f.shape("கி");
    assert_eq!(shaped.len(), 2, "expected [base, matra], got {shaped:?}");
    assert_eq!(shaped[0].glyph_id, f.gid('க'), "base keeps its glyph");
    assert_ne!(
        shaped[1].glyph_id,
        f.gid('ி'),
        "matra substitutes to its pre-base form: {shaped:?}"
    );
    assert_eq!((shaped[0].cluster, shaped[1].cluster), (0, 0));
    f.assert_sane("கி", &shaped);
}

/// `ஸ்ரீ` (SA + PULLI + RA + vowel sign II — "sri"): the ligature merges
/// four codepoints into one glyph under one cluster.
#[test]
fn tamil_sri_ligature_merges() {
    let f = Fixture::tamil();
    let shaped = f.shape("ஸ்ரீ");
    assert_eq!(shaped.len(), 1, "expected a ligature, got {shaped:?}");
    assert_eq!(shaped[0].cluster, 0);
    f.assert_sane("ஸ்ரீ", &shaped);
}

/// `க்ஷ` (KA + PULLI + SSA) merges to one glyph; `க்` keeps its pulli form.
#[test]
fn tamil_pulli_conjunct_merges() {
    let f = Fixture::tamil();
    let shaped = f.shape("க்ஷ");
    assert_eq!(shaped.len(), 1, "expected a conjunct, got {shaped:?}");
    f.assert_sane("க்ஷ", &shaped);
    let shaped = f.shape("க்");
    assert_eq!(shaped.len(), 1, "expected a pulli form, got {shaped:?}");
    f.assert_sane("க்", &shaped);
}

/// Tamil vowels, matras, and the `வணக்கம்`/`உலகம்` sentences.
#[test]
fn tamil_vowels_and_matras_cover() {
    let f = Fixture::tamil();
    for text in [
        "அ",
        "ஆ",
        "இ",
        "ஈ",
        "உ",
        "ஊ",
        "எ",
        "ஏ",
        "ஐ",
        "ஒ",
        "ஓ",
        "ஔ",
        "கா",
        "கீ",
        "கு",
        "கூ",
        "கெ",
        "கே",
        "கை",
        "கொ",
        "கோ",
        "கௌ",
        "வணக்கம்",
        "உலகம்",
    ] {
        let shaped = f.shape(text);
        f.assert_sane(text, &shaped);
        for ch in text.chars() {
            f.assert_char_covered(ch);
        }
    }
}

// ── Bengali ─────────────────────────────────────────────────────────────

/// `কি` (KA + vowel sign I): reordered first, clusters merged.
#[test]
fn bengali_i_matra_reorders_before_base() {
    let f = Fixture::bengali();
    let shaped = f.shape("কি");
    assert_eq!(shaped.len(), 2, "expected [matra, base], got {shaped:?}");
    assert_ne!(
        shaped[0].glyph_id,
        f.gid('ক'),
        "matra renders first: {shaped:?}"
    );
    assert_eq!(shaped[1].glyph_id, f.gid('ক'), "base keeps its glyph");
    assert_eq!((shaped[0].cluster, shaped[1].cluster), (0, 0));
    f.assert_sane("কি", &shaped);
}

/// `কো` written decomposed as KA + `ে` (U+09C7) + `া` (U+09BE) — three
/// chars where the precomposed form has two, by construction: the two-part
/// vowel splits into three glyphs in visual order `[e-part, ka, aa-part]`,
/// all merged to the syllable cluster.
#[test]
fn bengali_two_part_vowel_splits() {
    let f = Fixture::bengali();
    let split: String = ['ক', 'ে', 'া'].iter().collect();
    assert_eq!(split.chars().count(), 3);
    let shaped = f.shape(&split);
    assert_eq!(shaped.len(), 3, "expected the split vowel, got {shaped:?}");
    assert_eq!(shaped[1].glyph_id, f.gid('ক'), "base in the middle");
    assert!(
        shaped.iter().all(|g| g.cluster == 0),
        "split parts merge: {shaped:?}"
    );
    f.assert_sane(&split, &shaped);
}

/// `র্ক` (RA + HASANT + KA): `reph` — base first, ra only as a mark.
#[test]
fn bengali_reph_consumes_ra() {
    let f = Fixture::bengali();
    let shaped = f.shape("র্ক");
    assert_eq!(shaped.len(), 2, "expected [base, reph], got {shaped:?}");
    assert_eq!(shaped[0].glyph_id, f.gid('ক'), "base ka first");
    assert!(
        !shaped.iter().any(|g| g.glyph_id == f.gid('র')),
        "ra must not survive as its own glyph: {shaped:?}"
    );
    f.assert_sane("র্ক", &shaped);
}

/// `ক্র` (KA + HASANT + RA): the base substitutes to its `kra` form plus
/// two zero-advance `ra-phala` marks with negative offsets (decomposed,
/// not merged).
#[test]
fn bengali_ra_phala_decomposes() {
    let f = Fixture::bengali();
    let shaped = f.shape("ক্র");
    assert_eq!(shaped.len(), 3, "expected [base, mark, mark]");
    assert_ne!(
        shaped[0].glyph_id,
        f.gid('ক'),
        "base substitutes to its kra form: {shaped:?}"
    );
    for (n, g) in shaped.iter().enumerate().skip(1) {
        assert_eq!(g.x_advance, 0.0, "mark {n} has no advance: {g:?}");
        assert!(g.x_offset < 0.0, "mark {n} pulls back: {g:?}");
    }
    f.assert_sane("ক্র", &shaped);
}

/// Bengali vowels, matras, conjuncts, and the `নমস্কার`/`বিশ্ব` sentences.
#[test]
fn bengali_vowels_and_matras_cover() {
    let f = Fixture::bengali();
    for text in [
        "অ",
        "আ",
        "ই",
        "ঈ",
        "উ",
        "ঊ",
        "এ",
        "ঐ",
        "ও",
        "ঔ",
        "কা",
        "কী",
        "কু",
        "কূ",
        "কৃ",
        "কেঁ",
        "কাঁ",
        "ক্ষ",
        "ক্ত",
        "ন্ত্র",
        "জ্ঞ",
        "স্ব",
        "স্ক",
        "নমস্কার",
        "বিশ্ব",
    ] {
        let shaped = f.shape(text);
        f.assert_sane(text, &shaped);
        for ch in text.chars() {
            f.assert_char_covered(ch);
        }
    }
}

// ── Advance units (the corpus generator's `/W` source) ──────────────────

/// Shaping at the font's own units-per-em yields font-unit advances: the
/// base consonant's shaped advance matches its `hmtx` design advance, so
/// the generator can lift `/W` widths straight from shaping output.
#[test]
fn shaped_advances_are_font_units() {
    for (name, data, ch) in [
        ("deva", DEVA_TTF, 'क'),
        ("tamil", TAML_TTF, 'க'),
        ("bengali", BENG_TTF, 'ক'),
    ] {
        let f = Fixture::for_font(data);
        let text: String = [ch].iter().collect();
        let shaped = f.shape(&text);
        assert_eq!(shaped.len(), 1);
        let font = FontRef::new(data).expect("font parses");
        let gid = font.charmap().map(ch).expect("covered");
        let design = font
            .glyph_metrics(Size::unscaled(), LocationRef::default())
            .advance_width(gid)
            .expect("design advance");
        let shaped_advance = shaped[0].x_advance;
        assert!(
            (shaped_advance - design).abs() < 1.0,
            "{name}: shaped advance {shaped_advance} != design advance {design}"
        );
    }
}

// ── HarfBuzz parity ─────────────────────────────────────────────────────

/// Every fixture string the corpus generator emits, as
/// `(glyph id, cluster byte, x advance, x offset, y offset)` per glyph —
/// verified against HarfBuzz 12.1 (see the module docs for method and the
/// three documented divergence classes, marked below).
///
/// Glyph ids pin the committed fonts (they change with the font — that is
/// the point: a font update must consciously re-verify shaping).
#[allow(clippy::too_many_lines)]
const PARITY: &[(&str, &[(u16, u32, i32, i32, i32)])] = &[
    // Divergence legend (swash vs HarfBuzz 12.1 — shaping-side only):
    // * `cluster swash=A hb=B` — swash merges the conjunct to the
    //   syllable-start byte; HarfBuzz merges to the base consonant. Same
    //   glyphs, same positions.
    // * `gid swash=G hb=H` — swash keeps the isolated matra form where
    //   HarfBuzz substitutes the contextual form. Same advance.
    // * `off swash=(X,Y) hb=(X,Z)` — Bengali subjoined-mark y offsets differ
    //   in sign. Same x, same glyphs.
    // The raster impact of the gid/off classes is measured by the corpus
    // sweep (SL-3.SHAPE.04 render leg), not hidden.
    ("अ", &[(5, 0, 764, 0, 0)]),
    ("अं", &[(5, 0, 764, 0, 0), (100, 0, 0, 0, 0)]),
    ("अः", &[(5, 0, 764, 0, 0), (102, 0, 202, 0, 0)]),
    ("आ", &[(6, 0, 1022, 0, 0)]),
    ("इ", &[(7, 0, 491, 0, 0)]),
    ("ई", &[(7, 0, 491, 0, 0), (506, 0, 0, 2, 0)]),
    ("उ", &[(9, 0, 548, 0, 0)]),
    ("ऊ", &[(10, 0, 785, 0, 0)]),
    ("ए", &[(15, 0, 553, 0, 0)]),
    ("ऐ", &[(15, 0, 553, 0, 0), (40, 0, 0, 12, 0)]),
    ("ओ", &[(17, 0, 1023, 0, 0)]),
    ("औ", &[(18, 0, 1023, 0, 0)]),
    // DIVERGES from HarfBuzz (g1 gid swash=101 hb=34; g1 off swash=(0,-208) hb=(0,-221)).
    ("कँ", &[(56, 0, 768, 0, 0), (101, 0, 0, -208, 0)]),
    ("कं", &[(56, 0, 768, 0, 0), (100, 0, 0, -221, 0)]),
    ("कः", &[(56, 0, 768, 0, 0), (102, 0, 202, 0, 0)]),
    ("क़", &[(221, 0, 768, 0, 0)]),
    ("का", &[(56, 0, 768, 0, 0), (31, 0, 259, 0, 0)]),
    ("कि", &[(545, 0, 259, 0, 0), (56, 0, 768, 0, 0)]),
    ("की", &[(56, 0, 768, 0, 0), (634, 0, 259, 0, 0)]),
    ("कु", &[(56, 0, 768, 0, 0), (34, 0, 0, -221, 0)]),
    ("कू", &[(56, 0, 768, 0, 0), (35, 0, 0, -221, 0)]),
    ("के", &[(56, 0, 768, 0, 0), (40, 0, 0, -221, 0)]),
    ("कै", &[(56, 0, 768, 0, 0), (41, 0, 0, -221, 0)]),
    ("को", &[(56, 0, 768, 0, 0), (42, 0, 259, 0, 0)]),
    ("कौ", &[(56, 0, 768, 0, 0), (43, 0, 259, 0, 0)]),
    // DIVERGES from HarfBuzz (g1 cluster swash=0 hb=6).
    ("क्त", &[(232, 0, 536, 0, 0), (71, 0, 570, 0, 0)]),
    ("क्ष", &[(90, 0, 717, 0, 0)]),
    ("ज्ञ", &[(91, 0, 641, 0, 0)]),
    ("त्र", &[(304, 0, 552, 0, 0)]),
    (
        "दुनिया",
        &[
            (490, 0, 531, 0, 0),
            (545, 6, 259, 0, 0),
            (75, 6, 555, 0, 0),
            (81, 12, 580, 0, 0),
            (31, 12, 259, 0, 0),
        ],
    ),
    // DIVERGES from HarfBuzz (g3 cluster swash=6 hb=12; g4 cluster swash=6 hb=12).
    (
        "नमस्ते",
        &[
            (75, 0, 555, 0, 0),
            (80, 3, 598, 0, 0),
            (256, 6, 389, 0, 0),
            (71, 6, 570, 0, 0),
            (40, 6, 0, 0, 0),
        ],
    ),
    // DIVERGES from HarfBuzz (g1 cluster swash=0 hb=6).
    ("न्द", &[(245, 0, 309, 0, 0), (73, 0, 531, 0, 0)]),
    ("र्क", &[(56, 0, 768, 0, 0), (506, 0, 0, -221, 0)]),
    // DIVERGES from HarfBuzz (g1 cluster swash=0 hb=6).
    ("स्त", &[(256, 0, 389, 0, 0), (71, 0, 570, 0, 0)]),
    // DIVERGES from HarfBuzz (g1 cluster swash=0 hb=6).
    ("स्व", &[(256, 0, 389, 0, 0), (84, 0, 556, 0, 0)]),
    ("ह्म", &[(473, 0, 801, 0, 0)]),
    (
        "१२३",
        &[
            (114, 0, 520, 0, 0),
            (115, 3, 520, 0, 0),
            (116, 6, 520, 0, 0),
        ],
    ),
    ("অ", &[(13, 0, 893, 0, 0)]),
    ("আ", &[(14, 0, 1159, 0, 0)]),
    ("ই", &[(15, 0, 534, 0, 0)]),
    ("ঈ", &[(16, 0, 689, 0, 0)]),
    ("উ", &[(17, 0, 712, 0, 0)]),
    ("ঊ", &[(18, 0, 800, 0, 0)]),
    ("এ", &[(21, 0, 731, 0, 0)]),
    ("ঐ", &[(22, 0, 853, 0, 0)]),
    ("ও", &[(23, 0, 738, 0, 0)]),
    ("ঔ", &[(24, 0, 874, 0, 0)]),
    ("কা", &[(25, 0, 807, 0, 0), (59, 0, 266, 0, 0)]),
    (
        "কাঁ",
        &[(25, 0, 807, 0, 0), (10, 0, 0, -361, 0), (59, 0, 266, 0, 0)],
    ),
    ("কি", &[(60, 0, 266, 0, 0), (25, 0, 807, 0, 0)]),
    ("কী", &[(25, 0, 807, 0, 0), (61, 0, 266, 0, 0)]),
    ("কু", &[(25, 0, 807, 0, 0), (62, 0, 0, -393, 0)]),
    ("কূ", &[(25, 0, 807, 0, 0), (63, 0, 0, -393, 0)]),
    ("কৃ", &[(25, 0, 807, 0, 0), (64, 0, 0, -393, 0)]),
    // DIVERGES from HarfBuzz (g0 gid swash=66 hb=398).
    (
        "কেঁ",
        &[(66, 0, 361, 0, 0), (25, 0, 807, 0, 0), (10, 0, 0, -361, 0)],
    ),
    (
        "কো",
        &[(66, 0, 361, 0, 0), (25, 0, 807, 0, 0), (59, 0, 266, 0, 0)],
    ),
    (
        "কৌ",
        &[(66, 0, 361, 0, 0), (25, 0, 807, 0, 0), (72, 0, 266, 0, 0)],
    ),
    (
        "কো",
        &[(66, 0, 361, 0, 0), (25, 0, 807, 0, 0), (59, 0, 266, 0, 0)],
    ),
    (
        "কৌ",
        &[(66, 0, 361, 0, 0), (25, 0, 807, 0, 0), (72, 0, 266, 0, 0)],
    ),
    // DIVERGES from HarfBuzz (g1 cluster swash=0 hb=6; g1 off swash=(-365,-23) hb=(-365,23); g2 cluster swash=0 hb=6).
    (
        "ক্ত",
        &[
            (151, 0, 910, 0, 0),
            (320, 0, 0, -365, -23),
            (346, 0, 0, -200, 0),
        ],
    ),
    (
        "ক্র",
        &[
            (154, 0, 862, 0, 0),
            (346, 0, 0, -200, 0),
            (320, 0, 0, -370, 0),
        ],
    ),
    ("ক্ষ", &[(135, 0, 859, 0, 0)]),
    ("জ্ঞ", &[(143, 0, 939, 0, 0)]),
    // DIVERGES from HarfBuzz (g3 cluster swash=6 hb=12; g3 off swash=(-580,-125) hb=(-580,125); g4 cluster swash=6 hb=12; g4 off swash=(-263,-98) hb=(-263,98); g5 cluster swash=6 hb=12; g6 cluster swash=6 hb=12).
    (
        "নমস্কার",
        &[
            (44, 0, 602, 0, 0),
            (49, 3, 612, 0, 0),
            (250, 6, 745, 0, 0),
            (299, 6, 0, -580, -125),
            (321, 6, 0, -263, -98),
            (347, 6, 0, -250, 0),
            (59, 6, 266, 0, 0),
            (51, 18, 596, 0, 0),
        ],
    ),
    // DIVERGES from HarfBuzz (g1 cluster swash=0 hb=6; g1 off swash=(-627,136) hb=(-627,-136)).
    ("ন্ত্র", &[(230, 0, 613, 0, 0), (305, 0, 0, -627, 136)]),
    // DIVERGES from HarfBuzz (g3 off swash=(-457,-51) hb=(-457,51)).
    (
        "বিশ্ব",
        &[
            (60, 0, 266, 0, 0),
            (47, 0, 596, 0, 0),
            (245, 6, 681, 0, 0),
            (299, 6, 0, -457, -51),
            (327, 6, 0, -192, 0),
        ],
    ),
    ("র্ক", &[(25, 0, 807, 0, 0), (132, 0, 0, -361, 0)]),
    // DIVERGES from HarfBuzz (g1 cluster swash=0 hb=6; g1 off swash=(-580,-125) hb=(-580,125); g2 cluster swash=0 hb=6; g2 off swash=(-263,-98) hb=(-263,98); g3 cluster swash=0 hb=6).
    (
        "স্ক",
        &[
            (250, 0, 745, 0, 0),
            (299, 0, 0, -580, -125),
            (321, 0, 0, -263, -98),
            (347, 0, 0, -250, 0),
        ],
    ),
    // DIVERGES from HarfBuzz (g1 off swash=(-480,-125) hb=(-480,125)).
    (
        "স্ব",
        &[
            (250, 0, 645, 0, 0),
            (299, 0, 0, -480, -125),
            (346, 0, 0, -200, 0),
        ],
    ),
    ("அ", &[(6, 0, 1121, 0, 0)]),
    ("ஆ", &[(7, 0, 1313, 0, 0)]),
    ("இ", &[(8, 0, 1126, 0, 0)]),
    ("ஈ", &[(9, 0, 707, 0, 0)]),
    ("உ", &[(10, 0, 1067, 0, 0)]),
    (
        "உலகம்",
        &[
            (10, 0, 1067, 0, 0),
            (33, 3, 1013, 0, 0),
            (18, 6, 825, 0, 0),
            (88, 9, 858, 0, 0),
        ],
    ),
    ("ஊ", &[(11, 0, 1540, 0, 0)]),
    ("எ", &[(12, 0, 801, 0, 0)]),
    ("ஏ", &[(13, 0, 801, 0, 0)]),
    ("ஐ", &[(14, 0, 1022, 0, 0)]),
    ("ஒ", &[(15, 0, 985, 0, 0)]),
    ("ஓ", &[(16, 0, 985, 0, 0)]),
    ("ஔ", &[(17, 0, 2051, 0, 0)]),
    ("கா", &[(18, 0, 825, 0, 0), (41, 0, 640, 0, 0)]),
    ("கி", &[(18, 0, 825, 0, 0), (166, 0, 262, 0, 0)]),
    ("கீ", &[(101, 0, 825, 0, 0)]),
    ("கு", &[(102, 0, 1062, 0, 0)]),
    ("கூ", &[(103, 0, 1332, 0, 0)]),
    ("கெ", &[(46, 0, 901, 0, 0), (18, 0, 825, 0, 0)]),
    ("கே", &[(47, 0, 720, 0, 0), (18, 0, 825, 0, 0)]),
    ("கை", &[(48, 0, 1160, 0, 0), (18, 0, 825, 0, 0)]),
    (
        "கொ",
        &[(46, 0, 901, 0, 0), (18, 0, 825, 0, 0), (41, 0, 640, 0, 0)],
    ),
    (
        "கோ",
        &[(47, 0, 720, 0, 0), (18, 0, 825, 0, 0), (41, 0, 640, 0, 0)],
    ),
    (
        "கௌ",
        &[(46, 0, 901, 0, 0), (18, 0, 825, 0, 0), (54, 0, 1070, 0, 0)],
    ),
    ("க்", &[(77, 0, 825, 0, 0)]),
    ("க்ஷ", &[(76, 0, 1851, 0, 0)]),
    // DIVERGES from HarfBuzz (g3 cluster swash=6 hb=12).
    (
        "வணக்கம்",
        &[
            (36, 0, 1044, 0, 0),
            (24, 3, 1603, 0, 0),
            (77, 6, 825, 0, 0),
            (18, 6, 825, 0, 0),
            (88, 15, 858, 0, 0),
        ],
    ),
    ("ஸ்ரீ", &[(163, 0, 1514, 0, 0)]),
];

/// Glyph ids, clusters, advances, and offsets match the committed table
/// (HarfBuzz-verified); advances/offsets compare within one font unit.
#[test]
fn indic_harfbuzz_parity() {
    let deva = Fixture::deva();
    let tamil = Fixture::tamil();
    let bengali = Fixture::bengali();
    for (text, expected) in PARITY {
        let runs = selis_shape::script_runs(text);
        assert_eq!(runs.len(), 1, "{text:?}: one script run");
        let f = match runs[0].script {
            s if s == script_tag("Deva") => &deva,
            s if s == script_tag("Taml") => &tamil,
            s if s == script_tag("Beng") => &bengali,
            s => panic!("{text:?}: unexpected script {s:#X}"),
        };
        let shaped = f.shape(text);
        assert_eq!(
            shaped.len(),
            expected.len(),
            "{text:?}: glyph count: {shaped:?}"
        );
        for (n, (g, e)) in shaped.iter().zip(expected.iter()).enumerate() {
            assert_eq!(g.glyph_id, e.0, "{text:?} glyph {n}: id");
            assert_eq!(g.cluster, e.1, "{text:?} glyph {n}: cluster");
            for (label, got, want) in [
                ("advance", g.x_advance, e.2 as f32),
                ("x offset", g.x_offset, e.3 as f32),
                ("y offset", g.y_offset, e.4 as f32),
            ] {
                assert!(
                    (got - want).abs() <= 1.0,
                    "{text:?} glyph {n}: {label} {got} != {want}"
                );
            }
        }
    }
}

/// The ISO 15924 tag for a four-letter script name (mirrors the itemiser).
fn script_tag(name: &str) -> u32 {
    let bytes = name.as_bytes();
    assert_eq!(bytes.len(), 4);
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
