/**
 * Tile-ladder tests (SL-4.UI.02): the ordering of engine work, and the
 * scheduler's three load-bearing behaviours — bounded concurrency, cancellation
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
 * Drain the scheduler's promise chain deterministically: `.then → .catch →
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
 * to drain a queue — and a bounded loop keeps the test deterministic.
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

const keys = (tasks: readonly TileTask[]): string[] =>
	tasks.map((task) => tileKey(task.page, task.stage));

describe("planLadder", () => {
	it("asks for full-res on what the reader can see, nearest the middle first", () => {
		const tasks = planLadder(input());
		// Page 2 is the middle of the window: it goes first, then 1 and 3.
		expect(keys(tasks).slice(0, 3)).toEqual(["2:full", "1:full", "3:full"]);
	});

	it("fills in low-res for the whole window, then prefetches beyond it", () => {
		const tasks = planLadder(input({ prefetchPages: [{ page: 5 }, { page: 6 }] }));
		const ordered = keys(tasks);
		// Full-res for the viewport…
		expect(ordered.slice(0, 3)).toEqual(["2:full", "1:full", "3:full"]);
		// …low-res for the rest of the window, in reading order…
		expect(ordered.slice(3, 7)).toEqual(["0:lowres", "1:lowres", "2:lowres", "3:lowres"]);
		// …and only then the prefetch rows.
		expect(ordered.slice(7)).toEqual(["4:lowres", "5:lowres", "6:lowres"]);
	});

	it("scales full-res by zoom × DPR and low-res by the fixed cheap scale", () => {
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
			input({ prefetchPages: [{ page: 0 }, { page: 0 }], satisfied: new Set(["1:lowres"]) }),
		);
		const ordered = keys(tasks);
		expect(new Set(ordered).size).toBe(ordered.length);
		expect(ordered).not.toContain("1:lowres");
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
		expect(scheduler.satisfiedKeys().has("0:full")).toBe(false);
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
		expect(scheduler.get(3, "lowres")?.stage).toBe("lowres");
		expect(scheduler.bestFor(3)?.stage).toBe("full");
		expect(scheduler.get(4, "full")).toBeNull();
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
		expect(scheduler.get(1, "full")).not.toBeNull();
		expect(scheduler.get(0, "full")).not.toBeNull();
		// The third tile lands and evicts the least recently used, which is 1.
		await settleAll(engine);
		expect(scheduler.get(2, "full")).not.toBeNull();
		expect(scheduler.get(1, "full")).toBeNull();
		expect(scheduler.get(0, "full")).not.toBeNull();
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
		expect(scheduler.satisfiedKeys().has("0:full")).toBe(true);
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
		expect(scheduler.get(1, "full")).toBeNull();
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
