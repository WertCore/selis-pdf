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

- [x] **SL-2.CONT.05 — Inline images (`BI`/`ID`/`EI`)** · deps: CONT.01 · owner: AI+
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

- [x] **SL-2.COLOR.01 — Device spaces + `/ColorSpace` resolution** · owner: AI+
  - **Do:** DeviceGray/RGB/CMYK, `/CS` `/cs` `/SC` `/SCN` `/sc` `/scn` `/G` `/RG` `/K` operators,
    and the default-colour-space overrides (`/DefaultRGB` etc.).
- [x] **SL-2.COLOR.02 — CIE spaces: CalGray, CalRGB, Lab** · deps: COLOR.01 · owner: AI+
- [x] **SL-2.COLOR.03 — ICCBased + profile handling** · deps: COLOR.02 · owner: AI+
  - **Do:** `qcms` or `moxcms` behind `selis-color::IccEngine`. Fall back to the `/N`-implied device
    space when a profile is broken — never fail a page over a bad profile.
  - **DoD:** Corpus `color-icc` including a corrupt profile that must degrade gracefully.
- [x] **SL-2.COLOR.04 — Indexed, Separation, DeviceN + tint transforms** · deps: COLOR.03 · owner: AI+
  - **Do:** Requires PDF functions (FUNC.01) for the tint transform. Handle `/All` and `/None`
    separations correctly — `/None` paints nothing, which is a correctness trap.
- [x] **SL-2.COLOR.05 — PDF functions types 0, 2, 3, 4** · owner: AI+
  - **Do:** Sampled (type 0, with all interpolation orders), exponential (2), stitching (3), and
    the **PostScript calculator (type 4)** — a small stack language that must be budget-bounded
    and cannot be allowed to loop.
  - **Files:** `crates/selis-color/src/function/`
  - **DoD:** Fuzz target `pdf_function`; a type-4 function with a pathological expression hits the
    budget; numeric results match Ghostscript within 1e-6 on a generated test set.
- [x] **SL-2.COLOR.06 — Blend modes** · deps: COLOR.01 · owner: AI+
  - **Do:** All separable modes and the four non-separable (Hue, Saturation, Color, Luminosity),
    in the correct blend colour space.
  - **DoD:** A generated matrix of every blend mode × backdrop × source, compared to PDFium.
- [x] **SL-2.COLOR.07 — Overprint and overprint simulation** · deps: COLOR.04 · owner: AI
  - **Note:** Matters only for the print/prepress audience. Ship at `Identify` level in Phase 2,
    promote in Phase 9 if the prepress market is pursued.

---

## 2.RAST — The rasteriser

- [x] **SL-2.RAST.01 — `raster::Backend` trait + `tiny-skia` implementation** · owner: AI+
  - **Do:** The abstraction of ADR-P0008: fill path, stroke path, draw image, push/pop layer,
    set clip, set blend. `tiny-skia` behind it. Deliberately narrow so a replacement is contained.
  - **DoD:** Backend contract documented; a `RecordingBackend` for tests that asserts the call
    sequence rather than pixels.
- [x] **SL-2.RAST.02 — Fill and stroke with PDF semantics** · deps: RAST.01 · owner: AI+
  - **Do:** Nonzero and even-odd winding, PDF's stroke geometry (including the zero-width line =
    thinnest-renderable rule, and dash-phase semantics), and miter-limit behaviour.
  - **DoD:** Corpus `strokes`; zero-width and hairline cases match PDFium at 72/150/300 DPI.
- [x] **SL-2.RAST.03 — Clipping, including text clip modes** · deps: RAST.02 · owner: AI+
  - **Do:** Intersecting clip paths, and text render modes 4–7 which add glyphs to the clip path.
  - **DoD:** Corpus `clip-text`; nested clip depth budgeted.
- [x] **SL-2.RAST.04 — Transparency groups** · deps: RAST.03, COLOR.06 · owner: AI+
  - **Do:** Isolated/non-isolated, knockout/non-knockout, group colour space, and the correct
    compositing formula. This is the hardest correctness problem in PDF rendering.
  - **DoD:** The generated transparency matrix (isolated × knockout × blend × alpha) matches PDFium
    within tolerance; corpus `transparency`.
  - **Risk:** Budget two full weeks. Non-isolated non-knockout groups are where every renderer,
    including the well-funded ones, has bugs. Compare against *two* oracles here, not one.
- [x] **SL-2.RAST.05 — Soft masks (luminosity and alpha)** · deps: RAST.04 · owner: AI+
  - **Do:** `/SMask` in ExtGState, backdrop colour, transfer functions.
- [x] **SL-2.RAST.06 — Shadings 1–7** · deps: COLOR.05, RAST.03 · owner: AI+
  - **Do:** Function-based (1), axial (2), radial (3), free-form Gouraud triangle mesh (4),
    lattice-form (5), Coons patch (6), tensor-product patch (7). Types 4–7 need mesh decoding from
    a packed bit stream.
  - **DoD:** Corpus `shading-1` … `shading-7`, each matching PDFium; the radial "extend" cone cases
    are explicitly tested — they are a classic source of wrong output.
- [x] **SL-2.RAST.07 — Tiling patterns** · deps: RAST.04 · owner: AI+
  - **Do:** Coloured (type 1) and uncoloured (type 2), `/XStep`/`/YStep`, pattern matrix relative
    to the *default* page space (not the current CTM — a common bug), and a bounded tile cache.
  - **DoD:** Corpus `patterns`; a pattern with a tiny XStep hits the pixel budget rather than
    rendering for an hour.
- [x] **SL-2.RAST.08 — Image drawing** · deps: RAST.02, SL-1.FILT.05 · owner: AI+
  - **Do:** Image XObjects, image masks (`/ImageMask`), `/SMask`, `/Mask` (both stencil and colour-key),
    `/Decode` arrays, interpolation, and correct sampling at extreme downscales and upscales.
  - **DoD:** Corpus `images`; a 20 000×20 000 image at 1% scale does not allocate the full bitmap.
- [x] **SL-2.RAST.09 — Anti-aliasing policy + determinism** · deps: RAST.02 · owner: AI+
  - **Do:** Fix the AA approach and prove `01-ARCHITECTURE.md` determinism: same bytes twice, and
    across x86-64 Linux, arm64 macOS, and WASM.
  - **DoD:** The `cross-platform-determinism` CI job goes green here. Any SIMD path must prove
    bit-identity or be disabled.
- [x] **SL-2.RAST.10 — Tile decomposition and parallel rasterisation** · deps: RAST.09 · owner: AI+
  - **Do:** Split a page into tiles, render in parallel via `rayon` where threads exist, sequential
    on single-threaded WASM. **Output must be identical either way** — that is the test.
  - **DoD:** Threaded and unthreaded renders hash-equal on the whole corpus.
- [x] **SL-2.RAST.11 — Render parameter surface** · deps: RAST.10 · owner: AI
  - **Do:** DPI/matrix, target colour space, alpha/no-alpha, annotation inclusion, optional-content
    configuration, "for print" vs "for screen", text-rendering hints, and a `RenderIntent`.
  - **DoD:** Every parameter has a corpus case proving it changes output as documented.

- [ ] **SL-2.RAST.12 — Page `/Rotate` in the render surface** · deps: RAST.11 · owner: AI
  - **Do:** Apply page `/Rotate` (0/90/180/270) to the render canvas and CTM so a rotated page's
    output geometry matches other renderers. Found by the SL-2.CONF.01 sweep: 17 files render
    1240×1755 where the oracle renders 1755×1240 — selis paints the unrotated `/MediaBox`
    (ISO 32000-2 §14.11.2: rotation is part of the rendered page view, and `font_ascent_descent`,
    `synthetic/rotate_270`, and 15 govdocs files show it).
  - **Files:** `crates/selis-pdf-engine` (page geometry in the render path), `apps/cli/src/render.rs`.
  - **DoD:** Corpus `page-rotate` (90/180/270, plus `/Rotate` with swapped MediaBox dimensions)
    matches MuPDF at 150 DPI within tolerance; the sweep's `size_skew` cluster empties.

- [ ] **SL-2.RAST.13 — Silent blank renders (CONF.01 `blank_selis` cluster)** · deps: RAST.04 · owner: AI
  - **Do:** Root-cause the 25 files where selis paints page 1 (near-)blank while MuPDF paints
    content (`bug1743245`, veraPDF test suite `6-3-3-t01-fail-b` (annotation appearances),
    `govdocs1/000/000164`, `issue11124`, `issue11878`). Candidates: annotation `/AP` appearance
    streams, soft-mask backdrops, silently-skipped XObjects. A silent no-draw is a correctness
    bug even when the construct is otherwise unsupported — it must at least surface as a typed
    deviation.
  - **DoD:** Every cluster file either renders non-blank or reports a typed deviation; a corpus
    entry per confirmed root cause.

- [ ] **SL-2.RAST.14 — 1–2% band fidelity excess (CONF.03 calibration)** · deps: RAST.09 · owner: AI
  - **Do:** Close selis's ~36pp excess in the 1–2% differing-pixels band (42pp of pages vs the
    oracle pairs' ~6pp; CONF.01 p50 = 1.68 vs oracle pairs' p50 = 0.06). One mechanical signature
    across the corpus — investigate subpixel glyph positioning, stroke geometry rounding, and AA
    coverage scaling against the display-list IR. Success criterion: selis's ≤1% band fraction
    reaches the oracle envelope (≥ ~73%).
  - **DoD:** Root cause identified and fixed, or an ADR records why the divergence is accepted;
    CONF.03 matrix re-run showing the ≤1%/≤2% bands inside the envelope.

---

## 2.FILT — Codec completion

- [x] **SL-2.FILT.01 — JBIG2 symbol dictionary, text region, refinement** · deps: SL-1.FILT.07 · owner: AI+
  - **DoD:** Full JBIG2 corpus matches Ghostscript; fuzz target sustained 8 h.
- [x] **SL-2.FILT.02 — Progressive/arithmetic JPEG edge cases** · deps: SL-1.FILT.05 · owner: AI

---

## 2.PERF — Performance work

- [x] **SL-2.PERF.01 — Display-list and tile caching in `selis-pdf-engine`** · deps: CONT.07 · owner: AI+
  - **Do:** ADR-P0025: LRU under a memory budget, keyed by (page, matrix, params, revision).
    Invalidation on edit is exact, not "clear everything".
  - **DoD:** Cache-hit benchmark; a mutation invalidates only affected entries.
- [x] **SL-2.PERF.02 — Meet the render throughput budget** · deps: RAST.10 · owner: AI+
  - **DoD:** ≥0.6× PDFium at G2 on the benchmark set; a profile report naming the top 10 costs.
  - **Done 2026-09-10:** ratio MET — 2.78× geomean post-`main`-merge, every
    page ≥0.6× raw (release, 72 DPI native scale, median of 9; profile in
    `30-RENDER-PERF-REPORT.md`, record in `bench/render-results.json`,
    gate wired in `perf-budgets.toml`, nightly perf job regenerates it with
    the pinned driver). Honest scope: the §12 rows say @150 DPI but the
    engine has no page→device matrix yet (gap G-1, CONF.01 owns it); the
    18 ms absolute budget is NOT met (51.0 ms — gap G-2: batching atlas
    implemented, measured a 51→99 ms regression, and removed with a pinned
    probe; residual itemised in the report §9b). PERF.03 DONE below
    (unblocked by this task).
- [x] **SL-2.PERF.03 — WASM-specific render optimisation** · deps: PERF.02 · owner: AI+
  - **Do:** SIMD128 where it is provably deterministic, memory-growth strategy, and avoiding the
    JS↔WASM boundary in the tile path.
  - **DoD:** WASM within 2.5× of native on the same machine.
  - **Done 2026-09-10:** MET — 1.44× geomean (release, same machine,
    same process; every page ≤ 2.33×) via the wasmtime-driven harness
    (`xtask perf-wasm`, `crates/selis-pdf-wasm` one-call-per-page ABI per
    ADR-P0041 — no JS/browser harness exists in-repo, stated honestly).
    Guest checksums byte-identical to native on all six pages (RAST.09
    cross-arch proof, asserted per run). +simd128 adopted after
    checksum-proven determinism (2.13× → ~1.4×). Memory: exact pre-sizing
    + 512 MiB wasmtime limiter. Cold start: wasmtime compile ≈3–6 s +
    instantiate ≈1–15 ms (browsers differ — the 45 ms row stays
    not-measurable). Full ledger in `30-RENDER-PERF-REPORT.md` §9c, record
    in `bench/wasm-results.json`.

---

## 2.CONF — Conformance checkpoint

- [x] **SL-2.CONF.01 — Full-corpus differential render sweep** · deps: RAST.11 · owner: AI+
  - **Do:** Render the whole corpus at 72/150/300 DPI against PDFium and pdf.js; triage per
    `SL-0.ORACLE.05`; file a task per root cause.
  - **DoD:** The G2 threshold met, or an explicit, itemised list of what is not met and why.
  - **Done (2026-09-10):** `xtask oracle sweep` (ORACLE.05-style signature clustering over page 1
    of every corpus file, ΔE76 > 2.3, 0.5% per-file threshold) ran the full 3,836-file corpus at
    72/150/300 DPI — 11,508 (file, DPI) outcomes, 11,240 comparable — selis (per-file Viewer
    Budget, wall-clock bounded, every failure a typed outcome) vs the locally available oracle
    **MuPDF mutool 1.23.0**. Docker is unavailable on the dev host, so the PDFium/pdf.js legs run
    in the scheduled CI `render-conf` job (pinned GHCR oracle images; cron + workflow_dispatch).
    Sweep artifacts live outside the repo (`C:\selis-build\conf01-sweep\`).
    **G2 is NOT met locally: 27.17% within tolerance at 150 DPI (1,018/3,747 comparable) vs the
    ≥95% criterion** — the itemised gap list and the filed root-cause tasks are in
    *2.CONF.01 sweep readout* below.

- [x] **SL-2.CONF.02 — Promote conformance areas** · deps: CONF.01 · owner: AI
  - **Do:** Update `conformance/areas.toml`. Areas that did not reach `Render` stay at `Parse` and
    are marked as such publicly (ADR-P0010).
  - **Done (2026-09-11):** first ladder update (`conformance/areas.toml` + regenerated
    `conformance/REPORT.md`). **No area meets the full calibrated bar, so nothing promotes to
    Render** — that is the honest outcome per ADR-P0010, not a gap in the update:

    | Area | Level | Evidence |
    |---|---|---|
    | cos, xref | Parse | SL-1.COS.* DoDs; CONF.01 3,836-file parse coverage; CONF.05 zero unexplained refusals |
    | filters | Parse | SL-1.FILT.* + SL-2.FILT.01/02; CONF.04 JPX/JBIG2 decode suspects tracked, not parse failures |
    | encryption | Parse | SL-1.ENCRYPT.* DoDs; RC4/AESV2 read over the corpus |
    | document | Parse | SL-1.DOC.* DoDs; /Rotate page geometry open in RAST.12 (parse unaffected) |
    | **render** | **Parse (not Render)** | **Bar A pass** (97.0% ≤25% @150, n=3,747 vs MuPDF) **but Bar B fails** (≤2%: 76.2 vs best pair 82.3 — 6.1pp raw, 1.1pp past the 5pp allowance; ≤5%/≤10% pass). Open: size_skew 17 files (RAST.12), blank_selis 25 files (RAST.13), diff≥25 94 files (CONF.04) |
    | text, annot, forms, edit, redact, sign, pdfa, pdfua | None | Unmeasured — including the G2 Identify targets for annot/forms (no detection gate run; /AP appearances suspected in the blank_selis cluster). Claiming them would be the exact failure mode ADR-P0010 exists to prevent. |

    **Calibration verdict (local vs CI):** the CONF.03 matrix is **confirmed, unadjusted**. Local legs
    were re-derived from the verdicts and reproduce every published cell exactly (sanity 100% both
    legs; selis↔mutool full-corpus 27.3/34.1/76.2/85.1/92.0/97.0, p50 1.68, p99 68.4). The CI
    `render-conf` dispatch (run [34567473855](https://github.com/WertCore/selis-pdf/actions/runs/34567473855),
    2026-09-11, ref main @2aacac2) completed green at the job level but **measured 0 comparable
    pages on every oracle leg** — a job bug, not a measurement: (1) the sweep passes `--out`
    relative, and `container_plan` put it verbatim into the docker `-v` mount (daemon: `includes
    invalid characters for a local volume name`) — all pdfium/pdfjs legs; (2) the calibration legs
    name the binary `mutool` but the pin names the tool `mupdf`, so the lookup found no pin — all
    mutool legs. Both fixed on this branch (`xtask/src/oracle.rs`: `absolutize()` +
    `container_mounts()` + `pin_id()`, two regression tests; `oracle check` now resolves
    mutool→oracle-mupdf@digest). An unpushed branch cannot re-dispatch, so the confirming run
    fires on the first `render-conf` dispatch/cron after merge; the matrix stands on the local
    legs until then.
  - **SL-2.RAST.14 precondition verdict: PENDING (assessed, not implemented).** The precondition —
    a CONF.03 matrix re-run showing the ≤1%/≤2% bands inside the envelope — is not satisfied by
    any current data: selis ≤1% = 34.1% vs the pair envelope ~73.3–76.5%, and ≤2% = 76.2% vs
    80.5–82.3%. The envelope itself is confirmed (local legs reproduced exactly); the CI re-run
    that re-baselines it on the pinned identities is outstanding post-merge (see above). Nothing
    refutes the RAST.14 premise — the uniform 1–2% mechanical signature stands — so this is
    pending its owner's engine work, not failed.

### 2.CONF.01 sweep readout — G2 gap list (2026-09-10, selis vs MuPDF, page 1, ΔE76 > 2.3)

G2 exit criterion: "perceptual diff vs PDFium ≤ 0.5% differing pixels on ≥95% of the render corpus
at 150 DPI". **Not met.** Itemised, per signature (SL-0.ORACLE.05 clustering):

| Signature @150 DPI | Files | Reading / disposition |
|---|---|---|
| `match` (≤ 0.5%) | 1,018 of 3,747 comparable = **27.17%** (72 DPI: 24.0%, 300 DPI: 27.9%) | **G2 not met.** Agreement *rises* with DPI — the dominant mass sits just above the threshold |
| `diff<5` (0.5–5%) | 2,161 (median 1.68%) | One dominant mode ~1–2% — the rasteriser-AA/edge-coverage signature. Whether this is "wrong" or noise is exactly what oracle-vs-oracle calibration (SL-0.ORACLE.02) must decide: PDFium-vs-pdf.js agreement on this corpus is unmeasured, and two independent engines rarely agree to 0.5% at ΔE76 > 2.3. → **SL-2.CONF.03** |
| `diff<25` (5–25%) | 439 | Substantive divergence (fonts, images, shadings). → **SL-2.CONF.04** |
| `diff>=25` | 94 | Gross divergence; worst are DeviceN 6-colour (55.9%), ICC source profiles, softmask text — prepress colour paths. Ghent suite: **0/95** within tolerance. → **SL-2.CONF.04** |
| `blank_selis` | 18 (25 distinct files) | We paint page 1 (near-)blank where MuPDF paints — annotation `/AP` appearance candidates; a silent no-draw is a bug even for unsupported constructs. → **SL-2.RAST.13** |
| `size_skew` | 17 | Page `/Rotate` rendered unrotated (ours 1240×1755 vs oracle 1755×1240). A definite our-bug. → **SL-2.RAST.12** |
| `selis_rejects` | 26 | `budget exceeded (objects) limit 200000` on real-world govdocs files under the Viewer profile; hard refusals on zero-area / missing-`/MediaBox` pages. → **SL-2.CONF.05** |
| `oracle_rejects` | 35 | MuPDF-side failures (encryption/repair) — not a Selis conformance claim. |
| `both_reject` | 28 | Agreement on genuinely broken files (mutants, fuzzed files). |

By source @150 (within-tolerance / comparable): verapdf 805/2679 · flat (pdf.js corpus + pdfassoc)
169/610 · synthetic 42/165 · govdocs1 2/198 · ghent 0/95.

Explicitly **not yet measured**: the G2 oracle pair itself. PDFium and pdf.js render the same
sample in the CI `render-conf` job (pinned GHCR images, digest-locked in `xtask/oracles.toml`); its
artifacts close the two open questions — our agreement against each of them, and their agreement
with each other (calibration). Per §4 (21-TESTING-AND-ORACLES), two independent oracles are the
minimum for a `Render` promotion, so SL-2.CONF.02 consumes this readout plus the render-conf run.

- [x] **SL-2.CONF.03 — Oracle-vs-oracle calibration of the render tolerance** · deps: CONF.01 · owner: AI
  - **Do:** Measure PDFium-vs-pdf.js agreement over the same render sample (CI `render-conf`
    artifacts; SL-0.ORACLE.02). The CONF.01 `diff<5` bucket (2,161 files @150, median 1.68%) is a
    single mode just above the 0.5% threshold; if independent oracles agree at ~1–2% on those
    pages, the threshold measures rasteriser identity, not correctness, and the §3 tolerances of
    20-CONFORMANCE-PROGRAM.md get revised with the published calibration numbers.
  - **DoD:** Calibration table published next to the tolerances; the G2 threshold either confirmed
    or re-baselined with that data — never adjusted because a build is red.
  - **Done (2026-09-10, local legs; CI confirmation pending):** `xtask oracle sweep --calibrate`
    renders page 1 with every named oracle and compares all pairs under the identical metric
    (ΔE76 > 2.3, overlap, ≤0.5/5/25% bands). Local legs used the pinned-version drivers on the dev
    host — PDFium chromium/7961 (bblanchon win-x64 tarball + the checked-in C driver, gcc build),
    pdf.js 6.2.108 (pinned pdfjs-dist via `npm ci` + `driver.mjs`), MuPDF mutool 1.23.0 — with
    mutool run **twice** as the self-agreement sanity leg. Sample: 300-file stride over the
    general corpus + the full 95-file Ghent suite, at 150 DPI. Artifacts:
    `C:\selis-build\conf03-calibration{,-rest}\`. The CI `render-conf` job now runs the same two
    calibration legs with the pinned GHCR images (digest-locked) — its artifacts are the
    pin-identity confirmation of the numbers below.

  **Measured matrix @150 DPI (fraction of comparable pages within band):**

  | Pair | n | ≤0.5% | ≤1% | ≤2% | ≤5% | ≤10% | ≤25% | p50 | p90 | p99 |
  |---|---|---|---|---|---|---|---|---|---|---|
  | mutool↔mutool (sanity) | 388 | 100% | 100% | 100% | 100% | 100% | 100% | 0.0 | 0.0 | 0.0 |
  | pdfium↔pdfjs (general) | 296 | 70.3% | 75.0% | 81.1% | 86.1% | 91.9% | 97.0% | 0.06 | 7.89 | 39.0 |
  | pdfium↔mutool (general) | 584 | 68.2% | 73.3% | 80.5% | 86.3% | 94.2% | 97.9% | 0.06 | 7.24 | 29.3 |
  | pdfjs↔mutool (general) | 586 | 70.6% | 76.5% | 82.3% | 88.1% | 94.5% | 96.9% | 0.05 | 5.84 | 37.7 |
  | pdfium↔pdfjs (ghent) | 95 | 0% | 0% | 0% | 2.1% | 30.5% | 77.9% | 15.97 | 36.9 | 54.1 |
  | pdfium↔mutool (ghent) | 190 | 0% | 0% | 0% | 11.6% | 38.9% | 76.8% | 17.91 | 36.9 | 55.0 |
  | pdfjs↔mutool (ghent) | 190 | 0% | 0% | 0% | 2.1% | 23.2% | 73.7% | 16.93 | 39.2 | 54.7 |
  | **selis↔mutool (full corpus, CONF.01)** | 3,747 | **27.3%** | **34.1%** | **76.2%** | **85.1%** | **92.0%** | **97.0%** | **1.68** | **8.55** | **68.4** |

  **Findings:**
  1. **The current G2 bar (≤0.5% on ≥95%) is below the independent-renderer noise floor.** The
     best pair reaches 70.6% on general content and 0% on Ghent; no pair reaches 95% at any band
     below ≤25%. The criterion as written measures renderer identity, not correctness — exactly
     the SL-0.ORACLE.02 prediction.
  2. **Only part of the CONF.01 `diff<5` mass is noise.** The oracle pairs hold ~5pp of outcomes
     in the 0.5–1% band; selis holds ~6.8pp — the same. But the 1–2% band holds ~6pp for pairs
     and **~42pp for selis**: a real, uniform ~36pp fidelity excess with one mechanical signature
     (subpixel positioning / AA coverage), not per-page bugs. → **SL-2.RAST.14**.
  3. **From ≤2% upward, selis sits inside the oracle-pair envelope** (76.2 vs 80.5–82.3 at ≤2%;
     85.1 vs 86.1–88.1 at ≤5%; 92.0 vs 91.9–94.5 at ≤10%; 97.0 vs 96.9–97.9 at ≤25%) — within the
     pair spread + sampling noise.
  4. **Selis's tail is the real bug list:** p99 = 68.4 vs pairs' 29–39 — the already-filed
     `blank_selis` (RAST.13), `size_skew`/`/Rotate` (RAST.12), and gross-divergence (CONF.04)
     clusters.
  5. **Prepress content is a different regime for everyone:** all pairs at 0% within ≤2% on Ghent;
     colour management / overprint / DeviceN divergence is industry-wide, not a Selis defect.

  **Recommended calibrated G2 bar** (replaces the unattainable 0.5%/95% pair; tracked bands stay
  published):
  - **Bar A (per-page):** ≥95% of comparable pages ≤25% differing pixels @150 DPI — every oracle
    pair passes (96.9–97.9%); **selis passes today (97.0%)**.
  - **Bar B (fidelity envelope):** selis's ≤2%/≤5%/≤10% band fractions within 5pp of the best
    oracle pair (5pp = pair spread + n≈300 sampling noise). **Selis today: fails ≤2% by 1.1pp**
    (76.2 vs 82.3), passes ≤5% (−3.0pp) and ≤10% (−2.5pp). This is the honest current gap.
  - **Tracked, not gated:** ≤0.5%/≤1% strict-fidelity fractions, and the Ghent-class CDFs against
    the oracle-pair envelope (selis Ghent ≤25% = 61.1% vs pairs' 73.7–77.9% — the CONF.04 colour
    gap).
  - The 20-CONFORMANCE-PROGRAM.md §3 tolerances and SL-2.CONF.02 consume these numbers; the
    threshold was re-baselined **from calibration data only**, per the §5 verdict discipline.

  **CI confirmation status (2026-09-10):** dispatch attempt run
  [34523221142](https://github.com/WertCore/selis-pdf/actions/runs/34523221142)
  (`workflow_dispatch`, ref main) failed in the corpus-extraction step before reaching the sweep —
  GNU tar on ubuntu cannot read the Ghent **zip** (`This does not look like a tar archive`; the
  dev host's bsdtar could, which is why local legs worked). Fixed on this branch (python
  `zipfile`, verified against the pinned zip: 95 PDFs). Pushing is out of scope for this task, so
  the completed confirmation run fires on the first `render-conf` dispatch/cron after this branch
  merges — it then reports (a) selis↔pdfium/pdf.js on the pinned images (completing the
  calibration triangle: selis was only compared against MuPDF locally, and pair distances are not
  transitive) and (b) the oracle-vs-oracle legs of this matrix on the pinned identities. The
  same run's `size-check` failure (3 wasm cdylibs "NOT MEASURED — no artifact in this run") is a
   pre-existing main-branch issue: runs 34483545072 (28fca97) and 34479582812 (f95f6ff) failed
   before this branch's merge; not caused by this work.

   **CI confirmation status, update 2026-09-11 (SL-2.CONF.02):** dispatched run
   [34567473855](https://github.com/WertCore/selis-pdf/actions/runs/34567473855)
   (`workflow_dispatch`, ref main @2aacac2 — which carries the zip fix) completed green at the job
   level but measured **0 comparable pages on every oracle leg**: the sweep's 396 files × 3 DPIs
   vs pdfium/pdf.js all `oracle_rejects`, and all calibration pairs `reject` including the
   mutool↔mutool sanity leg. Root causes, both in `xtask/src/oracle.rs` (never exercised end to
   end before — the dev host has no Docker): (1) the output bind mount used the relative `--out`
   path verbatim (`sweep/tmp-wN/theirs.png` → daemon rejects it); the input mount was already
   canonicalized, which is why only the output side failed. (2) the mutool legs look up
   `[tool.mutool]`, but the pin is recorded as `[tool.mupdf]` (the binary vs the tool). Fixed on
   the CONF.02 branch with regression tests; the confirming measurement run fires on the first
   `render-conf` dispatch/cron after merge. The calibration matrix above stands on the local
   legs (re-derived exactly) until then.

- [ ] **SL-2.CONF.04 — Gross-divergence triage (CONF.01 `diff>=25%` cluster)** · deps: CONF.01 · owner: AI
  - **Do:** Root-cause the 94 files at ≥25% differing pixels @150 (worst: `ghent/GWG080_
    DeviceN-Support_6c_x3` 55.9%, `ghent/GWG1610_Softmasks_Text_part1_X4` 55.1%, `issue6296`,
    `issue6298`, `issue8565`, `franz_2`). Suspects: DeviceN/Separation tint→RGB paths, CMYK→RGB
    conversion, JPX/JBIG2 decode differences. One filed task per confirmed root cause; verdicts
    recorded per SL-0.ORACLE.05.
  - **DoD:** Every cluster file carries a triage verdict; behaviour-changing fixes add corpus
    entries.

- [x] **SL-2.CONF.05 — Typed-refusal review for batch render contexts** · deps: CONF.01 · owner: AI
  - **Do:** Review the sweep's `selis_rejects` cluster (26 @150): `budget exceeded (objects) limit
    200000` on real-world govdocs files under the Viewer profile, and hard refusals on pages with
    no `/MediaBox` or zero area where other renderers fall back to a default size. Decide per
    case: a batch/profile budget tier, a default-page-size fallback (with a recorded deviation),
    or keep the refusal as the designed posture (with annotations).
  - **DoD:** Each refusal cluster annotated with the decision; expectations updated.
  - **Done (2026-09-10):** all 26 files adjudicated; every file now either renders or refuses
    with a deliberate, annotated typed reason — zero unexplained refusals (re-run:
    `C:\selis-build\conf05-rerun\`, 26-file `--include` sweep @150).

  **Profile changes** (`crates/selis-sandbox/profiles.toml`, the code-generated source of truth,
  Viewer surface — both changes justified by the profile's own "500 MB behemoth" requirement):
  * `objects` 200,000 → **1,000,000** (matches Editor): 200k refused four real-world documents
    needing ~200,001 objects (govdocs1/000146, govdocs1/000380, issue12295, issue12810) that
    MuPDF renders; real 500 MB PDFs average 200–500 B/object ≈ 1–2.5 M objects. `bytes`
    (256 MiB) remains the binding memory constraint.
  * `wall` 5 s → **15 s**: govdocs1/000146 needed 5.0006 s — a boundary flap; behemoth-class
    open+first-paint measures ~5–11 s. Hostile work stays bounded; normal documents remain far
    under the §12 first-paint budget.

  **Behaviour change** (`Session::page_size`, engine): a page with no `/MediaBox` in its
  inheritance chain, or a non-positive one, renders at the ISO 32000-2 §7.10.1 default
  (612 × 792) — the fallback mainstream viewers apply; a missing box is incomplete authoring,
  not a reason to refuse. Engine test `page_size_falls_back_to_the_iso_default_...` covers both.

  **Per-file verdicts** (annotations in `corpus/expect/…`):
  | Files | Disposition |
  |---|---|
  | govdocs1/000146, govdocs1/000380, issue12295, issue12810 | `OurBug` fixed — Viewer profile mis-tune (objects + wall); now render and compare |
  | bug1721218_reduced | `OurBug` fixed — 200k-object reproducer needed ~9 s; renders within the 15 s wall and agrees within 5% |
  | boundingBox_invalid, synthetic/mutant_11 | `OurBug` fixed — zero-area/missing `/MediaBox`; now render, match MuPDF |
  | bug852992_reduced, issue7229, issue7872 | `OurBug` fixed — now render; compare as `size_skew` because MuPDF honours these pages' `/CropBox` while selis sizes from `/MediaBox` (CropBox-aware render size folds into SL-2.RAST.12's page-geometry work) |
  | GHOSTSCRIPT-698804-1-fuzzed | kept — typed refusal is correct: the page tree yields 0 pages and rendering "page 0" is out of range; MuPDF fabricates a letter page |
  | synthetic/mut_197–199/202, mutant_2/13/14/15/26/28/30/35/37/38 (14 files) | kept — `[E1103]` no `/Root`: refuse-by-design (typed error, no repair attempt); MuPDF's repair finds a root — both readings defensible (consistent with the seeded structural triage) |
  | issue269_2 | `selis_timeout` annotated — needs ~90–120 s; renders with a 120 s harness budget; known-slow robustness follow-up (SL-1.ROB) |
