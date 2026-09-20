# Phase 3 — Fonts and text (Weeks 18–23)

**Gate G3 exit criteria:** all 14 standard fonts substituted metric-compatibly · embedded
Type1/CFF/TrueType/CID/Type3 render at G2 tolerance · text extraction ≥98% edit-distance agreement
with PDFium on the extraction corpus · reading order matches tagged order on 100% of the tagged
corpus · CJK, RTL, and Indic corpora at G2 tolerance.

Text is where a PDF engine's quality is judged, because users read text and only look at graphics.
It is also the prerequisite for the entire edit product (ADR-P0024).

---

## 3.FONT — The font engine

- [x] **SL-3.FONT.01 — Font dictionary model + resolution** · owner: AI+
  - **Do:** Simple fonts (Type1, TrueType, Type3, MMType1) and composite (Type0) with descendant
    CIDFonts; `/FirstChar`/`/LastChar`/`/Widths`, `/FontDescriptor`, `/MissingWidth`. Resolve the
    real width for every code, because width errors accumulate into visibly wrong line lengths.
  - **DoD:** Widths match PDFium for every glyph on the font corpus; a missing `/Widths` falls back
    correctly to the embedded font's own metrics.
  - **Note:** The plan's named `ttf-parser` was declared unmaintained (RUSTSEC-2026-0192, denied
    by ADR-P0021); the font crate uses `skrifa` (MIT OR Apache-2.0, Google Fonts) instead.

- [x] **SL-3.FONT.02 — Encoding and character mapping** · deps: FONT.01 · owner: AI+
  - **Do:** StandardEncoding, WinAnsiEncoding, MacRomanEncoding, MacExpertEncoding, `/Differences`,
    the built-in font encoding, symbolic-vs-nonsymbolic TrueType cmap selection rules (3,0 vs 3,1
    vs 1,0 — the single messiest area of the spec), and the `/Encoding` precedence order.
  - **DoD:** A table-driven test over the encoding decision matrix; corpus `font-encoding`
    including symbolic TrueType fonts that other readers get wrong.
  - **Risk:** Budget more time than seems reasonable. This is the most common cause of "the text
    renders as boxes" bugs, and the rules are genuinely ambiguous in places. Document every
    decision with a spec citation or an observed-behaviour note.

- [x] **SL-3.FONT.03 — Embedded TrueType/OpenType** · deps: FONT.01 · owner: AI+
  - **Do:** `ttf-parser` for tables; glyph outlines including composite glyphs; broken-font
    tolerance (bad `loca`, missing `hmtx`, wrong `numGlyphs`).
  - **Note:** Tables via `skrifa` (ttf-parser unmaintained, see SL-3.FONT.01).
- [x] **SL-3.FONT.04 — Embedded CFF / Type1C** · deps: FONT.01 · owner: AI+
  - **Do:** CFF charstrings (Type 2), subrs, hintmask handling, seac, and CID-keyed CFF with FDSelect.
  - **Note:** CFF decoding via `skrifa` (covers Type 2 charstrings, subrs, hintmasks, seac, and
    CID FDSelect internally); verified against non-CID and CID-keyed fixtures.
- [x] **SL-3.FONT.05 — Embedded Type1 (PFB/PFA)** · deps: FONT.04 · owner: AI+
  - **Do:** eexec decryption, Type 1 charstrings, `/Subrs`, flex and hint-replacement, and the
    seac composite mechanism. Convert internally to the same outline representation as CFF.
  - **Note:** Type 1 is obsolete and ubiquitous in old documents. Skipping it means failing on
    exactly the archive documents users most need a tool for.
  - **Note:** Delegated to `read-fonts`' Type1 engine (via `skrifa::raw`; FreeType-derived, covers
    eexec/charstrings/subrs/flex/seac); verified against hand-built PFA and PFB fixtures.
- [x] **SL-3.FONT.06 — Type3 fonts** · deps: FONT.01, SL-2.CONT.04 · owner: AI+
  - **Do:** Glyph procedures as content streams with `/FontMatrix`, `d0`/`d1`, and their own
    resource dictionary. Depth-budgeted — a Type3 glyph can draw another Type3 font.
- [x] **SL-3.FONT.07 — CID fonts, CMaps, and predefined CMap resources** · deps: FONT.03 · owner: AI+
  - **Do:** `/Encoding` as a predefined CMap name or an embedded CMap stream; CIDToGIDMap;
    the Adobe-Japan1/GB1/CNS1/Korea1/KR registries; vertical writing (`/WMode 1`) with the
    `/W2` metrics and correct vertical origin.
  - **DoD:** Corpus `cjk` renders at G2 tolerance including a vertical-writing Japanese document.
  - **Note:** CMap parsing (`begincidrange`/`begincidchar`/`usecmap`/`/WMode`) and `/W`//`/DW`
    CID width resolution implemented; the predefined CMap registry names and `/W2` vertical
    metrics integrate with the text layer (SL-3.TEXT.01).
- [x] **SL-3.FONT.08 — Standard-14 metric-compatible substitution** · deps: FONT.02, SL-0.LEAD.07 · owner: AI+
  - **Do:** Ship metric-compatible substitutes for Helvetica/Times/Courier/Symbol/ZapfDingbats
    with the *exact* AFM widths, so unembedded-font documents lay out identically to Acrobat.
    Using the real AFM metrics with a substitute outline is the correct approach.
  - **DoD:** A document using all 14 fonts matches PDFium's layout within 0.5 px per line.
  - **Note:** AFM widths shipped for all 14 fonts (pdf.js metrics, Apache-2.0); the substitute
    *outlines* resolve through the fallback chain (SL-3.FONT.09).
- [x] **SL-3.FONT.09 — Fallback chain for arbitrary unembedded fonts** · deps: FONT.08 · owner: AI+
  - **Do:** Match on `/FontDescriptor` flags, `/FontFamily`, panose, and stem width; fall back
    through a bundled set, then to system fonts on native (via `fontdb`), then to a notdef box.
    Web/extension have no system fonts — the bundled set must stand alone there.
  - **DoD:** Documented, deterministic fallback order (determinism matters: ADR-P0012 means the
    same document must not render differently because a user installed a font — so **system fonts
    are disabled in the determinism CI job and in the corpus harness**).
- [ ] **SL-3.FONT.10 — CJK fallback without a 100 MB payload** · deps: FONT.09 · owner: AI+
  - **Do:** Noto CJK is far too large to bundle in a 3 MB WASM budget. Ship a subsetted core set,
    lazy-load additional ranges as separate chunks on demand, and cache them. Native bundles more.
  - **DoD:** A Chinese document renders correctly on the web with a measured incremental download;
    the viewer never blocks on a font fetch (renders notdef, then repaints).
  - **Status:** Engine side landed 2026-09-14 — the web DoD half stays open
    (no shell yet), so this box stays unchecked. `selis_font::cjk` now owns
    the real strategy: a 31-range Unicode→chunk table (main Ideographs and
    Hangul syllables quartered; 11 ranges marked core-static and never
    emitted as files), `CjkFontSet` (injected resident set — `provide`
    validates id + SFNT parse, sticky pending queue in table order,
    `revision` invalidation counter, `drain_requested` over an injected
    `CjkChunkSource`), and `build::build_set`, which runs the FONT.11
    subsetter over one source font to emit the subsetted core file plus one
    range-pure SFNT chunk file per covered range. Engine:
    `Session::render_page_cjk`/`render_page_cjk_with_stats` resolve
    `Uni…UCS2…` runs nothing-resident codes against the walk's immutable
    snapshot; uncovered CJK codes paint a `.notdef` box and queue their
    chunk; the render itself performs zero I/O. Proven by
    `cjk_lazy::lazy_cjk_renders_notdef_then_repaints_after_in_memory_fetch`
    (pass1: `needs=[ideographs-4, hangul-1]`, tofu inked, 0 source fetches;
    drain→provide bumps revision to 2; repaint: real glyphs, more ink,
    `needs` empty; next repaint byte-identical) and
    `arrival_order_does_not_change_pixels` (lazy == pre-provided == reverse
    arrival, ADR-P0012); `plain_render_path_is_unchanged` pins the legacy
    path. `xtask cjk-build --source <noto-ttf> --out <dir>` writes
    `cjk/core.ttf`, `cjk/<id>.ttf`, and a manifest pinning raw/brotli sizes
    + sha256 per file with budget gates (defaults core ≤ 1 200 000 /
    chunk ≤ 1 500 000 brotli). Measured sizes so far are the synthetic
    fixture only (core 484 B raw / 235 B brotli; per-code-point chunk
    436 B raw / ~220 B brotli); real Noto numbers land at the first
    release `cjk-build` run. The fetch/caching
    contract for the shell (chunk URL layout, immutable files,
    id+sha256 cache key, revision-driven repaint) is ADR-P0043 (DRAFT,
    pending human sign-off). Still open here: the web-side *measured
    incremental download* and live notdef→repaint under the WASM shell —
    SL-4.WASM.07.
- [x] **SL-3.FONT.11 — Font subsetting and re-embedding** · deps: FONT.03, FONT.04 · owner: AI+
  - **Do:** Subset TrueType and CFF to a glyph set, rebuild `loca`/`hmtx`/`cmap`/charstrings, and
    **merge new glyphs into an existing subset** — required by ADR-P0024, because editing text adds
    characters the original subset lacks.
  - **DoD:** Round-trip: subset → embed → parse → render matches the original; a test that adds a
    glyph absent from the original subset and renders it correctly.
  - **Risk:** This is the task that makes text editing possible. If it slips, ADR-P0024 slips.
  - **Note:** TrueType subsetting (open/close composite closure, `loca`/`hmtx`/`cmap`/`head`/
    `maxp`/`hhea`/`post` rebuild, checksum-adjusted) + `add_glyph` merge implemented. CFF subsetting
    deferred (returns `Ok(None)` — deviation).
- [x] **SL-3.FONT.12 — Font-program fuzzing** · deps: FONT.05 · owner: AI
  - **DoD:** Fuzz targets for TrueType, CFF, Type1, and CMap parsing; 8 h clean each.
  - **Note:** Targets added (`font_ttf`, `font_cff`, `font_type1`, `font_cmap`); the 8 h soak runs
    under the nightly fuzz CI job (SL-0.SEC.02), which is a HUMAN enablement item.

---

## 3.SHAPE — Shaping and layout (for authored and reflowed text)

- [x] **SL-3.SHAPE.01 — `Shaper` trait + `rustybuzz` backend** · owner: AI+
  - **Do:** Script itemisation, feature application, cluster mapping. Used for new/edited text
    only — existing content is replayed by glyph id, never re-shaped.
  - **DoD:** The trait boundary keeps `rustybuzz` out of the replay path entirely (checked by
    `check-layers`).
  - **Note:** `rustybuzz` declared unmaintained (RUSTSEC-2026-0206) — replaced with `swash` (the
    maintained fontations shaping engine), behind the same `Shaper` trait in the `selis-shape`
    L2 crate. `check-layers` enforces no edge from `selis-pdf-content` to `selis-shape`, keeping
    swash out of the replay path.
- [x] **SL-3.SHAPE.02 — Bidi and RTL** · deps: SHAPE.01 · owner: AI+
  - **Do:** UAX #9 via `unicode-bidi`, paragraph direction detection, and mirroring.
  - **DoD:** Corpus `rtl` (Arabic, Hebrew) renders and extracts in correct logical order.
- [x] **SL-3.SHAPE.03 — Line breaking and justification** · deps: SHAPE.02 · owner: AI+
  - **Do:** UAX #14 line breaking, plus PDF-specific justification (word spacing vs. char spacing
    vs. horizontal scaling) so reflowed text matches the original paragraph's visual style.
- [x] **SL-3.SHAPE.04 — Indic and complex-script validation** · deps: SHAPE.01 · owner: AI
  - **DoD:** Corpus `indic` (Devanagari, Tamil, Bengali) at G2 tolerance.
  - **Status:** Done. `crates/selis-shape/tests/indic.rs` pins per-construct
    known-goods (pre-base matra reorder, conjunct merge, `reph`, two-part
    vowel split, ligatures, `nukta`) plus a committed HarfBuzz-12.1 parity
    table over every generator string, with three documented shaping-side
    divergence classes (cluster merge targets, matra variant forms, Bengali
    subjoined-mark y-offset sign — glyphs and advances agree). Corpus
    `indic`: 9 shaped Type0/Identity-H fixtures generated by
    `xtask corpus synthetic-generate` (`corpus/pdfs/indic`, expectations in
    `corpus/expect/indic`), exercising the engine's CID replay (`/W` from
    shaped advances, `/CIDToGIDMap`, `/ToUnicode` recovery). Render + extract
    verified over all 9 (ink 0.09–0.38 % of the A4 page at 150 DPI; every
    extract recovers the correct script's Unicode scalars, no raw CIDs).
    Oracle-G2 sign-off happens in the nightly `render-conf` sweep (CI:302
    generates the subset before the sweep). Devanagari/Tamil/Bengali shaping
    itself routes ISO 15924 tags to the OpenType scripts (`Deva`→`dev2` etc.);
    before this, `Script::from_opentype` fell back to Latin for every ISO tag
    and complex shaping never engaged.

---

## 3.TEXT — Extraction and reading order

- [x] **SL-3.TEXT.01 — Text-showing operators and positioning** · deps: SL-2.CONT.02 · owner: AI+
  - **Do:** `Tj TJ ' "`, text matrix vs text line matrix, `Td TD Tm T*`, and the full advance
    formula including char spacing, word spacing (byte-0x20-only rule for simple fonts, and the
    trap that it does **not** apply to 2-byte CID codes), horizontal scaling, and rise.
  - **DoD:** Glyph positions match PDFium to sub-pixel on the text corpus.
  - **Note:** Text state machine (`selis_pdf_content::text`) implemented with all operators,
    the advance formula (moved to `selis-font` for the replay path), and the CID-0x20 trap.
    The DoD corpus comparison needs the engine's page-render path.
- [x] **SL-3.TEXT.02 — ToUnicode and text recovery** · deps: FONT.02 · owner: AI+
  - **Do:** `/ToUnicode` CMap parsing; fall back through the encoding's glyph names →
    Adobe Glyph List → the `uniXXXX`/`uXXXX` name conventions → the font's own cmap reverse map.
    Report a per-run confidence.
  - **DoD:** Extraction corpus ≥98% agreement with PDFium; files with no `/ToUnicode` and symbolic
    encodings are correctly flagged low-confidence rather than emitting mojibake silently.
  - **Status:** Marked done before a corpus-wide measurement existed. SL-3.CONF.01 (2026-09-11)
    falsifies the agreement claim: only 9.8% of comparable files reach ≥98% similarity, and the
    sweep exposed a concrete defect in this area — non-ASCII text is emitted as literal PDF-string
    octal escapes ("modèle" → "m o d 3 5 0 le") instead of decoded Unicode (SL-3.TEXT.08).
- [x] **SL-3.TEXT.03 — Run, word, and line assembly** · deps: TEXT.01 · owner: AI+
  - **Do:** Group glyphs into runs by style continuity, infer word boundaries from advance gaps
    relative to the font's space width, and assemble lines by baseline clustering.
  - **DoD:** Word segmentation matches PDFium on the extraction corpus; a test for the
    "no space characters in the content stream at all" case, which is common in generated PDFs.
  - **Status:** Marked done before a corpus-wide measurement existed. SL-3.CONF.01 (2026-09-11)
    falsifies the segmentation claim: the extractor inserts a word break between nearly every
    glyph ("n° d'identification" → "n 2 6 0 d 'id e n tific a tio n"; the single-word smoke
    fixture yields "S e lis o ra cle sm o ke te st"), dominating the diff>=25 signature
    (1,302 files). Root cause and fix tracked as SL-3.TEXT.09.
- [x] **SL-3.TEXT.04 — Reading order: structure-first, geometry-fallback** · deps: TEXT.03, SL-1.DOC.06 · owner: AI+
  - **Do:** When a structure tree exists, use it (ADR-P0031). Otherwise infer with column detection
    and an XY-cut or similar layout analysis. Return a confidence; never silently guess on a
    two-column document and produce interleaved nonsense.
  - **DoD:** 100% match against tagged order on the tagged corpus; a measured accuracy number on
    the untagged multi-column corpus, published in the conformance report.
- [x] **SL-3.TEXT.05 — Selection geometry and hit testing** · deps: TEXT.03 · owner: AI
  - **Do:** Character-level quads for selection highlighting, caret positions, word/line/paragraph
    expansion, and RTL-correct selection ranges.
- [x] **SL-3.TEXT.06 — Search** · deps: TEXT.03 · owner: AI
  - **Do:** Normalised search (case, diacritics, ligature decomposition, soft hyphens, and
    cross-line matches), incremental index built per resident page only.
  - **DoD:** Finds "ﬁrst" when searching "first"; finds a term broken across a line break.
- [x] **SL-3.TEXT.07 — Structured extraction output** · deps: TEXT.04 · owner: AI
  - **Do:** Emit text as plain, or as a structured document (blocks/lines/spans with style and
    bbox), or as Markdown/HTML. Foundation for the convert product and for any AI/RAG integration.
  - **DoD:** JSON schema versioned and documented; `selis extract --format=json|text|md|html`.

- [x] **SL-3.TEXT.08 — Text output must emit Unicode, not PDF string escapes** · deps: TEXT.02 ·
  owner: AI+ · **filed by SL-3.CONF.01**
  - **Defect:** The plain-text formatter emits non-ASCII bytes as literal PDF-string octal
    escapes: "modèle" extracts as "m o d 3 5 0 le" (\350 printed as digits), "n°" as "n 2 6 0".
    Affects every file with non-ASCII text; on the CONF.01 sweep it is a primary cause of the
    `diff>=25` signature (1,302 files, the corpus's largest divergence cluster after word
    splitting).
  - **Do:** Decode through ToUnicode/encoding to Unicode and emit UTF-8 in all text formats; add
    a formatter test with é/°/CJK through each of `--format text|json|md|html`; verify the CONF.01
    sweep's `diff>=25` cluster shrinks accordingly.
  - **Note (done 2026-09-14):** Fixed. One correction to the filing, verified across the 3,860-file
    sweep: octal-escape *digits* never actually occurred in the text path (both string lexers
    decode `\ddd` correctly) — "m o d 3 5 0 le" was the TEXT.09 per-glyph split of an invisible
    control/garbage character; the real defect is exactly the filing's fix clause: simple-font
    codes were **never decoded** — `Session::text_unicode` answered only CID fonts with a
    `/ToUnicode`, so simple-font bytes fell to the accidental `char::from_u32(byte)` reading.
    `Session::text_unicode` now runs the full SL-3.TEXT.02 chain for either font class:
    `/ToUnicode` first (also for simple fonts, §9.10.2), then the encoding's glyph name →
    AGL → `uniXXXX`/`uXXXX` → the embedded program's `post` name, confidence-scored via the
    unwired-until-now `selis_font::TextRecovery`; `None` keeps the legacy byte reading (never
    worse). The recovery is applied to every glyph before assembly in the CLI, the WASM worker,
    and `selis extract`/search alike. Tests: unit (engine `text_recovery_simple`: WinAnsi,
    MacRoman 0x8E→é, `/Differences` 25→é/uni4E8C, Symbol stays unguessed, 0x20 stays space);
    formatter (all four formats via `apps/cli/tests/extract_fidelity.rs` on the in-repo fixture
    `text08_encoding_unicode.pdf` — WinAnsi é+°, MacRoman é+°, Differences é/è, ToUnicode CJK
    二次元 through `--format text|json|md|html`, asserting zero escape/digit leakage); corpus pin
    `corpus/pdfs/synthetic/bugfix_text08_encoding_unicode.pdf`. Post-fix full sweep
    (`text-sweep` vs mutool 1.23.0, 3,848+12 files, pinned binaries): `match` 71→947,
    `diff>=25` 1,428→556 (−872), G3 ≥0.98 4.20%→**54.79%** (956/1,745), mean ≥50-char-page
    similarity 0.408→0.759, median 0.45→**1.00**. (Bands are the compound A+B+§9.4.1-BT effect —
    per-defect attribution: the é/°/CJK fixtures now extract exact; flat/PDF.js source went
    9→237-in-G3 of ~479.)
- [x] **SL-3.TEXT.09 — Word-gap inference splits every glyph** · deps: TEXT.03 · owner: AI+ ·
  **filed by SL-3.CONF.01**
  - **Defect:** The extractor's inter-glyph gap threshold treats nearly every advance as a word
    break, so all text extracts as single-glyph "words" ("S e lis o ra cle sm o ke te st" for the
    one-line smoke fixture; "n 2 6 0 d 'id e n tific a tio n" for "n° d'identification").
    Rendering is unaffected (the CONF.01 render sweep passes G2), so the defect is in the
    extraction path's gap→space inference, not in advance computation — prime suspect is a
    text-space/font-unit scale mismatch in the extractor's gap threshold.
  - **Do:** Fix the threshold against the font's space width; add the TEXT.03 DoD's
    "no space characters in the content stream" test at the extractor level; the CONF.01 sweep's
    `match` band should rise from 9.8% toward the G3 bar.
  - **Note (done 2026-09-14):** Fixed. Confirmed root cause: `assemble_words` compared the
    raw origin delta (which *is* the previous glyph's advance, ≈0.5 em for lowercase) against
    `0.5 × Tf-size`, so ordinary advances tripped the bar. The interpreter now records each
    glyph's pen step (`advance`) and the font's space width (`space`) on `TextGlyph`/`GlyphRun`
    — the justified advance mapped through the text matrix (same units as `at`; CID fonts clamp
    code-32 widths to the plausible space band with a 0.25 em fallback since a subset's CID 32
    is not a space), and inference splits only when the gap *exceeds the advance by* more than
    half a space width. Tests: `assembly.rs` (no-space-content-stream two-word case = the
    TEXT.03 DoD; `half_em_glyph_advances_never_split` pins the exact regression predicate;
    zero-metrics merge conservatively; two proptest invariants: uniform steps never split,
    a jump beyond half a space always splits exactly once — property-sweep, 256 cases);
    interpreter-level (advance/space equal the observed origin delta under any `Tm`);
    extractor-level e2e: `bugfix_text09_word_gap_spaceless` asserts *byte-by-byte* that the
    fixture's string literals hold no 0x20 and the extractor still yields "Hello World",
    plus the oracle-corpus smoke fixture now extracts `Selis oracle smoke test` exactly.
    Corpus + sweep numbers as per TEXT.08 note (these two plus the §9.4.1 fix are the band
    movers). Caveat kept honest: 8 veraPDF "Hello world" glyph-tension files moved
    diff<25→diff≥25 because *MuPDF itself* word-splits them per its own heuristics while our
    now-unified text scores lower — text-correct/oracle-noisy, listed for CONF.02 triage,
    not a fidelity regression. **TEXT.11 re-check (2026-09-15):** measured against the
    branch's HEAD goldens. Only ONE of the `6-3-8-t01-*` family re-anchors: `pass-a` (its
    multi-glyph `(test)Tj`/`(  .java)Tj` runs ride a `10.761 0 0 10.761 … Tm /T1_0 1 Tf`
    scale-trick) drifts its **render** golden and continues to read `match` 1.0 (4/4 chars,
    MuPDF-exact origins). `fail-a…d`, `pass-b…j` and `helloworld-bad` drift **nothing** —
    render and text hashes byte-identical with the pre-fix tree and their existing
    signatures (`diff<25` 0.907/0.909, `diff≥25` 0.0, `match` 1.0, `empty_oracle`) are
    unchanged. So the caveat's prediction that the corrected matrix math would pull these
    files toward MuPDF's *text band* score is **false**: whatever MuPDF does on them (its own
    stext segmentation of glyph-tension CID runs) is orthogonal to the inter-glyph pen step,
    and TEXT.11 leaves their verdicts exactly as TEXT.09 filed them — oracle-noise, CONF.02
    triage as listed. Matrix-direction *is* verified where a pen step exists: `pass-a`'s
    render and the wild `bug1057544` (render drifts, text `..`) plus `issue9972-1/2/3`
    (render drifts, text `match` 0.9984 unchanged) re-anchor their layouts to MuPDF's
    columns centipoint-for-centipoint (below).

- [ ] **SL-3.TEXT.10 — Silent empty extraction on text-bearing pages** · deps: TEXT.01 ·
  owner: AI+ · **filed by SL-3.CONF.01**
  - **Defect:** 117 corpus files extract zero characters with selis while MuPDF recovers text
    (e.g. TAMReview: mutool 1,696 chars, selis 0 — and the page renders non-blank at 72 DPI, so
    text-showing operators execute). No error is surfaced: the typed outcome is a silent empty
    string, indistinguishable from a blank page.
  - **Do:** Diagnose the operator/font path that drops the text (the sweep's verdicts carry the
    file list and per-file font inventory on `C:\selis-build\conf01-text`); emit a low-confidence
    marker instead of silently returning empty on pages whose display list drew text.
  - **Note (2026-09-14):** Diagnosis + marker done; DoD partially. The conf01 artifacts had been
    purged; the sweep was re-run on the regenerated 3,860-file layout (baseline pinned pre-fix
    binary: `match` 71, `empty_selis` 88 — the 87-file cluster reproduces).
    Root cause for the page-level empties found and fixed: `exec.rs` replaced the whole text
    state at `BT`, but §9.4.1 resets **only Tm/Tlm** — TCPDF-shaped producers (`BT /F 12 Tf ET`
    in one object, the `Tj` in the *next*) had their glyphs silently dropped from render *and*
    extraction. After the fix 13 baseline empties recover their page text (blendmode, basicapi,
    extgstate, issue13405, govdocs1/000/000060, ghent ReadMe overprints, …) and 650 expectation
    records refresh their render goldens as previously-lost text now paints (spot-probe:
    alphatrans mean pixel 232.4→231.9 toward mutool's 226.3; full G2 oracle re-measure is a CI
    step and CONF.02 numbers, not claimed here). The remaining 62 `empty_selis` are all
    annotation-AP text (widget `/Tx` captions, FreeText, /A appearance forms — verified
    content-stream-by-content-stream on bug1675139: the page stream is genuinely empty where
    MuPDF merges annotation appearances): they drew *no display-list text*, so the low-confidence
    rule correctly stays silent — the honest next task is an annotation appearance walk, filed
    for CONF.02, not a marker candidate. Implemented: `LOW_CONFIDENCE_MARKER` +
    `Structured::low_confidence` emitted in all four formats when the display list drew glyphs
    but zero characters were recovered (surrogate/undecodable-code pages — the fixture
    `bugfix_text10_low_confidence.pdf` pins marker-present/absent per page e2e).
    Remaining per DoD wording: the ">5 % divergence without a recovery → flag, wired into the
    sweep's `text_err`" clause is **not** implemented (the marker currently covers only the fully
    silent case), and the 62 annotation-file silent gap persists until the AP walk lands —
    checkbox stays open; its two remaining halves are owned here as **SL-3.TEXT.14**
    (annotation-appearance walk) and **SL-3.TEXT.15** (divergence watchdog), the ids
    SL-3.TEXT.12/13 having been taken by SL-3.TEXT.11's own residuals (paint-time `Tm`,
    vertical-run assembly) — this box now tracks only the shipped diagnosis + marker.
- [x] **SL-3.TEXT.11 — Text-space advances must map through the text matrix** · deps: TEXT.01 ·
  owner: AI+ · **filed by SL-3.TEXT.08/09/10 work (2026-09-14)**
  - **Defect:** `text::show_string` and `Td`/`TD` accumulate pen movement as
    `matrix.then(Matrix::translate(adv, 0))`, which adds the raw text-space advance to the
    matrix's `e`/`f` components instead of mapping it through the text matrix's linear part
    (§9.4.3: the translation is pre-multiplied — a user-space step of `adv × (a, b)`). For any
    non-identity `Tm` the glyphs land wrong in render **and** in extraction: an empirically
    probed 90° text matrix (`0 1 -1 0 x y Tm`) lays "ABCDEF" along **user-space +x**, while
    every oracle (mutool 1.23 confirmed) runs it along +y; the common generator trick
    `12 0 0 12 … Tm` with `/F 1 Tf` (bug1057544 line 3, the 8 veraPDF "Hello world" files)
    lays out at one-twelfth spacing. Identity-`Tm` documents (the overwhelming majority,
    including the oracle fixtures) are unaffected — which is how this hid through the CONF.01
    G2 pass (page `/Rotate` is a device transform, not `Tm`).
  - **Do:** Compose shows/Td per §9.4.3 (pre-multiply the translate), keeping the recorded
    `advance`/`space` metrics consistent with the new `at` deltas (they are pinned to the
    observed delta by the invariant test in `text.rs`); re-baseline the text+render goldens for
    the non-identity-`Tm` corpus and re-measure G2 at the next CONF gate.
  - **DoD:** Probe fixtures (rotated + scaled `Tm`; one eja-vi/CAD-style document from the
    wild corpus) place glyphs where mutool places them per the page-render diff; no regression
    of the existing text bands.
  - **Note (done 2026-09-15):** Fixed. `text.rs` composes every text-space step — the
    `show_string` advances, `Td`/`TD`/`T*` line moves, `TJ`/`'` adjustments — as
    `Translate × Tm` via `pre_translate` (§9.4.3), and `pen_x` records the **user-space** step
    while it rides the same mapping as `at`. Under identity `Tm`
    `Translate·I = I·Translate = Translate`, so the majority of the corpus is bit-identical.
    The TEXT.09 invariant test needed no re-anchoring: recorded `advance` is still exactly the
    observed origin x-delta, now the correctly mapped `adv × a` (a 10× probe asserts 10.0, not 1).
    Probes, MuPDF-paired (`mutool 1.23.0` per-glyph `draw -F svg`): rotated
    `0 1 -1 0 100 100 Tm /F 24 Tf` → x=100 fixed, y = 100/116.008/132.016/149.344/166.672/182.68
    = device f 692→609.32 (MuPDF); scale-trick `12 0 0 12 60 700 Tm /F 1 Tf (Hello World)` →
    origins 60/68.664/75.336/…/115.332 = MuPDF's column and *equal to the equivalent
    identity-`Tm` `/F 12 Tf` row line-for-line. Pinned as `text.rs` unit tests + `text09`-style
    e2e in `apps/cli/tests/text_matrix_layout.rs`
    (`scaled_tm_matches_mu_device_positions_and_splits_words`,
    `rotated_tm_lands_the_vertical_run_at_fixed_x_on_user_y`), fixtures
    `apps/cli/tests/fixtures/text11_tm_{rotated,scaled}.pdf`, corpus pins
    `bugfix_text11_tm_rotated` / `bugfix_text11_tm_scaled` registered in
    `xtask/src/synthetic.rs`. Wild reproductions: `bug1057544` line 3 carries exactly the
    filed trick (`12 0 0 12 67.2 735.9961 Tm /TT0 1 Tf (An Annual Report marks the )`), the
    `issue9972-1/2/3` OmniGraffle "CAD-style" trio `12 0 0 12`/`18 0 0 18 … Tm /F 1 Tf` with
    scaled `Td` kerning (AES-128 streams; inspected after `mutool clean -D`) — MuPDF pairs with
    the fixed binary on `bug1057544` centipoint-for-centipoint (`Annual` selis span
    84.54→117.66 = MuPDF glyph `A` 84.53999 / `l` 117.65999). **Blast radius measured** on the regenerated
    3,860+2-pins layout against HEAD's committed goldens: **358 records re-anchored** (214
    render, 207 text) through the pinned pipeline (`xtask oracle text-sweep --tool mutool
    --dpi 72,150,300` → `xtask corpus expect-merge --from C:\selis-build\text11-prebaseline`;
    pinned binary sha256 `5e30fffa…74b555`); audit says every drifting id contains a
    non-identity `Tm` (355 by decompressed stream scan; the 3 AES-256 `issue9972-*` by
    MuPDF-svg matrix column after `-D` decrypt), and the identity-`Tm` majority (~3,502
    records incl. spot-checks `issue3879r/TAMReview/vertical/rotation/standard_fonts`) is
    byte-identical. Bands vs the TEXT.08/09/10 note numbers (the 3,860-file sweep that
    recorded them): exact `match` **947→969**, `diff≥25` **556→540**, `empty_selis` **62→62**
    (unchanged — the annotation-AP cluster TEXT.10 named), G3 **54.79 %→56.01 %** (956 of
    1,745 comparable → 978 of 1,746). The other bands have no pre-fix record, so no claim is
    made; the identity-`Tm` majority is bit-identical, so every existing-corpus shift is a
    358-file-cohort shift and it nets positive. The two new pins land `match` (scaled, MuPDF
    chars 23/23) and `diff≥25` (rotated, 0.444, selis 18 chars / oracle 13 — the per-glyph
    vertical-run fragmentation counted as extra glyphs, see below). TEXT.08/09 caveat cohort:
    measured, *not* predicted — of the 14 `6-3-8-t01-*` files only `pass-a` drifts its
    **render** (multi-glyph runs under scaled `Tm`; MuPDF-exact, `match` 1.0) and every one of
    their **text** signature files stays byte-identical (`fail-a/b` 0.9079/0.9091, `fail-c/d`
    0.0, `pass-e…j` `.notdef` runs): the pen-step correction does *not* move MuPDF's own
    stext word-segmentation noise there. `bug1057544`'s render re-anchors (`12 0 0 12`-×-`/F 1`
    word origins now centipoint-equal to MuPDF's svg column: `Annual` 84.54→117.66 = MuPDF
    A 84.53999 / l 117.65999); its *text* band is unchanged (hash identical, 0.6748 both
    sides — the residual is its CID `ToUnicode`/glyph-name *inventory* delta, an unrelated
    axis, not layout). `issue9972-1/2/3` (the OmniGraffle CAD-style trio, `12 0 0 12`/
    `18 0 0 18 … Tm /F 1 Tf` with scaled `Td` kerns) likewise show *render-only* drift with
    text byte-identical at `match` (0.9984). Honest residual,
    filed nowhere yet: with §9.4.3 layout correct, the rasterizer still places glyph
    *outlines* upright and at `/F 1` size (the `Op::Text` path maps only `at`, not `Tm`'s
    linear part) — so rotated text is positioned like MuPDF but drawn unrotated and
    scale-trick text is positioned at 12× pitch with 1pt glyphs; the fixture centroid check
    (post-fix selis 91.4 vs MuPDF 91.9 on the scaled probe) confirms the *placement* is
    MuPDF's while the *outline extent* still differs.
    Also vertical runs now fragment 1-glyph-per-line in assembly (horizontal baseline
    tolerance) — MuPDF keeps them one word; that's the SL-3.TEXT.04 reading-order axis,
    unblocked-but-unowned by this fix (asserted, with the reason, in
    `text_matrix_layout.rs`'s doc comment; it is also why the *rotated* pin scores 0.444).
     **`corpus verify --golden`: 3,862 checked, 0 changed, 0 without expectation**
     (pinned binary `5e30fffa…`, full pass post-merge).
- [ ] **SL-3.TEXT.12 — Glyph painting must apply the text matrix, not only the pen origin**
  · deps: TEXT.11 · owner: AI+ · **filed by SL-3.TEXT.11 (2026-09-15, operator)**
  - **Do:** The `Op::Text` rasterizer path places outlines at `at` with axis-aligned
    `f` sizing; carry the full `Tm` linear part into glyph placement so rotated text
    draws rotated and the `12 0 0 12 Tf 1` scale-trick draws at 12 pt. Re-anchor the
    non-identity-`Tm` render goldens the TEXT.11 blast radius enumerated (358 cohort)
    and re-measure the render-corpus bands (the G2 claim in `conformance/REPORT.md`
    reads against them).
  - **DoD:** The TEXT.11 probe fixtures paint MuPDF-congruent geometry (not merely
    correct origins); no identity-`Tm` render regression (bit-identical expectation
    records); sweep bands recorded against the TEXT.11 numbers (969 / 540 / 56.01 %).
- [ ] **SL-3.TEXT.13 — Line assembly must keep vertical runs one line** · deps: TEXT.04 ·
  owner: AI+ · **filed by SL-3.TEXT.11 (2026-09-15, operator)**
  - **Do:** Line splitting tolerates horizontal baselines only; glyphs stepping along
    +y under a 90° `Tm` fragment one-per-line (MuPDF keeps them a word — the rotated
    pin scores 0.444 purely for this). Generalize the baseline model to the `Tm`
    writing direction (vertical CJK runs included).
  - **DoD:** The rotated fixture assembles as one line with reading order along the
    writing direction; `text_matrix_layout.rs`'s documented assertions flip to the
    MuPDF-kept-word behaviour; sweep `match` band re-measured.

---

## 3.CONF — Conformance checkpoint

- [x] **SL-3.CONF.01 — Full text/font differential sweep** · owner: AI+
  - **Do:** Render + extract the whole corpus against PDFium and pdf.js; triage; file per root cause.
  - **Status:** Sweep run twice on 2026-09-11 / 2026-09-12 — pre-merge (against selis @ 2aacac21
    with the pinned binary sha256 `f4a21235…22c911a`, 3,836 files) and again post-merge with main's
    SHAPE.04 (CID replay), RAST.12 (/Rotate page-to-device transform), and RAST.13 (blank-render
    fixes) landed (3,848 files, SHAPE.04 added 12 new synthetic indic fixtures; pinned post-merge
    binary sha256 `76deee66…d5dc533d`). The post-merge numbers below supersede the pre-merge ones.
    Harness: `cargo xtask oracle text-sweep` (`xtask/src/text_sweep.rs`); normaliser N1–N6
    documented and unit-tested in `xtask/src/text_norm.rs` (bidi controls / BOM / soft-hyphen /
    line-break hyphen / ligature folding, whitespace collapse; mirroring deliberately NOT folded).
    Oracle: locally `mutool draw -F txt` (mutool 1.23.0). Artifacts on `C:\selis-build\
    conf01-text-post-merge` (`verdicts.jsonl` + `golden.jsonl` + `text-report.json`), never in the
    repo. PDFium/pdf.js text legs wired into the scheduled `render-conf` job (drivers gained
    `--text` modes; oracle-images smoke extended); digest re-pin required after the oracle-images
    CI job rebuilds the images — until then the two CI legs fail loudly against the old images
    (`xtask: ok`-typed `oracle_rejects` in the artifacts), while the mutool leg uses the current
    pin immediately.
  - **Readout (honest):** G3 bar (≥98% normalised similarity on ≥95% of comparable files) NOT
    MET: 72/1,730 comparable files = **4.16%**. Comparable = both sides produced text (i.e., not
    both_reject, timeouts, or the empty_* signatures). Compared with the pre-merge run, the match
    band COLLAPSED: 167 → 70 matched, 1,302 → 1,429 in `diff>=25`. The overall text-corpus
    agreement got WORSE on merge — most likely a side effect of SHAPE.04's `apps/cli/src/
    extract.rs` change; the SHAPE.04 DoD was about Indic shaping parity at G2 render tolerance,
    and this is a text-agreement regression that its CI leg (Indic fixtures) does not cover. The
    itemised signature gap list:
    * `empty_both` 2,040 — no text on page 1 on either side (image-only + blank pages). Not a
      failure; expected in test-suite corpora.
    * `diff>=25` 1,429 — the systemic divergence, three root causes (SL-3.TEXT.08 non-ASCII
      emitted as literal PDF-string octal escapes: "modèle" → "m o d 3 5 0 le"; SL-3.TEXT.09
      word-gap inference splits nearly every glyph, e.g. "n° d'identification" →
      "n 2 6 0 d 'id e n tific a tio n" and the single-word smoke fixture yields
      "S e lis o ra cle sm o ke te st"; plus a post-SHAPE.04 regression cohort that shifted
      from `match`/`diff<25` into this band on merge).
    * `diff<25` 127 + `diff<5` 6 — same causes, milder similarity bands.
    * `empty_selis` 87 — selis extracts zero characters where MuPDF recovers text; SL-3.TEXT.10
      (the count dropped 117→87 after SHAPE.04, i.e. some CID cases improved — the CID replay
      fix worked for those, so this gap narrowed).
    * `empty_oracle` 11 — selis recovers text MuPDF misses (superset recovery). Not a bug.
    * Rejections/timeouts: `oracle_rejects` 34 (12 with a clean stderr prefix: mutool refuses
      some encrypted-no-password files selis recovers), `both_reject` 23, `selis_rejects` 17
      (14× E1103 recover-root, 2× budget, 1× zero-pages), `oracle_timeout` 4, and 2 selis
      timeouts on monster pages. None new.
  - **Font side (per file, oracle inventory + selis span-font names; verdicts carry both):**
    all_embedded 1,108 files comparable (38 within G3 = 3.4%), has_external 395 (29 = 7.3%),
    has_type3 63 (3 = 4.8%), unknown (no font inventory on this leg) 164 (2 = 1.2%). Divergence
    is **systemic, not substitution-driven** — unembedded-font and embedded-font files diverge at
    similar rates. The CONF.01 gap list correctly blames TEXT.08/09, not SL-3.FONT.08/09.
  - **SHAPE.04 side-effect finding (NEW):** Post-merge, the match band collapsed. The delta:
    pre-sweep 167 files at ≥99% similarity → post-sweep 70; a ~97-file regression cohort now
    disagrees at the highest band. SHAPE.04's `apps/cli/src/extract.rs` change is the top
    suspect; recommend a follow-up `bisect` task. Not filed here (this task's scope is running
    the sweep + filing root-cause tasks, not bisecting main).
- [x] **SL-3.CONF.02 — Promote conformance areas; publish the report** · owner: AI
  - **Do:** Re-run the full text differential sweep on *current main* (the TEXT.08/09/10
    expectations were only re-anchored in the merge — no numbers copied out of the
    per-task notes), re-measure the render bands on the post-fix tree, set the honest
    rung for the affected areas in `conformance/areas.toml`, regenerate `REPORT.md`,
    re-pin the CI oracle image digests, and file the remaining root-cause tasks.
  - **Status (2026-09-14):** Done. The CONF.01 artifacts were purged with the build dir,
    so the corpus was re-extracted from the fetch cache (`corpus/pdfs`, 3,860 files,
    `corpus verify` clean against the committed expectation tree) and the **full text
    sweep** was re-run on `03ece8ba` (`xtask oracle text-sweep --tool mutool`,
    out `C:\selis-build\conf02-text`, pinned selis sha256 `5e30fffa…4b555`, mutool
    1.23.0 local, golden DPIs 72/150/300). The **full render sweep** was also re-run
    (same tree, `C:\selis-build\conf02-render`, 3,860 × 3 DPI × MuPDF = 11,580
    outcomes). Nothing was measured on a pre-merge branch.
  - **Text readout vs the TEXT.08/09 notes (all deltas are the BT fix + the merged
    enc/font/JPX waves):** comparable 1,745 (unchanged); `match` **969** (947, +22 —
    the 13 recovered empties land in comparable/match and the CID/ToUnicode legs
    tightened); `diff>=25` **539** (556, −17); G3 ≥0.98 **978/1,745 = 56.05%**
    (54.79%, +1.26pp) — still `met: false`; ≥0.99 969 (55.53%), ≥0.95 997 (57.13%),
    ≥0.75 1,130 (64.76%); mean sim 0.7612, median 1.000 (p25 0.519 — bimodal, mirrors
    the oracle-vs-oracle shape; the notes' 0.759/1.00 reproduce inside the re-measure).
    Cohorts: `empty_selis` **62** (annotation-AP text; per-cause task SL-3.TEXT.14),
    `empty_oracle` 14, `oracle_rejects` 34, `both_reject` 23, `selis_rejects` 17,
    `oracle_timeout` 4, truncated flag 3, rtl-tagged 15. Per-source ≥0.98 of scored
    (source-comparable / g3-within): flat 237/479, govdocs1 52/180, verapdf 576/799,
    synthetic 106/108, indic 7/9, ghent **0/94**. Font cohorts: all_embedded 674/1,096
    (61.5%), has_external 184/391 (47.1%), has_type3 10/63 (15.9%), unknown-inventory
    110/119.
  - **Render readout:** @150 n=3,773: ≤0.5% **67.9%** (was 27.3% — +40.6pp, the
    1–2% band excess RAST.14 was chasing is gone), ≤1% 74.9 (34.1), ≤2% **80.9**,
    ≤5% 86.8, ≤10% 92.6, ≤25% **97.2** (97.0); p50 0.07, p95 13.8. Full bands:
    @72 65.6/75.3/79.6/85.2/90.6/96.7 (n=3,776), @300 70.5/77.1/82.7/88.9/94.3/97.5
    (n=3,768) — monotone across DPIs as the calibration predicts. On the published
    G2 calibration: **Bar A passes** and **Bar B now passes on the MuPDF leg**
    (≤2% 80.9 vs best pair 82.3 = 1.4pp inside the 5pp allowance; ≤5% −1.3pp,
    ≤10% −1.9pp) — the REPORT row's "Bar B fails by 1.1pp" was a stale CONF.01-era
    claim. Blank/skew clusters the old row blamed: `size_skew` 0 (RAST.12 closed),
    `blank_selis` 2 (@150: the two /6.3.3-t01-fail-b /Redact-appearance typed
    deviations, RAST.13; issue14497 only trips at 72 DPI), gross `diff>=25` **106**
    files @150 (flat 45 + ghent 38 + govdocs1 21 + 2 verapdf, SL-2.CONF.04 open).
    Prepress ghent: ≤25% 60.0% (57/95) vs the pair band 73.7–77.9 — selis trails its
    own envelope class there, tracked not gated.
  - **Promotions (measurement, SL-0.ORACLE.04 floor applied):**
    * `text` **None → Parse (2)**. Extraction is built and measured (median 1.000; the
      TEXT.08/09/10 chain holds across the whole corpus), but **Render is withheld**: the
      text-bearing slice fails the calibrated bars on its own population (≥50 chars,
      n=634/635: ≤2% 37.9% vs the pair floor ≈77, ≤25% 89.9% vs Bar A ≥95 — the
      CONF.02 pass read 37.8/89.8; the two-independent-
       oracle legs are unmeasured post-merge (below), and the paint-time `Tm`
       residual SL-3.TEXT.11 filed as SL-3.TEXT.12 mis-places whole words. **Extract
       is withheld too**, and not merely because
      56.05% (CONF.05 re-run: 56.04%) ≪ the ≥98%-on-95% wording: per ORACLE.04 the G3 gate as published sits
      *under* the oracle-noise ceiling (pdfium↔pdfjs reach 79.0%, mutool pairs ≈72.7)
      — reading the Extract rung requires a CONF.03-style recalibration first
      (SL-3.CONF.03); this wave deliberately does not re-draw the bar to go green.
    * `render` stays Parse — Bar B passing on one independent oracle is not the rung;
      the pinned-container two-oracle confirmation (still unmeasured — see the CI
      re-pin note) and the SL-2.CONF.04 gross-cluster triage gate it.
    * `filters` (encoders) stays Parse — SL-1.FILT.08 contained JPX under the wasm
      host but produced **no oracle tolerance leg** (Ghostscript comparison unmeasured);
      `encryption`/`document`/`annot`/`forms` unchanged (ENC.07/09 shipped pending human
      review; no new ladder evidence from this wave).
    Published via `cargo xtask conformance report` → `conformance/REPORT.md`.
  - **CI oracle re-pin + what was actually red:** two defects stack up on the
    pinned-container legs, and the run evidence carries both strings
    (`34806315351`/`34946893403`: every leg `oracle_rejects`, 0 comparable).
    *Pulls:* the 2026-09-09 manifests no longer resolve —
    `Unable to find image 'ghcr.io/wertcore/selis-pdf/oracle-pdfium@sha256:162ef39f...'`
    (pdfjs's `@9105570...` and mupdf's `@89be099d...` in the pair/cali-
    bration legs) — so the earlier retraction of "stale digests" was wrong:
    the *oracle-images* rebuild/push (run `34820871908`, drivers with
    `--text`) is what `xtask/oracles.toml` now records, which is the fix for
    this half. *Binds:* on legs that do pull, a `-v` to a not-yet-written
    host **file** becomes a **directory** inside the container, so the tools
    fail with `pdfium_driver: cannot write /out.img` / Node `EISDIR` / pair
    legs' `cannot remove '/out.img': Device or resource busy`; the CONF.02-
    era `absolutize()` fixed the *relative*-path variant of that error, not
    the file-vs-directory one, and the text sweep additionally lost the
    `mutool`→`mupdf` pin alias (`mutool: no [tool.mutool] pin recorded`) and
    mounts its output relative (`sweep-text/tmp-w0/oracle.txt includes
    invalid characters for a local volume name`). All three are fixed in
    4e7f40d4: `/out` parent-directory binds via `out_dir_bind()` (created,
    absolutised) in both plans, `pin_id()` on the text plan, plus regression
    tests. With *both* halves addressed the legs can finally measure, so any
    CI cell quoted here from before 2026-09-14 is treated as unconfirmed
    (the retained artifacts return zeros) — CONF.06 asserts on real counts.
    **Honest leftover:** confirmation is a *post-merge* scheduled run (dispatch is
    push-scoped; this branch does not push), so no PDFium/pdf.js number is claimed
    here and the mutool legs (local 1.23.0 / container 1.23.9, drift recorded) stay
    the only two-oracle-independent-ish pair. Render-rung and Extract-rung
    promotions wait on that artifact. One asymmetry to close with it: both the
    render and text steps exit 0 even when a pinned leg rejects *every* file —
    CI has been green while measuring nothing. `render-conf` should fail a leg
    whose oracle side produces 0 comparable pages with a pin/driver present
    (that is the "loud failure" SL-0.ORACLE.04 asked for, filed as SL-3.CONF.06 so
    this wave stays non-blocking).
  - **Filed here (the remaining-cause obligations from the TEXT notes):**
    * **SL-3.TEXT.14** — annotation `/AP` appearance text walk (62 silent-cohort files).
    * **SL-3.TEXT.15** — the >5 % silent-divergence watchdog into the sweep's
      `text_err` (TEXT.10's unwired clause; the ids TEXT.12/13 went to TEXT.11's own
      residuals).
    * **SL-3.CONF.03** — re-baseline the Extract bar on the oracle-vs-oracle floor and
      triage the 540-file `diff>=25` cohort into root-cause clusters (expect ≈20,
      `xtask oracle triage` over the sweep verdicts).
    * **SL-3.CONF.04** — record the MuPDF per-glyph word-split cohort's verdicts
      (OracleBug vs OurBug) in the expectation records.
    * **SL-3.CONF.05** — post-SL-3.TEXT.11 text+render golden re-baseline and the G2
      re-measure (TEXT.11's own obligation; landed in this wave).
    * **SL-3.CONF.06** — make a 0-comparable pinned-container leg fail its
      `render-conf` job and record the first two-oracle confirmations.
- [ ] **SL-3.TEXT.14 — Annotation appearances contribute text** · deps: TEXT.01,
  TEXT.10 · owner: AI+ · **filed by SL-3.CONF.02** (renumbered from the first filing:
  TEXT.12/13 went to SL-3.TEXT.11's own residuals)
  - **Defect:** 62 corpus files extract zero characters while MuPDF recovers annotation
    text (widget `/Tx` captions, FreeText contents, form-field appearances); MuPDF merges
    `/AP` streams into the page, we do not, and the TEXT.10 low-confidence marker stays
    silent because the page stream genuinely drew nothing (`empty_selis`, artifacts
    `C:\selis-build\conf02-text\verdicts.jsonl`; 33 flat annotation fixtures + 17 govdocs +
    10 verapdf forms/interactive + 2 synthetic). Reproduce: `mutool draw -F txt` on
    `annotation-tx2` yields `tx annotation` where selis yields nothing.
  - **Do:** Walk `/Annots` appearances into the text assembly in
    order — same encoding chain as page content, same word/space rules — for extraction,
    search, and the reading-order tree; render-side `/AP` inclusion policy unchanged.
  - **Files:** `crates/selis-pdf-engine` (text walk), `apps/cli/src/extract.rs`, corpus +
    sweep expectations.
  - **DoD:** The CONF.02 sweep's `empty_selis` cohort is empty or each file carries a typed
    deviation; per-file deltas published in the next CONF gate; no silent zero on a page
    whose annotations carry text.
- [ ] **SL-3.TEXT.15 — Flag divergent extractions that recover nothing** · deps: TEXT.10 ·
  owner: AI+ · **filed by SL-3.CONF.02 (SL-3.TEXT.10's unwired DoD clause; renumbered
  from the first filing)**
  - **Do:** The sweep must surface ">5 % of characters diverge with zero recovery" as a
    typed signal (`text_err` channel, `xtask oracle text-sweep` + engine low-confidence),
    not only the fully-silent case the `LOW_CONFIDENCE_MARKER` covers today.
  - **DoD:** Sweep verdicts carry the flag; a corpus case pins both the flagged and the
    exact-match neighbour so the threshold cannot drift silently.
- [ ] **SL-3.CONF.03 — Recalibrate the Extract gate; triage the diff≥25 long tail** ·
  deps: CONF.02, ORACLE.04 · owner: AI+
  - **Do:** ORACLE.04 measured that independent extractors top out at 79.0 % (pdfium↔pdfjs)
    and ≈72.7 % (mutool pairs) agreement at ≥0.98 on their own curated smoke set — the G3
    criterion "≥98 % similarity on ≥95 % of the extractor corpus" measures noise at that
    depth and can never be *met*; write the recalibrated text-gate bars the same way
    SL-2.CONF.03 re-baselined G2 (envelope + Bar B-style tolerance), *with the calibration
    data as justification*, then cluster the 540-file `diff>=25` residue from the
    post-TEXT.11 sweep `C:\selis-build\conf05-text` (source/font/similarity/rtl/truncated
    axes — flat 191 / verapdf 196 / govdocs1 90 / ghent 61 / synthetic 2, ghent 0/94 in
    G3 and has_type3 10/63 the standing suspects, 15 rtl-flagged, 3 truncated) into ~20
    root-cause clusters and file one task per confirmed cause. This is the gate's recalibration step — it does not
    promote any area.
  - **DoD:** Bars published in 21-TESTING/§5 with the measured floor; each cluster has a
    task or an annotated expectation record; the ladder stays at its CONF.02 levels.
- [ ] **SL-3.CONF.04 — Verdict the MuPDF per-glyph word-split cohort** · deps: CONF.02 ·
  owner: AI+
  - **Do:** TEXT.09 flagged the short-text veraPDF fixtures that flipped
    `diff<25 → diff>=25` because *MuPDF itself* breaks `"Hello world"` into
    `"H e llo …"` (per-glyph advance space inference) where our unified output is
    plausibly the spec-correct reading. Measured on both CONF sweeps the family is
    **27** files, identical before and after SL-3.TEXT.11: 13 at `selis` 12 / oracle 11
    chars with `sim 0.0` (the `6-3-8-t01-pass-*` and `6-2-11-3-x-t01/t02-*` composites),
    and 14 short-text mid-band rows at `sim 0.043–0.58` (`6-3-5-t03-fail-*`,
    `6-2-11-7-2-t01-*`, `7.21.3.3-t01..03-fail-a`, `8.4.5.4-t01..03-fail-a`,
    `7.2-t25-*`) — TEXT.09's eight plus the CMap families and re-anchor growth
    (SL-3.CONF.02 first logged "13" by an over-narrow filter; the counts here are the
    reproducible ones: see the char columns in `C:\selis-build\conf05-text\verdicts.jsonl`).
    Run them through the §21-TESTING §5 verdict workflow and record the annotation in
    `corpus/expect` (OracleBug with the §9.4.3 citation if MuPDF's spacing is the
    deviation, OurBug if ours is).
  - **DoD:** Every cluster file carries a `[annotation]` verdict record; the sweep gap
    list stops carrying them unlabelled.
- [x] **SL-3.CONF.05 — Post-SL-3.TEXT.11 sweep and golden re-baseline** · deps: CONF.02,
  TEXT.11 · owner: AI+
  - **Do:** SL-3.TEXT.11 (text-matrix pre-multiply, §9.4.3) moves extraction and layout for
    the non-identity-`Tm` corpus. Once it landed, re-run both CONF gates on the merged tree,
    refresh the sweep expectations (`corpus expect-merge`, golden/text/positions re-anchor),
    and publish the readout deltas — the same obligation TEXT.10 recorded against the BT fix.
  - **Status (2026-09-16):** Done on the merge tree (base `c8cbd904` ⊇ the TEXT.11 advance
    mapping + the CONF.02/05 gate work; `corpus verify` clean: 3,862 checked, 0 changed).
    Both gates re-measured, text vs the pinned release binary (sha256
    `0374f3f1…b6fa3099`) and local `mutool 1.23.0` (drift 1.23.9 recorded); artifacts
    `C:\selis-build\conf05-text` + `C:\selis-build\conf05-render`:
    * text: 1,747 comparable, **979 ≥0.98 (56.04%**) / 970 ≥0.99 / 540 `diff>=25` / 62
      `empty_selis` / 14 `empty_oracle` / 34 `oracle_rejects` / 4 timeouts / 23 both_reject /
      17 selis_rejects — the 03ece8ba pass reproduced **within one file per count**: the +1
      match is `/synthetic/bugfix_text11_tm_scaled` (swept identical, renders at 0.09%), the
      +1 tail file is `…_tm_rotated` (`sim 0.444`, the vertical-run word split SL-3.TEXT.13
      owns). Mean 0.7612 / median 1.000; cohorts flat 191 + verapdf 196 + govdocs1 90 +
      ghent 61 + synthetic 2.
    * render: @150 n=3,774 ≤0.5 67.9 / ≤1 74.9 / ≤2 **80.9** / ≤5 86.9 / ≤10 92.6 / ≤25
      **97.2**, p50 0.07 — Bar A and Bar B still pass on the MuPDF leg; gross `diff>=25` 105
      (flat 45 / ghent 38 / govdocs1 20 / verapdf 2), `blank_selis` 2 (both typed
      /Redact-appearance `6.3.3-t01-fail-b`, RAST.13), `blank_oracle` 1, `issue14497` blank
      at 72 only; ghent ≤25% 60.0 (vs 73.7–77.9 pairs) and the ≥50-char text cohort ≤25%
      89.9 / ≤2% 37.9 — i.e. TEXT.11 is band-neutral for pixels *and* for the withhold
      reasoning: extraction still cannot say Render.
    * `conformance/areas.toml` re-quotes these merged-tree numbers (rows + promotion record);
      no rung moves; no `corpus expect-merge` re-anchor was needed — TEXT.11's own merge
      anchored them, which is what the clean `corpus verify` above demonstrates.
    * the pinned-container two-oracle legs stay unmeasured: SL-3.CONF.02's bind/pin fixes +
      the run-34820871908 digests make them *producible* now, and a 0-comparable leg should
      fail its job (SL-3.CONF.06), but until a post-merge `render-conf` run exists
      the ladder still reads one oracle for render and one for extract.

- [ ] **SL-3.CONF.06 — Make a 0-comparable pinned-container leg red; record the
  first two-oracle confirmations** · deps: CONF.02, CONF.05 · owner: AI
  - **Do:** The `render-conf` render/text steps exit 0 when a whole leg is typed
    `oracle_rejects`: correct taxonomy, and while the container binds were broken it
    hid a *green job measuring nothing* (runs 34806315351/34946893403). With the
    bind/alias fixes + run-34820871908 digests this wave carries, a leg with
    a resolved pin should *fail* its step if 0 of its outcomes are comparable, quoting
    the first few typed details, and the first green post-merge run must then supply the
    PDFium and pdf.js cells the §4 calibration table still lacks.
  - **Files:** `xtask/src/sweep.rs` / `xtask/src/text_sweep.rs` exit path,
    `.github/workflows/ci.yml` (`render-conf`), the §4/§5 cells in
    `21-TESTING-AND-ORACLES.md`, `conformance/areas.toml` rows + regenerated report.
  - **Status (2026-09-20):** the fail-loud half is implemented and unit-tested; the
    two-oracle half cannot be recorded from this branch.
    * `xtask/src/sweep.rs::check_comparable_or_fail` and the mirror in
      `text_sweep.rs` run *after* the report is written, so a leg whose every
      outcome is a rejection/timeout (0 comparable) now exits non-zero, quoting the
      first three typed details (`pdfium_driver: cannot write /out.img` is the
      string the historical runs carried). Unit tests pin both directions: a
      synthetic all-`oracle_rejects` leg fails with the tool name, the count and a
      quoted detail; a measured leg stays green. The CI uploads gained `if: always()`
      so the failing step's report + verdicts still reach the artifacts for triage.
    * Verified locally: `cargo test -p xtask --bin xtask -- sweep::` 25/25 green;
      `cargo fmt --check` clean; `cargo clippy -p xtask --all-targets` adds no new
      warning (the two pre-existing `sweep.rs` `ptr_arg`/`unnecessary_mut_passed`
      notes reproduce on `ec3472cf` without this branch's changes).
    * **The two-oracle cells are still unmeasured, and this box records that
      honestly rather than closing on the gate alone.** The last scheduled run is
      34946893403 (2026-09-15, `03ece8ba`), which predates the SL-3.CONF.02 bind
      fix — no post-fix scheduled `render-conf` exists. Cron is `0 3 */2 * *`, and
      the plan records that the next window after this merge carries the first
      credible legs. Dispatch is push-scoped and this task does not push
      (no `git push`, no `gh workflow run`), so the run cannot be produced from
      here; the DoD's "green with non-zero comparable pages for pdfium and pdf.js"
      is the one clause held open, owned by the next scheduled run rather than by
      another code change. Promotion of the `render`/`text` rungs stays a separate
      decision (CONF.03 owns the Extract recalibration; `render` waits on the
      two-oracle cells + SL-2.CONF.04).
  - **DoD clause-by-clause:** (1) a synthetic all-`oracle_rejects` leg turns the job
    red — **met** (unit-tested both directions); (2) the post-merge scheduled run is
    green *with* non-zero comparable pages for pdfium and pdf.js — **open**, no
    post-bind-fix scheduled run exists (last: 34946893403 on `03ece8ba`), and cannot
    be produced from a branch that does not push; (3) the Render-withhold re-argument
    — **blocked on (2)**. The box stays unchecked for exactly the reason
    SL-3.FONT.10's does: the shipped half is real, the unshipped half is named.
