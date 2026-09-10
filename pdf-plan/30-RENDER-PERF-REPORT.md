# SL-2.PERF.02 — Render throughput: profile report

Date: 2026-09-10. Host: Windows x64 laptop (shared CI-dev host, two other
agents active — timings carry host noise; see §7). Build: release
(`cargo run --release -p xtask -- perf-render`). Engine: this worktree.

Verdict: **the ≥0.6× PDFium DoD is MET** — 2.62× geomean across the set,
every page ≥0.6× raw (see §4). The §12 absolute budget (18 ms @150 DPI) is
NOT met and a literal 150 DPI number is not measurable yet — both recorded
as honest gaps (§6), as the task allows.

## 1. The benchmark set

`xtask render-set` generates six deterministic single-page PDFs into
`bench/render-set/` (fixed seeds, no RNG/clock/input; `xtask render-set
--check` byte-compares the committed fixtures against the generator).
One page per cost axis:

| File | Axis | Workload shape (ops / glyphs) | Bytes |
|---|---|---|---|
| `text-heavy.pdf` | text | 4 557 text ops, 4 557 glyphs, Helvetica 9pt × 64 lines (A4) | 8 535 |
| `vector-heavy.pdf` | vector | 900 rect fills + 250 stroked cubics + 250 stroked lines | 66 050 |
| `large-image.pdf` | image | one 960×1200 RGB FlateDecode image, full-page `cm` + `Do` | 489 830 |
| `shading.pdf` | shading | 1 axial + 1 radial DeviceRGB type-2 shading, full-page | 1 117 |
| `transparency.pdf` | transparency | nested Multiply/Screen BDC groups (83 fills, 2 push/pop) | 4 043 |
| `mixed.pdf` | mixed | 1 406 glyphs + 250 rects + 4 image draws + clip + 1 group | 224 207 |

The text page is A4 (595×842) with a realistic dense body (~4 600 glyphs);
it doubles as the §12 "Render A4 text page" harness input
(`bench/benches/render.rs:render_text_heavy`).

## 2. Method

`xtask perf-render` (xtask/src/render_perf.rs) renders every set page:

- **Engine, in-process**: one `Session::open` (timed separately), one
  untimed warm-up, then N timed `render_page_with_stats` calls (display-list
  build + raster walk = first-paint cost); the median is recorded. A wrapping
  pixmap checksum must agree across all repeats or the run fails — a
  throughput number over a non-deterministic renderer is meaningless
  (SL-2.RAST.09). Walk workload counters (`RenderStats`) are recorded beside
  the times; that pairing is this report's evidence.
- **Oracles, spawned at the same DPI**: `mutool draw -r <dpi> -o <out>
  <in>` (local 1.23.0) and `pdfium_driver --page 1 --dpi <dpi> <in> <out>`
  (chromium/7961, win-x64 tarball sha256
  `88276459…6406ADF4`, driver compiled locally with MSVC — see §8).
  Median of N wall times, **spawn-inclusive**; per-tool spawn cost (trivial
  invocation, median of 5) is measured and recorded, not subtracted.
- **Ratio** = oracle_ms / selis_ms per page; headline = geometric mean.
- **DPI = 72** (1 pt = 1 px): the engine has no page→device matrix yet, so
  at 150 DPI it would paint unscaled content on a 150-DPI canvas while the
  oracles rasterise 4.3× the pixels — not a comparison. 72 DPI rasterises
  identical pixel counts on both sides (gap G-1, §6).

## 3. Top-10 render costs (the DoD profile)

Ranked by measured contribution. "Pre" = before this task's optimisation
(release, median of 5); counters from `RenderStats`.

| # | Cost | Evidence | Status |
|---|---|---|---|
| 1 | **Font program re-resolution per glyph.** The walk called the `font_data` closure per run-per-glyph: fresh resolver + guard + font-dict parse + a ~140 KB `to_vec` copy **per glyph** — 4 557 resolves, **636 MB copied per text render** (196 MB on mixed). | counters `font_resolves=4557, font_bytes=635756184`; text render 220.8 ms | FIXED — walk-local `TextCache` (§5): 1 resolve |
| 2 | **skrifa outline extraction per glyph** (`outline_glyph`: `FontRef` parse + outline walk, 4 557×). | `glyph_outlines=4557` | FIXED — cached per (font, gid) |
| 3 | **cmap lookup per glyph** (`glyph_id_for_char`, incl. `FontRef::new`). | `glyph_lookups=4557` | FIXED — cached per (font, code), `None` included (spaces must not re-extract) |
| 4 | **Display-list-build `font_width` per glyph** (guard + resolver + resources + widths-table parse, 4 557× = **123.5 ms** of DL build). | `dl_ms` 123.5 → 7.1 | FIXED — `(font, scope, code)` width cache in `build_display_list` (§5) |
| 5 | **Per-glyph heap-alloc chains**: transform `Vec` + `RasterPath` copy + tiny-skia `Path` build per glyph (3 allocations per glyph). | residual analysis §5 | PARTLY — fused to one `Vec` with exact `reserve`; tiny-skia internal remains |
| 6 | **tiny-skia per-glyph fill rasterisation**: the residual floor (~60% of post-opt text time, ~9 µs/glyph). Every instance re-rasterises; no glyph atlas exists. | text render 50.9 ms = 11.2 µs/glyph all-in | NOT FIXED — needs a raster cache/atlas (follow-up, §6 G-2) |
| 7 | **Full-image RGBA copies per draw, twice**: `Op::Image` walk does `rgba8.to_vec()`, then `TinySkiaBackend::draw_image` does `Pixmap::from_vec(clone)` — 2× 4.6 MB on large-image, 2× 691 KB ×4 on mixed. | `image_bytes` counter; source-visible | NOT FIXED — killing the second copy needs `Image: Vec<u8> → Bytes` plus a `selis-raster` dep or a by-value `draw_image` trait change; image pages already ≥1×, so churn deferred (§6 G-7) |
| 8 | **FlateDecode + RGB→RGBA at DL build** (20–32 ms on image pages). Paid by every renderer; inherent to the workload, not a defect. | `dl_ms` 25–32 on image pages | INHERENT |
| 9 | **Transparency-group push/pop** (full-canvas scratch + composite per group). 2 groups on 0.5 MP ≈ the whole 14.9 ms transparency page — yet 3.4× faster than PDFium. | `layers_pushed=2`; ratio 3.37× | HEALTHY |
| 10 | **Per-op state comparisons** (clip/smask/blend): change-detected already (`current_clip`/`current_smask`/`current_blend`), rebuilds only on change — `clip_rebuilds ≤ 2` on the whole set. The task's "batch scissor/clip state" prompt is answered: batching is in place and the profile confirms it costs nothing measurable. | counters | HEALTHY, no change |

Honourable mentions (measured cheap): axial/radial per-pixel shading loop
(0.9 ms full-page — the function evaluator is a lerp, not a bottleneck);
page open (<0.3 ms); white backdrop fill.

Profiling method note: no `cargo flamegraph`/`perf` sampling exists for this
Windows host, so the report uses the task-sanctioned alternative —
**criterion + instrumented counters**: stage splits (open / DL-build /
raster) per page, `RenderStats` event counts per walk, and op histograms
from the public display list. Deeper attribution (per-glyph primitive
timings) was cross-checked by code inspection against the counters, not by
sampling; the counters are committed (`RenderStats`, `render_page_with_stats`)
so any host can reproduce them.

## 4. Measured ratios (final record, release, 72 DPI, median of 9)

Committed as `bench/render-results.json` (reference record; the CI perf job
regenerates it with the pinned driver before `perf-check`).

| Page | selis render | dl-build | × pdfium (raw) |
|---|---|---|---|
| text-heavy | 51.0 ms | 9.3 ms | **0.96×** |
| mixed | 57.5 ms | 31.2 ms | **0.78×** |
| large-image | 45.7 ms | 37.4 ms | **1.41×** |
| shading | 1.09 ms | 0.04 ms | **56×** |
| transparency | 16.5 ms | 0.7 ms | **3.01×** |
| vector-heavy | 27.3 ms | 12.3 ms | **2.60×** |
| **geomean** | | | **2.78×** |

(Per-page oracle times in `bench/render-results.json`; spawn recorded
there too. Earlier record for reference: 0.92/0.71/1.25/73/3.37/1.60,
geomean 2.62× — same walk code, different machine-load window.)

Spawn recorded in the file: mutool ≈ 9.9 ms, pdfium ≈ 13.4 ms per
invocation (spawn-inclusive oracle times flatter us on fast pages — §6 G-3).
All six pages pass the 0.6× gate raw; spawn-adjusted sensitivity
((oracle−spawn)/selis): text 0.66×, mixed 0.53×, image 0.94×, rest ≫1×.

Optimisation trajectory (text render / text DL-build): 220.8/123.5 →
99.2/8.3 (font+outline+width caches) → 66.9/6.8 (per-font sub-maps, fused
path) → 43.7–50.9/4.5–7.1 (per-run hoists, exact reserve). Mixed:
143.8/68.7 → 76.6/32.2 (2–3×). Pixmap checksums are **byte-identical across
every run before and after** (`c95ee13e…`, `01ccee95…`, …) — RAST.09 holds.

## 5. What was optimised (and what was deliberately not)

`crates/selis-pdf-engine/src/render.rs`:

- `TextCache`: walk-local font-program cache (one `font_data` call per
  distinct font), per-font `u16`-keyed cmap and outline caches (`None`
  cached), per-run hoists (paint, scaled base matrix, one `Arc`-bump bytes
  clone). Hostile-input caps (256 fonts / 4 096 cmap / 1 024 outlines);
  past-cap runs render from a run-local scratch with identical output
  (`FontView::Scratch`) — caps bound memory, never change pixels.
- Fused glyph path construction (one exact-sized `Vec`, no
  `raster_path_from_commands` copy — function removed).
- `RenderStats` (+3 hit counters) and `render_page_with_stats`
  (`render_page` delegates) — the committed instrumentation.

`crates/selis-pdf-engine/src/session.rs`: `(font, resource-scope, code)`
glyph-width cache in `build_display_list` (the scope is part of the key —
same name may resolve differently per scope).

Deliberately not changed: clip batching (already change-detected — cost
#10); shading/pattern cross-op caches (each shading drawn once on the set;
no evidence); image double-copy (cost #7 — public-type churn for pages
already ≥1×); glyph merging across ops (invalid under alpha/blend);
backdrop fast-fill (needs a trait change for ~0.2 ms).

Tests: `text_cache_resolves_once_and_hits_thereafter` (resolve-once, hits,
identical outlines), `text_walk_is_deterministic_with_stats` (real fallback
font through `exec` → walk: identical pixels + identical counters, second
op all-hits). Full workspace suite green (see commit).

## 6. Honest gaps

- **G-1 — No page→device matrix.** The engine renders user space 1:1 onto
  whatever canvas the caller sizes (the `--dpi` flag only sizes the canvas).
  A literal 150 DPI measurement — and the CONF.01 150-DPI perceptual diff —
  needs the device transform (plus the y-flip question) as an explicit
  render parameter. Measured at 72 DPI instead (identical pixel counts both
  sides). Owner: CONF.01 follow-up; this task's set/harness carry over.
- **G-2 — 18 ms absolute NOT met.** 50.9 ms at 72 DPI native vs 18 ms @150
  DPI. Residual is cost #6 (per-instance rasterisation). Needs a glyph
  raster cache/atlas. The `duration-ms` row stays `not-measurable` with a
  note pointing at criterion `render_text_heavy` (wired, native scale).
- **G-3 — Oracle spawn bias.** ~10–14 ms of every oracle number is process
  spawn, not rendering (measured, recorded in the JSON, not subtracted from
  the gate). Raw ratios are upper bounds on our relative throughput; the
  spawn-adjusted column is the honest lower bound.
- **G-4 — mixed is marginal content-adjusted (0.53×).** Passes raw (0.71×)
  but the adjusted view + host noise (§7) make it the watch item; the atlas
  (G-2) lifts it together with text.
- **G-5 — Shared-host noise.** Identical binaries measured ±30–50% across
  runs (e.g. text 43.7→50.9, mixed 46.2→77.2, pdfium vector 38.6→103.9)
  with two other agents building/testing concurrently. Medians of 5–9 tame
  it but do not remove it; the quiet CI runner is authoritative for the
  gate, and the 5%-regression rule applies to same-machine comparisons.
- **G-6 — G5 0.9× target** remains future work (the row notes it).
- **G-7 — Image double-copy kept** (cost #7) as a documented tradeoff.

## 7. Reproduction

```powershell
$env:CARGO_TARGET_DIR='C:\selis-build'
cargo xtask render-set --check          # fixtures match their generator
cargo run --release -p xtask -- perf-render --pdfium-driver <path> --repeats 9
cargo xtask perf-check                  # ratio row enforced (needs the JSON)
cargo bench -p selis-bench --bench render  # per-axis criterion means
```

Reference criterion means (this host, 2026-09-10, ns): render_text_heavy
93087000.0, render_vector_heavy 51914000.0, render_large_image 63805000.0,
render_shading 2381400.0, render_transparency 17394000.0, render_mixed
65276000.0 — merged into `bench/baselines.json` (kernel values untouched).

## 8. Local PDFium provenance (not the gate — the container is)

`oracle check` on this host: mutool local (1.23.0), pdfium NOT installed.
For local numbers the pinned win-x64 prebuilt (chromium/7961, same release
line as the container pin) was fetched from
`github.com/bblanchon/pdfium-binaries` (tarball sha256
`88276459349B291C41F10422DAD0210F007C04D919C8FA56472B6B7C6406ADF4`),
extracted outside the repo (`C:\selis-build\pdfium`, untracked), and the
in-repo driver (`docker/oracles/pdfium/driver/pdfium_driver.c`) compiled
with MSVC (`cl /O2 … /link …\lib\pdfium.dll.lib`) exactly per its header
comment. The authoritative comparison stays the GHCR image in the CI perf
job (`xtask oracle image-ref pdfium` prints the pinned ref; the job
extracts the driver from it — §12 wiring, no shell TOML parsing).

## 9b. G-2 ledger — the 18 ms absolute budget (2026-09-10, follow-up)

Task: close G-2 (text page ≈51 ms at 72 DPI native vs the 18 ms @150 DPI
budget) with the glyph atlas the report predicted, per ADR-P0025 (LRU under
budget), ADR-P0012 (bit-identical output), RAST.09 (determinism).

**What the specified atlas would be.** A raster cache keyed per (font
instance, glyph id, size, transform-class), LRU-evicted under a byte budget
(reusing PERF.01's `LruCache` pattern). Design analysis killed it before
code: every text instance carries a unique *fractional* device offset, so a
translation-invariant bitmap must either key the exact offset (hit rate ~0
on body text — a memo that never hits) or stamp with resampling (changes
pixels — violates ADR-P0012/RAST.09). A translation-invariant raster cache
is fundamentally incompatible with byte-identical fractional positioning.
The maximal exact structure is therefore **paint-state batching**: merge
same-paint glyph paths into one fill — same pixels *iff* merged inks never
share a pixel.

**What was implemented (then removed).** A walk-local keyed batcher
(`BatchKey` = FNV over resolved paint + blend; `LruCache<BatchKey,
GlyphBatch>` with 4 MB budget + 16-entry cap + per-batch cmd cap; eviction
paints first; conflict-flush on arrival preserves walk order; same-key
overlap flushes). Correctness proof: simultaneously-open batches are
pairwise strictly-separated (>1px gap ⇒ no shared pixel under the
rasteriser's half-open edge convention), so every emission is order-free;
evicted/conflicting batches emit before later paints. All six set checksums
stayed byte-identical with batching on.

**Measurement (median of 5, release, pinned driver): text 51.0 → 98.5 ms,
mixed 57.5 → 71.4 ms — a ~2× REGRESSION.** Counters diagnose it:
`batch_fills=2481` for 3853 drawn glyphs = **1.55 glyphs/fill**. At 9pt body
text, neighbour ink gaps cluster at ~1px, so the strict-separation rule
flushes almost constantly — while each glyph pays bbox computation,
conflict scans, and LRU take+reinsert overhead, and 1.55-glyph merged fills
visit gap pixels the single fills never touch. Exact batching cannot pay on
dense body text.

**Probe (pinned in-tree test
`merged_fill_differs_from_sequential_fills_for_same_paint`).** Two
overlapping fractional rects (left edges 10.2 vs 10.7 share pixel column
[10,11)) painted as two fills vs one merged fill DIFFER: shared-pixel
coverages do not combine multiplicatively under tiny-skia, so merging
without strict separation would change pixels. This test pins the property:
if a rasteriser upgrade ever makes them equal, separation-free batching
becomes valid — revisit then.

**Decision: batcher REMOVED** (tree restored to the unbatched walk;
post-removal checksums equal the committed record on all six pages).
Also reverted: the `LruCache<K,V>` generalisation evaluated for the batcher
(streaming batches don't need cross-entry LRU; the reverted file keeps the
tree warning-free). Kept: the probe test, the G-2 analysis, the
`RenderStats` counters that made it measurable.

**Remaining delta to 18 ms (itemised).** Post-removal text page: ≈51 ms
render + ≈7–9 ms DL-build at 72 DPI native. Residual, in order: (1)
tiny-skia scalar per-glyph rasterisation (~60% — irreducible without
changing pixels or the rasteriser); (2) per-glyph path build + fill call
(~25% — only a separation-free merge would remove it, barred above);
(3) DL-build exec dispatch (~15% — one op per glyph by construction).
Reaching 18 ms needs at least one of: (a) a raster *atlas with quantised
subpixel positioning* — CHANGES pixels, i.e. a new rendering-model decision
owned by RAST/CONF with rebaselining, not a perf tweak; (b) a SIMD
rasteriser (dependency/upstream territory — ADR-P0008 keeps tiny-skia);
(c) at true 150 DPI the pixel workload quadruples anyway (G-1 device
matrix first). The `duration-ms` row stays `not-measurable` with this
pointer. The ratio gate (the PERF.02 DoD) is unaffected: 2.78× geomean.

Record note: `bench/render-results.json` now holds the post-`main`-merge
run (median of 9, release, pinned driver, geomean 2.78×) — same walk code,
same checksums; the G-2 experiment runs (`batched.json`: 98.5 ms text with
the batcher on) are evidence in this ledger, not records.

## 9c. PERF.03 ledger — WASM within 2.5× of native (2026-09-10)

DoD MET: **1.44× geomean** (reference run, release, same machine, same
process), every page ≤ 2.33×, guest checksums byte-identical to native on
all six pages. There is no JS/browser harness in the repo, so the comparison
runs wasmtime-driven — recorded as such everywhere below.

**What was built.** `crates/selis-pdf-wasm` (L4, `cdylib`, already listed in
`xtask/layers.toml`): three exports over guest linear memory —
`selis_input_alloc` / `selis_render_page` / `selis_free` (ADR-P0041, on the
`unsafe` allowlist with SAFETY cases). One call per page: input bytes in,
pixels out; display list, tiling, and rasterisation never cross the
boundary — the tile path stays inside the module by construction, and the
driver deliberately exposes no tile API. Native smoke tests cover the ABI
(alloc/reject/free roundtrip + the engine's own minimal fixture end to end).

**Harness.** `xtask perf-wasm` (native-only; the wasmtime dep is
`cfg(not(wasm32))`-gated so the size-check wasm build of xtask still
works): builds the driver for `wasm32-unknown-unknown`, instantiates it
under embedded wasmtime, and renders the set twice per page — in-process
native steady state and guest `render_page` call (median of 5, one warm-up).
Guest time includes the out-copy (the honest boundary cost). Results in
`bench/wasm-results.json` (reference record; the WASM budget rows stay
`not-measurable` — CI cannot regenerate guest numbers, so the file
documents rather than gates).

**Numbers** (native → guest, same run):

| Page | native | wasm (+simd128) | × native |
|---|---|---|---|
| text-heavy | 39.8 ms | 59.9 ms | 1.50× |
| mixed | 82.5 ms | 73.3 ms | 0.89× |
| large-image | 38.7 ms | 46.3 ms | 1.20× |
| shading | 0.72 ms | 1.49 ms | 2.07× |
| transparency | 10.3 ms | 21.4 ms | 2.08× |
| vector-heavy | 16.9 ms | 21.5 ms | 1.27× |
| **geomean** | | | **1.44×** |

(Earlier runs: scalar guest 2.13×; +simd128 variant 1.33× on a loaded
machine. Sub-ms pages price the boundary call itself — shading's 2.07× is
~0.8 ms of call+copy overhead, not rendering.)

**SIMD128 (adopted).** The driver builds with `-C target-feature=+simd128`
(scoped to the harness invocation env, never the repo config).
Transparency 5.73× → ~2.1×, geomean 2.13× → ~1.4×, and every guest checksum
still matches native — the determinism proof the task demands ("provably
deterministic"): WASM SIMD has fixed spec semantics, so the same module is
bit-identical on any engine, and the per-page checksum equality asserts it
per run instead of assuming it.

**Memory-growth strategy.** Guest pre-sizes every buffer exactly (input,
canvas `w×h×4` checked before allocation, out-copy); absurd canvases are
rejected, never attempted. Host caps guest linear memory at 512 MiB via a
wasmtime resource limiter — growth is bounded and loud (a trap names the
limit).

**Cold start.** wasmtime/Cranelift module compile ≈3–6 s (load-dependent;
43 s observed under full machine load) + instantiate ≈1–15 ms. Browsers
compile differently (baseline/JIT tiers), so the §12 45 ms cold-start row
stays `not-measurable` with this breakdown — the wasmtime number is
reported, not gated.

**Honest gaps.** (1) No browser/V8 measurement — wasmtime Cranelift is the
stand-in; same spec, different compiler; re-measure when the Phase 5 shell
lands. (2) The four wasm-bindgen runtime shims (`__wbindgen_describe`,
version-suffixed `__wbindgen_throw`, two externref table ops) come from
`selis-crypto`'s deliberate getrandom→JS routing on wasm32 (ADR-P0011);
the harness stubs them (never called on the render path — any call would
trap loudly) and fails on any other import. (3) Sub-ms pages are
boundary-dominated; the ratio floor there is call overhead, not engine
speed. (4) Shared-host noise applies (see §6 G-5); the quiet-runner rule
holds for this gate too.

## 9d. Files changed (PERF.03 on top of the above)

- `crates/selis-pdf-wasm/` (new): the render driver + native ABI tests.
- `pdf-plan/02-ADRS.md`: ADR-P0041 (one call per page; unsafe only at the
  boundary).
- `xtask/unsafe-allow.toml`: `selis-pdf-wasm` entry naming ADR-P0041.
- `xtask/src/perf_wasm.rs` + `perf-wasm` command (native-only): build,
  instantiate, compare, record `bench/wasm-results.json` (committed).
- `xtask/Cargo.toml`: wasmtime dep, `cfg(not(wasm32))`-gated (size-check
  wasm build of xtask unaffected — verified).
- `xtask/perf-budgets.toml`: WASM twin note carries the PERF.03 numbers
  (row stays `not-measurable` — CI cannot regenerate guest numbers).
- `xtask/size-budgets.toml`: comment updated (two linked artifacts now;
  `engine-render-cdylib` name needs revisiting — cargo cannot emit that
  dashed stem).
- PERF.03 checkbox below.

## 9. Files changed (this task)

- `xtask/src/render_set.rs` (+ `render-set` command): deterministic set
  generator; `bench/render-set/*.pdf` committed fixtures.
- `xtask/src/render_perf.rs` (+ `perf-render` command): stage-timed
  measurement, counters, checksums, spawn accounting; writes
  `bench/render-results.json` (committed reference record).
- `xtask/src/oracle.rs` (+ `oracle image-ref`): pinned image ref printer.
- `xtask/src/perf_check.rs`: enforcement for `ratio` rows (results file +
  key, higher-is-better) and `duration-ms` rows (ns→ms); config test updated.
- `xtask/perf-budgets.toml`: ratio row → `measuring` (gate `none`,
  results+key); 18 ms row notes record the miss + harness pointer.
- `bench/benches/render.rs` (+ `[[bench]]`): per-axis criterion benches;
  means merged into `bench/baselines.json`.
- `crates/selis-pdf-engine/src/{render,session}.rs` + lib export:
  `TextCache`, width cache, `RenderStats` hits, `render_page_with_stats`,
  two regression tests.
- `.github/workflows/ci.yml`: nightly perf job verifies fixtures,
  extracts the pinned PDFium driver, measures, then `perf-check`s.
- This report; SL-2.PERF.02 checkbox (honest: ratio DoD met, absolute+150
  DPI gaps recorded, PERF.03 stays blocked behind this task — not started).
