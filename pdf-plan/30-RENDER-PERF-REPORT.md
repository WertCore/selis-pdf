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

| Page | selis render | dl-build | mutool | pdfium | × pdfium (raw) |
|---|---|---|---|---|---|
| text-heavy | 50.9 ms | 7.1 ms | 40.7 ms | 46.8 ms | **0.92×** |
| mixed | 76.6 ms | 32.2 ms | 48.1 ms | 54.1 ms | **0.71×** |
| large-image | 38.2 ms | 25.5 ms | 33.6 ms | 47.7 ms | **1.25×** |
| shading | 0.86 ms | 0.03 ms | 67.4 ms | 63.2 ms | **73×** |
| transparency | 14.9 ms | 0.8 ms | 33.5 ms | 50.2 ms | **3.37×** |
| vector-heavy | 38.7 ms | 8.3 ms | 105.5 ms | 62.0 ms | **1.60×** |
| **geomean** | | | | | **2.62×** |

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
