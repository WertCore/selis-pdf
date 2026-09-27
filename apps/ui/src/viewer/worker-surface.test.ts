/**
 * The worker surface and the worker's compositor, end to end (SL-4.UI.03).
 *
 * The two halves of the OffscreenCanvas design live in `worker-surface.ts` (UI
 * thread) and `offscreen-compositor.ts` (worker). Neither can be exercised for
 * real in Node — there is no `OffscreenCanvas` and no `Worker` — but both are
 * written against injected primitives precisely so that *this* file can drive
 * the real functions against recorders and assert the properties that matter:
 *
 * - the canvas moves to the worker **by transfer**, once;
 * - a frame posted to the worker carries **no pixels at all** (the tile
 *   payload is stripped, so the copy that would cost megabytes per frame never
 *   happens);
 * - the worker draws each op at the compositor's destination rectangle, with
 *   smoothing off exactly for the ops that are not at their own resolution;
 * - the composed frame comes back **by transfer**;
 * - the worker's tile keys are the ladder's cache keys, so a tile the ladder
 *   believes is cached is the tile the worker can actually find.
 *
 * What is *not* proved here, and is stated rather than implied: that a real
 * browser's `transferControlToOffscreen` / `transferToImageBitmap` behave as
 * their specifications say. The browser leg of this task is
 * `cargo xtask compositor-bench`, which runs this code in a real engine.
 */

import { describe, expect, it } from "vitest";
import type { CompositorContext, OffscreenTarget, RecordedDraw } from "./offscreen-compositor.js";
import { createOffscreenCompositor } from "./offscreen-compositor.js";
import type { CompositorFrame, FrameHandle, SurfaceSize } from "./surface.js";
import { SurfaceFaultError } from "./surface.js";
import { tileKey } from "./tile-ladder.js";
import type { SurfaceCommand, SurfaceFrame } from "./worker-surface.js";
import {
	SURFACE_PROTOCOL,
	commandTransfer,
	createAnimationFrameClock,
	createWorkerSurface,
	serialiseFrame,
	surfaceOpKey,
} from "./worker-surface.js";

interface Posted {
	readonly message: unknown;
	readonly transfer: readonly unknown[];
}

/**
 * A canvas that records what was drawn into it.
 *
 * The context is a real object with the real `imageSmoothingEnabled` accessor,
 * so the smoothing assertions observe the value the compositor actually set
 * rather than a copy the recorder maintains separately.
 */
function recordingCanvas(): {
	target: OffscreenTarget;
	smoothing: () => boolean;
	draws: RecordedDraw[];
	clears: number;
	bitmaps: () => number;
} {
	const draws: RecordedDraw[] = [];
	let smoothing = true;
	let clears = 0;
	let bitmaps = 0;
	const context: CompositorContext = {
		get imageSmoothingEnabled() {
			return smoothing;
		},
		set imageSmoothingEnabled(value: boolean) {
			smoothing = value;
		},
		clearRect: () => {
			clears += 1;
		},
		drawImage: (source, sx, sy, sw, sh, dx, dy, dw, dh) => {
			draws.push({ source, sx, sy, sw, sh, dx, dy, dw, dh, smoothing });
		},
	};
	const target: OffscreenTarget = {
		width: 0,
		height: 0,
		getContext: () => context,
		transferToImageBitmap: () => {
			bitmaps += 1;
			return { id: `frame-${bitmaps}` };
		},
	};
	return {
		target,
		smoothing: () => smoothing,
		draws,
		get clears() {
			return clears;
		},
		bitmaps: () => bitmaps,
	};
}

/** A frame the compositor would hand a surface: geometry plus a real payload. */
function frameWith(
	ops: readonly SurfaceFrame["ops"][number][],
	size?: Partial<SurfaceSize>,
): CompositorFrame {
	const full: SurfaceSize = {
		cssWidth: 1000,
		cssHeight: 800,
		deviceWidth: 2000,
		deviceHeight: 1600,
		scale: 2,
		devicePixelRatio: 2,
		...size,
	};
	return {
		index: 7,
		at: 16,
		size: full,
		scrollTop: 0,
		clear: true,
		placeholders: 0,
		ops: ops.map((op) => ({
			page: op.page,
			stage: op.stage,
			deviceScale: op.deviceScale,
			targetDeviceScale: op.deviceScale,
			sourceWidth: op.sourceWidth,
			sourceHeight: op.sourceHeight,
			x: op.x,
			y: op.y,
			width: op.width,
			height: op.height,
			scaled: op.scaled,
			provisional: op.provisional,
			// A real payload with real bytes: this is what must *not* be posted.
			source: {
				page: op.page,
				stage: op.stage,
				deviceScale: op.deviceScale,
				tile: {
					page: op.page,
					width: op.sourceWidth,
					height: op.sourceHeight,
					format: "rgba8" as const,
					data: new ArrayBuffer(64 * 64 * 4),
				},
			},
		})) as CompositorFrame["ops"],
	};
}

const oneOp = {
	page: 3,
	stage: "full" as const,
	deviceScale: 2,
	sourceWidth: 1224,
	sourceHeight: 1584,
	x: 32,
	y: 64,
	width: 1224,
	height: 1584,
	scaled: false,
	provisional: false,
};

/** Every `ArrayBuffer` reachable from a value, by identity. */
function buffersIn(value: unknown, seen = new Set<unknown>()): unknown[] {
	if (value === null || typeof value !== "object") {
		return [];
	}
	if (seen.has(value)) {
		return [];
	}
	seen.add(value);
	if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) {
		return [value];
	}
	const found: unknown[] = [];
	for (const inner of Object.values(value as Record<string, unknown>)) {
		found.push(...buffersIn(inner, seen));
	}
	return found;
}

describe("serialiseFrame", () => {
	it("strips the tile payload, so no pixels are posted to the worker", () => {
		const frame = frameWith([oneOp]);
		// The frame the compositor produced really does carry the buffer.
		expect(buffersIn(frame.ops)).toHaveLength(1);
		const wire = serialiseFrame(frame);
		// The frame the worker receives does not. This is the assertion that
		// makes "transferred, not copied" true of the *UI → worker* direction:
		// the worker is where the engine is, so every byte here would be a
		// copy that goes nowhere.
		expect(buffersIn(wire)).toEqual([]);
		expect(wire.ops[0]).not.toHaveProperty("source");
	});

	it("keeps everything the worker needs to draw and to identify the tile", () => {
		const wire = serialiseFrame(frameWith([oneOp]));
		expect(wire.index).toBe(7);
		expect(wire.clear).toBe(true);
		expect(wire.size.deviceWidth).toBe(2000);
		expect(wire.ops).toEqual([oneOp]);
	});

	it("addresses tiles with the same key the ladder caches them under", () => {
		// If these two ever drift, a tile the ladder believes is cached is one
		// the worker cannot find, and the viewer shows placeholders forever.
		// Cheap to assert, expensive to debug.
		expect(surfaceOpKey(oneOp)).toBe(tileKey(oneOp.page, oneOp.stage, oneOp.deviceScale));
		expect(surfaceOpKey({ page: 9, stage: "lowres", deviceScale: 0.25 })).toBe(
			tileKey(9, "lowres", 0.25),
		);
	});
});

describe("createWorkerSurface", () => {
	function surface(posted: Posted[]) {
		const transfers: unknown[] = [];
		return {
			transfers,
			surface: createWorkerSurface({
				post: (message, transfer) => {
					posted.push({ message, transfer });
				},
				// The real thing hands back an OffscreenCanvas and *detaches*
				// the element's control; the identity stand-in is enough to
				// prove the surface puts it on the transfer list.
				transfer: (canvas) => ({ offscreen: true, from: canvas }),
			}),
		};
	}

	it("transfers the canvas to the worker exactly once", () => {
		const posted: Posted[] = [];
		const { surface: s } = surface(posted);
		s.attach("canvas-element");
		expect(posted).toHaveLength(1);
		expect(posted[0]?.message).toMatchObject({ v: SURFACE_PROTOCOL, op: "attach" });
		// The transfer list carries the OffscreenCanvas, not the element: this
		// is the ownership move, and it happens once for the whole session.
		expect(posted[0]?.transfer).toHaveLength(1);
		expect(() => s.attach("canvas-element")).toThrow(SurfaceFaultError);
	});

	it("posts geometry only, with an empty transfer list, for every later command", () => {
		const posted: Posted[] = [];
		const { surface: s } = surface(posted);
		s.attach("canvas-element");
		s.configure({
			cssWidth: 1000,
			cssHeight: 800,
			deviceWidth: 2000,
			deviceHeight: 1600,
			scale: 2,
			devicePixelRatio: 2,
		});
		s.draw(frameWith([oneOp]));
		s.present();
		s.dispose();
		s.dispose();
		const ops = posted.map((entry) => (entry.message as SurfaceCommand).op);
		expect(ops).toEqual(["attach", "configure", "draw", "present", "release"]);
		for (const entry of posted.slice(1)) {
			expect(entry.transfer).toEqual([]);
		}
		expect(buffersIn(posted[2]?.message)).toEqual([]);
	});

	it("refuses to draw before there is a backing store", () => {
		const posted: Posted[] = [];
		const { surface: s } = surface(posted);
		expect(() => s.draw(frameWith([oneOp]))).toThrow(SurfaceFaultError);
		expect(() =>
			s.configure({
				cssWidth: 1,
				cssHeight: 1,
				deviceWidth: 1,
				deviceHeight: 1,
				scale: 1,
				devicePixelRatio: 1,
			}),
		).toThrow(SurfaceFaultError);
	});
});

describe("createOffscreenCompositor (the worker half)", () => {
	it("sizes the canvas to the frame's device pixels and clears it", () => {
		const canvas = recordingCanvas();
		const worker = createOffscreenCompositor({ target: canvas.target, post: () => undefined });
		worker.draw(serialiseFrame(frameWith([oneOp])));
		expect(canvas.target.width).toBe(2000);
		expect(canvas.target.height).toBe(1600);
		expect(canvas.clears).toBe(1);
	});

	it("draws each op at the compositor's destination rectangle", () => {
		const canvas = recordingCanvas();
		const worker = createOffscreenCompositor({ target: canvas.target, post: () => undefined });
		const bitmap = { id: "page-3" };
		worker.put(oneOp, bitmap);
		const outcome = worker.draw(serialiseFrame(frameWith([oneOp])));
		expect(outcome).toEqual({ index: 7, drawn: 1, missing: 0 });
		expect(canvas.draws).toHaveLength(1);
		const draw = canvas.draws[0];
		expect(draw?.source).toBe(bitmap);
		// The source rect is the whole bitmap; the destination rect is the op's.
		expect([draw?.sx, draw?.sy, draw?.sw, draw?.sh]).toEqual([0, 0, 1224, 1584]);
		expect([draw?.dx, draw?.dy, draw?.dw, draw?.dh]).toEqual([32, 64, 1224, 1584]);
	});

	it("turns smoothing off exactly for the ops that are not at their own scale", () => {
		const canvas = recordingCanvas();
		const worker = createOffscreenCompositor({ target: canvas.target, post: () => undefined });
		const sharp = oneOp;
		const stretched = {
			...oneOp,
			page: 4,
			deviceScale: 1,
			sourceWidth: 612,
			sourceHeight: 792,
			scaled: true,
			provisional: true,
		};
		worker.put(sharp, { id: "sharp" });
		worker.put(stretched, { id: "stretched" });
		worker.draw(serialiseFrame(frameWith([sharp, stretched])));
		expect(canvas.draws[0]?.smoothing).toBe(true);
		// A stretched tile drawn with smoothing on reads as a soft blur, which
		// looks like a bug; drawn with it off it reads as "not yet sharp", which
		// is what it is.
		expect(canvas.draws[1]?.smoothing).toBe(false);
	});

	it("skips an op it has no bitmap for, and says so, rather than drawing garbage", () => {
		const canvas = recordingCanvas();
		const worker = createOffscreenCompositor({ target: canvas.target, post: () => undefined });
		const outcome = worker.draw(serialiseFrame(frameWith([oneOp])));
		expect(outcome).toEqual({ index: 7, drawn: 0, missing: 1 });
		expect(canvas.draws).toHaveLength(0);
	});

	it("hands the composed frame back by transfer, not by copy", () => {
		const canvas = recordingCanvas();
		const posted: Posted[] = [];
		const worker = createOffscreenCompositor({
			target: canvas.target,
			post: (message, transfer) => {
				posted.push({ message, transfer });
			},
		});
		worker.put(oneOp, { id: "page-3" });
		const frame = serialiseFrame(frameWith([oneOp]));
		const outcome = worker.draw(frame);
		worker.present(frame, outcome);
		expect(posted).toHaveLength(1);
		// The bitmap is on the transfer list: ownership, not a structured clone
		// of the whole viewport, every frame.
		expect(posted[0]?.transfer).toHaveLength(1);
		expect((posted[0]?.message as { bitmap: unknown }).bitmap).toBe(posted[0]?.transfer[0]);
		expect(posted[0]?.message).toMatchObject({ op: "frame", index: 7, ops: 1, missing: 0 });
		expect(canvas.bitmaps()).toBe(1);
	});

	it("closes a bitmap it replaces, and everything it holds on release", () => {
		const canvas = recordingCanvas();
		const worker = createOffscreenCompositor({ target: canvas.target, post: () => undefined });
		let closed = 0;
		const bitmap = () => ({
			id: "b",
			close: () => {
				closed += 1;
			},
		});
		worker.put(oneOp, bitmap());
		worker.put(oneOp, bitmap());
		expect(closed).toBe(1);
		expect(worker.held).toBe(1);
		worker.release();
		// A zoom-spam session that never closed its bitmaps would hold every
		// scale it ever drew; this is the bound.
		expect(closed).toBe(2);
		expect(worker.held).toBe(0);
	});

	it("re-resolves the context after a resize, which is what assigning width does", () => {
		const canvas = recordingCanvas();
		const worker = createOffscreenCompositor({ target: canvas.target, post: () => undefined });
		worker.put(oneOp, { id: "page-3" });
		worker.draw(serialiseFrame(frameWith([oneOp])));
		expect(canvas.draws).toHaveLength(1);
		worker.draw(
			serialiseFrame(frameWith([oneOp], { deviceWidth: 1000, deviceHeight: 800, scale: 1 })),
		);
		expect(canvas.target.width).toBe(1000);
		// Still drawing after the resize, so the context was re-acquired.
		expect(canvas.draws).toHaveLength(2);
	});
});

describe("the round trip", () => {
	it("carries a real frame from the UI thread to the worker with no pixel copy", () => {
		const posted: Posted[] = [];
		const canvas = recordingCanvas();
		const surface = createWorkerSurface({
			post: (message, transfer) => {
				posted.push({ message, transfer });
			},
			transfer: (target) => ({ offscreen: true, from: target }),
		});
		const worker = createOffscreenCompositor({
			target: canvas.target,
			post: () => undefined,
		});

		surface.attach("canvas-element");
		const frame = frameWith([oneOp]);
		surface.configure(frame.size);
		// The worker already has the tile, because the engine rendered it there.
		worker.put(oneOp, { id: "page-3" });
		surface.draw(frame);
		surface.present();

		// Everything the worker was sent, in order.
		expect(posted.map((entry) => (entry.message as SurfaceCommand).op)).toEqual([
			"attach",
			"configure",
			"draw",
			"present",
		]);
		// The transferred object is the OffscreenCanvas and nothing else.
		expect(posted[0]?.transfer).toEqual([{ offscreen: true, from: "canvas-element" }]);
		expect(commandTransfer(posted[1]?.message as SurfaceCommand)).toEqual([]);
		// The `draw` message carried geometry and no megabytes.
		const draw = posted[2]?.message as { frame: SurfaceFrame };
		expect(draw.frame.ops).toEqual([oneOp]);
		expect(buffersIn(draw.frame)).toEqual([]);
		// And feeding that message to the worker draws the tile.
		worker.draw(draw.frame);
		expect(canvas.draws).toHaveLength(1);
		surface.dispose();
	});
});

describe("createAnimationFrameClock", () => {
	it("cancels through the host's own cancel, once per handle", () => {
		const cancelled: number[] = [];
		let next = 0;
		const clock = createAnimationFrameClock({
			request: () => {
				next += 1;
				return next;
			},
			cancel: (id) => cancelled.push(id),
		});
		const a = clock.requestFrame(() => undefined);
		const b = clock.requestFrame(() => undefined);
		expect(a.cancelled).toBe(false);
		clock.cancelFrame(a);
		clock.cancelFrame(a);
		// The second cancel is a no-op, not a double cancel of someone else's
		// frame.
		expect(cancelled).toEqual([1]);
		clock.cancelFrame(b);
		expect(cancelled).toEqual([1, 2]);
	});

	it("ignores a cancel for a handle it never issued", () => {
		const cancelled: number[] = [];
		const clock = createAnimationFrameClock({
			request: () => 7,
			cancel: (id) => cancelled.push(id),
		});
		clock.cancelFrame({ cancelled: false } as FrameHandle);
		expect(cancelled).toEqual([]);
	});
});
