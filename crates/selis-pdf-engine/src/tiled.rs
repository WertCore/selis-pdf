//! Tiled page rendering with a runtime-selected executor (SL-4.WASM.03).
//!
//! One page becomes a grid of tiles, each tile is rendered with its own
//! integer-translated `ctm` into its own canvas, and the tiles are stitched in
//! decomposition order. Two things follow, and they are the whole point of
//! this module:
//!
//! 1. **The executor is a policy, not a build.** [`TiledRender::parallelism`]
//!    selects [`Parallelism::Single`] or [`Parallelism::Threads`] at run time.
//!    The decomposition, the per-tile render, and the stitch are identical in
//!    both cases, so the two paths cannot produce different bytes (ADR-P0004:
//!    the engine degrades by policy, never by `#[cfg]`).
//! 2. **Tiled output equals untiled output.** A tile rendered with a
//!    translated `ctm` equals the matching sub-rectangle of the full-canvas
//!    render, so the tiled pipeline can replace the untiled one without moving
//!    a pixel — the x86-64/arm64/WASM determinism discipline (SL-2.RAST.09)
//!    applied to the decomposition.
//!
//! # The overlap margin
//!
//! Tiles overlap their neighbours by [`TiledRender::overlap`] pixels and the
//! driver keeps only the inner region. Without it, a tile's *canvas boundary*
//! changes the anti-aliased coverage of the pixels at that boundary: the
//! rasteriser computes coverage for the visible sub-rectangle, so a pixel
//! straddling the boundary loses (or gains) part of its coverage — measured at
//! exactly one 1/16 coverage quantum (tiny-skia's 4×4 supersampling) on several
//! corpus documents. A 2-pixel margin is *not* sufficient: it still differs on
//! `protect_unencrypted_source.pdf`, where the boundary pixel's coverage is
//! graded across three sub-rectangles rather than two. Four is the smallest
//! margin that holds over the whole in-repo corpus, and it is the shipped
//! default ([`DEFAULT_OVERLAP`]), clamped at the page border so a page-edge tile
//! clips exactly where the untiled render clips. `xtask wasm-threads` asserts the
//! identity over the in-repo corpus on every PR.
//!
//! The margin costs `(w + 2m) × (h + 2m) - w × h` extra pixels per tile and
//! nothing else: the overlap region is rendered by both tiles and discarded
//! once.
//!
//! # Cost, honestly
//!
//! Each tile re-walks the page: `Session::render_page` rebuilds the page's
//! display list per call (SL-2.PERF.01 owns sharing it). The threaded path
//! therefore trades "one walk, one canvas" for "N walks, N tiles" — it wins
//! where the walk dominates and loses on pages whose cost is in parsing or
//! image decoding. WASM.03's contract is the executor policy and byte-identical
//! output; the display-list-sharing win is PERF work, and `xtask wasm-threads`
//! prints both wall times over the corpus rather than asserting a speedup it
//! cannot yet promise.

use selis_error::{err, Code, Result};
use selis_geom::Matrix;
use selis_raster::tile::{render_tiles, stitch, tile_decompose, Parallelism, Tile};
use selis_raster::TinySkiaBackend;
use selis_sandbox::{Budget, CancelToken, Clock};

use crate::page::PageView;
use crate::session::Session;

/// Tile width/height in device pixels.
pub const DEFAULT_TILE: u32 = 256;

/// Overlap margin in device pixels. Four is the smallest margin that keeps the
/// tiled render byte-identical to the untiled render over the whole in-repo
/// corpus (see the module docs); two is measurably *not* enough.
pub const DEFAULT_OVERLAP: u32 = 4;

/// A tiled page render: the tile geometry, the overlap, and the executor.
///
/// Construct with [`TiledRender::new`] (defaults for the overlap) or
/// [`TiledRender::with_overlap`], then [`TiledRender::render`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TiledRender {
    /// Tile width in device pixels (clamped up to 1).
    pub tile_w: u32,
    /// Tile height in device pixels (clamped up to 1).
    pub tile_h: u32,
    /// Overlap margin in device pixels (clamped at the page border).
    pub overlap: u32,
    /// The executor.
    pub parallelism: Parallelism,
}

impl Default for TiledRender {
    fn default() -> Self {
        Self::new(DEFAULT_TILE, DEFAULT_TILE, Parallelism::SINGLE)
    }
}

impl TiledRender {
    /// A tiled render with [`DEFAULT_OVERLAP`].
    #[must_use]
    pub const fn new(tile_w: u32, tile_h: u32, parallelism: Parallelism) -> Self {
        Self {
            tile_w,
            tile_h,
            overlap: DEFAULT_OVERLAP,
            parallelism,
        }
    }

    /// The same render with an explicit overlap margin (tests, calibration).
    #[must_use]
    pub const fn with_overlap(mut self, overlap: u32) -> Self {
        self.overlap = overlap;
        self
    }

    /// The same render with a different executor.
    #[must_use]
    pub const fn with_parallelism(mut self, parallelism: Parallelism) -> Self {
        self.parallelism = parallelism;
        self
    }

    /// Render one page as tiles and stitch the result.
    ///
    /// `view` supplies the canvas size (`view.width × view.height`); `matrix`
    /// is the page-to-device transform to render with — `view.ctm`, or the
    /// caller's override (the protocol's `params.matrix`), which is translated
    /// per tile exactly as `view.ctm` would be.
    ///
    /// The stitched raster is charged against `budget` through a fresh guard
    /// before it is allocated (the same discipline [`stitch`] documents); each
    /// tile's own walk runs under its own guard from the same `budget`, the
    /// same `clock`, and the caller's `cancel`, so cancellation is observed at
    /// the next budget tick of whichever tile is running.
    ///
    /// # Errors
    ///
    /// * The first tile's error, in decomposition order — a budget exhaustion
    ///   (`BUDGET_BYTES`/`BUDGET_PIXELS`/`BUDGET_OBJECTS`/`BUDGET_DEPTH`), the
    ///   caller's `CANCELLED`, or whatever the render walk reports. A failed
    ///   tile means no raster is returned at all, never a partial one.
    /// * `BUDGET_PIXELS` when a tile canvas cannot be allocated.
    ///
    /// # Malformed Input
    ///
    /// A zero-sized canvas returns an empty raster and renders nothing; the
    /// tile geometry is clamped to at least 1 pixel so the decomposition always
    /// makes progress.
    pub fn render(
        &self,
        session: &Session,
        page: usize,
        view: &PageView,
        matrix: Matrix,
        budget: &Budget,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>> {
        let (w, h) = (view.width, view.height);
        if w == 0 || h == 0 {
            return Ok(Vec::new());
        }
        let tiles = tile_decompose(w, h, self.tile_w, self.tile_h);
        let rendered = render_tiles(&tiles, self.parallelism, |t| {
            self.render_tile(session, page, view, matrix, t, budget, clock, cancel)
        });
        // Decomposition order, so the first failure is the deterministic one.
        let mut placed = Vec::with_capacity(rendered.len());
        for (tile, result) in rendered {
            placed.push((tile, result?));
        }
        let mut g = budget.guard_with(clock, cancel.clone());
        stitch(w, h, &placed, &mut g)
    }

    /// Render one tile's inner region (its neighbours' overlap is discarded).
    fn render_tile(
        &self,
        session: &Session,
        page: usize,
        view: &PageView,
        matrix: Matrix,
        tile: &Tile,
        budget: &Budget,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>> {
        // The margin is clamped at the page border: a page-edge tile must clip
        // exactly where the untiled render clips (nothing renders outside the
        // page box). `tile.pixel_*` are in bounds by construction.
        let left = self.overlap.min(tile.pixel_x);
        let top = self.overlap.min(tile.pixel_y);
        let right = self.overlap.min(
            view.width
                .saturating_sub(tile.pixel_x)
                .saturating_sub(tile.pixel_w),
        );
        let bottom = self.overlap.min(
            view.height
                .saturating_sub(tile.pixel_y)
                .saturating_sub(tile.pixel_h),
        );
        let canvas_w = tile
            .pixel_w
            .saturating_add(left)
            .saturating_add(right)
            .max(1);
        let canvas_h = tile
            .pixel_h
            .saturating_add(top)
            .saturating_add(bottom)
            .max(1);
        let mut backend = TinySkiaBackend::new(canvas_w, canvas_h).ok_or_else(|| {
            err!(
                Code::BudgetPixels,
                during = "tiled-render",
                detail = "tile canvas allocation refused"
            )
        })?;
        let origin_x = tile.pixel_x.saturating_sub(left);
        let origin_y = tile.pixel_y.saturating_sub(top);
        let tile_ctm = matrix.then(Matrix::translate(
            -f64::from(origin_x),
            -f64::from(origin_y),
        ));
        let mut g = budget.guard_with(clock, cancel.clone());
        session.render_page(page, &mut backend, tile_ctm, budget, &mut g)?;
        Ok(crop_inner(
            backend.pixmap().data(),
            canvas_w,
            left,
            top,
            tile.pixel_w,
            tile.pixel_h,
        ))
    }
}

/// Copy the inner `w × h` region at `(left, top)` out of a `canvas_w`-wide
/// RGBA8 canvas. Out-of-range reads yield a shorter buffer rather than a panic
/// (the geometry is in bounds by construction; this is the same defence in
/// depth `worker::crop_tile` applies on the protocol side).
fn crop_inner(canvas: &[u8], canvas_w: u32, left: u32, top: u32, w: u32, h: u32) -> Vec<u8> {
    let stride = usize::try_from(canvas_w).unwrap_or(0).saturating_mul(4);
    let left_bytes = usize::try_from(left).unwrap_or(0).saturating_mul(4);
    let row_bytes = usize::try_from(w).unwrap_or(0).saturating_mul(4);
    let mut out = Vec::new();
    for row in 0..h {
        let start = usize::try_from(top.saturating_add(row))
            .unwrap_or(usize::MAX)
            .saturating_mul(stride)
            .saturating_add(left_bytes);
        let end = start.saturating_add(row_bytes);
        if let Some(src) = canvas.get(start..end) {
            out.extend_from_slice(src);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use selis_error::Code;
    use selis_raster::aa::stable_hash;
    use selis_sandbox::{FixedClock, Surface};

    use super::*;

    /// The in-repo corpus (engine fixtures + `corpus/fixtures`) — the subset
    /// small enough for a unit test. `xtask wasm-threads` runs the same
    /// comparison over the whole in-repo corpus (adding `bench/render-set`).
    fn corpus() -> Vec<(String, Vec<u8>)> {
        let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures")];
        dirs.push(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("corpus/fixtures"),
        );
        let mut out = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            for path in paths {
                if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                out.push((
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    bytes,
                ));
            }
        }
        out
    }

    /// One named corpus document.
    fn fixture(name: &str) -> Vec<u8> {
        corpus()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("missing corpus fixture {name}"))
            .1
    }

    /// The untiled reference render at 72 DPI (the same steady state the CLI
    /// and the WASM harness use).
    fn untiled(doc: &[u8]) -> Option<(Budget, FixedClock, Session, PageView, Vec<u8>)> {
        let budget = Budget::profile(Surface::Viewer);
        let clock = FixedClock(0);
        let session = Session::open(doc.to_vec(), &budget, &clock).expect("open");
        let view = session.page_view(0, 72.0)?;
        let mut backend = TinySkiaBackend::new(view.width, view.height)?;
        let mut g = budget.guard_with(&clock, CancelToken::new());
        session
            .render_page(0, &mut backend, view.ctm, &budget, &mut g)
            .expect("untiled render");
        let pixels = backend.pixmap().data().to_vec();
        Some((budget, clock, session, view, pixels))
    }

    /// DoD (SL-4.WASM.03, clause 1): the tiled pipeline is byte-identical to
    /// the untiled one at three tile geometries, including the small tiles and
    /// the two documents whose boundary coverage exposed the quantum.
    #[test]
    fn tiled_equals_untiled_over_the_corpus() {
        let mut checked = 0usize;
        for (name, doc) in corpus() {
            let Some((budget, clock, session, view, full)) = untiled(&doc) else {
                continue;
            };
            for render in [
                TiledRender::new(256, 256, Parallelism::Single),
                TiledRender::new(view.width.max(1), 128, Parallelism::Single),
                TiledRender::new(32, 32, Parallelism::Single),
            ] {
                let tiled = render
                    .render(
                        &session,
                        0,
                        &view,
                        view.ctm,
                        &budget,
                        &clock,
                        &CancelToken::new(),
                    )
                    .expect("tiled render");
                assert_eq!(
                    tiled.len(),
                    full.len(),
                    "{name}: tiled {}x{} raster size",
                    render.tile_w,
                    render.tile_h
                );
                assert_eq!(
                    stable_hash(&tiled),
                    stable_hash(&full),
                    "{name}: tiled {}x{} differs from the untiled render",
                    render.tile_w,
                    render.tile_h
                );
                checked += 1;
            }
        }
        assert!(checked >= 15, "corpus too small: {checked} comparisons");
    }

    /// DoD (SL-4.WASM.03, clause 1): the two executors hash-equal over the
    /// corpus, with the threaded run really using several lanes.
    #[test]
    fn executors_hash_equal_over_the_corpus() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("a 4-lane pool builds natively");
        let mut checked = 0usize;
        for (name, doc) in corpus() {
            let Some((budget, clock, session, view, _)) = untiled(&doc) else {
                continue;
            };
            let single = TiledRender::new(256, 256, Parallelism::Single)
                .render(
                    &session,
                    0,
                    &view,
                    view.ctm,
                    &budget,
                    &clock,
                    &CancelToken::new(),
                )
                .expect("single executor");
            let threads = pool.install(|| {
                TiledRender::new(256, 256, Parallelism::Threads { lanes: 4 })
                    .render(
                        &session,
                        0,
                        &view,
                        view.ctm,
                        &budget,
                        &clock,
                        &CancelToken::new(),
                    )
                    .expect("threaded executor")
            });
            assert_eq!(
                stable_hash(&single),
                stable_hash(&threads),
                "{name}: the threaded executor moved a byte"
            );
            checked += 1;
        }
        assert!(checked >= 5, "corpus too small: {checked} comparisons");
    }

    /// A cancelled token fails the whole render: every tile observes it at its
    /// first budget tick, and the driver returns the error rather than a
    /// partially stitched raster.
    #[test]
    fn cancellation_fails_the_whole_tiled_render() {
        let doc = fixture("text.pdf");
        let (budget, clock, session, view, _) = untiled(&doc).expect("reference render");
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = TiledRender::default()
            .render(&session, 0, &view, view.ctm, &budget, &clock, &cancel)
            .expect_err("a cancelled render must fail");
        assert_eq!(err.code(), Code::Cancelled);
    }

    /// A budget too small for the stitched raster fails typed, never with a
    /// partial raster.
    #[test]
    fn a_tight_byte_budget_fails_typed() {
        let doc = fixture("text.pdf");
        let (_, _, session, view, _) = untiled(&doc).expect("reference render");
        let budget = Budget {
            bytes: 64,
            ..Budget::profile(Surface::Viewer)
        };
        let clock = FixedClock(0);
        let err = TiledRender::default()
            .render(
                &session,
                0,
                &view,
                view.ctm,
                &budget,
                &clock,
                &CancelToken::new(),
            )
            .expect_err("64 bytes cannot hold a page raster");
        assert!(
            matches!(err.code(), Code::BudgetBytes | Code::BudgetPixels),
            "typed budget failure, got {:?}",
            err.code()
        );
    }
}
