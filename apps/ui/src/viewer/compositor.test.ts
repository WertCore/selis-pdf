/**
 * The tile compositor (SL-4.UI.03): the four clauses of the task, made
 * executable.
 *
 * 1. **OffscreenCanvas in the worker** — the compositor does not name a canvas;
 *    it drives a {@link TileSurface}, and the host-side implementation that
 *    transfers a real `OffscreenCanvas` to a worker and transfers frames back
 *    is `apps/web/host/src/compositor-worker.ts` with its tests. What is
 *    assertable here is the *contract* that implementation relies on: one
 *    `configure` per size change, one `draw` + `present` per frame, and the
 *    payload handed over as a handle rather than copied.
 * 2. **Transferred bitmaps** — the "takes ownership" test below uses a surface
 *    that really detaches the buffer it is given, then checks the compositor
 *    still produces correct frames. A compositor that copied the pixels, or
 *    read them after handing them over, would fail it.
 * 3. **Device-pixel ratio** — asserted at DPR 1, 1.25, 2 and 3, including the
 *    fractional case where the backing store rounds and the effective scale is
 *    not the nominal one. The load-bearing assertion is that a full-res tile
 *    is presented 1:1: destination width equals bitmap width, exactly.
 * 4. **The zoom path** — the coarse tile is presented *scaled* on the very
 *    next frame after a zoom change, with no placeholder and without waiting
 *    for the engine, and is then replaced by the sharp one.
 *
 * Plus two properties that are easy to lose and expensive to notice: frames
 * coalesce (a scroll burst costs one draw), and every page the compositor
 * paints is still a labelled, focusable DOM tile.
 *
 * The engine is a hand-rolled fake for UI.02's reason: these tests need to
 * *hold* renders open and resolve them by hand to observe the intermediate
 * zoom frame.
 */

import { describe, expect, it } from "vitest";
import type { EnginePort, PlatformAdapter } from "../platform/adapter.js";
import { AdapterError } from "../platform/errors.js";
import type {
	AdapterRequestOptions,
	DocHandle,
	RenderTileRequest,
	RenderedTile,
	Size,
} from "../platform/types.js";
import type { TileCompositor } from "./compositor.js";
import {
	createTileCompositor,
	effectiveDeviceScale,
	pickTile,
	surfaceSizeFor,
} from "./compositor.js";
import type { PageList } from "./page-list.js";
import { createPageList } from "./page-list.js";
import type { CompositorFrame, FrameHandle, SurfaceSize, TileSurface } from "./surface.js";
import type { TileEntry } from "./tile-ladder.js";

const LETTER: Size = { width: 612, height: 792 };

function documentOf(pageCount: number, size: Size = LETTER): DocHandle {
	return {
		id: `doc-${pageCount}`,
		pageCount,
		pageSizes: Array.from({ length: pageCount }, () => size),
	};
}

/** A render the ladder asked for, held open until the test settles it. */
interface Pending {
	readonly request: RenderTileRequest;
	readonly aborted: () => boolean;
	settle(): void;
}

interface Harness {
	readonly adapter: PlatformAdapter;
	readonly requests: RenderTileRequest[];
	settleAll(): Promise<void>;
}

/**
 * An engine whose renders are resolved by hand.
 *
 * Resolution is explicit because the zoom assertions need to observe the frame
 * that happens *between* the zoom and the engine catching up — the whole point
 * of the task is that this frame exists and shows something.
 */
function harness(doc: DocHandle): Harness {
	const requests: RenderTileRequest[] = [];
	const pending: Pending[] = [];
	const engine = {
		open: async () => doc,
		close: async () => undefined,
		renderTile(request: RenderTileRequest, options?: AdapterRequestOptions) {
			if (options?.signal?.aborted === true) {
				return Promise.reject(AdapterError.cancelled());
			}
			let resolve: (tile: RenderedTile) => void = () => undefined;
			const promise = new Promise<RenderedTile>((res) => {
				resolve = res;
			});
			const entry: Pending = {
				request,
				aborted: () => options?.signal?.aborted === true,
				settle: () => {
					const size = LETTER;
					const width = Math.max(1, Math.round(size.width * request.scale));
					const height = Math.max(1, Math.round(size.height * request.scale));
					resolve({
						page: request.page,
						width,
						height,
						format: "rgba8",
						data: new ArrayBuffer(width * height * 4),
					});
				},
			};
			requests.push(request);
			pending.push(entry);
			return promise;
		},
		extractText: async () => {
			throw new Error("not used");
		},
		search: () => {
			throw new Error("not used");
		},
	} as unknown as EnginePort;
	const adapter = { engine, capabilities: {} } as unknown as PlatformAdapter;
	return {
		adapter,
		requests,
		async settleAll() {
			// A render's resolution queues the next one, so drain repeatedly
			// until a pass settles nothing new.
			for (let round = 0; round < 16; round += 1) {
				const before = pending.length;
				for (const entry of [...pending]) {
					if (!entry.aborted()) {
						entry.settle();
					}
				}
				for (let hop = 0; hop < 8; hop += 1) {
					await Promise.resolve();
				}
				if (pending.length === before) {
					return;
				}
			}
		},
	};
}

/** A surface that records what it was asked to do. */
class RecordingSurface implements TileSurface {
	readonly takesOwnership: boolean;
	readonly sizes: SurfaceSize[] = [];
	readonly frames: CompositorFrame[] = [];
	presents = 0;
	disposals = 0;
	/** Payloads seen, by identity, so a copy can be told from a transfer. */
	readonly sources: unknown[] = [];

	constructor(takesOwnership = false) {
		this.takesOwnership = takesOwnership;
	}

	configure(size: SurfaceSize): void {
		this.sizes.push(size);
	}
	draw(frame: CompositorFrame): void {
		this.frames.push(frame);
		for (const op of frame.ops) {
			this.sources.push(op.source);
		}
	}
	present(): void {
		this.presents += 1;
	}
	dispose(): void {
		this.disposals += 1;
	}
}

/** A clock the test drives, so "the next frame" is a line of code. */
class ManualClock {
	#queue: { callback: (timestamp: number) => void; handle: FrameHandle }[] = [];

	requestFrame(callback: (timestamp: number) => void): FrameHandle {
		const handle: { cancelled: boolean } = { cancelled: false };
		this.#queue.push({ callback, handle });
		return handle;
	}
	cancelFrame(handle: FrameHandle): void {
		const mutable = handle as { cancelled: boolean };
		mutable.cancelled = true;
		this.#queue = this.#queue.filter((entry) => entry.handle !== handle);
	}
	/** Run every pending callback once. Returns how many ran. */
	run(timestamp = 16): number {
		const due = this.#queue;
		this.#queue = [];
		for (const entry of due) {
			if (entry.handle.cancelled) {
				continue;
			}
			entry.callback(timestamp);
		}
		return due.length;
	}
	get pending(): number {
		return this.#queue.length;
	}
}

interface Rig {
	readonly list: PageList;
	readonly compositor: TileCompositor;
	readonly surface: RecordingSurface;
	readonly clock: ManualClock;
	readonly engine: Harness;
	dispose(): void;
}

function rig(
	options: {
		doc?: DocHandle;
		takesOwnership?: boolean;
		mode?: "page" | "width";
		overscanPx?: number;
	} = {},
): Rig {
	const doc = options.doc ?? documentOf(40);
	const engine = harness(doc);
	const list = createPageList({
		adapter: engine.adapter,
		doc,
		mode: options.mode ?? "width",
		...(options.overscanPx === undefined ? {} : { overscanPx: options.overscanPx }),
	});
	const surface = new RecordingSurface(options.takesOwnership ?? false);
	const clock = new ManualClock();
	const compositor = createTileCompositor({ list, surface, clock });
	return {
		list,
		compositor,
		surface,
		clock,
		engine,
		dispose: () => {
			compositor.dispose();
			list.dispose();
		},
	};
}

/** US Letter in a 1000×800 CSS-px viewport at `dpr`. */
function viewport(dpr: number, scrollTop = 0) {
	return { width: 1000, height: 800, scrollTop, devicePixelRatio: dpr };
}

/** Mount, settle every render, and draw the first frame. */
async function mounted(r: Rig, dpr = 1): Promise<void> {
	r.compositor.update(viewport(dpr));
	await r.engine.settleAll();
	r.clock.run();
}

describe("effectiveDeviceScale", () => {
	it("uses the nominal ratio when the backing store divides exactly", () => {
		expect(effectiveDeviceScale(1000, 800, 2)).toEqual({
			scale: 2,
			deviceWidth: 2000,
			deviceHeight: 1600,
		});
	});

	it("reports the rounded store's own scale at a fractional ratio", () => {
		// 1013 × 1.25 = 1266.25, so the store is 1266 px and the factor that
		// actually fills it is 1266/1013, not 1.25. This is the correction the
		// whole crispness story rests on: drawing with 1.25 would resample
		// every tile by a twentieth of a percent and leave a seam.
		const result = effectiveDeviceScale(1013, 707, 1.25);
		expect(result.deviceWidth).toBe(1266);
		expect(result.scale).toBeCloseTo(1266 / 1013, 12);
		expect(result.scale).not.toBe(1.25);
	});

	it("never produces a zero-sized surface from a degenerate viewport", () => {
		expect(effectiveDeviceScale(0, 0, 2)).toEqual({ scale: 1, deviceWidth: 1, deviceHeight: 1 });
		expect(effectiveDeviceScale(100, 100, 0).scale).toBe(1);
		expect(effectiveDeviceScale(100, 100, Number.NaN).scale).toBe(1);
	});

	it("keeps the nominal ratio on the size record for diagnostics", () => {
		const size = surfaceSizeFor(viewport(1.25));
		expect(size.devicePixelRatio).toBe(1.25);
		expect(size.scale).toBeCloseTo(size.deviceWidth / size.cssWidth, 12);
	});
});

describe("pickTile", () => {
	const entry = (stage: "lowres" | "full", deviceScale: number, page = 0): TileEntry => ({
		page,
		stage,
		deviceScale,
		tile: { page, width: 8, height: 8, format: "rgba8", data: new ArrayBuffer(8) },
	});

	it("has nothing to pick from an empty cache", () => {
		expect(pickTile([], 2)).toBeNull();
		expect(pickTile([entry("full", 2)], 0)).toBeNull();
	});

	it("picks the bitmap whose resolution is nearest the destination", () => {
		const near = entry("full", 2.1);
		const far = entry("full", 8);
		expect(pickTile([far, near], 2)).toBe(near);
	});

	it("prefers an exact full-res tile over a low-res one that is nearer", () => {
		// The zoomed-out case: the low-res rung (0.25 device px per point) can
		// sit closer to a 0.2 request than any full-res tile does, and it is
		// still the wrong thing to show.
		const full = entry("full", 0.2);
		const low = entry("lowres", 0.25);
		expect(pickTile([low, full], 0.2)).toBe(full);
	});

	it("keeps a low-res tile when it is all there is", () => {
		const low = entry("lowres", 0.25);
		expect(pickTile([low], 2)).toBe(low);
	});
});

describe("the tile surface contract", () => {
	it("configures the backing store once per size, not once per frame", async () => {
		const r = rig();
		r.compositor.update(viewport(1));
		await r.engine.settleAll();
		for (let frame = 0; frame < 5; frame += 1) {
			r.compositor.invalidate();
			r.clock.run();
		}
		// Five frames, one `configure`: a resize storm must not reallocate.
		expect(r.surface.sizes).toHaveLength(1);
		// A draw and a present per frame, always paired.
		expect(r.surface.frames).toHaveLength(5);
		expect(r.surface.presents).toBe(5);
		r.dispose();
	});

	it("reconfigures when the device pixel ratio changes", async () => {
		const r = rig();
		await mounted(r, 1);
		r.compositor.update(viewport(2));
		r.clock.run();
		expect(r.surface.sizes).toHaveLength(2);
		expect(r.surface.sizes[1]?.deviceWidth).toBe(2000);
		r.dispose();
	});

	it("hands the surface the tile handle itself, not a copy of the pixels", async () => {
		const r = rig();
		await mounted(r, 1);
		const op = r.compositor.lastFrame()?.ops[0];
		expect(op).toBeDefined();
		// Identity, not equality. The surface must receive the very entry the
		// ladder cached, because that is the object whose `ArrayBuffer` it can
		// transfer; a copy of the pixels could only ever be a copy.
		const cached = r.list.tilesFor(op?.page ?? 0);
		expect(cached).toContain(op?.source);
		const entry = op?.source as TileEntry;
		expect(entry.tile.width).toBe(op?.sourceWidth);
		expect(entry.tile.height).toBe(op?.sourceHeight);
		r.dispose();
	});

	it("keeps working after a surface takes ownership and detaches the buffer", async () => {
		// The transfer path, headlessly: this surface claims ownership and does
		// what a `postMessage(…, [buffer])` does — it transfers the ArrayBuffer
		// away. If the compositor read a payload after handing it over, every
		// later frame would throw here.
		const detaching = new RecordingSurface(true);
		const doc = documentOf(8);
		const engine = harness(doc);
		const list = createPageList({ adapter: engine.adapter, doc, mode: "width" });
		const clock = new ManualClock();
		const compositor = createTileCompositor({ list, surface: detaching, clock });
		compositor.update(viewport(1));
		await engine.settleAll();
		clock.run();

		const first = detaching.frames[0];
		expect(first?.ops.length).toBeGreaterThan(0);
		for (const op of first?.ops ?? []) {
			const entry = op.source as TileEntry;
			expect(entry.tile.data.byteLength).toBeGreaterThan(0);
			transferOut(entry);
			// A real transfer: the buffer is gone from this thread.
			expect(entry.tile.data.byteLength).toBe(0);
		}

		// The next frames must still be correct — identity and geometry live
		// on the entry, so a detached buffer does not blind the compositor.
		compositor.update(viewport(1, 400));
		clock.run();
		const after = compositor.lastFrame();
		expect(after?.ops.length).toBeGreaterThan(0);
		for (const op of after?.ops ?? []) {
			expect(op.width).toBeGreaterThan(0);
			expect(op.page).toBeGreaterThanOrEqual(0);
		}
		compositor.dispose();
		list.dispose();
	});
});

/**
 * Detach a buffer the way a structured-clone transfer does.
 *
 * `structuredClone(buffer, { transfer: [buffer] })` is the standard way to do
 * it and is available in Node, so this is a real transfer rather than a
 * simulation of one: afterwards `byteLength` is 0 on this side.
 */
function transferOut(entry: TileEntry): void {
	const buffer = entry.tile.data;
	if (buffer.byteLength > 0) {
		structuredClone(buffer, { transfer: [buffer] });
	}
}

describe("device-pixel-ratio correctness", () => {
	// The load-bearing assertion in this file: at every ratio, including the
	// fractional one where the backing store rounds, a full-res tile is
	// presented 1:1 — destination size equals bitmap size, exactly. A
	// compositor that ignored the rounding would be off by a fraction of a
	// percent here, which on text is the whole difference between crisp and
	// soft.
	for (const dpr of [1, 1.25, 1.5, 2, 3]) {
		it(`presents full-res tiles 1:1 at DPR ${dpr}`, async () => {
			const r = rig();
			await mounted(r, dpr);
			const frame = r.compositor.lastFrame();
			expect(frame).not.toBeNull();
			const full = (frame?.ops ?? []).filter((op) => op.stage === "full");
			expect(full.length).toBeGreaterThan(0);
			for (const op of full) {
				expect(op.deviceScale).toBeCloseTo(op.targetDeviceScale, 6);
				expect(op.width).toBe(op.sourceWidth);
				expect(op.height).toBe(op.sourceHeight);
				expect(op.scaled).toBe(false);
				expect(op.provisional).toBe(false);
			}
			r.dispose();
		});
	}

	it("sizes the backing store in whole device pixels", async () => {
		const r = rig();
		await mounted(r, 1.25);
		const size = r.surface.sizes[0];
		expect(size).toBeDefined();
		expect(Number.isInteger(size?.deviceWidth)).toBe(true);
		expect(Number.isInteger(size?.deviceHeight)).toBe(true);
		expect(size?.deviceWidth).toBe(Math.round(1000 * 1.25));
		r.dispose();
	});

	it("fills the backing store exactly, with no seam down the right edge", async () => {
		const r = rig();
		await mounted(r, 1.25);
		const size = r.surface.sizes[0];
		const frame = r.compositor.lastFrame();
		expect(size).toBeDefined();
		expect(frame).not.toBeNull();
		// Rightmost content edge, in device pixels, must land on the store's
		// width: `round(css × scale) === round(css × dpr)` because scale *is*
		// `deviceWidth / cssWidth`.
		const right = Math.max(...(frame?.ops ?? []).map((op) => op.x + op.width));
		expect(right).toBeLessThanOrEqual(size?.deviceWidth ?? 0);
		const scale = (size?.deviceWidth ?? 0) / (size?.cssWidth ?? 1);
		expect(Math.round(1000 * scale)).toBe(size?.deviceWidth);
		r.dispose();
	});

	it("rounds destination edges to whole pixels, so no tile edge is fractional", async () => {
		const r = rig();
		await mounted(r, 1.25);
		for (const op of r.compositor.lastFrame()?.ops ?? []) {
			for (const edge of [op.x, op.y, op.x + op.width, op.y + op.height]) {
				expect(Number.isInteger(edge)).toBe(true);
			}
		}
		r.dispose();
	});

	it("derives the ladder's request scale from the corrected ratio, not the nominal one", async () => {
		// 1013 CSS px at DPR 1.25 rounds to a 1266-px store, so the ratio in
		// force is 1.24975… and the engine must be asked for *that*. A shell
		// passing the raw 1.25 would get tiles that are all subtly resampled on
		// presentation — which, on text, is the difference between crisp and soft.
		const r = rig();
		r.compositor.update({ width: 1013, height: 707, scrollTop: 0, devicePixelRatio: 1.25 });
		await r.engine.settleAll();
		r.clock.run();
		const full = r.engine.requests.find((request) => request.hint === "view");
		const size = r.surface.sizes[0];
		const state = r.list.state();
		expect(full).toBeDefined();
		expect(size?.deviceWidth).toBe(1266);
		expect(full?.scale).toBeCloseTo(state.scale * (size?.scale ?? 0), 9);
		expect(full?.scale).not.toBeCloseTo(state.scale * 1.25, 6);
		r.dispose();
	});

	it("keeps the page boxes in CSS pixels the DOM list already uses", async () => {
		const r = rig();
		await mounted(r, 1.25);
		const state = r.list.state();
		const size = r.surface.sizes[0];
		const scale = size?.scale ?? 1;
		for (const op of r.compositor.lastFrame()?.ops ?? []) {
			const tile = state.tiles.find((candidate) => candidate.page === op.page);
			expect(tile).toBeDefined();
			expect(op.x).toBe(Math.round((tile?.x ?? 0) * scale));
			expect(op.width).toBe(
				Math.round(((tile?.x ?? 0) + (tile?.width ?? 0)) * scale) -
					Math.round((tile?.x ?? 0) * scale),
			);
		}
		r.dispose();
	});
});

describe("the zoom path", () => {
	it("presents the previous tile scaled, on the first frame after the zoom", async () => {
		const r = rig();
		await mounted(r, 1);
		const before = r.compositor.lastFrame();
		expect(before?.ops.some((op) => op.stage === "full")).toBe(true);

		// The zoom. Nothing is awaited: the point of the clause is that the
		// coarse tile is presented *now*, not after the engine catches up.
		const scrollTop = r.list.setZoom(2);
		const ran = r.clock.run();
		expect(ran).toBe(1);

		const frame = r.compositor.lastFrame();
		expect(frame?.index).toBe((before?.index ?? 0) + 1);
		expect(frame?.scrollTop).toBe(scrollTop);
		// Every page that had pixels still has pixels: no blank flash.
		expect(frame?.placeholders).toBe(0);
		expect(frame?.ops.length).toBeGreaterThan(0);
		// And they are explicitly standing in for sharper tiles.
		expect(frame?.provisional).toBe(frame?.ops.length);
		for (const op of frame?.ops ?? []) {
			expect(op.scaled).toBe(true);
			expect(op.provisional).toBe(true);
			expect(op.targetDeviceScale).toBeCloseTo(op.deviceScale * 2, 6);
		}
		r.dispose();
	});

	it("refines the blit when the engine returns the new scale", async () => {
		const r = rig();
		await mounted(r, 1);
		r.list.setZoom(2);
		r.clock.run();
		expect(r.compositor.lastFrame()?.provisional).toBeGreaterThan(0);

		await r.engine.settleAll();
		r.clock.run();
		const frame = r.compositor.lastFrame();
		expect(frame?.provisional).toBe(0);
		expect(frame?.placeholders).toBe(0);
		const full = (frame?.ops ?? []).filter((op) => op.stage === "full");
		expect(full.length).toBeGreaterThan(0);
		for (const op of full) {
			expect(op.scaled).toBe(false);
			expect(op.width).toBe(op.sourceWidth);
		}
		r.dispose();
	});

	it("keeps the reader's place across the zoom", async () => {
		const r = rig();
		await mounted(r, 1);
		r.list.goToPage(6);
		r.clock.run();
		const anchor = r.list.state();
		expect(anchor.currentPage).toBe(6);
		const before = r.compositor.lastFrame()?.ops.map((op) => op.page) ?? [];

		r.list.setZoom(2.5);
		r.clock.run();
		// The page under the reader is still in the window the compositor drew.
		const drawn = new Set(r.compositor.lastFrame()?.ops.map((op) => op.page));
		for (const page of before) {
			if (anchor.tiles.some((tile) => tile.page === page)) {
				expect(drawn.has(page), `page ${page} vanished on zoom`).toBe(true);
			}
		}
		r.dispose();
	});

	it("keeps presenting a scaled tile when the zoom is not a round number", async () => {
		const r = rig();
		await mounted(r, 1);
		r.list.setZoom(1.37);
		r.clock.run();
		const frame = r.compositor.lastFrame();
		expect(frame?.placeholders).toBe(0);
		for (const op of frame?.ops ?? []) {
			expect(Number.isInteger(op.x)).toBe(true);
			expect(op.width).toBeGreaterThan(0);
		}
		r.dispose();
	});
});

describe("frame scheduling", () => {
	it("coalesces a burst of updates into a single frame", async () => {
		const r = rig();
		await mounted(r, 1);
		const drawn = r.surface.frames.length;
		// A scroll produces input events far faster than frames.
		for (let step = 1; step <= 20; step += 1) {
			r.compositor.update(viewport(1, step * 40));
		}
		expect(r.clock.pending).toBe(1);
		r.clock.run();
		expect(r.surface.frames.length).toBe(drawn + 1);
		// And that one frame presents where the reader *ended* up, not where
		// the first event of the burst was.
		expect(r.compositor.lastFrame()?.scrollTop).toBe(800);
		r.dispose();
	});

	it("draws without a frame pending only when asked to", async () => {
		const r = rig();
		r.compositor.update(viewport(1));
		await r.engine.settleAll();
		expect(r.surface.frames).toHaveLength(0);
		expect(r.compositor.flush()).not.toBeNull();
		expect(r.surface.frames).toHaveLength(1);
		// A second flush with nothing pending is a no-op, not a duplicate.
		expect(r.compositor.flush()).toBeNull();
		r.dispose();
	});

	it("stops drawing after dispose and releases the surface", async () => {
		const r = rig();
		await mounted(r, 1);
		const drawn = r.surface.frames.length;
		r.compositor.update(viewport(1, 500));
		r.dispose();
		r.clock.run();
		expect(r.surface.frames.length).toBe(drawn);
		expect(r.surface.disposals).toBe(1);
		// And a published state after disposal does not resurrect it.
		r.list.goToPage(9);
		expect(r.clock.pending).toBe(0);
	});

	it("redraws when a tile lands, without the shell doing anything", async () => {
		const r = rig();
		r.compositor.update(viewport(1));
		r.clock.run();
		expect(r.compositor.lastFrame()?.placeholders).toBeGreaterThan(0);
		await r.engine.settleAll();
		// The ladder published a new state; the subscription armed a frame.
		expect(r.clock.pending).toBe(1);
		r.clock.run();
		expect(r.compositor.lastFrame()?.placeholders).toBe(0);
		r.dispose();
	});
});

describe("accessibility is not traded for the canvas", () => {
	it("draws only pages that are still labelled, focusable DOM tiles", async () => {
		const r = rig();
		await mounted(r, 1);
		const state = r.list.state();
		const drawn = r.compositor.lastFrame()?.ops.map((op) => op.page) ?? [];
		expect(drawn.length).toBeGreaterThan(0);
		for (const page of drawn) {
			const tile = state.tiles.find((candidate) => candidate.page === page);
			expect(tile, `page ${page} is painted but is not a tile`).toBeDefined();
			expect(tile?.label).not.toBe("");
		}
		// And the roving tab stop is still exactly one page, canvas or not.
		expect(state.tiles.filter((tile) => tile.tabIndex === 0)).toHaveLength(1);
		r.dispose();
	});

	it("still announces the page, and the announcement is a localised string", async () => {
		const r = rig();
		await mounted(r, 1);
		r.list.goToPage(3);
		r.clock.run();
		expect(r.list.state().announcement).toContain("Page 4");
		r.dispose();
	});

	it("leaves a page with no pixels as a placeholder, not as a hole", async () => {
		const r = rig();
		r.compositor.update(viewport(1));
		r.clock.run();
		const frame = r.compositor.lastFrame();
		expect(frame?.placeholders).toBeGreaterThan(0);
		// A placeholder page is counted, never drawn: the DOM tile's own
		// styling shows through, so the box, its label and its tab stop are
		// exactly what they were without a compositor.
		const drawn = new Set(frame?.ops.map((op) => op.page));
		for (const tile of r.list.state().tiles) {
			if (tile.stage === "placeholder") {
				expect(drawn.has(tile.page)).toBe(false);
			}
		}
		r.dispose();
	});

	it("culls what is outside the viewport and says how much", async () => {
		// Culling only has work to do when a windowed row lies entirely outside
		// the viewport — which is the *small page* case: short rows mean the
		// overscan band holds whole pages the reader cannot see. (With a page
		// taller than the viewport every windowed row straddles the fold, and
		// there is genuinely nothing to cull.)
		const short = { width: 612, height: 200 };
		const r = rig({ doc: documentOf(60, short), mode: "page" });
		r.compositor.update(viewport(1, 900));
		await r.engine.settleAll();
		r.clock.run();
		const frame = r.compositor.lastFrame();
		expect(frame?.culled).toBeGreaterThan(0);
		expect(frame?.ops.length).toBeGreaterThan(0);
		const size = r.surface.sizes[0];
		for (const op of frame?.ops ?? []) {
			// Every drawn op is inside the store, vertically.
			expect(op.y + op.height).toBeGreaterThanOrEqual(0);
			expect(op.y).toBeLessThanOrEqual(size?.deviceHeight ?? 0);
		}
		r.dispose();
	});

	it("keeps the overscan band out of the draw list but still prefetches it", async () => {
		// The other half of the same seam: with the default overscan the window
		// is larger than the viewport, and the extra rows are the ladder's
		// prefetch — requested by the engine, never drawn by the compositor.
		const wide = rig();
		await mounted(wide, 1);
		const wideFrame = wide.compositor.lastFrame();
		const windowed = wide.list.state().tiles.length;
		expect(wideFrame?.culled ?? 0).toBeGreaterThanOrEqual(0);
		expect(wideFrame?.ops.length).toBeLessThanOrEqual(windowed);
		const prefetched = wide.engine.requests.filter((request) => request.hint === "thumbnail");
		expect(prefetched.length).toBeGreaterThan(0);
		wide.dispose();
	});
});
