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

- [ ] **SL-3.FONT.02 — Encoding and character mapping** · deps: FONT.01 · owner: AI+
  - **Do:** StandardEncoding, WinAnsiEncoding, MacRomanEncoding, MacExpertEncoding, `/Differences`,
    the built-in font encoding, symbolic-vs-nonsymbolic TrueType cmap selection rules (3,0 vs 3,1
    vs 1,0 — the single messiest area of the spec), and the `/Encoding` precedence order.
  - **DoD:** A table-driven test over the encoding decision matrix; corpus `font-encoding`
    including symbolic TrueType fonts that other readers get wrong.
  - **Risk:** Budget more time than seems reasonable. This is the most common cause of "the text
    renders as boxes" bugs, and the rules are genuinely ambiguous in places. Document every
    decision with a spec citation or an observed-behaviour note.

- [ ] **SL-3.FONT.03 — Embedded TrueType/OpenType** · deps: FONT.01 · owner: AI+
  - **Do:** `ttf-parser` for tables; glyph outlines including composite glyphs; broken-font
    tolerance (bad `loca`, missing `hmtx`, wrong `numGlyphs`).
- [ ] **SL-3.FONT.04 — Embedded CFF / Type1C** · deps: FONT.01 · owner: AI+
  - **Do:** CFF charstrings (Type 2), subrs, hintmask handling, seac, and CID-keyed CFF with FDSelect.
- [ ] **SL-3.FONT.05 — Embedded Type1 (PFB/PFA)** · deps: FONT.04 · owner: AI+
  - **Do:** eexec decryption, Type 1 charstrings, `/Subrs`, flex and hint-replacement, and the
    seac composite mechanism. Convert internally to the same outline representation as CFF.
  - **Note:** Type 1 is obsolete and ubiquitous in old documents. Skipping it means failing on
    exactly the archive documents users most need a tool for.
- [ ] **SL-3.FONT.06 — Type3 fonts** · deps: FONT.01, SL-2.CONT.04 · owner: AI+
  - **Do:** Glyph procedures as content streams with `/FontMatrix`, `d0`/`d1`, and their own
    resource dictionary. Depth-budgeted — a Type3 glyph can draw another Type3 font.
- [ ] **SL-3.FONT.07 — CID fonts, CMaps, and predefined CMap resources** · deps: FONT.03 · owner: AI+
  - **Do:** `/Encoding` as a predefined CMap name or an embedded CMap stream; CIDToGIDMap;
    the Adobe-Japan1/GB1/CNS1/Korea1/KR registries; vertical writing (`/WMode 1`) with the
    `/W2` metrics and correct vertical origin.
  - **DoD:** Corpus `cjk` renders at G2 tolerance including a vertical-writing Japanese document.
- [ ] **SL-3.FONT.08 — Standard-14 metric-compatible substitution** · deps: FONT.02, SL-0.LEAD.07 · owner: AI+
  - **Do:** Ship metric-compatible substitutes for Helvetica/Times/Courier/Symbol/ZapfDingbats
    with the *exact* AFM widths, so unembedded-font documents lay out identically to Acrobat.
    Using the real AFM metrics with a substitute outline is the correct approach.
  - **DoD:** A document using all 14 fonts matches PDFium's layout within 0.5 px per line.
- [ ] **SL-3.FONT.09 — Fallback chain for arbitrary unembedded fonts** · deps: FONT.08 · owner: AI+
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
- [ ] **SL-3.FONT.11 — Font subsetting and re-embedding** · deps: FONT.03, FONT.04 · owner: AI+
  - **Do:** Subset TrueType and CFF to a glyph set, rebuild `loca`/`hmtx`/`cmap`/charstrings, and
    **merge new glyphs into an existing subset** — required by ADR-P0024, because editing text adds
    characters the original subset lacks.
  - **DoD:** Round-trip: subset → embed → parse → render matches the original; a test that adds a
    glyph absent from the original subset and renders it correctly.
  - **Risk:** This is the task that makes text editing possible. If it slips, ADR-P0024 slips.
- [ ] **SL-3.FONT.12 — Font-program fuzzing** · deps: FONT.05 · owner: AI
  - **DoD:** Fuzz targets for TrueType, CFF, Type1, and CMap parsing; 8 h clean each.

---

## 3.SHAPE — Shaping and layout (for authored and reflowed text)

- [ ] **SL-3.SHAPE.01 — `Shaper` trait + `rustybuzz` backend** · owner: AI+
  - **Do:** Script itemisation, feature application, cluster mapping. Used for new/edited text
    only — existing content is replayed by glyph id, never re-shaped.
  - **DoD:** The trait boundary keeps `rustybuzz` out of the replay path entirely (checked by
    `check-layers`).
- [ ] **SL-3.SHAPE.02 — Bidi and RTL** · deps: SHAPE.01 · owner: AI+
  - **Do:** UAX #9 via `unicode-bidi`, paragraph direction detection, and mirroring.
  - **DoD:** Corpus `rtl` (Arabic, Hebrew) renders and extracts in correct logical order.
- [ ] **SL-3.SHAPE.03 — Line breaking and justification** · deps: SHAPE.02 · owner: AI+
  - **Do:** UAX #14 line breaking, plus PDF-specific justification (word spacing vs. char spacing
    vs. horizontal scaling) so reflowed text matches the original paragraph's visual style.
- [ ] **SL-3.SHAPE.04 — Indic and complex-script validation** · deps: SHAPE.01 · owner: AI
  - **DoD:** Corpus `indic` (Devanagari, Tamil, Bengali) at G2 tolerance.

---

## 3.TEXT — Extraction and reading order

- [ ] **SL-3.TEXT.01 — Text-showing operators and positioning** · deps: SL-2.CONT.02 · owner: AI+
  - **Do:** `Tj TJ ' "`, text matrix vs text line matrix, `Td TD Tm T*`, and the full advance
    formula including char spacing, word spacing (byte-0x20-only rule for simple fonts, and the
    trap that it does **not** apply to 2-byte CID codes), horizontal scaling, and rise.
  - **DoD:** Glyph positions match PDFium to sub-pixel on the text corpus.
- [ ] **SL-3.TEXT.02 — ToUnicode and text recovery** · deps: FONT.02 · owner: AI+
  - **Do:** `/ToUnicode` CMap parsing; fall back through the encoding's glyph names →
    Adobe Glyph List → the `uniXXXX`/`uXXXX` name conventions → the font's own cmap reverse map.
    Report a per-run confidence.
  - **DoD:** Extraction corpus ≥98% agreement with PDFium; files with no `/ToUnicode` and symbolic
    encodings are correctly flagged low-confidence rather than emitting mojibake silently.
- [ ] **SL-3.TEXT.03 — Run, word, and line assembly** · deps: TEXT.01 · owner: AI+
  - **Do:** Group glyphs into runs by style continuity, infer word boundaries from advance gaps
    relative to the font's space width, and assemble lines by baseline clustering.
  - **DoD:** Word segmentation matches PDFium on the extraction corpus; a test for the
    "no space characters in the content stream at all" case, which is common in generated PDFs.
- [ ] **SL-3.TEXT.04 — Reading order: structure-first, geometry-fallback** · deps: TEXT.03, SL-1.DOC.06 · owner: AI+
  - **Do:** When a structure tree exists, use it (ADR-P0031). Otherwise infer with column detection
    and an XY-cut or similar layout analysis. Return a confidence; never silently guess on a
    two-column document and produce interleaved nonsense.
  - **DoD:** 100% match against tagged order on the tagged corpus; a measured accuracy number on
    the untagged multi-column corpus, published in the conformance report.
- [ ] **SL-3.TEXT.05 — Selection geometry and hit testing** · deps: TEXT.03 · owner: AI
  - **Do:** Character-level quads for selection highlighting, caret positions, word/line/paragraph
    expansion, and RTL-correct selection ranges.
- [ ] **SL-3.TEXT.06 — Search** · deps: TEXT.03 · owner: AI
  - **Do:** Normalised search (case, diacritics, ligature decomposition, soft hyphens, and
    cross-line matches), incremental index built per resident page only.
  - **DoD:** Finds "ﬁrst" when searching "first"; finds a term broken across a line break.
- [ ] **SL-3.TEXT.07 — Structured extraction output** · deps: TEXT.04 · owner: AI
  - **Do:** Emit text as plain, or as a structured document (blocks/lines/spans with style and
    bbox), or as Markdown/HTML. Foundation for the convert product and for any AI/RAG integration.
  - **DoD:** JSON schema versioned and documented; `selis extract --format=json|text|md|html`.

---

## 3.CONF — Conformance checkpoint

- [ ] **SL-3.CONF.01 — Full text/font differential sweep** · owner: AI+
  - **Do:** Render + extract the whole corpus against PDFium and pdf.js; triage; file per root cause.
- [ ] **SL-3.CONF.02 — Promote conformance areas; publish the report** · owner: AI
