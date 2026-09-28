/**
 * Tile-ladder tests (SL-4.UI.02): the ordering of engine work, and the
 * scheduler's three load-bearing behaviours â€” bounded concurrency, cancellation
 * of work the reader scrolled away from, and a bounded LRU cache.
 *
 * The engine here is a hand-rolled fake rather than the UI.01 mock adapter:
 * these tests need to *hold* renders open and resolve them by hand, which the
 * mock's microtask scheduling deliberately does not allow.
 */

import { describe, expect, it } from "vitest";
import type { EnginePort } from "../platform/adapter.js";
import { AdapterError, ErrorCode } from "../platform/errors.js";
import type {
	AdapterRequestOptions,
	DocHandle,
	RenderTileRequest,
	RenderedTile,
} from "../platform/types.js";
import type { LadderInput, TileTask } from "./tile-ladder.js";
import {
	LOW_RES_DEVICE_SCALE,
	MAX_CONCURRENT_TILES,
	TileScheduler,
	planLadder,
	taskKey,
	tileKey,
} from "./tile-ladder.js";

const LETTER = { width: 612, height: 792 } as const;

const doc: DocHandle = {
	id: "doc-1",
	pageCount: 10,
	pageSizes: Array.from({ length: 10 }, () => LETTER),
};

interface PendingRender {
	readonly request: RenderTileRequest;
	readonly options: AdapterRequestOptions | undefined;
	readonly aborted: () => boolean;
	settle(tile: RenderedTile): void;
	fail(error: unknown): void;
	/** True once this render has been settled or failed by the test. */
	readonly done: () => boolean;
}

interface FakeEngine extends EnginePort {
	readonly started: PendingRender[];
	readonly inFlight: number;
	peakInFlight: number;
}

function fakeEngine(): FakeEngine {
	const started: PendingRender[] = [];
	let inFlight = 0;
	const engine: FakeEngine = {
		started,
		get inFlight() {
			return inFlight;
		},
		peakInFlight: 0,
		open: async () => doc,
		close: async () => undefined,
		extractText: async (_d, page) => ({ page, text: "" }),
		// The ladder never asks for character geometry — UI.04's text layer does,
		// and it has its own port. The fake answers rather than being cast past,
		// so that adding a method to `EnginePort` fails this test at the cast
		// (which is the point of the `FakeEngine` shape) instead of at runtime.
		textLayer: async (_d, page) => ({
			page,
			width: LETTER.width,
			height: LETTER.height,
			lines: [],
			text: "",
			lowConfidence: false,
		}),
		// The fake resolves on demand rather than on a timer, so this is a plain
		// function returning a promise, not an `async` one.
		renderTile: (request: RenderTileRequest, options?: AdapterRequestOptions) => {
			const controller = new AbortController();
			const signal = controller.signal;
			// Honour the caller's signal the way a real transport must.
			options?.signal?.addEventListener("abort", () => {
				controller.abort();
			});
			let finished = false;
			const entry: PendingRender = {
				request,
				options,
				aborted: () => signal.aborted,
				done: () => finished,
				settle: (tile) => {
					if (finished) {
						return;
					}
					finished = true;
					inFlight -= 1;
					if (controller.signal.aborted) {
						return;
					}
					resolve(tile);
				},
				fail: (error) => {
					if (finished) {
						return;
					}
					finished = true;
					inFlight -= 1;
					reject(error);
				},
			};
			let resolve: (tile: RenderedTile) => void = () => undefined;
			let reject: (error: unknown) => void = () => undefined;
			const promise = new Promise<RenderedTile>((res, rej) => {
				resolve = res;
				reject = rej;
			});
			inFlight += 1;
			engine.peakInFlight = Math.max(engine.peakInFlight, inFlight);
			started.push(entry);
			return promise;
		},
		// eslint-disable-next-line require-yield
		search: () => {
			throw new Error("not used by the ladder tests");
		},
	} as FakeEngine;
	return engine;
}

function tileFor(page: number, size = 8): RenderedTile {
	return {
		page,
		width: size,
		height: size,
		format: "rgba8",
		data: new ArrayBuffer(size * size * 4),
	};
}

/**
 * Drain the scheduler's promise chain deterministically: `.then â†’ .catch â†’
 * .finally` is three microtask hops, and the tests must not depend on timers.
 */
async function flush(hops = 8): Promise<void> {
	for (let hop = 0; hop < hops; hop += 1) {
		await Promise.resolve();
	}
}

/**
 * Settle everything the engine has started, repeatedly, until no new renders
 * appear. Resolving one render queues the next, so a single pass is not enough
 * to drain a queue â€” and a bounded loop keeps the test deterministic.
 */
async function settleAll(engine: FakeEngine, rounds = 12): Promise<void> {
	for (let round = 0; round < rounds; round += 1) {
		for (const pending of engine.started) {
			if (!pending.done()) {
				pending.settle(tileFor(pending.request.page));
			}
		}
		await flush();
	}
}

function input(overrides: Partial<LadderInput> = {}): LadderInput {
	return {
		doc,
		windowPages: [0, 1, 2, 3, 4].map((page) => ({ page })),
		visiblePages: new Set([1, 2, 3]),
		devicePixelRatio: 1,
		cssScale: 1.5,
		satisfied: new Set<string>(),
		...overrides,
	};
}

/**
 * The `satisfied`/cache key of a planned task, as the scheduler will compute it.
 *
 * UI.03 put the rasterisation scale in the key (see `tileKey`), so these
 * expectations carry it: `input()` asks for full-res at `cssScale Ã— DPR` =
 * 1.5 and low-res at the fixed `LOW_RES_DEVICE_SCALE Ã— DPR` = 0.25.
 */
const keys = (tasks: readonly TileTask[]): string[] => tasks.map((task) => taskKey(task));

const FULL_KEY = "2:full@1.500000";
const LOW_KEY = "0:lowres@0.250000";
/** The key `input()`'s page 1 low-res task carries. */
const LOW_RES_KEY = "1:lowres@0.250000";

describe("planLadder", () => {
	it("asks for full-res on what the reader can see, nearest the middle first", () => {
		const tasks = planLadder(input());
		// Page 2 is the middle of the window: it goes first, then 1 and 3.
		expect(keys(tasks).slice(0, 3)).toEqual([FULL_KEY, "1:full@1.500000", "3:full@1.500000"]);
	});

	it("fills in low-res for the whole window, then prefetches beyond it", () => {
		const tasks = planLadder(input({ prefetchPages: [{ page: 5 }, { page: 6 }] }));
		const ordered = keys(tasks);
		// Full-res for the viewportâ€¦
		expect(ordered.slice(0, 3)).toEqual([FULL_KEY, "1:full@1.500000", "3:full@1.500000"]);
		// â€¦low-res for the rest of the window, in reading orderâ€¦
		expect(ordered.slice(3, 7)).toEqual([
			LOW_KEY,
			"1:lowres@0.250000",
			"2:lowres@0.250000",
			"3:lowres@0.250000",
		]);
		// â€¦and only then the prefetch rows.
		expect(ordered.slice(7)).toEqual([
			"4:lowres@0.250000",
			"5:lowres@0.250000",
			"6:lowres@0.250000",
		]);
	});

	it("scales full-res by zoom Ã— DPR and low-res by the fixed cheap scale", () => {
		const tasks = planLadder(input({ devicePixelRatio: 2, cssScale: 1.5 }));
		const full = tasks.find((task) => task.stage === "full");
		const low = tasks.find((task) => task.stage === "lowres");
		expect(full?.request.scale).toBeCloseTo(3, 6);
		expect(low?.request.scale).toBeCloseTo(LOW_RES_DEVICE_SCALE * 2, 6);
		expect(full?.request.hint).toBe("view");
		expect(low?.request.hint).toBe("thumbnail");
		expect(full?.request.doc).toBe(doc);
	});

	it("treats a nonsensical device pixel ratio as 1 rather than as zero", () => {
		expect(planLadder(input({ devicePixelRatio: 0 }))[0]?.request.scale).toBeCloseTo(1.5, 6);
		expect(planLadder(input({ devicePixelRatio: Number.NaN }))[0]?.request.scale).toBeCloseTo(
			1.5,
			6,
		);
	});

	it("never asks twice for the same rung", () => {
		const tasks = planLadder(
			input({
				prefetchPages: [{ page: 0 }, { page: 0 }],
				satisfied: new Set([LOW_RES_KEY]),
			}),
		);
		const ordered = keys(tasks);
		expect(new Set(ordered).size).toBe(ordered.length);
		expect(ordered).not.toContain(LOW_RES_KEY);
	});

	it("asks again when only the scale changed, which is the zoom path", () => {
		// The heart of UI.03's zoom: a cached tile at the *old* scale does not
		// satisfy a request at the new one, so the ladder re-renders behind the
		// blit instead of treating the stale tile as done. 0.75 is half of
		// `input()`'s 1.5, i.e. the same page one zoom step out.
		const stale = tileKey(2, "full", 0.75);
		const tasks = planLadder(input({ satisfied: new Set([stale]) }));
		expect(keys(tasks)).toContain(FULL_KEY);
		expect(keys(tasks)).not.toContain(stale);
	});

	it("respects a per-frame budget, and a zero budget asks for nothing", () => {
		expect(planLadder(input({ budget: 2 }))).toHaveLength(2);
		expect(planLadder(input({ budget: 0 }))).toHaveLength(0);
	});

	it("has nothing to plan for an empty window", () => {
		expect(planLadder(input({ windowPages: [], visiblePages: new Set() }))).toEqual([]);
	});
});

describe("TileScheduler", () => {
	function task(page: number, stage: "lowres" | "full" = "full"): TileTask {
		return {
			page,
			stage,
			request: { doc, page, scale: 1, hint: stage === "full" ? "view" : "thumbnail" },
		};
	}

	/** The cache key the scheduler will file `task(page, stage)` under. */
	function cachedAt(page: number, stage: "lowres" | "full" = "full", scale = 1): string {
		return tileKey(page, stage, scale);
	}

	it("never exceeds its concurrency ceiling", async () => {
		const engine = fakeEngine();
		const scheduler = new TileScheduler({ engine, doc, maxConcurrent: 2 });
		scheduler.submit([0, 1, 2, 3, 4, 5].map((page) => task(page)));
		expect(engine.started).toHaveLength(2);
		engine.started[0]?.settle(tileFor(0));
		await flush();
		expect(engine.started).toHaveLength(3);
		await settleAll(engine);
		scheduler.dispose();
		expect(engine.peakInFlight).toBeLessThanOrEqual(2);
		// Everything drained through the queue, not by luck.
		expect(engine.started).toHaveLength(6);
	});

	it("defaults to the shared ceiling", () => {
		const engine = fakeEngine();
		const scheduler = new TileScheduler({ engine, doc });
		scheduler.submit(Array.from({ length: 20 }, (_, page) => task(page)));
		expect(engine.started.length).toBe(MAX_CONCURRENT_TILES);
		scheduler.dispose();
	});

	it("aborts in-flight work the reader scrolled away from", async () => {
		const engine = fakeEngine();
		const errors: AdapterError[] = [];
		const scheduler = new TileScheduler({
			engine,
			doc,
			maxConcurrent: 4,
			onError: (error) => errors.push(error),
		});
		scheduler.submit([task(0), task(1), task(2)]);
		const abandoned = engine.started[0];
		expect(abandoned?.aborted()).toBe(false);
		// The next frame wants page 1 and 2 only.
		scheduler.submit([task(1), task(2)]);
		expect(abandoned?.aborted()).toBe(true);
		expect(scheduler.satisfiedKeys().has(cachedAt(0))).toBe(false);
		// The abandoned render rejects as a cancellation, which is not a failure.
		abandoned?.fail(AdapterError.cancelled());
		await flush();
		await flush();
		expect(errors).toEqual([]);
		scheduler.dispose();
	});

	it("caches what lands and prefers full-res over low-res", async () => {
		const engine = fakeEngine();
		const scheduler = new TileScheduler({ engine, doc, maxConcurrent: 2 });
		scheduler.submit([task(3, "lowres"), task(3, "full")]);
		for (const pending of engine.started) {
			pending.settle(tileFor(3));
		}
		await flush();
		await flush();
		expect(scheduler.get(3, "lowres", 1)?.stage).toBe("lowres");
		expect(scheduler.bestFor(3)?.stage).toBe("full");
		expect(scheduler.get(4, "full", 1)).toBeNull();
		scheduler.dispose();
	});

	it("evicts least-recently-used tiles past the cache bound", async () => {
		const engine = fakeEngine();
		const scheduler = new TileScheduler({ engine, doc, maxConcurrent: 4, maxCached: 2 });
		scheduler.submit([task(0), task(1), task(2)]);
		// 0 and 1 land first, filling the two-tile cache.
		engine.started[0]?.settle(tileFor(0));
		engine.started[1]?.settle(tileFor(1));
		await flush();
		// Read 1 then 0, so 0 is the most recently used and 1 the least.
		expect(scheduler.get(1, "full", 1)).not.toBeNull();
		expect(scheduler.get(0, "full", 1)).not.toBeNull();
		// The third tile lands and evicts the least recently used, which is 1.
		await settleAll(engine);
		expect(scheduler.get(2, "full", 1)).not.toBeNull();
		expect(scheduler.get(1, "full", 1)).toBeNull();
		expect(scheduler.get(0, "full", 1)).not.toBeNull();
		scheduler.dispose();
	});

	it("reports a render failure as an AdapterError and does not retry it", async () => {
		const engine = fakeEngine();
		const errors: AdapterError[] = [];
		const scheduler = new TileScheduler({
			engine,
			doc,
			maxConcurrent: 1,
			onError: (error) => errors.push(error),
		});
		scheduler.submit([task(0)]);
		engine.started[0]?.fail(new Error("worker died"));
		await flush();
		await flush();
		expect(errors).toHaveLength(1);
		expect(errors[0]?.code).toBe(ErrorCode.BindingBadArgument);
		expect(errors[0]?.docState).toBe("Loaded");
		expect(scheduler.satisfiedKeys().has(cachedAt(0))).toBe(true);
		scheduler.dispose();
	});

	it("passes a registry-coded engine error straight through", async () => {
		const engine = fakeEngine();
		const errors: AdapterError[] = [];
		const scheduler = new TileScheduler({
			engine,
			doc,
			maxConcurrent: 1,
			onError: (error) => errors.push(error),
		});
		scheduler.submit([task(0)]);
		engine.started[0]?.fail(AdapterError.badArgument("page out of range"));
		await flush();
		await flush();
		expect(errors[0]?.code).toBe(ErrorCode.BindingBadArgument);
		expect(errors[0]?.message).toContain("page out of range");
		scheduler.dispose();
	});

	it("drops a result that a newer frame superseded, and ignores work after dispose", async () => {
		const engine = fakeEngine();
		const landed: number[] = [];
		const scheduler = new TileScheduler({
			engine,
			doc,
			maxConcurrent: 2,
			onTile: (entry) => landed.push(entry.page),
		});
		scheduler.submit([task(0)]);
		// A new frame arrives: page 0 is cancelled, so its result is stale.
		scheduler.submit([task(1)]);
		scheduler.dispose();
		for (const pending of engine.started) {
			pending.settle(tileFor(pending.request.page));
		}
		await flush();
		await flush();
		expect(landed).toEqual([]);
		expect(scheduler.get(1, "full", 1)).toBeNull();
	});

	it("exposes the document it renders and clears its cache on demand", () => {
		const engine = fakeEngine();
		const scheduler = new TileScheduler({ engine, doc });
		expect(scheduler.renderedDocument).toBe(doc);
		scheduler.clearCache();
		expect(scheduler.satisfiedKeys().size).toBe(0);
		scheduler.dispose();
	});
});
