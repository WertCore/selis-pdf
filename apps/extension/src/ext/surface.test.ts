/**
 * SL-4.EXT.06 - the main-thread surface, and the clock it reuses.
 *
 * `SurfaceRaster` has three methods on purpose. Anything more and this test
 * stops being a recorder and starts being a second canvas implementation,
 * which is the thing the port exists to avoid. So the recording is total: the
 * surface's entire observable behaviour is the sequence of raster calls.
 *
 * The clock half matters just as much. `createAnimationFrameClock` is imported
 * from `apps/ui`, not copied, and the property worth pinning is that a
 * cancelled frame is *observably* cancelled - the id-to-handle bookkeeping in
 * that shared module is what makes it so, and a cancelled frame that quietly
 * still fires would show up much later as a dropped frame.
 */

import { describe, expect, it } from "vitest";
import type {
	CompositorFrame,
	DrawOp,
	FrameClock,
	SurfaceSize,
} from "../../../ui/src/viewer/surface.js";
import {
	type SurfaceRaster,
	createAnimationFrameClock,
	createMainThreadSurface,
} from "./surface.js";

/** A raster that records everything and does nothing. */
function recorder() {
	const calls: {
		resize: { width: number; height: number }[];
		clears: number;
		draws: {
			x: number;
			y: number;
			destWidth: number;
			destHeight: number;
			smoothing: boolean;
			firstPixel: number;
		}[];
	} = { resize: [], clears: 0, draws: [] };
	const raster: SurfaceRaster = {
		resize(width, height) {
			calls.resize.push({ width, height });
		},
		clear() {
			calls.clears += 1;
		},
		drawRgba(op) {
			calls.draws.push({
				x: op.x,
				y: op.y,
				destWidth: op.destWidth,
				destHeight: op.destHeight,
				smoothing: op.smoothing,
				firstPixel: op.pixels[0] ?? -1,
			});
		},
	};
	return { raster, calls };
}

const SIZE: SurfaceSize = {
	cssWidth: 800,
	cssHeight: 600,
	deviceWidth: 800,
	deviceHeight: 600,
	devicePixelRatio: 1,
	scale: 1,
};

/** A `RenderedTile` for one 2x2 RGBA op. */
function tile(first: number) {
	const data = new ArrayBuffer(2 * 2 * 4);
	const pixels = new Uint8Array(data);
	for (let i = 0; i < pixels.length; i += 4) {
		pixels[i] = first;
		pixels[i + 3] = 255;
	}
	return { page: 0, width: 2, height: 2, format: "rgba8" as const, data };
}

function op(over: Partial<DrawOp> = {}): DrawOp {
	return {
		page: 0,
		stage: "full",
		deviceScale: 1,
		targetDeviceScale: 1,
		sourceWidth: 2,
		sourceHeight: 2,
		x: 10,
		y: 20,
		width: 100,
		height: 200,
		scaled: false,
		provisional: false,
		source: tile(7),
		...over,
	};
}

function frame(ops: readonly DrawOp[]): CompositorFrame {
	return { index: 0, at: 0, size: SIZE, scrollTop: 0, clear: true, ops, placeholders: 0 };
}

describe("the main-thread surface (SL-4.EXT.06)", () => {
	it("does not take ownership of tile buffers", () => {
		// The transport copied these pixels out of base64; nothing is
		// transferred, so the surface may read the payload freely. Claiming
		// ownership would make the compositor detach buffers it still needs.
		expect(createMainThreadSurface({ raster: recorder().raster }).takesOwnership).toBe(false);
	});

	it("sizes the backing store and clears before every frame", () => {
		const { raster, calls } = recorder();
		const surface = createMainThreadSurface({ raster });
		surface.configure(SIZE);
		expect(calls.resize).toEqual([{ width: 800, height: 600 }]);
		surface.draw(frame([op()]));
		expect(calls.clears).toBe(1);
		// The compositor repaints the whole viewport every frame and carries
		// `clear: true`; the surface is expected to honour it.
		expect(frame([op()]).clear).toBe(true);
	});

	it("refuses to draw before it is configured", () => {
		const surface = createMainThreadSurface({ raster: recorder().raster });
		expect(() => surface.draw(frame([op()]))).toThrow(/before it was configured/);
	});

	it("turns smoothing off for a scaled tile and on for a sharp one", () => {
		const { raster, calls } = recorder();
		const surface = createMainThreadSurface({ raster });
		surface.configure(SIZE);
		// A low-res rung or a zoom blit must read as "not yet sharp" rather
		// than as a smooth blur. That is the whole reason `scaled` is a
		// parameter of the port rather than something the surface infers.
		surface.draw(frame([op({ scaled: true }), op({ scaled: false, x: 0 })]));
		expect(calls.draws.map((draw) => draw.smoothing)).toEqual([false, true]);
	});

	it("passes the destination box through unchanged", () => {
		const { raster, calls } = recorder();
		const surface = createMainThreadSurface({ raster });
		surface.configure(SIZE);
		surface.draw(frame([op()]));
		expect(calls.draws[0]).toMatchObject({ x: 10, y: 20, destWidth: 100, destHeight: 200 });
	});

	it("reads the tile's pixels", () => {
		const { raster, calls } = recorder();
		const surface = createMainThreadSurface({ raster });
		surface.configure(SIZE);
		surface.draw(frame([op({ source: tile(200) })]));
		expect(calls.draws[0]?.firstPixel).toBe(200);
	});

	it("treats an op carrying no tile as a placeholder, not an error", () => {
		// The compositor legitimately emits ops for tiles it does not have yet,
		// and `DrawOp.source` is `unknown` precisely because the worker path
		// puts an ImageBitmap there and this one puts a RenderedTile.
		const { raster, calls } = recorder();
		const surface = createMainThreadSurface({ raster });
		surface.configure(SIZE);
		surface.draw(frame([op({ source: undefined, provisional: true }), op()]));
		expect(calls.draws).toHaveLength(1);
	});

	it("stops drawing after dispose, and tolerates a second dispose", () => {
		const { raster, calls } = recorder();
		const surface = createMainThreadSurface({ raster });
		surface.configure(SIZE);
		surface.dispose();
		surface.dispose();
		// A draw after dispose is a no-op rather than a throw: the compositor
		// may have a frame in flight when a teardown races it, and throwing
		// there would surface as an unhandled error during page teardown.
		surface.draw(frame([op()]));
		expect(calls.draws).toHaveLength(0);
	});

	it("presents without doing anything, because drawing is the presentation", () => {
		const surface = createMainThreadSurface({ raster: recorder().raster });
		expect(() => surface.present()).not.toThrow();
	});
});

describe("the frame clock, reused from apps/ui (SL-4.EXT.06)", () => {
	/** A frame requester the test drives by hand. */
	function requester() {
		const pending = new Map<number, (t: number) => void>();
		let next = 1;
		return {
			source: {
				request(callback: (t: number) => void) {
					const id = next++;
					pending.set(id, callback);
					return id;
				},
				cancel(id: number) {
					pending.delete(id);
				},
			},
			run(): number {
				const entries = [...pending.entries()];
				pending.clear();
				for (const [, callback] of entries) {
					callback(16);
				}
				return entries.length;
			},
			get pending() {
				return pending.size;
			},
		};
	}

	it("runs a requested frame", () => {
		const clock: FrameClock = createAnimationFrameClock(requester().source);
		const frames = requester();
		const built = createAnimationFrameClock(frames.source);
		const seen: number[] = [];
		built.requestFrame((at) => seen.push(at));
		expect(frames.run()).toBe(1);
		expect(seen).toEqual([16]);
		void clock;
	});

	it("makes a cancelled frame observably cancelled", () => {
		const frames = requester();
		const clock = createAnimationFrameClock(frames.source);
		const handle = clock.requestFrame(() => {
			throw new Error("a cancelled frame must not run");
		});
		clock.cancelFrame(handle);
		expect(frames.run()).toBe(0);
	});

	it("tolerates cancelling an already-run handle", () => {
		const frames = requester();
		const clock = createAnimationFrameClock(frames.source);
		const handle = clock.requestFrame(() => {});
		frames.run();
		expect(() => clock.cancelFrame(handle)).not.toThrow();
	});
});
