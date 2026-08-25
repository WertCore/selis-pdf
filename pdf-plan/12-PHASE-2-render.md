# Phase 2 — Rendering (Weeks 10–17)

**Gate G2 exit criteria:** perceptual diff vs PDFium ≤ 0.5% differing pixels on ≥95% of the render
corpus at 150 DPI · transparency groups, soft masks, and shadings 1–7 implemented · deterministic
raster proven across Linux/macOS/WASM · render throughput ≥ 0.6× PDFium on the benchmark set.

This is the longest and least forgiving phase. Rendering is where "mostly right" is visible to
every user on every page.

---

## 2.CONT — The content-stream interpreter

- [x] **SL-2.CONT.01 — Content tokeniser + operator dispatch** · owner: AI+
  - **Do:** Tokenise content streams (a different grammar from COS — no indirect refs, inline
    images embedded in the token stream) and dispatch all ~73 operators. Unknown operators are
    recorded as a deviation and skipped with correct operand consumption, never fatal.
  - **Files:** `crates/selis-pdf-content/src/{lex.rs,dispatch.rs}`
  - **DoD:** Every operator in ISO 32000-2 Table 50 has a handler or an explicit "ignored, and why";
    fuzz target `content_interp`; corpus `content-operators`.
  - **Risk:** Operand-count recovery after an unknown operator is the difference between "one
    missing glyph" and "the rest of the page is garbage". Test it deliberately.

- [x] **SL-2.CONT.02 — Graphics state machine** · deps: CONT.01 · owner: AI+
  - **Do:** The full state: CTM, colour spaces and colours (stroke/fill separately), line width,
    cap, join, miter, dash, rendering intent, flatness, smoothness, stroke adjustment, blend mode,
    soft mask, alpha constants, alpha-is-shape, text state (font, size, char/word spacing,
    horizontal scale, leading, rise, render mode), and `q`/`Q` stack with depth budgeting.
  - **API:** `struct GState` + `struct GStateStack`
  - **DoD:** `q` without `Q` at stream end, and `Q` underflow, both handled per spec and tested;
    `/ExtGState` merging tested for every key.

- [x] **SL-2.CONT.03 — Path construction and painting** · deps: CONT.02 · owner: AI+
  - **Do:** `m l c v y h re`, painting operators `S s f F f* B B* b b* n`, and clipping `W W*`
    with the "clip takes effect after the painting op" rule that is easy to get subtly wrong.
  - **DoD:** Corpus `paths` including degenerate cases: zero-length subpath with round caps (must
    paint a dot), zero-area fill, self-intersecting even-odd fills.

- [x] **SL-2.CONT.04 — Form XObjects with depth budgeting** · deps: CONT.02 · owner: AI+
  - **Do:** `Do` for form XObjects: resource-dictionary scoping (including inheritance from the
    page when the form omits `/Resources` — a real-world case), `/Matrix`, `/BBox` clipping, and
    recursion depth via the worklist pattern (no native recursion).
  - **DoD:** A self-referential form XObject terminates with `DEPTH_EXCEEDED`; corpus `xobject-form`.

- [ ] **SL-2.CONT.05 — Inline images (`BI`/`ID`/`EI`)** · deps: CONT.01 · owner: AI+
  - **Do:** Including the notorious `EI` detection problem — binary image data may contain the
    bytes `EI`. Implement length-driven detection where `/L` is present and heuristic scanning
    with validation where it is not.
  - **DoD:** Corpus `inline-images` including a file whose image data contains `EI`; matches PDFium.

- [x] **SL-2.CONT.06 — Marked content and the MCID map** · deps: CONT.01 · owner: AI
  - **Do:** `BMC BDC EMC`, the marked-content stack, and a page-level MCID → content-range map.
    Required by `selis-pdf-text` for structure-driven reading order and by `selis-pdf-redact` to find what a
    region contains. Per ADR-P0031, not optional.

- [x] **SL-2.CONT.07 — Display-list IR** · deps: CONT.03 · owner: AI+
  - **Do:** Define and emit the immutable IR of `01-ARCHITECTURE.md §8`: an arena-allocated,
    serialisable op list with resolved (not referenced) state per op. Must be `Send`, cacheable,
    and diffable.
  - **API:** `fn build_display_list(page, budget) -> Result<DisplayList>`
  - **DoD:** Serialise→deserialise round-trip; a structural differ that reports "op 412 changed
    fill colour" rather than "pixels differ"; memory per op measured and budgeted.
  - **Note:** Everything downstream — raster, text, redaction, edit, print, convert — consumes
    this. Getting the IR right is worth a week of design.

- [x] **SL-2.CONT.08 — Resumable/incremental interpretation** · deps: CONT.07, SL-1.COS.07 · owner: AI+
  - **Do:** Interpretation yields on `Pending` and on budget-tick, so a huge page can be rendered
    progressively and cancelled instantly.
  - **DoD:** A 50 MB content stream renders progressively; cancellation returns within one tick.

---

## 2.COLOR — Colour

- [ ] **SL-2.COLOR.01 — Device spaces + `/ColorSpace` resolution** · owner: AI+
  - **Do:** DeviceGray/RGB/CMYK, `/CS` `/cs` `/SC` `/SCN` `/sc` `/scn` `/G` `/RG` `/K` operators,
    and the default-colour-space overrides (`/DefaultRGB` etc.).
- [ ] **SL-2.COLOR.02 — CIE spaces: CalGray, CalRGB, Lab** · deps: COLOR.01 · owner: AI+
- [ ] **SL-2.COLOR.03 — ICCBased + profile handling** · deps: COLOR.02 · owner: AI+
  - **Do:** `qcms` or `moxcms` behind `selis-color::IccEngine`. Fall back to the `/N`-implied device
    space when a profile is broken — never fail a page over a bad profile.
  - **DoD:** Corpus `color-icc` including a corrupt profile that must degrade gracefully.
- [ ] **SL-2.COLOR.04 — Indexed, Separation, DeviceN + tint transforms** · deps: COLOR.03 · owner: AI+
  - **Do:** Requires PDF functions (FUNC.01) for the tint transform. Handle `/All` and `/None`
    separations correctly — `/None` paints nothing, which is a correctness trap.
- [ ] **SL-2.COLOR.05 — PDF functions types 0, 2, 3, 4** · owner: AI+
  - **Do:** Sampled (type 0, with all interpolation orders), exponential (2), stitching (3), and
    the **PostScript calculator (type 4)** — a small stack language that must be budget-bounded
    and cannot be allowed to loop.
  - **Files:** `crates/selis-color/src/function/`
  - **DoD:** Fuzz target `pdf_function`; a type-4 function with a pathological expression hits the
    budget; numeric results match Ghostscript within 1e-6 on a generated test set.
- [ ] **SL-2.COLOR.06 — Blend modes** · deps: COLOR.01 · owner: AI+
  - **Do:** All separable modes and the four non-separable (Hue, Saturation, Color, Luminosity),
    in the correct blend colour space.
  - **DoD:** A generated matrix of every blend mode × backdrop × source, compared to PDFium.
- [ ] **SL-2.COLOR.07 — Overprint and overprint simulation** · deps: COLOR.04 · owner: AI
  - **Note:** Matters only for the print/prepress audience. Ship at `Identify` level in Phase 2,
    promote in Phase 9 if the prepress market is pursued.

---

## 2.RAST — The rasteriser

- [ ] **SL-2.RAST.01 — `raster::Backend` trait + `tiny-skia` implementation** · owner: AI+
  - **Do:** The abstraction of ADR-P0008: fill path, stroke path, draw image, push/pop layer,
    set clip, set blend. `tiny-skia` behind it. Deliberately narrow so a replacement is contained.
  - **DoD:** Backend contract documented; a `RecordingBackend` for tests that asserts the call
    sequence rather than pixels.
- [ ] **SL-2.RAST.02 — Fill and stroke with PDF semantics** · deps: RAST.01 · owner: AI+
  - **Do:** Nonzero and even-odd winding, PDF's stroke geometry (including the zero-width line =
    thinnest-renderable rule, and dash-phase semantics), and miter-limit behaviour.
  - **DoD:** Corpus `strokes`; zero-width and hairline cases match PDFium at 72/150/300 DPI.
- [ ] **SL-2.RAST.03 — Clipping, including text clip modes** · deps: RAST.02 · owner: AI+
  - **Do:** Intersecting clip paths, and text render modes 4–7 which add glyphs to the clip path.
  - **DoD:** Corpus `clip-text`; nested clip depth budgeted.
- [ ] **SL-2.RAST.04 — Transparency groups** · deps: RAST.03, COLOR.06 · owner: AI+
  - **Do:** Isolated/non-isolated, knockout/non-knockout, group colour space, and the correct
    compositing formula. This is the hardest correctness problem in PDF rendering.
  - **DoD:** The generated transparency matrix (isolated × knockout × blend × alpha) matches PDFium
    within tolerance; corpus `transparency`.
  - **Risk:** Budget two full weeks. Non-isolated non-knockout groups are where every renderer,
    including the well-funded ones, has bugs. Compare against *two* oracles here, not one.
- [ ] **SL-2.RAST.05 — Soft masks (luminosity and alpha)** · deps: RAST.04 · owner: AI+
  - **Do:** `/SMask` in ExtGState, backdrop colour, transfer functions.
- [ ] **SL-2.RAST.06 — Shadings 1–7** · deps: COLOR.05, RAST.03 · owner: AI+
  - **Do:** Function-based (1), axial (2), radial (3), free-form Gouraud triangle mesh (4),
    lattice-form (5), Coons patch (6), tensor-product patch (7). Types 4–7 need mesh decoding from
    a packed bit stream.
  - **DoD:** Corpus `shading-1` … `shading-7`, each matching PDFium; the radial "extend" cone cases
    are explicitly tested — they are a classic source of wrong output.
- [ ] **SL-2.RAST.07 — Tiling patterns** · deps: RAST.04 · owner: AI+
  - **Do:** Coloured (type 1) and uncoloured (type 2), `/XStep`/`/YStep`, pattern matrix relative
    to the *default* page space (not the current CTM — a common bug), and a bounded tile cache.
  - **DoD:** Corpus `patterns`; a pattern with a tiny XStep hits the pixel budget rather than
    rendering for an hour.
- [ ] **SL-2.RAST.08 — Image drawing** · deps: RAST.02, SL-1.FILT.05 · owner: AI+
  - **Do:** Image XObjects, image masks (`/ImageMask`), `/SMask`, `/Mask` (both stencil and colour-key),
    `/Decode` arrays, interpolation, and correct sampling at extreme downscales and upscales.
  - **DoD:** Corpus `images`; a 20 000×20 000 image at 1% scale does not allocate the full bitmap.
- [ ] **SL-2.RAST.09 — Anti-aliasing policy + determinism** · deps: RAST.02 · owner: AI+
  - **Do:** Fix the AA approach and prove `01-ARCHITECTURE.md` determinism: same bytes twice, and
    across x86-64 Linux, arm64 macOS, and WASM.
  - **DoD:** The `cross-platform-determinism` CI job goes green here. Any SIMD path must prove
    bit-identity or be disabled.
- [ ] **SL-2.RAST.10 — Tile decomposition and parallel rasterisation** · deps: RAST.09 · owner: AI+
  - **Do:** Split a page into tiles, render in parallel via `rayon` where threads exist, sequential
    on single-threaded WASM. **Output must be identical either way** — that is the test.
  - **DoD:** Threaded and unthreaded renders hash-equal on the whole corpus.
- [ ] **SL-2.RAST.11 — Render parameter surface** · deps: RAST.10 · owner: AI
  - **Do:** DPI/matrix, target colour space, alpha/no-alpha, annotation inclusion, optional-content
    configuration, "for print" vs "for screen", text-rendering hints, and a `RenderIntent`.
  - **DoD:** Every parameter has a corpus case proving it changes output as documented.

---

## 2.FILT — Codec completion

- [ ] **SL-2.FILT.01 — JBIG2 symbol dictionary, text region, refinement** · deps: SL-1.FILT.07 · owner: AI+
  - **DoD:** Full JBIG2 corpus matches Ghostscript; fuzz target sustained 8 h.
- [ ] **SL-2.FILT.02 — Progressive/arithmetic JPEG edge cases** · deps: SL-1.FILT.05 · owner: AI

---

## 2.PERF — Performance work

- [ ] **SL-2.PERF.01 — Display-list and tile caching in `selis-pdf-engine`** · deps: CONT.07 · owner: AI+
  - **Do:** ADR-P0025: LRU under a memory budget, keyed by (page, matrix, params, revision).
    Invalidation on edit is exact, not "clear everything".
  - **DoD:** Cache-hit benchmark; a mutation invalidates only affected entries.
- [ ] **SL-2.PERF.02 — Meet the render throughput budget** · deps: RAST.10 · owner: AI+
  - **DoD:** ≥0.6× PDFium at G2 on the benchmark set; a profile report naming the top 10 costs.
- [ ] **SL-2.PERF.03 — WASM-specific render optimisation** · deps: PERF.02 · owner: AI+
  - **Do:** SIMD128 where it is provably deterministic, memory-growth strategy, and avoiding the
    JS↔WASM boundary in the tile path.
  - **DoD:** WASM within 2.5× of native on the same machine.

---

## 2.CONF — Conformance checkpoint

- [ ] **SL-2.CONF.01 — Full-corpus differential render sweep** · deps: RAST.11 · owner: AI+
  - **Do:** Render the whole corpus at 72/150/300 DPI against PDFium and pdf.js; triage per
    `SL-0.ORACLE.05`; file a task per root cause.
  - **DoD:** The G2 threshold met, or an explicit, itemised list of what is not met and why.
- [ ] **SL-2.CONF.02 — Promote conformance areas** · deps: CONF.01 · owner: AI
  - **Do:** Update `conformance/areas.toml`. Areas that did not reach `Render` stay at `Parse` and
    are marked as such publicly (ADR-P0010).
