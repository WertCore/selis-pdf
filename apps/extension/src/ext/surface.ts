/**
 * SL-4.EXT.06 - the viewer page's `TileSurface`, and the clock beside it.
 *
 * ## Why this host presents on the main thread
 *
 * `apps/ui`'s `viewer/surface.ts` exists so the compositor names a port rather
 * than a canvas, and it names two implementations: `createWorkerSurface` (the
 * web build, which transfers the canvas once and gets an `ImageBitmap` back per
 * frame) and this one. The choice is forced, not preferred:
 *
 * - A `chrome.runtime` port serialises with **JSON**. It has no transfer list,
 *   so an `OffscreenCanvas` cannot be handed to another context and no
 *   `ImageBitmap` can come back. UI.03's whole performance argument rests on
 *   moving those by ownership, and neither can move here.
 * - A `Worker` spawned by the viewer page would be the right shape, but it dies
 *   with the page - the exact lifetime the `offscreen` permission was justified
 *   to avoid - and the engine is in the offscreen document, not in it.
 *
 * So the extension pays the main-thread cost and says so here rather than
 * shipping `createWorkerSurface` in the hope it works. It is still in the
 * package, because `createAnimationFrameClock` lives in the same module and is
 * reused verbatim below; see `REUSE.md` for the size note.
 *
 * ## The clock is not reimplemented
 *
 * `createAnimationFrameClock` is imported from `apps/ui`, not copied. The
 * id-to-handle bookkeeping in it is what makes a cancelled frame observably
 * cancelled, and the 16 ms coalescing budget depends on it - a second
 * implementation here would be the exact drift the `PlatformAdapter` seam
 * exists to prevent.
 */

import type { RenderedTile } from "../../../ui/src/platform/types.js";
import type {
	CompositorFrame,
	DrawOp,
	SurfaceSize,
	TileSurface,
} from "../../../ui/src/viewer/surface.js";
import { createAnimationFrameClock } from "../../../ui/src/viewer/worker-surface.js";
import type { FrameRequester } from "../../../ui/src/viewer/worker-surface.js";

export { createAnimationFrameClock };
export type { FrameRequester };

/**
 * The drawing operations a canvas-shaped destination has to provide.
 *
 * Three operations, deliberately. Anything more (layer state, transforms,
 * clipping) would be a 2D context leaking into the port, and the test
 * implementation in `surface.test.ts` is a recorder - which only stays a
 * recorder while the port stays this small.
 */
export interface SurfaceRaster {
	/** Resize the backing store. Whole device pixels. */
	resize(width: number, height: number): void;
	/** Clear to the page background. */
	clear(): void;
	/**
	 * Draw RGBA8 pixels into a box, smoothing them when `smoothing` is set.
	 *
	 * `smoothing: false` is what makes a stretched tile read as "not yet sharp"
	 * rather than as a smooth blur, so it is a parameter of the port rather than
	 * something the surface infers.
	 */
	drawRgba(op: {
		readonly pixels: Uint8ClampedArray<ArrayBuffer>;
		readonly width: number;
		readonly height: number;
		readonly x: number;
		readonly y: number;
		readonly destWidth: number;
		readonly destHeight: number;
		readonly smoothing: boolean;
	}): void;
}

/** One op's worth of drawing, after the surface has resolved its source. */
type ResolvedOp =
	| {
			readonly kind: "pixels";
			readonly op: DrawOp;
			readonly pixels: Uint8ClampedArray<ArrayBuffer>;
	  }
	| { readonly kind: "placeholder"; readonly op: DrawOp };

/** Build the main-thread surface over `raster`. */
export function createMainThreadSurface(options: { raster: SurfaceRaster }): TileSurface {
	const { raster } = options;
	let configured = false;
	let disposed = false;
	let last: SurfaceSize | null = null;

	return {
		// The transport copied these pixels out of base64 for us; nothing here
		// is transferred, so the surface may keep reading the payload freely.
		takesOwnership: false,

		configure(size: SurfaceSize): void {
			if (disposed) {
				return;
			}
			raster.resize(size.deviceWidth, size.deviceHeight);
			configured = true;
			last = size;
		},

		draw(frame: CompositorFrame): void {
			// Disposal first: the compositor may have a frame in flight when a
			// teardown races it, and throwing there would surface as an
			// unhandled error during page teardown. Being unconfigured is a
			// different matter - that is a wiring bug, and it is loud.
			if (disposed) {
				return;
			}
			if (!configured || last === null) {
				throw new Error("the extension surface was drawn before it was configured");
			}
			raster.clear();
			for (const op of frame.ops) {
				const resolved = resolve(op);
				if (resolved.kind === "placeholder") {
					// A placeholder is the compositor telling us a page has no
					// usable pixels this frame. Drawing the background for it
					// (already done by `clear`) is the whole behaviour; the frame
					// counter in `frame.placeholders` is what the UI reports.
					continue;
				}
				raster.drawRgba({
					pixels: resolved.pixels,
					width: resolved.op.sourceWidth,
					height: resolved.op.sourceHeight,
					x: resolved.op.x,
					y: resolved.op.y,
					destWidth: resolved.op.width,
					destHeight: resolved.op.height,
					smoothing: !resolved.op.scaled,
				});
			}
		},

		present(): void {
			// Drawing to a main-thread context *is* the presentation; there is no
			// bitmap to hand anywhere. The method exists because the port names
			// it, and an interface with a no-op is better than an optional
			// method every call site has to check.
		},

		dispose(): void {
			if (disposed) {
				return;
			}
			disposed = true;
			configured = false;
			last = null;
		},
	};
}

/**
 * Read an op's pixels, or decide it has none.
 *
 * `DrawOp.source` is `unknown` by design - it is an `ImageBitmap` on the worker
 * path and a `RenderedTile` here - so this is the one place the two are told
 * apart. An op carrying anything else is a placeholder, not an error: the
 * compositor legitimately emits ops for tiles it does not have yet.
 */
function resolve(op: DrawOp): ResolvedOp {
	const tile = asTile(op.source);
	if (tile === null) {
		return { kind: "placeholder", op };
	}
	return { kind: "pixels", op, pixels: new Uint8ClampedArray(tile.data) };
}

function asTile(source: unknown): RenderedTile | null {
	if (typeof source !== "object" || source === null) {
		return null;
	}
	const candidate = source as Partial<RenderedTile>;
	if (
		candidate.format !== "rgba8" ||
		typeof candidate.width !== "number" ||
		typeof candidate.height !== "number" ||
		!(candidate.data instanceof ArrayBuffer)
	) {
		return null;
	}
	return candidate as RenderedTile;
}

/**
 * A {@link SurfaceRaster} over a real `<canvas>`.
 *
 * The scratch canvas is what turns a `RenderedTile` into something `drawImage`
 * accepts. It is allocated once and reused at the largest size seen, because
 * per-frame canvas allocation is the classic way a "main-thread surface" turns
 * a smooth path into a stuttering one.
 */
export function createCanvasRaster(canvas: HTMLCanvasElement): SurfaceRaster {
	const context = canvas.getContext("2d", { alpha: false });
	if (context === null) {
		throw new Error("the viewer canvas has no 2d context");
	}
	const scratch = globalThis.document.createElement("canvas");
	const scratchContext = scratch.getContext("2d", { alpha: false });
	if (scratchContext === null) {
		throw new Error("the viewer scratch canvas has no 2d context");
	}

	return {
		resize(width, height) {
			if (canvas.width !== width) {
				canvas.width = width;
			}
			if (canvas.height !== height) {
				canvas.height = height;
			}
		},

		clear() {
			context.save();
			context.setTransform(1, 0, 0, 1, 0, 0);
			context.fillStyle = getComputedStyle(canvas).getPropertyValue("--selis-bg") || "#ffffff";
			context.fillRect(0, 0, canvas.width, canvas.height);
			context.restore();
		},

		drawRgba(op) {
			if (scratch.width < op.width) {
				scratch.width = op.width;
			}
			if (scratch.height < op.height) {
				scratch.height = op.height;
			}
			const image = new ImageData(op.pixels, op.width, op.height);
			scratchContext.putImageData(image, 0, 0);
			context.imageSmoothingEnabled = op.smoothing;
			context.drawImage(
				scratch,
				0,
				0,
				op.width,
				op.height,
				op.x,
				op.y,
				op.destWidth,
				op.destHeight,
			);
		},
	};
}
