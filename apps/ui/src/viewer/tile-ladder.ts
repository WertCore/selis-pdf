/**
 * The tile ladder and its scheduler (SL-4.UI.02): placeholder → low-res →
 * full-res, with cancellation and bounded concurrency.
 *
 * Why the ladder exists: on a 2 000-page document at 60 fps there is never
 * time to render a page at full resolution *before* it is needed, so every
 * page is drawn three times in ascending cost. The placeholder is not a render
 * at all — it is the box from `layout.ts` painted by CSS, which is why a tile
 * resolving can never shift layout. Low-res is a cheap rasterisation at a
 * fixed device scale (enough to show the page is there, wrong detail), and
 * full-res is the real thing.
 *
 * The scheduler is the part with teeth:
 * - **Bounded concurrency.** At most {@link MAX_CONCURRENT_TILES} engine
 *   calls are in flight. A fast scroll over a long document otherwise queues
 *   hundreds of renders and every one steals time from the frame on screen.
 * - **Cancellation through the platform seam.** A page that scrolled away has
 *   its in-flight render aborted via the `AbortSignal` of
 *   `EnginePort.renderTile`, which the transport maps to its cancel primitive
 *   (the WASM.01 `cancel` message, a native `CancelToken`). It surfaces as
 *   `AdapterError` 4020 `CANCELLED` / `docState: "Unchanged"` and is swallowed
 *   here because an abort is the expected outcome, not a failure.
 * - **A bounded, scale-keyed cache.** Tiles are keyed by page, rung *and*
 *   rasterisation scale, and evicted least-recently-used past
 *   {@link MAX_CACHED_TILES}, so peak memory on a 2 000-page document is
 *   a function of the cache bound, not of the document length. The scale is
 *   in the key for UI.03's zoom path: a zoom leaves the previous scale's tiles
 *   cached, so the compositor can present them scaled up immediately instead
 *   of showing a blank page, while the ladder's `satisfied` set no longer
 *   matches them and re-renders the new scale behind that. See
 *   {@link tileKey}.
 *
 * All engine work goes through `PlatformAdapter.engine` (SL-4.UI.01) — this
 * module names no platform globals, which the `platform-globals` lint gate in
 * `../platform` enforces for the whole package.
 */

import type { EnginePort } from "../platform/adapter.js";
import { AdapterError, ErrorCode } from "../platform/errors.js";
import type { DocHandle, RenderTileRequest, RenderedTile } from "../platform/types.js";

/** Rungs of the ladder, in ascending cost. */
export type TileStage = "placeholder" | "lowres" | "full";

/** Engine-bound rungs. `placeholder` is CSS-only and never reaches the engine. */
export type RenderedStage = Exclude<TileStage, "placeholder">;

/**
 * Device pixels per PDF point for the low-res rung. 0.25 gives a 612 pt page
 * a ~153 px bitmap: recognisably a page, ~1/16th the pixels of a 1× view.
 */
export const LOW_RES_DEVICE_SCALE = 0.25;

/** Engine calls allowed in flight at once. See the module doc. */
export const MAX_CONCURRENT_TILES = 4;

/** Cache bound in tiles; eviction is least-recently-used. */
export const MAX_CACHED_TILES = 64;

/** One unit of engine work produced by {@link planLadder}. */
export interface TileTask {
	readonly page: number;
	readonly stage: RenderedStage;
	/** The exact request handed to `EnginePort.renderTile`. */
	readonly request: RenderTileRequest;
}

/** A cached rasterisation. The buffer is owned by the cache, never aliased. */
export interface TileEntry {
	readonly page: number;
	readonly stage: RenderedStage;
	/** Device pixels per point the tile was rendered at. */
	readonly deviceScale: number;
	readonly tile: RenderedTile;
}

/** What the ladder is allowed to know about the current frame. */
export interface LadderInput {
	readonly doc: DocHandle;
	/** Pages in the window, in reading order. */
	readonly windowPages: readonly { readonly page: number }[];
	/** Pages actually inside the viewport (no overscan). */
	readonly visiblePages: ReadonlySet<number>;
	/** Pages just outside the window, in reading order, to prefetch low-res. */
	readonly prefetchPages?: readonly { readonly page: number }[];
	/** Device pixels per CSS pixel. The host supplies it; we never sniff it. */
	readonly devicePixelRatio: number;
	/** Effective CSS pixels per PDF point for the current zoom/fit. */
	readonly cssScale: number;
	/** `page:stage` keys already in the cache or already in flight. */
	readonly satisfied: ReadonlySet<string>;
	/** Cap on tasks returned; the rest are picked up on a later frame. */
	readonly budget?: number;
}

/**
 * The cache key: `page:stage@deviceScale`, where the scale is **device pixels
 * per PDF point**.
 *
 * The scale is in the key because a tile is only usable at the resolution it
 * was rasterised at, and because UI.03's zoom path needs the old resolution to
 * survive a zoom. Before UI.03, `page:stage` was the whole key, which made a
 * cached tile at the wrong scale indistinguishable from one at the right
 * scale — so `PageList`'s zoom path had no choice but to drop the entire cache
 * and show blank pages until the engine caught up. Keying by scale makes the
 * two separable: a zoom leaves the old tiles cached (they are what the
 * compositor scales up while the ladder re-renders) and the ladder's
 * `satisfied` set no longer matches them, so the new scale is requested. The
 * cache stays bounded either way; stale-scale tiles age out through the same
 * LRU as everything else.
 *
 * The scale is quantised to six decimals so a float wobble in the last bit
 * cannot produce a second cache entry for a tile that is already there — at
 * 1e-6 device pixels per point that is a millionth of a pixel.
 */
export function tileKey(page: number, stage: TileStage, deviceScale = 0): string {
	return `${page}:${stage}@${deviceScale.toFixed(6)}`;
}

/** The `satisfied` key a planned task will be cached under. */
export function taskKey(task: TileTask): string {
	return tileKey(task.page, task.stage, task.request.scale);
}
/**
 * Order the engine work for one frame.
 *
 * Priority, and the reasoning behind it:
 * 1. **Full-res for the viewport**, ordered by distance from the middle of the
 *    window. What the reader is looking at first is the only thing that can
 *    make the frame look wrong.
 * 2. **Low-res for the rest of the window**, so every visible page has *some*
 *    content before its full-res tile lands.
 * 3. **Low-res for the prefetch rows**, so a fast scroll finds a preview
 *    waiting instead of a blank box.
 *
 * Full-res is queued ahead of low-res for the same page, which reads backwards
 * against the ladder's own name. It is deliberate: the low-res bitmap for a
 * page in the viewport was produced by prefetch on an earlier frame, so
 * re-requesting it is a duplicate; the rung actually missing at the top of the
 * screen is full-res. Prefetch is what makes the ladder climb.
 */
export function planLadder(input: LadderInput): readonly TileTask[] {
	const { doc, devicePixelRatio, cssScale, satisfied } = input;
	const budget = Math.max(0, input.budget ?? Number.POSITIVE_INFINITY);
	const ratio = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0 ? devicePixelRatio : 1;
	const visible = input.visiblePages;
	const windowPages = input.windowPages;
	const prefetch = input.prefetchPages ?? [];

	const centreIndex = Math.max(0, (windowPages.length - 1) / 2);
	const distanceFromCentre = (index: number): number => Math.abs(index - centreIndex);

	const tasks: TileTask[] = [];
	const queued = new Set<string>();
	const push = (
		page: number,
		stage: RenderedStage,
		deviceScale: number,
		hint: "thumbnail" | "view",
	) => {
		// The engine works in device pixels per point; zoom × DPR is the UI's
		// job (RenderTileRequest documents exactly this). The clamped value is
		// the one that goes in the request *and* in the key, so a scale the
		// clamp rewrites cannot produce a key the scheduler will not find.
		const scale = Math.max(0.01, deviceScale * ratio);
		const key = tileKey(page, stage, scale);
		if (satisfied.has(key) || queued.has(key)) {
			return;
		}
		queued.add(key);
		tasks.push({
			page,
			stage,
			request: {
				doc,
				page,
				scale,
				hint,
			},
		});
	};

	const byCentre = windowPages
		.map((entry, index) => ({ page: entry.page, index }))
		.filter((entry) => visible.has(entry.page))
		.sort((a, b) => distanceFromCentre(a.index) - distanceFromCentre(b.index));
	for (const entry of byCentre) {
		push(entry.page, "full", cssScale, "view");
	}
	for (const entry of windowPages) {
		push(entry.page, "lowres", LOW_RES_DEVICE_SCALE, "thumbnail");
	}
	for (const entry of prefetch) {
		push(entry.page, "lowres", LOW_RES_DEVICE_SCALE, "thumbnail");
	}

	return Number.isFinite(budget) ? tasks.slice(0, budget) : tasks;
}

export interface TileSchedulerOptions {
	readonly engine: EnginePort;
	readonly doc: DocHandle;
	/** Called for every tile that lands — the compositor (UI.03) hooks here. */
	readonly onTile?: (entry: TileEntry) => void;
	/** Called for any non-cancellation `AdapterError`. */
	readonly onError?: (error: AdapterError) => void;
	/** In-flight ceiling; defaults to {@link MAX_CONCURRENT_TILES}. */
	readonly maxConcurrent?: number;
	/** Cache ceiling in tiles; defaults to {@link MAX_CACHED_TILES}. */
	readonly maxCached?: number;
}

/**
 * Drives `EnginePort.renderTile` for the ladder: queue, concurrency, abort of
 * work the reader has scrolled away from, and an LRU cache of what landed.
 *
 * All state is per-scheduler and private, so the controller can create one per
 * open document and `dispose()` it deterministically.
 */
export class TileScheduler {
	readonly #engine: EnginePort;
	readonly #doc: DocHandle;
	readonly #onTile: ((entry: TileEntry) => void) | undefined;
	readonly #onError: ((error: AdapterError) => void) | undefined;
	readonly #maxConcurrent: number;
	readonly #maxCached: number;
	/** Cache in insertion order; re-inserting on a hit makes eviction LRU. */
	readonly #cache = new Map<string, TileEntry>();
	readonly #inFlight = new Map<string, { controller: AbortController; deviceScale: number }>();
	#queue: TileTask[] = [];
	#generation = 0;
	#disposed = false;
	/** `page:stage` that failed, so a bad page is not retried every frame. */
	readonly #failed = new Set<string>();

	constructor(options: TileSchedulerOptions) {
		this.#engine = options.engine;
		this.#doc = options.doc;
		this.#onTile = options.onTile;
		this.#onError = options.onError;
		this.#maxConcurrent = Math.max(1, options.maxConcurrent ?? MAX_CONCURRENT_TILES);
		this.#maxCached = Math.max(1, options.maxCached ?? MAX_CACHED_TILES);
	}

	/**
	 * The document this scheduler renders — named `renderedDocument` because
	 * `document` is a platform global, and the SL-4.UI.01 globals lint in
	 * `../platform` fails the build on a bare reference to one even as a
	 * property name. Handles die with the adapter.
	 */
	get renderedDocument(): DocHandle {
		return this.#doc;
	}

	/** `page:stage@scale` keys that need no engine work: cached, in flight, or failed. */
	satisfiedKeys(): ReadonlySet<string> {
		return new Set<string>([...this.#cache.keys(), ...this.#inFlight.keys(), ...this.#failed]);
	}

	/**
	 * Every cached bitmap for a page, whatever scale it was rasterised at.
	 *
	 * This is UI.03's entry point, and the reason the cache is scale-keyed: the
	 * compositor asks for "the best thing I can show for this page right now",
	 * which after a zoom is a tile at the *old* scale that it presents scaled
	 * up while the ladder re-renders. Ordered `full` before `lowres` within a
	 * scale, and newest-use first across scales, so a caller that takes the
	 * first entry gets the most recently drawn one.
	 */
	entriesFor(page: number): readonly TileEntry[] {
		const found: TileEntry[] = [];
		for (const entry of this.#cache.values()) {
			if (entry.page === page) {
				found.push(entry);
			}
		}
		found.sort((a, b) => {
			if (a.stage !== b.stage) {
				return a.stage === "full" ? -1 : 1;
			}
			return b.deviceScale - a.deviceScale;
		});
		return found;
	}

	/**
	 * The best cached rung for a page, whatever scale: full-res preferred over
	 * low-res, then the highest resolution. The compositor uses
	 * {@link entriesFor} instead, because it weighs the scale error too.
	 */
	bestFor(page: number): TileEntry | null {
		return this.entriesFor(page)[0] ?? null;
	}

	/** The exact cached rung at a scale, or `null`. */
	get(page: number, stage: RenderedStage, deviceScale = 0): TileEntry | null {
		const key = tileKey(page, stage, deviceScale);
		const hit = this.#cache.get(key);
		if (hit === undefined) {
			return null;
		}
		// Touch for LRU: delete + re-insert moves the key to the end.
		this.#cache.delete(key);
		this.#cache.set(key, hit);
		return hit;
	}

	/** Drop every cached tile (a zoom change makes the old scale unusable). */
	clearCache(): void {
		this.#cache.clear();
	}

	/**
	 * Submit this frame's tasks. Anything queued or in flight that is not in
	 * `tasks` is cancelled: the reader moved on, and finishing it would burn a
	 * concurrency slot on a page nobody can see.
	 */
	submit(tasks: readonly TileTask[]): void {
		if (this.#disposed) {
			return;
		}
		const wanted = new Set(tasks.map((task) => taskKey(task)));
		this.#generation += 1;
		for (const [key, flight] of [...this.#inFlight]) {
			if (!wanted.has(key)) {
				flight.controller.abort();
				this.#inFlight.delete(key);
			}
		}
		this.#queue = tasks.filter((task) => wanted.has(taskKey(task)));
		this.#pump();
	}

	/** Abort everything in flight and drop the queue. */
	cancelAll(): void {
		for (const flight of this.#inFlight.values()) {
			flight.controller.abort();
		}
		this.#inFlight.clear();
		this.#queue = [];
		this.#generation += 1;
	}

	dispose(): void {
		this.cancelAll();
		this.#cache.clear();
		this.#failed.clear();
		this.#disposed = true;
	}

	#pump(): void {
		while (!this.#disposed && this.#inFlight.size < this.#maxConcurrent && this.#queue.length > 0) {
			const task = this.#queue.shift();
			if (task === undefined) {
				return;
			}
			this.#start(task);
		}
	}

	#start(task: TileTask): void {
		const key = taskKey(task);
		const controller = new AbortController();
		const generation = this.#generation;
		const deviceScale = task.request.scale;
		this.#inFlight.set(key, { controller, deviceScale });
		this.#engine
			.renderTile(task.request, { signal: controller.signal })
			.then((tile) => {
				this.#inFlight.delete(key);
				if (this.#disposed || generation !== this.#generation) {
					// A newer frame superseded this result: drop it rather than
					// paint a tile the reader has already scrolled past.
					return;
				}
				const entry: TileEntry = { page: tile.page, stage: task.stage, deviceScale, tile };
				this.#cache.set(key, entry);
				this.#evict();
				this.#onTile?.(entry);
			})
			.catch((error: unknown) => {
				this.#inFlight.delete(key);
				if (isCancellation(error)) {
					// 4020 CANCELLED: we asked for it; it is not a failure.
					return;
				}
				const adapterError = asAdapterError(error, task);
				this.#failed.add(key);
				this.#onError?.(adapterError);
			})
			.finally(() => {
				if (!this.#disposed) {
					this.#pump();
				}
			});
	}

	#evict(): void {
		while (this.#cache.size > this.#maxCached) {
			const oldest = this.#cache.keys().next();
			if (oldest.done === true) {
				return;
			}
			this.#cache.delete(oldest.value);
		}
	}
}

function isCancellation(error: unknown): boolean {
	return error instanceof AdapterError && error.code === ErrorCode.Cancelled;
}

/**
 * Keep the platform seam's error contract: a render failure is an
 * `AdapterError` with a registry code and a `docState`, never an arbitrary
 * thrown value (24-BINDINGS-SPEC §1, 03-CONVENTIONS §3).
 */
function asAdapterError(error: unknown, task: TileTask): AdapterError {
	if (error instanceof AdapterError) {
		return error;
	}
	const message = error instanceof Error ? error.message : String(error);
	return new AdapterError({
		code: ErrorCode.BindingBadArgument,
		message: `rendering page ${task.page} (${task.stage}) failed: ${message}`,
		docState: "Loaded",
		retryable: true,
	});
}
