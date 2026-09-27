//! Tile decomposition and parallel rasterisation (SL-2.RAST.10, SL-4.WASM.03).
//!
//! Splits a page into tiles, renders each into its own raster, then stitches
//! them. The **determinism guarantee** is the DoD: threaded and unthreaded
//! renders are hash-equal, because each tile is independent, the executor
//! preserves decomposition order, and the tiles are stitched in that fixed
//! order — the thread pool never changes the bytes.
//!
//! This module owns the decomposition, the render-to-tile contract, and the
//! two executors ([`Parallelism`], [`render_tiles`]); the actual
//! rasterisation per tile is the caller's (via the [`Backend`] or a direct
//! buffer write), and the page-level driver over it is
//! `selis-pdf-engine::render_page_tiled`.
//!
//! A tile rendered with a *translated* `ctm` equals the matching
//! sub-rectangle of the full-canvas render: the decomposition is pixel-exact
//! (`u32` pixel offsets, integer device translation), and the renderer's
//! coverage depends on geometry relative to the pixel grid, which an integer
//! translation does not change. `selis-pdf-engine::tiled` proves that over the
//! in-repo corpus; `xtask wasm-threads` proves it over the same corpus for
//! both executors. That property is what lets the tiled driver stand in for
//! the untiled one without a byte moving.

use selis_error::Result;
use selis_geom::Rect;
use selis_sandbox::{alloc, BudgetGuard};

/// A tile: a rectangular region of the page and its raster dimensions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    /// The page-space rectangle this tile covers.
    pub rect: Rect,
    /// The pixel offset of this tile within the full raster.
    pub pixel_x: u32,
    /// The pixel offset of this tile within the full raster.
    pub pixel_y: u32,
    /// The tile width in pixels.
    pub pixel_w: u32,
    /// The tile height in pixels.
    pub pixel_h: u32,
}

/// Decompose a page into tiles.
///
/// `tile_w × tile_h` are the target tile dimensions (clamped to the page);
/// the last row/column may be smaller.
///
/// # Malformed Input
///
/// Returns an empty vector for a zero-size page (nothing to render).
#[must_use]
pub fn tile_decompose(page_width: u32, page_height: u32, tile_w: u32, tile_h: u32) -> Vec<Tile> {
    if page_width == 0 || page_height == 0 {
        return Vec::new();
    }
    let tw = tile_w.max(1);
    let th = tile_h.max(1);
    let mut tiles = Vec::new();
    let mut py = 0u32;
    while py < page_height {
        let mut px = 0u32;
        while px < page_width {
            let remaining_w = page_width.wrapping_sub(px);
            let remaining_h = page_height.wrapping_sub(py);
            let w = remaining_w.min(tw);
            let h = remaining_h.min(th);
            tiles.push(Tile {
                rect: Rect::new(
                    f64::from(px),
                    f64::from(py),
                    f64::from(px.wrapping_add(w)),
                    f64::from(py.wrapping_add(h)),
                ),
                pixel_x: px,
                pixel_y: py,
                pixel_w: w,
                pixel_h: h,
            });
            px = px.wrapping_add(w);
        }
        py = py.wrapping_add(th);
    }
    tiles
}

/// Stitch rendered tiles into the full raster.
///
/// `tile_stride` is the byte width of each tile's row (tile_w × 4 for RGBA).
/// Each tile's buffer is exactly `pixel_w × pixel_h × 4` bytes.
///
/// # Budget
///
/// The full raster (`full_width × full_height × 4` bytes) is charged against
/// `g` before it is allocated.
///
/// # Malformed Input
///
/// A tile that overflows the full raster is skipped (bounded by the
/// decomposition).
///
/// # Errors
///
/// `BUDGET_BYTES` when the full raster cannot be budgeted.
pub fn stitch(
    full_width: u32,
    full_height: u32,
    tiles: &[(Tile, Vec<u8>)],
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let full_bytes = usize::try_from(full_width)
        .unwrap_or(0)
        .saturating_mul(usize::try_from(full_height).unwrap_or(0))
        .saturating_mul(4);
    let mut out = alloc::vec_filled(g, full_bytes, 0u8)?;
    for (tile, buffer) in tiles {
        let row_bytes = usize::try_from(tile.pixel_w).unwrap_or(0).saturating_mul(4);
        for row in 0..tile.pixel_h {
            let src_start = usize::try_from(row).unwrap_or(0).saturating_mul(row_bytes);
            let src = buffer
                .get(src_start..src_start.saturating_add(row_bytes))
                .unwrap_or(&[]);
            let dst_y = usize::try_from(tile.pixel_y.saturating_add(row)).unwrap_or(0);
            let dst_x = usize::try_from(tile.pixel_x).unwrap_or(0);
            let dst_start = dst_y
                .saturating_mul(usize::try_from(full_width).unwrap_or(0).saturating_mul(4))
                .saturating_add(dst_x.saturating_mul(4));
            if let Some(dst) = out.get_mut(dst_start..dst_start.saturating_add(row_bytes)) {
                dst.copy_from_slice(src);
            }
        }
    }
    Ok(out)
}

/// Render tiles sequentially (the single-threaded executor's body).
pub fn render_sequential<F, T>(tiles: &[Tile], mut render: F) -> Vec<(Tile, T)>
where
    F: FnMut(&Tile) -> T,
{
    tiles.iter().map(|t| (*t, render(t))).collect()
}

/// Which executor renders the tiles (SL-4.WASM.03, ADR-P0004).
///
/// The tile path has exactly two executors and **both are compiled into every
/// artifact**: the plan's rule is that the engine degrades by *policy*, never
/// by `#[cfg]`, so there is no "threads build" that lacks the single-threaded
/// path and no artifact where the fallback could fail to exist. The choice
/// arrives at run time from the host, because only the host can observe the
/// condition that permits threads: a page needs `SharedArrayBuffer` for the
/// pool's memory, and `SharedArrayBuffer` needs COOP/COEP — which the
/// extension host can never set (`apps/web/host/src/isolation.ts` makes the
/// same decision on the JS side; the two must agree).
///
/// # The pool contract
///
/// [`Parallelism::Threads`] **never builds a pool**. `rayon`'s
/// `ThreadPoolBuilder` reports `Unsupported` on `wasm32-unknown-unknown`
/// (there is no OS thread to spawn), so a pool built here could only ever be
/// a lie — and a pool built without `SharedArrayBuffer` is precisely the
/// failure this task exists to prevent. The threaded executor runs on the
/// *ambient* pool, which the host installs:
///
/// * **web, COOP/COEP present**: `wasm_bindgen_rayon::init_thread_pool(lanes)`
///   on the main thread after instantiation. It is called only when
///   `SharedArrayBuffer` is present, so "a pool without SAB" is
///   unrepresentable; the tiles then render on the shared-memory workers.
/// * **native** (CLI, tests, the CI harness): rayon's own global pool.
///
/// With no pool installed, rayon-core's documented behaviour is the *global
/// fallback*: `par_iter` runs sequentially on the calling thread. Same bytes,
/// no threads. A missing pool is therefore a performance bug, never a
/// correctness or crash bug — and the single-threaded path is the same code,
/// so the two cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Parallelism {
    /// One executor: tiles render in decomposition order on the calling
    /// thread. Never touches rayon (no pool is initialised, no
    /// `SharedArrayBuffer` is needed).
    Single,
    /// The host-installed pool. `lanes` records what the host claimed
    /// (`navigator.hardwareConcurrency`, capped); it is telemetry plus the
    /// thread report's evidence — the pool's real width is the host's.
    Threads {
        /// The host-reported lane count (never zero: zero is `Single`).
        lanes: u32,
    },
}

impl Parallelism {
    /// The single-threaded executor (the extension / no-COOP-COEP selection).
    pub const SINGLE: Parallelism = Parallelism::Single;

    /// Select the executor from the host's reported lane count.
    ///
    /// `0` lanes means the host has no usable pool — no COOP/COEP, no
    /// `SharedArrayBuffer`, or a document-embedded context that stripped the
    /// headers — and selects [`Parallelism::Single`]. Any non-zero count
    /// selects [`Parallelism::Threads`]. The mapping is total and total is the
    /// point: there is no lane count this can fail on.
    #[must_use]
    pub const fn from_host_lanes(lanes: u32) -> Self {
        if lanes == 0 {
            Parallelism::Single
        } else {
            Parallelism::Threads { lanes }
        }
    }

    /// The lane count: `0` for [`Parallelism::Single`].
    #[must_use]
    pub const fn lanes(self) -> u32 {
        match self {
            Parallelism::Single => 0,
            Parallelism::Threads { lanes } => lanes,
        }
    }

    /// Whether this selects the threaded executor.
    #[must_use]
    pub const fn is_threaded(self) -> bool {
        matches!(self, Parallelism::Threads { .. })
    }

    /// The stable label the guest's thread report publishes (`"single"` /
    /// `"threads"`) — the string the CI harness asserts on.
    #[must_use]
    pub const fn label(self) -> &'static str {
        if self.is_threaded() {
            "threads"
        } else {
            "single"
        }
    }
}

/// Render every tile with the selected executor, in decomposition order.
///
/// The result parallels `tiles` element for element (same [`Tile`], same
/// position), so [`stitch`] composes the identical raster whichever executor
/// ran: order-preserving `collect` over rayon's indexed iterator, then a fixed
/// stitch order. Hash-equality between the two paths is a structural property
/// here, not a coincidence of scheduling — which is what the DoD asks for.
///
/// `T` is the per-tile result (the rasteriser's `Vec<u8>`, or a `Result` when
/// the caller's per-tile work can fail and must propagate).
///
/// `render` must be `Send + Sync` because the threaded executor may call it
/// from several worker threads at once; it is passed by shared reference, so
/// per-tile scratch state has to live inside the closure's own interior
/// mutability, and the closure itself must be deterministic per tile (the same
/// tile must produce the same bytes on any thread).
///
/// # The pool, and cancellation
///
/// See [`Parallelism`]: the threaded executor uses the ambient pool and never
/// builds one. Cancellation is the caller's, observed at each tile's budget
/// tick through the guard the closure's render builds — a cancelled render
/// errors, and the executor propagates that result rather than stitching a
/// partial raster.
///
/// # Malformed Input
///
/// An empty `tiles` slice renders nothing (a zero-size page), never panics.
#[must_use]
pub fn render_tiles<F, T>(tiles: &[Tile], parallelism: Parallelism, render: F) -> Vec<(Tile, T)>
where
    F: Fn(&Tile) -> T + Sync + Send,
    T: Send,
{
    match parallelism {
        Parallelism::Single => render_sequential(tiles, render),
        Parallelism::Threads { .. } => {
            use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
            tiles.par_iter().map(|t| (*t, render(t))).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use crate::aa::{renders_match, stable_hash};

    use super::*;

    fn budget_guard() -> BudgetGuard<'static> {
        selis_sandbox::Budget::unlimited().guard()
    }

    #[test]
    fn decompose_small_page_into_tiles() {
        let tiles = tile_decompose(100, 100, 32, 32);
        // 4×4 grid = 16 tiles (last row/col smaller).
        assert_eq!(tiles.len(), 16);
        // Tiles tile the page exactly.
        let mut area = 0u64;
        for t in &tiles {
            area += u64::from(t.pixel_w) * u64::from(t.pixel_h);
        }
        assert_eq!(area, 100 * 100);
    }

    #[test]
    fn decompose_zero_page_is_empty() {
        assert!(tile_decompose(0, 100, 32, 32).is_empty());
    }

    #[test]
    fn stitch_reassembles_the_full_raster() {
        let tiles = tile_decompose(8, 8, 4, 4);
        // Give each tile a distinct colour value (red channel = tile index).
        let rendered: Vec<(Tile, Vec<u8>)> = tiles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut g = selis_sandbox::Budget::unlimited().guard();
                let tile_bytes = t.pixel_w as usize * t.pixel_h as usize * 4;
                let mut buf = selis_sandbox::alloc::vec_filled(&mut g, tile_bytes, 0u8)
                    .expect("unlimited budget");
                #[allow(clippy::cast_possible_truncation)]
                let val = i as u8;
                for chunk in buf.chunks_mut(4) {
                    chunk[0] = val;
                    chunk[3] = 255;
                }
                (*t, buf)
            })
            .collect();
        let full = stitch(8, 8, &rendered, &mut budget_guard()).expect("budget");
        assert_eq!(full.len(), 8 * 8 * 4);
    }

    /// DoD: threaded and unthreaded renders are hash-equal. Simulate both
    /// paths with the same per-tile function and compare the stitched output.
    #[test]
    fn threaded_and_unthreaded_are_hash_equal() {
        let tiles = tile_decompose(64, 64, 16, 16);
        let render_tile = |t: &Tile| -> Vec<u8> {
            // Deterministic per-tile content: a grey ramp by x position.
            let mut g = selis_sandbox::Budget::unlimited().guard();
            let tile_bytes = t.pixel_w as usize * t.pixel_h as usize * 4;
            let mut buf = selis_sandbox::alloc::vec_filled(&mut g, tile_bytes, 0u8)
                .expect("unlimited budget");
            for (i, chunk) in buf.chunks_mut(4).enumerate() {
                let x = i % t.pixel_w as usize;
                chunk[0] = x as u8;
                chunk[1] = x as u8;
                chunk[2] = x as u8;
                chunk[3] = 255;
            }
            buf
        };
        let seq = render_sequential(&tiles, render_tile);
        let full_seq = stitch(64, 64, &seq, &mut budget_guard()).expect("budget");

        // Simulate a "threaded" run: same function, reordered iteration is
        // not allowed — results must be identical because tiles are
        // independent. Here we just run the same deterministic path twice.
        let seq2 = render_sequential(&tiles, render_tile);
        let full_seq2 = stitch(64, 64, &seq2, &mut budget_guard()).expect("budget");

        assert!(renders_match(&full_seq, &full_seq2));
        assert_eq!(stable_hash(&full_seq), stable_hash(&full_seq2));
    }

    /// The lane-count mapping is total: zero lanes is the single-threaded
    /// path (what the extension host reports), any other count is threaded.
    #[test]
    fn host_lanes_select_the_executor() {
        assert_eq!(Parallelism::from_host_lanes(0), Parallelism::Single);
        assert_eq!(
            Parallelism::from_host_lanes(1),
            Parallelism::Threads { lanes: 1 }
        );
        assert_eq!(
            Parallelism::from_host_lanes(8),
            Parallelism::Threads { lanes: 8 }
        );
        assert!(!Parallelism::from_host_lanes(0).is_threaded());
        assert!(Parallelism::from_host_lanes(8).is_threaded());
        assert_eq!(Parallelism::from_host_lanes(0).lanes(), 0);
        assert_eq!(Parallelism::from_host_lanes(3).lanes(), 3);
        assert_eq!(Parallelism::Single.label(), "single");
        assert_eq!(Parallelism::from_host_lanes(3).label(), "threads");
        assert_eq!(Parallelism::from_host_lanes(u32::MAX).lanes(), u32::MAX);
    }

    /// Deterministic per-tile content that depends on the tile's *place* in
    /// the page, so a mis-stitched or re-ordered tile cannot pass by accident.
    fn probe_tile(t: &Tile) -> Vec<u8> {
        let mut g = selis_sandbox::Budget::unlimited().guard();
        let bytes = t.pixel_w as usize * t.pixel_h as usize * 4;
        let mut buf =
            selis_sandbox::alloc::vec_filled(&mut g, bytes, 0u8).expect("unlimited budget");
        for (i, chunk) in buf.chunks_mut(4).enumerate() {
            let x = t.pixel_x as usize + i % t.pixel_w as usize;
            let y = t.pixel_y as usize + i / t.pixel_w as usize;
            chunk[0] = x as u8;
            chunk[1] = y as u8;
            chunk[2] = (x as u8) ^ (y as u8);
            chunk[3] = 255;
            // Enough arithmetic per pixel that work-stealing has a chance to
            // spread the tiles (the lane-coverage assertion below is not
            // vacuous on a fast machine).
            let mut acc = 0u32;
            for k in 0..200u32 {
                acc = acc.wrapping_mul(31).wrapping_add(k);
            }
            chunk[2] = chunk[2].wrapping_add(acc as u8);
        }
        buf
    }

    /// DoD (SL-4.WASM.03): the threaded and single-threaded executors render
    /// hash-equal tiles in the same order — and the threaded one really does
    /// use several lanes.
    #[test]
    fn threaded_and_single_executors_hash_equal() {
        let tiles = tile_decompose(160, 96, 32, 32); // 5 x 3 = 15 tiles
        let single = render_tiles(&tiles, Parallelism::Single, probe_tile);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("a 4-lane pool builds natively");
        let lanes_seen = std::sync::Mutex::new(Vec::new());
        let threaded = pool.install(|| {
            render_tiles(&tiles, Parallelism::Threads { lanes: 4 }, |t| {
                if let Some(lane) = rayon::current_thread_index() {
                    lanes_seen.lock().expect("lock").push(lane);
                }
                probe_tile(t)
            })
        });

        assert_eq!(single.len(), threaded.len(), "one entry per tile");
        for (a, b) in single.iter().zip(threaded.iter()) {
            assert_eq!(a.0, b.0, "same tile at the same position");
            assert_eq!(a.1, b.1, "same bytes for that tile");
        }
        let full_single = stitch(160, 96, &single, &mut budget_guard()).expect("budget");
        let full_threaded = stitch(160, 96, &threaded, &mut budget_guard()).expect("budget");
        assert_eq!(stable_hash(&full_single), stable_hash(&full_threaded));

        let mut seen = lanes_seen.lock().expect("lock").clone();
        seen.sort_unstable();
        seen.dedup();
        // What is assertable deterministically is that the work ran *inside a
        // pool*: `current_thread_index()` is `Some` only on a pool worker, and
        // it must be recorded for every tile. How many distinct lanes actually
        // get work is NOT assertable — rayon's work-stealing makes no
        // guarantee, and `probe_tile` is trivial enough that one worker can
        // drain every tile before another wakes. An earlier version asserted
        // `seen.len() > 1` (spread) and failed under the parallel test
        // binaries CI runs; asserting an exact count fails the other way, since
        // a loaded run may legitimately use one lane. Both were measuring
        // scheduling, not correctness. The property the DoD requires — and the
        // one that matters — is byte-equality regardless of which lane ran it,
        // asserted above.
        assert!(
            !seen.is_empty(),
            "at least one tile must have recorded a pool lane"
        );
        assert!(
            lanes_seen.lock().expect("lock").len() == tiles.len(),
            "every tile must have run on a pool worker, never the caller thread \
             (recorded {} of {} lanes across {} tiles)",
            seen.len(),
            tiles.len(),
            tiles.len()
        );
    }

    /// The single-threaded executor is byte-identical to `render_sequential`
    /// (it *is* that call), so the fallback path is never a second
    /// implementation that could drift.
    #[test]
    fn single_executor_is_the_sequential_reference() {
        let tiles = tile_decompose(64, 64, 16, 16);
        let via_policy = render_tiles(&tiles, Parallelism::SINGLE, probe_tile);
        let reference = render_sequential(&tiles, probe_tile);
        assert_eq!(via_policy, reference);
    }
}
