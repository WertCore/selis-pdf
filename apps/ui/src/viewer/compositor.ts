/**
 * The tile compositor (SL-4.UI.03).
 *
 * UI.02 decides *what exists* — which pages are in the window, where their
 * boxes are, which rung of the ladder is available. This module decides *what
 * the pixels look like*: which cached tile to present, at which device scale,
 * into which device-pixel box, and whether it is standing in for a sharper one.
 * The division matters because only one of the two needs a canvas: `layout.ts`
 * and `windowing.ts` stay pure and testable, and so does this, because the
 * surface and the clock are ports (`surface.ts`).
 *
 * ## The four things this has to get right
 *
 * 1. **Device-pixel ratio.** A tile is only crisp when it is presented 1:1 —
 *    the destination box in surface device pixels must be *exactly* the
 *    bitmap's size. Getting there takes two corrections that are easy to miss:
 *
 *    - The backing store is whole device pixels, so the device scale actually
 *      in force is `round(css × dpr) / css`, not `dpr`. At DPR 1.25 and a
 *      1013-px viewport that is 1266/1013 = 1.24975…, and drawing with 1.25
 *      leaves a seam down the right-hand edge and resamples every tile.
 *      {@link effectiveDeviceScale} is that number, and the compositor
 *      *reports it back to the page list* so the ladder asks the engine for
 *      exactly the resolution the surface will draw — which is what makes the
 *      presentation 1:1 rather than nearly-1:1.
 *    - Destination edges are rounded, and rounded from *shared* edges: the
 *      right edge of one page and the left edge of the next are the same
 *      number, so a fractional device scale cannot open a seam between them.
 *
 * 2. **The zoom path.** `PageList.setZoom` relayouts and re-renders, and the
 *    engine needs tens of milliseconds to catch up. The reader must not see a
 *    blank page in the meantime, so the compositor presents the *old* tile
 *    immediately, scaled to the new geometry ({@link DrawOp.provisional} =
 *    true), and the ladder refines it behind that. This is why the tile cache
 *    is keyed by scale and why a zoom does not clear it — see the note on
 *    `TileScheduler` in `tile-ladder.ts`.
 *
 * 3. **Transferred, not copied.** The compositor hands the surface the tile
 *    handle itself and never reads the payload afterwards; see
 *    {@link TileSurface.takesOwnership}. A frame is a dozen `drawImage` calls
 *    and nothing else, which is the whole cost model of the 16 ms budget.
 *
 * 4. **Coalescing.** A scroll produces far more input events than frames. The
 *    compositor holds at most one pending frame request, so N events cost one
 *    draw, and the draw reads the controller's *current* state rather than a
 *    queue of stale ones.
 *
 * ## What it deliberately does not do
 *
 * It does not keep a second copy of "what is on screen": every box it draws
 * comes from `PageListState.tiles`, the same values the DOM list paints, and
 * every tile it draws keeps its DOM tile, its `aria-label` and its place in
 * the roving tab order. The canvas is a *painting* of the list, never a
 * replacement for it — that is what keeps this from being an accessibility
 * regression (SL-4.UI.07 owns the full pass).
 */

import type { PageList, PageListViewport } from "./page-list.js";
import type {
	CompositorFrame,
	DrawOp,
	FrameClock,
	FrameHandle,
	SurfaceSize,
	TileSurface,
} from "./surface.js";
import type { TileEntry } from "./tile-ladder.js";

/**
 * How much a `lowres` bitmap is penalised against a `full` one when both are
 * candidates for the same destination, in octaves of scale error.
 *
 * A quarter octave is small enough that a genuinely closer bitmap wins, and
 * large enough that an exact-resolution `full` tile beats a `lowres` one even
 * when the low-res happens to be nearer — which is the zoomed-out case, where
 * the low-res rung (a fixed 0.25 device px per point) can land closer to the
 * requested scale than a full-res tile does.
 */
const LOW_RES_PENALTY_OCTAVES = 0.25;

/**
 * Relative tolerance for "this bitmap is at the scale being asked for".
 *
 * A tenth of a percent. The ladder computes its request scale from the same
 * `cssScale × deviceScale` product the compositor draws with, so an exact hit
 * is exact to floating point; the tolerance only absorbs that noise, and is
 * far below the half-percent difference a viewer can see.
 */
const SCALE_EPSILON = 1e-3;

/**
 * The device-pixel-ratio correction described in the module doc.
 *
 * A canvas backing store has whole pixels, so the surface cannot honour a
 * fractional `devicePixelRatio` exactly. What it *can* do is honour the
 * rounded store exactly, and that is the number the compositor draws with —
 * and reports to the ladder, so the tile is rasterised at the size it will be
 * shown. Degenerate inputs (0, NaN, a zero-width viewport before first
 * layout) collapse to 1 rather than producing a zero-sized surface.
 */
export function effectiveDeviceScale(
	cssWidth: number,
	cssHeight: number,
	devicePixelRatio: number,
): { scale: number; deviceWidth: number; deviceHeight: number } {
	const ratio = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0 ? devicePixelRatio : 1;
	const width = Number.isFinite(cssWidth) && cssWidth > 0 ? cssWidth : 0;
	const height = Number.isFinite(cssHeight) && cssHeight > 0 ? cssHeight : 0;
	const deviceWidth = Math.max(1, Math.round(width * ratio));
	const deviceHeight = Math.max(1, Math.round(height * ratio));
	return {
		// With no viewport yet there is nothing to fill, so 1:1 is the only
		// honest answer; the first real `update` replaces it.
		scale: width > 0 ? deviceWidth / width : 1,
		deviceWidth,
		deviceHeight,
	};
}

/** The surface size for a viewport, ready to hand to {@link TileSurface.configure}. */
export function surfaceSizeFor(viewport: PageListViewport): SurfaceSize {
	const { scale, deviceWidth, deviceHeight } = effectiveDeviceScale(
		viewport.width,
		viewport.height,
		viewport.devicePixelRatio,
	);
	return {
		cssWidth: Math.max(0, viewport.width),
		cssHeight: Math.max(0, viewport.height),
		deviceWidth,
		deviceHeight,
		scale,
		devicePixelRatio:
			Number.isFinite(viewport.devicePixelRatio) && viewport.devicePixelRatio > 0
				? viewport.devicePixelRatio
				: 1,
	};
}

/** True when a bitmap is already at the scale being drawn. */
export function isAtScale(entry: TileEntry, targetDeviceScale: number): boolean {
	if (!(targetDeviceScale > 0) || !(entry.deviceScale > 0)) {
		return false;
	}
	return Math.abs(entry.deviceScale - targetDeviceScale) <= SCALE_EPSILON * targetDeviceScale;
}

/**
 * Choose which cached bitmap to present for a page.
 *
 * The rule is "the bitmap whose resolution is nearest the one we are drawing
 * at", because that is what a resample costs: a bitmap at exactly the target
 * is a blit, one at 2× is a clean downscale, and one at 1/8× (the low-res
 * rung under a deep zoom) is the honest placeholder it is. Ties — and near
 * ties, within {@link LOW_RES_PENALTY_OCTAVES} — go to `full`, so an exact
 * full-res tile is never beaten by a low-res one that happens to be a
 * fraction nearer.
 *
 * Pure, and unit-tested in `compositor.test.ts` against the cases that
 * actually occur: a fresh full-res tile, a fresh low-res tile, both after a
 * zoom, and an empty cache.
 */
export function pickTile(
	entries: readonly TileEntry[],
	targetDeviceScale: number,
): TileEntry | null {
	if (entries.length === 0 || !(targetDeviceScale > 0)) {
		return null;
	}
	let best: TileEntry | null = null;
	let bestScore = Number.POSITIVE_INFINITY;
	for (const entry of entries) {
		const ratio = entry.deviceScale / targetDeviceScale;
		if (!(ratio > 0)) {
			continue;
		}
		const penalty = entry.stage === "lowres" ? LOW_RES_PENALTY_OCTAVES : 0;
		const score = Math.abs(Math.log2(ratio)) + penalty;
		if (score < bestScore) {
			best = entry;
			bestScore = score;
		}
	}
	return best;
}

/** What the host can learn about a presented frame without reading the surface. */
export interface PresentedFrame {
	readonly index: number;
	/** Clock timestamp the frame was built for. */
	readonly at: number;
	readonly size: SurfaceSize;
	readonly scrollTop: number;
	/** Draw ops issued, in document order. */
	readonly ops: readonly DrawOp[];
	/** Ops presenting a bitmap that is not at the current scale. */
	readonly provisional: number;
	/** Windowed pages with no usable pixels; the CSS placeholder shows through. */
	readonly placeholders: number;
	/** Windowed pages that fell outside the viewport and were not drawn at all. */
	readonly culled: number;
	/** The frame handed to the surface, kept so a host can assert on it. */
	readonly frame: CompositorFrame;
}

export interface TileCompositorOptions {
	/** The controller that owns the window, the ladder and the boxes. */
	readonly list: PageList;
	/** Where frames go. See `surface.ts` for the host variants. */
	readonly surface: TileSurface;
	/** The animation-frame clock; the compositor coalesces on it. */
	readonly clock: FrameClock;
	/** Called after every presented frame — diagnostics, and the UI.07 probes. */
	readonly onFrame?: (frame: PresentedFrame) => void;
}

/** What a shell programs against. Mirrors `PageList`: report facts, get a frame. */
export interface TileCompositor {
	/**
	 * Report new viewport metrics — a scroll, a resize, a DPR change — and
	 * schedule a frame.
	 *
	 * The `devicePixelRatio` is passed on to the ladder *corrected* for the
	 * backing store's rounding (see {@link effectiveDeviceScale}), because a
	 * tile rasterised at 1.25 and drawn at 1.24975 is a resample of the whole
	 * page, and on text that softness is exactly what "not crisp" means.
	 */
	update(viewport: PageListViewport): void;
	/** Schedule a frame without changing the viewport (a tile landed). */
	invalidate(): void;
	/**
	 * Draw now if a frame is pending, and return it. `timestampMs` defaults to
	 * a monotonic counter; hosts that drive their own clock (and every test)
	 * use this instead of waiting for a frame that may not be coming.
	 */
	flush(timestampMs?: number): PresentedFrame | null;
	/** The last presented frame, or `null` if none has been drawn. */
	lastFrame(): PresentedFrame | null;
	/** The backing store the surface is currently sized for. */
	size(): SurfaceSize;
	dispose(): void;
}

/**
 * Create the compositor and subscribe it to the page list.
 *
 * From here on the loop is one call per input event: the shell measures the
 * viewport and calls {@link TileCompositor.update}. A tile landing from the
 * engine needs nothing — the controller republishes its state, the
 * subscription marks a frame, and the next frame picks up the new rung.
 */
export function createTileCompositor(options: TileCompositorOptions): TileCompositor {
	const { list, surface, clock } = options;
	let size: SurfaceSize = surfaceSizeFor({
		width: 0,
		height: 0,
		scrollTop: 0,
		devicePixelRatio: 1,
	});
	let configured: SurfaceSize | null = null;
	let pending: FrameHandle | null = null;
	let frameIndex = 0;
	let clockTicks = 0;
	let last: PresentedFrame | null = null;
	let disposed = false;

	/** Reallocate the backing store only when the size really changed. */
	function ensureConfigured(): void {
		const changed =
			configured === null ||
			configured.deviceWidth !== size.deviceWidth ||
			configured.deviceHeight !== size.deviceHeight;
		if (changed) {
			surface.configure(size);
			configured = size;
		}
	}

	/**
	 * Build the frame for the controller's *current* state.
	 *
	 * Reading the live state rather than a queued snapshot is what makes
	 * coalescing safe: a frame scheduled during a scroll burst presents where
	 * the reader ended up, not where the first event of the burst was.
	 */
	function buildFrame(at: number): PresentedFrame {
		const state = list.state();
		const ops: DrawOp[] = [];
		let placeholders = 0;
		let culled = 0;
		let provisional = 0;
		const deviceScale = size.scale;
		const targetDeviceScale = state.scale * deviceScale;
		const bottom = state.scrollTop + size.cssHeight;

		for (const tile of state.tiles) {
			// Cull in CSS space, before any rounding: an overscan row that is
			// nowhere near the viewport must not cost a draw call. The window is
			// deliberately larger than the viewport — that is the ladder's
			// prefetch runway — so this is where the two meet.
			if (tile.y + tile.height < state.scrollTop || tile.y > bottom) {
				culled += 1;
				continue;
			}
			const entry = pickTile(list.tilesFor(tile.page), targetDeviceScale);
			if (entry === null) {
				// No pixels at all: leave the box alone. The DOM tile's
				// placeholder styling shows through the canvas, which is why
				// this costs nothing visually and nothing in a11y — the tile
				// element, its label and its tab stop are untouched.
				placeholders += 1;
				continue;
			}
			// Edges are rounded from shared list coordinates, so adjacent tiles
			// cannot open a seam between them at a fractional device scale.
			const left = Math.round(tile.x * deviceScale);
			const top = Math.round((tile.y - state.scrollTop) * deviceScale);
			const right = Math.round((tile.x + tile.width) * deviceScale);
			const foot = Math.round((tile.y + tile.height - state.scrollTop) * deviceScale);
			const scaled = right - left !== entry.tile.width || foot - top !== entry.tile.height;
			const standing = !isAtScale(entry, targetDeviceScale);
			if (standing) {
				provisional += 1;
			}
			ops.push({
				page: tile.page,
				stage: entry.stage,
				deviceScale: entry.deviceScale,
				targetDeviceScale,
				sourceWidth: entry.tile.width,
				sourceHeight: entry.tile.height,
				x: left,
				y: top,
				width: right - left,
				height: foot - top,
				scaled,
				provisional: standing,
				// The handle itself, not a copy of the pixels: a surface that
				// takes ownership transfers it, and a surface that does not
				// reads it. See TileSurface.takesOwnership.
				source: entry,
			});
		}

		const frame: CompositorFrame = {
			index: frameIndex,
			at,
			size,
			scrollTop: state.scrollTop,
			clear: true,
			ops,
			placeholders,
		};
		frameIndex += 1;
		return {
			index: frame.index,
			at,
			size,
			scrollTop: state.scrollTop,
			ops,
			provisional,
			placeholders,
			culled,
			frame,
		};
	}

	function present(presented: PresentedFrame): void {
		surface.draw(presented.frame);
		surface.present();
		last = presented;
		options.onFrame?.(presented);
	}

	function schedule(): void {
		if (disposed || pending !== null) {
			// Already armed: the frame reads the latest state anyway, so this
			// is the coalescing, and it is the only reason a scroll burst costs
			// one draw rather than one draw per event.
			return;
		}
		pending = clock.requestFrame((timestamp) => {
			pending = null;
			if (disposed) {
				return;
			}
			ensureConfigured();
			present(buildFrame(timestamp));
		});
	}

	// Every published state — a scroll, a zoom, a tile landing — is a reason to
	// draw, and the controller is the only thing that knows when that is. The
	// subscription is what keeps a shell from having to remember to call
	// `invalidate`, which is the mistake that would show up as a tile that
	// never appears.
	const unsubscribe = list.subscribe(() => {
		schedule();
	});

	const compositor: TileCompositor = {
		update(viewport) {
			if (disposed) {
				return;
			}
			size = surfaceSizeFor(viewport);
			// The corrected ratio goes to the ladder; the nominal one stays on
			// the size record for diagnostics and for hosts that need it.
			list.update({
				width: viewport.width,
				height: viewport.height,
				scrollTop: viewport.scrollTop,
				devicePixelRatio: size.scale,
			});
			schedule();
		},
		invalidate() {
			schedule();
		},
		flush(timestampMs) {
			if (disposed || pending === null) {
				return null;
			}
			pending = null;
			// A default clock: monotonically increasing, so two frames in the
			// same millisecond are still ordered. Only used when a host flushes
			// by hand instead of waiting for a real frame.
			clockTicks += 1;
			ensureConfigured();
			const presented = buildFrame(timestampMs ?? clockTicks);
			present(presented);
			return presented;
		},
		lastFrame() {
			return last;
		},
		size() {
			return size;
		},
		dispose() {
			if (disposed) {
				return;
			}
			disposed = true;
			if (pending !== null) {
				clock.cancelFrame(pending);
				pending = null;
			}
			unsubscribe();
			surface.dispose();
		},
	};

	return compositor;
}
