/**
 * The worker half of the surface: a `TileSurface` that draws in a worker
 * (SL-4.UI.03).
 *
 * This is the web/extension implementation of `surface.ts`'s
 * {@link TileSurface}, and it is where "OffscreenCanvas in the worker" and
 * "transferred bitmaps" stop being a design and become code. Two transfers,
 * both by ownership rather than by copy:
 *
 * 1. **The canvas itself.** `transferControlToOffscreen()` is called exactly
 *    once, and the resulting `OffscreenCanvas` is put in the *transfer list* of
 *    the `attach` message. After that the UI thread cannot touch the canvas at
 *    all — it is the worker's — and no per-frame pixel data crosses the
 *    boundary, because there is no per-frame pixel data on this side.
 * 2. **The frame back.** The worker ends each frame with
 *    `transferToImageBitmap()` and posts the resulting `ImageBitmap` in the
 *    transfer list, so displaying it is a pointer move rather than a copy of
 *    the whole viewport.
 *
 * And one thing that is deliberately *not* transferred, because it is the
 * mistake this design exists to prevent: the tiles. The compositor's ops carry
 * the `TileEntry` they were read from, and {@link serialiseFrame} strips the
 * payload down to geometry and identity before the frame is posted. The worker
 * already holds the pixels — it is where the engine is — so posting them would
 * copy every tile to the UI thread and straight back.
 *
 * Everything host-specific arrives as a parameter: `post` is
 * `worker.postMessage`, `transfer` is `(canvas) => canvas.transferControlToOffscreen()`,
 * and the clock is built from the frame requester. This module names none of
 * them, so `../platform/platform-globals.test.ts` stays green and the same code serves the
 * web app, the MV3 offscreen document, and the tests.
 */

import type {
	CompositorFrame,
	DrawOp,
	FrameClock,
	FrameHandle,
	SurfaceSize,
	TileSurface,
} from "./surface.js";
import { SurfaceFaultError } from "./surface.js";

/** Protocol version, so a stale worker fails loudly instead of drawing nothing. */
export const SURFACE_PROTOCOL = 1;

/** UI thread → worker. */
export type SurfaceCommand =
	| { readonly v: 1; readonly op: "attach"; readonly canvas: unknown }
	| { readonly v: 1; readonly op: "configure"; readonly size: SurfaceSize }
	| { readonly v: 1; readonly op: "draw"; readonly frame: SurfaceFrame }
	| { readonly v: 1; readonly op: "present" }
	| { readonly v: 1; readonly op: "release" };

/** Worker → UI thread. */
export interface SurfaceFrameEvent {
	readonly v: 1;
	readonly op: "frame";
	/** The composed frame, transferred. */
	readonly bitmap: unknown;
	/** The frame index it corresponds to, for diagnostics and gap detection. */
	readonly index: number;
	readonly ops: number;
	/** How many ops the worker could not resolve to a bitmap. */
	readonly missing: number;
}

/** One op as it crosses to the worker: geometry and identity, never pixels. */
export interface SurfaceDrawOp {
	readonly page: number;
	readonly stage: "lowres" | "full";
	readonly deviceScale: number;
	readonly sourceWidth: number;
	readonly sourceHeight: number;
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
	readonly scaled: boolean;
	readonly provisional: boolean;
}

/** One frame as it crosses to the worker. */
export interface SurfaceFrame {
	readonly index: number;
	readonly clear: true;
	readonly size: SurfaceSize;
	readonly scrollTop: number;
	readonly ops: readonly SurfaceDrawOp[];
}

/**
 * The key the worker files a bitmap under.
 *
 * The worker owns the pixels, so it addresses them the same way the ladder
 * keys its cache — page, rung, rasterisation scale — and derives the key from
 * the op rather than being told it. This mirrors `tileKey` in `tile-ladder.ts`
 * and the two must agree, which `worker-surface.test.ts` checks.
 */
export function surfaceOpKey(op: Pick<DrawOp, "page" | "stage" | "deviceScale">): string {
	return `${op.page}:${op.stage}@${op.deviceScale.toFixed(6)}`;
}

/**
 * Strip a frame down to what the worker needs.
 *
 * Pure, and the reason the tile payload never crosses: an op's `source` is the
 * cached `TileEntry`, complete with its `ArrayBuffer`, and posting that would
 * clone several megabytes per frame in exactly the wrong direction. What
 * survives is the box, the source size, and enough identity for the worker to
 * find the copy it already has.
 */
export function serialiseFrame(frame: CompositorFrame): SurfaceFrame {
	return {
		index: frame.index,
		clear: frame.clear,
		size: frame.size,
		scrollTop: frame.scrollTop,
		ops: frame.ops.map((op) => ({
			page: op.page,
			stage: op.stage,
			deviceScale: op.deviceScale,
			sourceWidth: op.sourceWidth,
			sourceHeight: op.sourceHeight,
			x: op.x,
			y: op.y,
			width: op.width,
			height: op.height,
			scaled: op.scaled,
			provisional: op.provisional,
		})),
	};
}

/**
 * The transfer list for a command.
 *
 * Only `attach` transfers anything: the `OffscreenCanvas` moves by ownership.
 * Every other message is small geometry, and putting a tile buffer on one would
 * be the copy this design exists to avoid.
 */
export function commandTransfer(command: SurfaceCommand): readonly unknown[] {
	return command.op === "attach" ? [command.canvas] : [];
}

export interface WorkerSurfaceOptions {
	/** `worker.postMessage` — or the MV3 offscreen document's equivalent. */
	readonly post: (message: SurfaceCommand, transfer: readonly unknown[]) => void;
	/**
	 * `canvas.transferControlToOffscreen`, injected. Its result is transferred
	 * to the worker, so this may be called at most once per canvas.
	 */
	readonly transfer: (canvas: unknown) => unknown;
	/** Protocol version to announce; a worker that disagrees must refuse. */
	readonly version?: number;
}

/**
 * A `TileSurface` that draws in a worker.
 *
 * `takesOwnership` is true: the tile payloads the compositor hands over belong
 * to the worker — in the real architecture they never arrive here at all,
 * because the worker is where the engine is — and the compositor's promise to
 * stop reading them is what keeps the engine's bitmaps off the UI thread.
 */
export interface WorkerSurface extends TileSurface {
	/** Hand the canvas over. Call once, before the first `configure`. */
	attach(canvas: unknown): void;
	/** Frames the worker has sent back, newest last. */
	readonly received: readonly SurfaceFrameEvent[];
}

export function createWorkerSurface(options: WorkerSurfaceOptions): WorkerSurface {
	const version = options.version ?? SURFACE_PROTOCOL;
	const received: SurfaceFrameEvent[] = [];
	let attached = false;
	let released = false;
	let configured = false;

	const send = (command: SurfaceCommand): void => {
		options.post(command, commandTransfer(command));
	};

	return {
		takesOwnership: true,
		received,

		attach(canvas: unknown) {
			if (attached) {
				// A canvas's control can only be transferred once; a second call
				// throws in the browser, and ignoring it would leave the worker
				// with no surface and a viewer that silently never draws.
				throw new SurfaceFaultError("already-attached");
			}
			attached = true;
			send({ v: version as 1, op: "attach", canvas: options.transfer(canvas) });
		},

		configure(size: SurfaceSize) {
			if (!attached) {
				throw new SurfaceFaultError("not-attached");
			}
			configured = true;
			send({ v: version as 1, op: "configure", size });
		},

		draw(frame: CompositorFrame) {
			if (!configured) {
				throw new SurfaceFaultError("not-configured");
			}
			send({ v: version as 1, op: "draw", frame: serialiseFrame(frame) });
		},

		present() {
			send({ v: version as 1, op: "present" });
		},

		dispose() {
			if (released) {
				return;
			}
			released = true;
			send({ v: version as 1, op: "release" });
		},
	};
}

/** The two-function shape of `requestAnimationFrame`/`cancelAnimationFrame`. */
export interface FrameRequester {
	request(callback: (timestampMs: number) => void): number;
	cancel(id: number): void;
}

/**
 * The browser `FrameClock`.
 *
 * A shell builds it from the two globals and hands it to the compositor:
 *
 * ```ts
 * createTileCompositor({
 *   list,
 *   surface: createWorkerSurface({
 *     post: (message, transfer) => worker.postMessage(message, transfer),
 *     transfer: (canvas) => canvas.transferControlToOffscreen(),
 *   }),
 *   clock: createAnimationFrameClock({
 *     request: (cb) => requestAnimationFrame(cb),
 *     cancel: (id) => cancelAnimationFrame(id),
 *   }),
 * });
 * ```
 *
 * The id → handle mapping lives here so the compositor never holds a closure
 * per frame, and so a cancelled handle is observably cancelled: the 16 ms
 * budget is spent on coalescing, and a cancel that silently did nothing would
 * show up only as a dropped frame much later.
 */
export function createAnimationFrameClock(source: FrameRequester): FrameClock {
	const ids = new Map<FrameHandle, number>();
	return {
		requestFrame(callback: (timestampMs: number) => void): FrameHandle {
			const id = source.request(callback);
			const handle: { cancelled: boolean } = { cancelled: false };
			ids.set(handle, id);
			return handle;
		},
		cancelFrame(handle: FrameHandle): void {
			const id = ids.get(handle);
			if (id === undefined) {
				return;
			}
			ids.delete(handle);
			source.cancel(id);
		},
	};
}
