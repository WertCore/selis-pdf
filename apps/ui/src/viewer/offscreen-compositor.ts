/**
 * The rendering worker: it owns the `OffscreenCanvas` and does the drawing
 * (SL-4.UI.03).
 *
 * This module runs *inside* the worker, next to the engine. It is the reason
 * the frame is cheap on the UI thread: rasterising a dozen `drawImage` calls
 * into a canvas that is not on the UI thread at all, and handing the result
 * back by transfer, means the main thread's per-frame cost is a message post
 * and a single `drawImage` of the finished frame.
 *
 * The seam is the same one the rest of `apps/ui` uses: the canvas, its 2D
 * context, `transferToImageBitmap` and `postMessage` all arrive as parameters
 * (see `worker-surface.ts`), so this file is testable in Node against a
 * recording context and names no platform global. `offscreen-compositor.test.ts`
 * drives the real functions against that recorder, so the draw calls, the
 * smoothing decisions and the transfer list are asserted rather than described.
 *
 * ## The tile store
 *
 * The worker already holds every rendered tile, because the engine is here.
 * {@link OffscreenCompositor.put} files a bitmap under {@link surfaceOpKey}'s
 * key when a render lands, and `draw` looks tiles up by that key. Nothing is
 * transferred *to* this side for a tile — the pixels never left. An op with no
 * entry is counted as missing and skipped rather than drawn as garbage: the DOM
 * tile's placeholder shows through, which is the same visual answer as on the
 * main-thread path.
 */

import { SurfaceFaultError } from "./surface.js";
import type { SurfaceFrame } from "./worker-surface.js";
import { SURFACE_PROTOCOL, surfaceOpKey } from "./worker-surface.js";

/**
 * The slice of `OffscreenCanvasRenderingContext2D` this module uses.
 *
 * Narrow on purpose: it is the intersection of what a real worker context
 * provides and what a recorder can implement, so the tests exercise the real
 * code path rather than a mock of a different interface.
 */
export interface CompositorContext {
	imageSmoothingEnabled: boolean;
	clearRect(x: number, y: number, width: number, height: number): void;
	drawImage(
		source: unknown,
		sx: number,
		sy: number,
		sw: number,
		sh: number,
		dx: number,
		dy: number,
		dw: number,
		dh: number,
	): void;
}

/** The slice of `OffscreenCanvas` this module uses. */
export interface OffscreenTarget {
	width: number;
	height: number;
	getContext(kind: "2d"): CompositorContext | null;
	/** Present the canvas's contents and hand back ownership of the image. */
	transferToImageBitmap(): unknown;
}

/** One `drawImage` as a recorder saw it. */
export interface RecordedDraw {
	readonly source: unknown;
	readonly sx: number;
	readonly sy: number;
	readonly sw: number;
	readonly sh: number;
	readonly dx: number;
	readonly dy: number;
	readonly dw: number;
	readonly dh: number;
	readonly smoothing: boolean;
}

/** What one `draw` did, for the worker's own diagnostics. */
export interface FrameOutcome {
	readonly index: number;
	readonly drawn: number;
	/** Ops with no bitmap in the store: skipped, not faked. */
	readonly missing: number;
}

export interface OffscreenCompositorOptions {
	/** The transferred canvas. */
	readonly target: OffscreenTarget;
	/** `postMessage` with a transfer list; the frame goes on it. */
	readonly post: (message: unknown, transfer: readonly unknown[]) => void;
}

/**
 * The worker's drawing surface.
 *
 * The surface is *viewport-sized*, not document-sized, and that is a
 * deliberate choice rather than an oversight: a canvas the size of the document
 * would be 2 000 pages tall, which at DPR 2 is far past what a canvas backing
 * store can hold, and reallocating it on every zoom is a stall. So the canvas
 * covers the viewport and the compositor's ops are already in viewport
 * coordinates — scrolling redraws, which is what a compositor does, and at a
 * dozen `drawImage` calls a frame that is not the expensive part.
 */
export interface OffscreenCompositor {
	/** File a rendered tile, closing any bitmap it replaces. */
	put(op: { page: number; stage: "lowres" | "full"; deviceScale: number }, bitmap: unknown): void;
	/** Draw one serialised frame. */
	draw(frame: SurfaceFrame): FrameOutcome;
	/** Hand the composed frame back, transferred. */
	present(frame: SurfaceFrame, outcome: FrameOutcome): void;
	/** Drop the tiles and the surface. */
	release(): void;
	/** How many bitmaps are held, for the worker's memory diagnostics. */
	readonly held: number;
}

export function createOffscreenCompositor(
	options: OffscreenCompositorOptions,
): OffscreenCompositor {
	const tiles = new Map<string, unknown>();
	let context: CompositorContext | null = null;

	return {
		get held() {
			return tiles.size;
		},

		put(op, bitmap) {
			const key = surfaceOpKey(op);
			const previous = tiles.get(key);
			tiles.set(key, bitmap);
			// The old bitmap is a GPU-backed object; leaving it to the collector
			// is how a zoom-spam session ends up holding every scale ever drawn.
			if (previous !== undefined) {
				closeIfPossible(previous);
			}
		},

		draw(frame) {
			if (context === null) {
				context = options.target.getContext("2d");
			}
			// Resize here rather than trusting a `configure` to have arrived
			// first: assigning `width` resets the context, and doing it in `draw`
			// keeps the two operations in one place.
			const target = options.target;
			if (target.width !== frame.size.deviceWidth || target.height !== frame.size.deviceHeight) {
				target.width = frame.size.deviceWidth;
				target.height = frame.size.deviceHeight;
				context = target.getContext("2d");
			}
			const ctx = context;
			if (ctx === null) {
				throw new SurfaceFaultError("no-2d-context");
			}
			ctx.clearRect(0, 0, target.width, target.height);
			let drawn = 0;
			let missing = 0;
			for (const op of frame.ops) {
				const bitmap = tiles.get(surfaceOpKey(op));
				if (bitmap === undefined) {
					missing += 1;
					continue;
				}
				// Smoothing off exactly when the bitmap is not being shown at its
				// own resolution. A stretched low-res or a zoom blit then reads as
				// "not yet sharp", which is true, instead of as a soft blur, which
				// reads as a rendering bug.
				ctx.imageSmoothingEnabled = !op.scaled;
				ctx.drawImage(
					bitmap,
					0,
					0,
					op.sourceWidth,
					op.sourceHeight,
					op.x,
					op.y,
					op.width,
					op.height,
				);
				drawn += 1;
			}
			return { index: frame.index, drawn, missing };
		},

		present(frame, outcome) {
			const bitmap = options.target.transferToImageBitmap();
			// The transfer list is the point: the frame moves to the UI thread by
			// ownership. Without it this is a structured clone of the whole
			// viewport, every frame.
			options.post(
				{
					v: SURFACE_PROTOCOL,
					op: "frame",
					bitmap,
					index: outcome.index,
					ops: outcome.drawn,
					missing: outcome.missing,
					scrollTop: frame.scrollTop,
				},
				[bitmap],
			);
		},

		release() {
			for (const bitmap of tiles.values()) {
				closeIfPossible(bitmap);
			}
			tiles.clear();
			context = null;
		},
	};
}

/** Close an `ImageBitmap` when the host has one; harmless otherwise. */
function closeIfPossible(bitmap: unknown): void {
	const closable = bitmap as { close?: () => void };
	if (typeof closable.close === "function") {
		closable.close();
	}
}
