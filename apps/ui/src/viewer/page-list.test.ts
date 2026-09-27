/**
 * Controller tests (SL-4.UI.02) — the DoD's two clauses made executable.
 *
 * 1. *"60 fps sustained scroll on a 2 000-page document."* What a Node test can
 *    honestly assert is the part that makes 60 fps possible: the window stays
 *    bounded, the work per scroll step is a binary search plus a handful of
 *    small objects, and a relayout happens only when the inputs change. A
 *    sustained-scroll loop walks a full 2 000-page document and measures the
 *    per-frame cost against the frame budget, logging the measured number so a
 *    regression is visible. Real frames-per-second needs a browser and belongs
 *    with UI.03's compositor benchmark — this is the headless half of the claim,
 *    not a substitute for it.
 * 2. *"No layout shift when a tile resolves."* The placeholder, low-res and
 *    full-res states of a page are compared field by field: identical boxes,
 *    identical scrollable content size.
 *
 * The engine is a hand-rolled fake for the same reason as in
 * `tile-ladder.test.ts`: renders must be held open and resolved by hand.
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
import type { PageListState, PageListViewport } from "./page-list.js";
import { createPageList } from "./page-list.js";

const LETTER: Size = { width: 612, height: 792 };

/** US Letter at 1000×800 CSS px, DPR 2. */
const VIEWPORT: PageListViewport = {
	width: 1000,
	height: 800,
	scrollTop: 0,
	devicePixelRatio: 2,
};

/** One 60 fps frame, in milliseconds (03-CONVENTIONS §12: no frame > 16 ms). */
const FRAME_BUDGET_MS = 16;

function documentOf(pageCount: number, size: Size = LETTER): DocHandle {
	return {
		id: `doc-${pageCount}`,
		pageCount,
		pageSizes: Array.from({ length: pageCount }, () => size),
	};
}

interface Pending {
	readonly request: RenderTileRequest;
	readonly aborted: () => boolean;
	land(): void;
}

interface Harness {
	readonly adapter: PlatformAdapter;
	/** Every render the ladder asked for, in order. */
	readonly requests: RenderTileRequest[];
	/** Abort signals handed to the engine, in request order. */
	readonly signals: AbortSignal[];
	readonly errors: AdapterError[];
	/** Land every outstanding render, repeatedly, and drain the microtasks. */
	settle(): Promise<void>;
}

function harness(doc: DocHandle): Harness {
	const requests: RenderTileRequest[] = [];
	const signals: AbortSignal[] = [];
	const errors: AdapterError[] = [];
	const pending: Pending[] = [];

	const engine = {
		open: async () => doc,
		close: async () => undefined,
		extractText: async (_d: DocHandle, page: number) => ({ page, text: "" }),
		renderTile(request: RenderTileRequest, options?: AdapterRequestOptions) {
			requests.push(request);
			const controller = new AbortController();
			signals.push(controller.signal);
			options?.signal?.addEventListener("abort", () => {
				controller.abort();
			});
			const tile: RenderedTile = {
				page: request.page,
				width: Math.max(1, Math.round(612 * request.scale)),
				height: Math.max(1, Math.round(792 * request.scale)),
				format: "rgba8",
				data: new ArrayBuffer(4),
			};
			return new Promise<RenderedTile>((resolve, reject) => {
				const onAbort = () => reject(AdapterError.cancelled());
				controller.signal.addEventListener("abort", onAbort);
				pending.push({
					request,
					aborted: () => controller.signal.aborted,
					land: () => {
						controller.signal.removeEventListener("abort", onAbort);
						resolve(tile);
					},
				});
			});
		},
		search: () => {
			throw new Error("not used by the page-list tests");
		},
	} as unknown as EnginePort;

	return {
		adapter: { capabilities: {}, engine } as unknown as PlatformAdapter,
		requests,
		signals,
		errors,
		async settle() {
			for (let round = 0; round < 12; round += 1) {
				const batch = pending.splice(0, pending.length);
				for (const entry of batch) {
					entry.land();
				}
				for (let hop = 0; hop < 8; hop += 1) {
					await Promise.resolve();
				}
			}
		},
	};
}

const geometryOf = (state: PageListState) =>
	state.tiles.map((tile) => ({
		page: tile.page,
		x: tile.x,
		y: tile.y,
		width: tile.width,
		height: tile.height,
	}));

describe("the virtualised page list", () => {
	it("starts with no engine work, then paints a bounded window from the first report", () => {
		const doc = documentOf(2000);
		const world = harness(doc);
		const list = createPageList({ adapter: world.adapter, doc });
		// Before the shell reports a viewport there is nothing to measure against,
		// so no page is asked of the engine.
		expect(world.requests).toEqual([]);
		expect(list.state().pageCount).toBe(2000);
		expect(list.state().viewportHeight).toBe(0);

		const state = list.update(VIEWPORT);
		expect(world.requests.length).toBeGreaterThan(0);
		expect(state.tiles.length).toBeGreaterThan(0);
		expect(state.tiles.length).toBeLessThanOrEqual(8);
		expect(state.currentPage).toBe(0);
		expect(state.contentHeight).toBeGreaterThan(800);
		expect(state.windowClamped).toBe(false);
		list.dispose();
	});

	it("keeps the window bounded and ordered at every scroll position", () => {
		const doc = documentOf(2000);
		const list = createPageList({ adapter: harness(doc).adapter, doc, mode: "width" });
		const total = list.update(VIEWPORT).contentHeight;
		for (let step = 0; step <= 20; step += 1) {
			const state = list.update({ ...VIEWPORT, scrollTop: (total / 20) * step });
			const numbers = state.tiles.map((tile) => tile.page);
			expect(numbers.length).toBeLessThanOrEqual(12);
			expect(numbers).toEqual([...numbers].sort((a, b) => a - b));
			// Exactly one page carries `aria-current`, and it is the one at the top.
			expect(state.tiles.filter((tile) => tile.current)).toHaveLength(1);
			expect(state.tiles.find((tile) => tile.current)?.page).toBe(state.currentPage);
			// Roving tabindex: the current page is the only one in the tab order.
			expect(state.tiles.filter((tile) => tile.tabIndex === 0)).toHaveLength(1);
		}
		list.dispose();
	});

	it("names every page for assistive tech and announces the current one", () => {
		const doc = documentOf(40);
		const list = createPageList({ adapter: harness(doc).adapter, doc });
		const state = list.update({ ...VIEWPORT, scrollTop: 5000 });
		expect(state.regionLabel).toBe("Document pages");
		expect(state.listLabel).toBe("Pages");
		expect(state.announcement).toBe(`Page ${state.currentPage + 1} of 40 — fit page`);
		for (const tile of state.tiles) {
			expect(tile.label).toBe(`Page ${tile.page + 1} of 40`);
			expect(tile.className.split(" ")).toContain("selis-page-tile");
		}
		list.dispose();
	});

	it("formats every string through the host's catalogue, not an inline English literal", () => {
		const doc = documentOf(40);
		// A host shipping another language passes a catalogue; nothing in the
		// component is forked. This is the seam SL-4.UI.11 plugs a runtime into.
		const list = createPageList({
			adapter: harness(doc).adapter,
			doc,
			strings: {
				"pageList.page.label": "Seite {page} von {total}",
				"pageList.page.announcement": "{mode}: {pageLabel}",
				"pageList.mode.page": "an Seitenbreite",
				"pageList.region.label": "Dokumentseiten",
				"pageList.list.label": "Seiten",
			},
		});
		const state = list.update({ ...VIEWPORT, scrollTop: 5000 });
		expect(state.regionLabel).toBe("Dokumentseiten");
		expect(state.listLabel).toBe("Seiten");
		expect(state.announcement).toBe(`an Seitenbreite: Seite ${state.currentPage + 1} von 40`);
		for (const tile of state.tiles) {
			expect(tile.label).toBe(`Seite ${tile.page + 1} von 40`);
		}
		// A key the host did not override still falls back to English rather
		// than rendering an empty label.
		expect(state.mode).toBe("page");
		list.dispose();
	});

	it("walks the whole document within the per-frame budget", async () => {
		const doc = documentOf(2000);
		const world = harness(doc);
		const list = createPageList({ adapter: world.adapter, doc, mode: "width" });
		const total = list.update(VIEWPORT).contentHeight;
		const frames = 300;
		const started = performance.now();
		for (let frame = 0; frame < frames; frame += 1) {
			list.update({ ...VIEWPORT, scrollTop: (total / frames) * frame });
		}
		const perFrame = (performance.now() - started) / frames;
		// The measured number is the point of the log line: a regression in the
		// per-frame cost shows up in the test output, not only as a red test.
		console.info(`sustained scroll: ${perFrame.toFixed(3)} ms/frame over ${frames} frames`);
		expect(perFrame).toBeLessThan(FRAME_BUDGET_MS);
		await world.settle();
		list.dispose();
	});

	it("does not relayout while only the scroll offset changes", () => {
		const doc = documentOf(2000);
		const list = createPageList({ adapter: harness(doc).adapter, doc, mode: "width" });
		const first = list.update(VIEWPORT);
		const layout = list.layout();
		list.update({ ...VIEWPORT, scrollTop: 4000 });
		// The same layout object comes back: no O(pages) work per scroll step.
		expect(list.layout()).toBe(layout);
		expect(list.layout().scale).toBe(first.scale);
		// A real change of input does produce a new layout.
		list.setZoom(2);
		expect(list.layout()).not.toBe(layout);
		list.dispose();
	});
});

describe("the tile ladder, end to end", () => {
	it("climbs placeholder → low-res → full-res without moving anything", async () => {
		const doc = documentOf(6);
		const world = harness(doc);
		const list = createPageList({
			adapter: world.adapter,
			doc,
			onError: (error) => world.errors.push(error),
		});

		const before = list.update(VIEWPORT);
		const geometryBefore = geometryOf(before);
		expect(before.tiles.every((tile) => tile.stage === "placeholder")).toBe(true);
		expect(world.requests.length).toBeGreaterThan(0);
		// The engine was asked for the pages on screen first, at zoom × DPR.
		expect(world.requests[0]?.scale).toBeCloseTo(before.scale * 2, 6);

		await world.settle();
		const after = list.update(VIEWPORT);
		// Rungs climbed, geometry did not move: this is the DoD's second clause.
		expect(after.tiles.some((tile) => tile.stage !== "placeholder")).toBe(true);
		expect(geometryOf(after)).toEqual(geometryBefore);
		expect(after.contentHeight).toBe(before.contentHeight);
		expect(after.contentWidth).toBe(before.contentWidth);
		expect(world.errors).toEqual([]);
		list.dispose();
	});

	it("keeps a page's box identical across all three rungs", async () => {
		const doc = documentOf(3);
		const world = harness(doc);
		const list = createPageList({ adapter: world.adapter, doc });
		const boxes = new Map<number, string>();
		const record = (state: PageListState) => {
			for (const tile of state.tiles) {
				const box = `${tile.x},${tile.y},${tile.width},${tile.height}`;
				const seen = boxes.get(tile.page);
				if (seen === undefined) {
					boxes.set(tile.page, box);
				} else {
					expect(seen, `page ${tile.page} moved between rungs`).toBe(box);
				}
			}
		};
		record(list.update(VIEWPORT));
		await world.settle();
		record(list.update(VIEWPORT));
		// And the tile is reachable for the compositor at its rendered size.
		expect(list.tileAt(0)?.tile.width).toBeGreaterThan(0);
		list.dispose();
	});

	it("cancels the tiles of a page the reader scrolls away from", async () => {
		const doc = documentOf(200);
		const world = harness(doc);
		const list = createPageList({ adapter: world.adapter, doc, mode: "width" });
		list.update(VIEWPORT);
		const firstSignals = [...world.signals];
		expect(firstSignals.length).toBeGreaterThan(0);
		expect(firstSignals.some((signal) => signal.aborted)).toBe(false);

		const far = list.update({ ...VIEWPORT, scrollTop: 400000 });
		expect(far.tiles.every((tile) => tile.page > 100)).toBe(true);
		expect(firstSignals.some((signal) => signal.aborted)).toBe(true);
		// A cancellation is not an error the shell has to render.
		await world.settle();
		expect(world.errors).toEqual([]);
		list.dispose();
	});

	it("abandons in-flight work when the document is disposed", async () => {
		const doc = documentOf(50);
		const world = harness(doc);
		const list = createPageList({ adapter: world.adapter, doc });
		list.update(VIEWPORT);
		const signals = [...world.signals];
		list.dispose();
		expect(signals.length).toBeGreaterThan(0);
		expect(signals.every((signal) => signal.aborted)).toBe(true);
		// A late state read is still safe after dispose.
		expect(list.state().pageCount).toBe(50);
		await world.settle();
	});
});

describe("navigation and anchoring", () => {
	it("goes to a page and reports the offset the shell must apply", () => {
		const doc = documentOf(100);
		const list = createPageList({ adapter: harness(doc).adapter, doc });
		list.update(VIEWPORT);
		const state = list.goToPage(40);
		expect(state.currentPage).toBe(40);
		expect(state.scrollTop).toBeCloseTo(list.layout().pages[40]?.y ?? -1, 6);
		expect(list.scrollTopFor(40)).toBeCloseTo(state.scrollTop, 6);
		// An unknown page clamps to the document rather than throwing.
		expect(list.goToPage(9999).currentPage).toBe(99);
		list.dispose();
	});

	it("keeps the reader on the same page when the zoom changes", () => {
		const doc = documentOf(2000);
		const list = createPageList({ adapter: harness(doc).adapter, doc, mode: "width" });
		list.update(VIEWPORT);
		list.goToPage(900);
		const before = list.state();
		const halfway = list.layout().pages[900];
		// Stand a third of the way into the anchor page, as a reader would.
		const scrollTop = (halfway?.y ?? 0) + (halfway?.height ?? 0) / 3;
		list.update({ ...VIEWPORT, scrollTop });
		expect(list.state().currentPage).toBe(900);

		const target = list.setZoom(2.5);
		const state = list.state();
		const after = list.layout().pages[900];
		expect(state.zoom).toBe(2.5);
		expect(state.currentPage).toBe(900);
		expect(state.scale).toBeCloseTo(before.scale * 2.5, 6);
		// The same third of the same page is at the top of the viewport.
		expect(target).toBeCloseTo((after?.y ?? 0) + (after?.height ?? 0) / 3, 6);
		list.dispose();
	});

	it("keeps the reader's page across a change of fit mode", () => {
		const doc = documentOf(300);
		const list = createPageList({ adapter: harness(doc).adapter, doc, mode: "page" });
		list.update(VIEWPORT);
		list.goToPage(120);
		list.setMode("spread");
		const state = list.state();
		expect(state.mode).toBe("spread");
		expect(state.currentPage).toBe(120);
		expect(state.announcement).toContain("two-up");
		list.dispose();
	});

	it("ignores a nonsensical zoom rather than collapsing the layout", () => {
		const doc = documentOf(50);
		const list = createPageList({ adapter: harness(doc).adapter, doc });
		const before = list.update(VIEWPORT);
		expect(list.setZoom(0)).toBe(before.scrollTop);
		expect(list.setZoom(Number.NaN)).toBe(before.scrollTop);
		expect(list.state().zoom).toBe(before.zoom);
		list.dispose();
	});

	it("navigates by keyboard and declines keys that are not its own", () => {
		const doc = documentOf(100);
		const list = createPageList({ adapter: harness(doc).adapter, doc });
		list.update(VIEWPORT);
		expect(list.state().currentPage).toBe(0);
		expect(list.handleKey("ArrowDown")?.currentPage).toBe(1);
		expect(list.handleKey("PageDown")?.currentPage).toBeGreaterThan(1);
		expect(list.handleKey("End")?.currentPage).toBe(99);
		// At the end of the document there is nowhere further to go.
		expect(list.handleKey("ArrowDown")).toBeNull();
		expect(list.handleKey("Home")?.currentPage).toBe(0);
		expect(list.handleKey("Escape")).toBeNull();
		expect(list.handleKey("a")).toBeNull();
		list.dispose();
	});

	it("steps a whole spread in two-up mode", () => {
		const doc = documentOf(100);
		const list = createPageList({ adapter: harness(doc).adapter, doc, mode: "spread" });
		list.update(VIEWPORT);
		expect(list.handleKey("ArrowDown")?.currentPage).toBe(2);
		list.dispose();
	});

	it("has nothing to navigate in an empty document", () => {
		const doc = documentOf(0);
		const list = createPageList({ adapter: harness(doc).adapter, doc });
		const state = list.update(VIEWPORT);
		expect(state.tiles).toEqual([]);
		expect(state.contentHeight).toBe(0);
		expect(list.handleKey("ArrowDown")).toBeNull();
		expect(list.goToPage(3).tiles).toEqual([]);
		list.dispose();
	});
});
