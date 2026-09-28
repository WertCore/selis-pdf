/**
 * Search controller tests (SL-4.UI.05).
 *
 * The DoD is one line - *"incremental search with match count, highlight-all,
 * next/previous, and progressive results as pages load"* - and each of its four
 * clauses is asserted here as something a Node test can honestly assert. The
 * repo ships no jsdom (ADR-P0021), so there is no DOM to assert against; what
 * the viewer is responsible for is a set of numbers and a key map, and that is
 * what these check. A browser pass is still owed for the painted result, and
 * the viewer README says so rather than implying otherwise.
 *
 * Two of the tests are the ones that would be easiest to write and hardest to
 * notice missing:
 *
 * - *"a superseded search cannot repaint"* is run against a **hostile** engine
 *   that ignores its `AbortSignal` entirely and keeps yielding. An engine that
 *   honours cancellation proves nothing here, because the interesting case is
 *   the transport that does not.
 * - *"highlight geometry is not coupled to the render ladder"* changes the
 *   layout scale the way a zoom does and asserts the rectangles scale with it,
 *   with no tile, no rung and no device pixel ratio anywhere in the input.
 *
 * The engine is a hand-rolled fake rather than `createMockAdapter` because
 * these tests need to control exactly when a batch arrives - including never,
 * and including after a supersession. The mock's own `search` is exercised by
 * `contract.test.ts`; what is under test here is the controller.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { generateTokensCss } from "@selis/ui-kit";
import { describe, expect, it } from "vitest";
import type { PlatformAdapter } from "../platform/adapter.js";
import { AdapterError } from "../platform/errors.js";
import type {
	AdapterRequestOptions,
	DocHandle,
	PageTextLayer,
	Rect,
	SearchBatch,
	SearchOptions,
	TextLayerChar,
	TextLayerLine,
} from "../platform/types.js";
import { resolveSearchKey } from "./keyboard.js";
import type { PlacedPage } from "./layout.js";
import type { SearchState } from "./search.js";
import { caretAtTextIndex, createSearch } from "./search.js";
import { buildTextLayer } from "./text-layer.js";

const LETTER = { width: 612, height: 792 };

const here = dirname(fileURLToPath(import.meta.url));

/** US Letter placed at one CSS pixel per PDF point. */
const PLACED_1: PlacedPage = {
	page: 0,
	row: 0,
	column: 0,
	x: 0,
	y: 0,
	width: LETTER.width,
	height: LETTER.height,
	scale: 1,
};

function documentOf(pageCount: number): DocHandle {
	return {
		id: `doc-${pageCount}`,
		pageCount,
		pageSizes: Array.from({ length: pageCount }, () => LETTER),
	};
}

/** One inked glyph, 10pt wide and 12pt tall, ascending 10pt per character. */
function line(text: string, startX = 0, baselineY = 700): TextLayerLine {
	const chars: TextLayerChar[] = [];
	let pen = startX;
	for (const unit of text) {
		const rect: Rect = { x: pen, y: baselineY, width: 10, height: 12 };
		chars.push({ rect, advance: 10, inked: !/\s/.test(unit) });
		for (let extra = 1; extra < unit.length; extra += 1) {
			// A surrogate half shares the first's quad and takes no advance.
			chars.push({ rect, advance: 0, inked: false });
		}
		pen += 10;
	}
	return {
		text,
		rect: { x: startX, y: baselineY, width: Math.max(0, pen - startX), height: 12 },
		direction: "ltr",
		chars,
	};
}

/**
 * A page's text layer, and the placement the controller will project it into.
 *
 * Lines descend the page by one line height each, the way recovered text
 * arrives, so a test can tell one line's box from another's. A layer whose
 * lines all shared a baseline would hide a y-flip mistake rather than expose
 * one.
 */
function layerOf(page: number, lines: readonly string[]): PageTextLayer {
	const built = lines.map((text, index) => line(text, 0, 700 - index * 12));
	return {
		page,
		width: LETTER.width,
		height: LETTER.height,
		lines: built,
		text: built.map((entry) => entry.text).join("\n"),
		lowConfidence: false,
	};
}

/**
 * An async iterable the test drives by hand: values the test pushes, in the
 * test's order, whenever the test says so. This is what "progressive" means
 * for a test - the engine never decides when a batch lands.
 */
function createQueue<T>(): {
	push(value: T): void;
	fail(error: unknown): void;
	close(): void;
	[Symbol.asyncIterator](): AsyncIterator<T>;
} {
	const values: T[] = [];
	const waiters: (() => void)[] = [];
	let failure: { error: unknown } | null = null;
	let closed = false;
	const wake = () => {
		while (waiters.length > 0) {
			(waiters.shift() as () => void)();
		}
	};
	return {
		push(value) {
			values.push(value);
			wake();
		},
		fail(error) {
			failure = { error };
			closed = true;
			wake();
		},
		close() {
			closed = true;
			wake();
		},
		async *[Symbol.asyncIterator]() {
			for (;;) {
				while (values.length > 0) {
					yield values.shift() as T;
				}
				if (failure !== null) {
					throw (failure as { error: unknown }).error;
				}
				if (closed) {
					return;
				}
				await new Promise<void>((resolve) => {
					waiters.push(resolve);
				});
			}
		},
	};
}

/** One search the controller started, and the test's handle on it. */
interface PendingScan {
	readonly query: string;
	readonly options: SearchOptions | undefined;
	/** Whether the controller aborted this scan. */
	aborted(): boolean;
	/** Deliver one batch. */
	push(batch: SearchBatch): void;
	/** Fail the scan the way a cancelled or broken transport would. */
	fail(error: unknown): void;
	/** End the scan without a `done` batch. */
	close(): void;
}

interface Harness {
	readonly adapter: PlatformAdapter;
	/** Scans started, in order. */
	readonly scans: PendingScan[];
	/** Pages whose text layer the controller asked for, in order. */
	readonly layerRequests: number[];
	readonly errors: AdapterError[];
	/** Drain the microtask queue so pending promises settle. */
	settle(): Promise<void>;
}

/**
 * An engine whose search is driven entirely by the test.
 *
 * `ignoreAbort` is the point of the whole harness: a transport that keeps
 * yielding after cancellation is what the run id has to survive, and it is the
 * case "we call abort()" would never have covered.
 */
function harness(options: {
	readonly doc: DocHandle;
	readonly layers?: Record<number, PageTextLayer>;
	readonly ignoreAbort?: boolean;
	readonly layerError?: AdapterError;
}): Harness {
	const scans: PendingScan[] = [];
	const layerRequests: number[] = [];
	const errors: AdapterError[] = [];

	const engine = {
		open: async () => options.doc,
		close: async () => undefined,
		renderTile: async () => {
			throw new Error("not used by the search tests");
		},
		extractText: async (_doc: DocHandle, page: number) => ({ page, text: "" }),
		textLayer: async (_doc: DocHandle, page: number, request?: AdapterRequestOptions) => {
			layerRequests.push(page);
			await Promise.resolve();
			if (request?.signal?.aborted === true) {
				throw AdapterError.cancelled();
			}
			if (options.layerError !== undefined) {
				throw options.layerError;
			}
			return options.layers?.[page] ?? layerOf(page, [""]);
		},
		search: (
			_doc: DocHandle,
			query: string,
			searchOptions?: SearchOptions,
			request?: AdapterRequestOptions,
		): AsyncIterable<SearchBatch> => {
			const queue = createQueue<SearchBatch>();
			scans.push({
				query,
				options: searchOptions,
				aborted: () => request?.signal?.aborted === true,
				push: (batch) => queue.push(batch),
				fail: (error) => queue.fail(error),
				close: () => queue.close(),
			});
			return queue;
		},
	};

	return {
		adapter: { capabilities: {}, engine } as unknown as PlatformAdapter,
		scans,
		layerRequests,
		errors,
		async settle() {
			for (let round = 0; round < 16; round += 1) {
				await Promise.resolve();
			}
		},
	};
}

/** A controller wired to a harness, with the states it published recorded. */
function controllerFor(
	h: Harness,
	doc: DocHandle,
	overrides: Partial<Parameters<typeof createSearch>[0]> = {},
): { published: SearchState[]; search: ReturnType<typeof createSearch> } {
	const published: SearchState[] = [];
	const search = createSearch({
		adapter: h.adapter,
		doc,
		placePage: () => PLACED_1,
		onState: (state) => published.push(state),
		onError: (error) => h.errors.push(error),
		...overrides,
	});
	return { published, search };
}

/** A page's text layer, and the placement the controller will project it into. */
function placedAt(scale: number, page = 0): PlacedPage {
	return { ...PLACED_1, page, width: LETTER.width * scale, height: LETTER.height * scale, scale };
}

describe("incremental search (SL-4.UI.05)", () => {
	it("counts matches as batches arrive, and finishes on the done batch", async () => {
		const doc = documentOf(10);
		const h = harness({ doc, layers: { 0: layerOf(0, ["alpha beta"]) } });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });

		search.setQuery("alpha");
		expect(search.state().status).toBe("searching");
		expect(search.state().matchCount).toBe(0);

		const scan = h.scans[0] as PendingScan;
		scan.push({ matches: [{ page: 0, start: 0, end: 5 }], progress: 0.1, done: false });
		await h.settle();
		// The count is a running count: it is already true while the scan runs.
		expect(search.state().matchCount).toBe(1);
		expect(search.state().status).toBe("searching");
		expect(search.state().progress).toBeCloseTo(0.1, 6);

		scan.push({
			matches: [
				{ page: 3, start: 2, end: 7 },
				{ page: 3, start: 20, end: 25 },
			],
			progress: 0.5,
			done: false,
		});
		await h.settle();
		expect(search.state().matchCount).toBe(3);

		scan.push({ matches: [], progress: 1, done: true });
		await h.settle();
		expect(search.state().status).toBe("complete");
		expect(search.state().progress).toBe(1);
		expect(search.state().matchCount).toBe(3);
		expect(search.state().announcement).toBe("3 matches found");
	});

	it("never says no matches while the scan is still running", async () => {
		// The bug this pins: a live region that reads "No matches" at the first
		// batch of a 2 000-page document, and then has to walk it back.
		const doc = documentOf(2000);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("needle");
		(h.scans[0] as PendingScan).push({ matches: [], progress: 0.0005, done: false });
		await h.settle();
		expect(search.state().announcement).not.toBe("No matches");
		expect(search.state().announcement).toBe("Searching, 0 so far");
		expect(search.state().status).toBe("searching");
	});

	it("counts no match as none only once the scan is done", async () => {
		const doc = documentOf(3);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("absent");
		const scan = h.scans[0] as PendingScan;
		scan.push({ matches: [], progress: 0.33, done: false });
		await h.settle();
		expect(search.state().announcement).toBe("Searching, 0 so far");
		scan.push({ matches: [], progress: 1, done: true });
		await h.settle();
		expect(search.state().announcement).toBe("No matches");
		expect(search.state().hasMatches).toBe(false);
	});

	it("does not restart a scan when the same query is set again", async () => {
		const doc = documentOf(3);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("alpha");
		await h.settle();
		search.setQuery("alpha");
		search.setQuery("alpha");
		await h.settle();
		// One keystroke, one scan. A shell reporting every input event would
		// otherwise restart a document-wide scan on every character.
		expect(h.scans).toHaveLength(1);
	});

	it("re-runs when a modifier changes, and passes it to the engine", async () => {
		const doc = documentOf(3);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("alpha");
		await h.settle();
		search.setModifiers({ caseSensitive: true });
		await h.settle();
		expect(h.scans).toHaveLength(2);
		expect(h.scans[1]?.options?.caseSensitive).toBe(true);
		// Re-setting the same modifiers is a no-op, not a third scan.
		search.setModifiers({ caseSensitive: true });
		await h.settle();
		expect(h.scans).toHaveLength(2);
		expect(search.state().modifiers).toEqual({ caseSensitive: true, wholeWord: false });
	});

	it("reports a failed scan through onError and says so", async () => {
		const doc = documentOf(3);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("alpha");
		(h.scans[0] as PendingScan).fail(AdapterError.badHandle("engine gone"));
		await h.settle();
		expect(search.state().status).toBe("failed");
		expect(search.state().announcement).toBe("Search could not be completed");
		expect(h.errors).toHaveLength(1);
	});

	it("treats a transport that just stops as finished, not failed", async () => {
		// `done: true` is the contract, but a transport that ends its iterable
		// without it must not leave the reader looking at a progress bar that
		// will never move again.
		const doc = documentOf(3);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("alpha");
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 1, start: 0, end: 1 }],
			progress: 0.5,
			done: false,
		});
		await h.settle();
		(h.scans[0] as PendingScan).close();
		await h.settle();
		expect(search.state().status).toBe("complete");
		expect(search.state().progress).toBe(1);
		expect(h.errors).toEqual([]);
	});
});

describe("highlight-all", () => {
	it("highlights every match on a visible page, from the text layer's quads", async () => {
		// "alpha beta" at 10pt per glyph, 12pt tall, baseline 700 on US Letter.
		// At one CSS pixel per point the highlight over "beta" is therefore
		// x 60..70, y 80..92 - arithmetic done here, not read back from the
		// code under test.
		const doc = documentOf(4);
		const h = harness({ doc, layers: { 0: layerOf(0, ["alpha beta"]) } });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("a");
		const scan = h.scans[0] as PendingScan;
		scan.push({
			matches: [
				{ page: 0, start: 0, end: 1 },
				{ page: 0, start: 6, end: 7 },
			],
			progress: 0.25,
			done: false,
		});
		scan.push({ matches: [], progress: 1, done: true });
		await h.settle();

		const state = search.state();
		expect(state.highlights).toHaveLength(2);
		expect(state.highlights.map((entry) => entry.rects)).toEqual([
			[{ x: 0, y: 80, width: 10, height: 12 }],
			[{ x: 60, y: 80, width: 10, height: 12 }],
		]);
		expect(state.pageCounts).toEqual([{ page: 0, count: 2 }]);
		// Every highlight names a class the stylesheet actually styles, and the
		// current one is distinguishable without relying on colour alone.
		for (const entry of state.highlights) {
			expect(entry.className).toContain("selis-search-highlight");
		}
	});

	it("spans the lines a match crosses, one box per line", async () => {
		const doc = documentOf(2);
		const h = harness({ doc, layers: { 0: layerOf(0, ["one two", "three"]) } });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("two");
		// 3..10 is "two" plus the newline and "thr": a match that starts on one
		// line and ends on the next.
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 3, end: 10 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		const rects = search.state().highlights[0]?.rects ?? [];
		expect(rects).toHaveLength(2);
		expect(rects[0]).toEqual({ x: 30, y: 80, width: 40, height: 12 });
		// Line 1 holds "three", and the match ends two characters into it. It
		// is one line height lower on the page, which is 12 CSS pixels lower on
		// screen: the y-flip belongs to the projection, not to this file.
		expect(rects[1]).toEqual({ x: 0, y: 92, width: 20, height: 12 });
	});

	it("clamps a match that runs past its line instead of spanning the gap", async () => {
		const doc = documentOf(2);
		const h = harness({ doc, layers: { 0: layerOf(0, ["abc", "def"]) } });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("\n");
		// 2..4 is the newline and the first character of line 1; the highlight
		// must stop at the end of line 0 rather than starting at line 1.
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 2, end: 4 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(search.state().highlights[0]?.rects).toEqual([{ x: 20, y: 80, width: 10, height: 12 }]);
	});

	it("keeps a match counted and navigable when its range cannot be placed", async () => {
		// The engine normalises text before searching (ligatures, soft hyphens,
		// NFD), so a range can come back that does not index the page's own
		// string. The honest degradation is a counted match with no rectangle,
		// not a wrong rectangle and not a missing count.
		const doc = documentOf(2);
		const h = harness({ doc, layers: { 0: layerOf(0, ["abc"]) } });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("abc");
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 40, end: 43 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(search.state().matchCount).toBe(1);
		expect(search.state().highlights[0]?.rects).toEqual([]);
		expect(search.next().currentIndex).toBe(0);
	});

	it("ignores SearchMatch.rects, and projects from the text layer instead", async () => {
		// The engine's own `SearchMatch` carries the *line's* rect, so a
		// transport that forwarded it verbatim would highlight the whole line.
		// The controller must not read it, and this asserts that it does not.
		const doc = documentOf(2);
		const h = harness({ doc, layers: { 0: layerOf(0, ["alpha beta"]) } });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("beta");
		(h.scans[0] as PendingScan).push({
			matches: [
				{
					page: 0,
					start: 6,
					end: 10,
					rects: [{ x: 0, y: 700, width: 1000, height: 12 }],
				},
			],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(search.state().highlights[0]?.rects).toEqual([{ x: 60, y: 80, width: 40, height: 12 }]);
	});

	it("asks for a text layer only for pages in the window, and only once each", async () => {
		// A 2 000-page report with a common word matches on most pages. Fetching
		// a layer per matching page would spend the whole scan budget on pages
		// nobody is looking at: the count is document-wide, the geometry is not.
		const doc = documentOf(50);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 10, pages: [10, 11] });
		search.setQuery("a");
		(h.scans[0] as PendingScan).push({
			matches: Array.from({ length: 50 }, (_unused, page) => ({ page, start: 0, end: 1 })),
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(h.layerRequests).toEqual([10, 11]);
		expect(search.state().matchCount).toBe(50);
		expect(search.state().pageCounts).toEqual([
			{ page: 10, count: 1 },
			{ page: 11, count: 1 },
		]);
		search.update({ currentPage: 11, pages: [10, 11] });
		await h.settle();
		expect(h.layerRequests).toEqual([10, 11]);
	});

	it("keeps a page's matches when its text layer cannot be read", async () => {
		const doc = documentOf(2);
		const h = harness({ doc, layerError: AdapterError.badHandle("layer gone") });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("alpha");
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 0, end: 5 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(search.state().matchCount).toBe(1);
		expect(search.state().highlights).toEqual([]);
		expect(h.errors).toHaveLength(1);
	});
});

describe("highlight geometry across a zoom", () => {
	it("scales with the layout and never with a tile rung or a device ratio", async () => {
		// The one coupling this file refuses. During a zoom the compositor is
		// presenting a *previous* scale's bitmap (`DrawOp.provisional`), so any
		// highlight derived from what is painted would be wrong exactly during
		// the zoom it has to survive. Here the only thing that changes is
		// `PlacedPage.scale` - no tile, no rung, no device pixel ratio - and the
		// rectangles scale with it, exactly, from the engine's points.
		const doc = documentOf(2);
		const h = harness({ doc, layers: { 0: layerOf(0, ["alpha beta"]) } });
		let scale = 1;
		const { search } = controllerFor(h, doc, { placePage: () => placedAt(scale) });
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("beta");
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 6, end: 10 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(search.state().highlights[0]?.rects).toEqual([{ x: 60, y: 80, width: 40, height: 12 }]);

		// Zoom to 250%: the same four characters, four times as far apart.
		scale = 2.5;
		search.update({ currentPage: 0, pages: [0] });
		const zoomed = search.state().highlights[0]?.rects ?? [];
		expect(zoomed).toEqual([{ x: 150, y: 200, width: 100, height: 30 }]);
		// The engine layer was fetched once, at the start, and re-projected on
		// the zoom. Nothing was thrown away and nothing was re-requested.
		expect(h.layerRequests).toEqual([0]);
	});

	it("keeps the current match current across a zoom", async () => {
		const doc = documentOf(2);
		const h = harness({ doc, layers: { 0: layerOf(0, ["alpha beta"]) } });
		let scale = 1;
		const { search } = controllerFor(h, doc, { placePage: () => placedAt(scale) });
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("beta");
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 6, end: 10 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		search.next();
		scale = 3;
		search.update({ currentPage: 0, pages: [0] });
		const current = search.state().highlights.filter((entry) => entry.current);
		expect(current).toHaveLength(1);
		expect(current[0]?.className).toContain("selis-search-highlight--current");
		expect(current[0]?.rects).toEqual([{ x: 180, y: 240, width: 120, height: 36 }]);
	});
});

describe("next and previous", () => {
	async function threeMatches() {
		// Eight pages so that "past the last match" is a page the reader can
		// actually be on: matches stop at page 5.
		const doc = documentOf(8);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0, 1, 2, 3, 4, 5, 6, 7] });
		search.setQuery("x");
		(h.scans[0] as PendingScan).push({
			matches: [
				{ page: 1, start: 0, end: 1 },
				{ page: 3, start: 0, end: 1 },
				{ page: 5, start: 0, end: 1 },
			],
			progress: 1,
			done: true,
		});
		await h.settle();
		return { h, search };
	}

	it("steps forward through the matches and wraps at the end", async () => {
		const { search } = await threeMatches();
		expect(search.state().currentIndex).toBe(-1);
		expect(search.next().currentIndex).toBe(0);
		expect(search.next().currentIndex).toBe(1);
		expect(search.next().currentIndex).toBe(2);
		// Past the last match, a reader expects the first again, not a dead key.
		expect(search.next().currentIndex).toBe(0);
	});

	it("steps backward and wraps at the start", async () => {
		const { search } = await threeMatches();
		search.next();
		expect(search.previous().currentIndex).toBe(2);
		expect(search.previous().currentIndex).toBe(1);
	});

	it("starts from where the reader is, not from the top of the document", async () => {
		// Pressing "next" on page 4 with matches on pages 1, 3 and 5 should
		// land on page 5. Starting at the first match would scroll the reader
		// back through three pages to reach the one they are next to. Each
		// assertion below starts from a *fresh* controller, because the
		// anchoring only applies while nothing is selected: once a match is
		// current, next/previous must step, not re-anchor, or the same key would
		// mean two different things.
		const anchored = async (currentPage: number, move: "next" | "previous") => {
			const { search } = await threeMatches();
			search.update({ currentPage, pages: [currentPage] });
			return search[move]().currentMatch?.page;
		};
		expect(await anchored(4, "next")).toBe(5);
		expect(await anchored(0, "next")).toBe(1);
		expect(await anchored(2, "previous")).toBe(1);
		// On a page that itself has a match, the match on that page wins over
		// wrapping: the reader is already looking at it.
		expect(await anchored(5, "next")).toBe(5);
		// Past the last match, or before the first, it wraps round.
		expect(await anchored(6, "next")).toBe(1);
		expect(await anchored(0, "previous")).toBe(5);
	});

	it("marks exactly one highlight as current", async () => {
		const doc = documentOf(2);
		const h = harness({ doc, layers: { 0: layerOf(0, ["alpha alpha"]) } });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("alpha");
		(h.scans[0] as PendingScan).push({
			matches: [
				{ page: 0, start: 0, end: 5 },
				{ page: 0, start: 6, end: 11 },
			],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(search.state().highlights.filter((entry) => entry.current)).toHaveLength(0);
		search.next();
		const state = search.state();
		expect(state.highlights.filter((entry) => entry.current)).toHaveLength(1);
		expect(state.highlights.find((entry) => entry.current)?.index).toBe(0);
		expect(state.announcement).toBe("Match 1 of 2, Page 1 of 2");
	});

	it("clamps an out-of-range selection instead of throwing", async () => {
		const { search } = await threeMatches();
		expect(search.select(99).currentIndex).toBe(2);
		expect(search.select(-5).currentIndex).toBe(0);
	});

	it("does nothing when there is nothing to step to", async () => {
		const doc = documentOf(2);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("absent");
		(h.scans[0] as PendingScan).push({ matches: [], progress: 1, done: true });
		await h.settle();
		expect(search.next().currentIndex).toBe(-1);
		expect(search.next().currentMatch).toBeNull();
		expect(search.state().announcement).toBe("No matches");
	});
});

describe("cancellation", () => {
	it("cannot repaint, even when the engine ignores the abort signal", async () => {
		// The hostile transport. It is handed an AbortSignal and keeps yielding
		// anyway, which is the only way to find out whether the guard is the
		// abort or something real. It is not the abort: a batch is applied only
		// behind `run === runId`, so a superseded scan's results are discarded
		// however badly the transport behaves.
		const doc = documentOf(6);
		const h = harness({ doc, ignoreAbort: true });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("first");
		await h.settle();
		const stale = h.scans[0] as PendingScan;
		stale.push({ matches: [{ page: 0, start: 0, end: 5 }], progress: 0.5, done: false });
		await h.settle();
		expect(search.state().matchCount).toBe(1);

		// The reader types another character mid-scan.
		search.setQuery("second");
		await h.settle();
		expect(stale.aborted(), "the controller did ask the transport to stop").toBe(true);

		// The stale transport now keeps going, ignoring the abort entirely.
		stale.push({ matches: [{ page: 4, start: 0, end: 6 }], progress: 0.9, done: false });
		stale.push({ matches: [{ page: 5, start: 0, end: 6 }], progress: 1, done: true });
		await h.settle();
		expect(search.state().query).toBe("second");
		expect(search.state().matchCount, "a superseded scan repainted results").toBe(0);
		expect(search.state().highlights).toEqual([]);
		expect(search.state().pageCounts).toEqual([]);
		expect(search.state().status).toBe("searching");

		// The current scan's own results still land.
		const live = h.scans[1] as PendingScan;
		live.push({ matches: [{ page: 2, start: 3, end: 9 }], progress: 1, done: true });
		await h.settle();
		expect(search.state().matchCount).toBe(1);
		expect(search.state().status).toBe("complete");
	});

	it("does not report a superseded scan's cancellation as an error", async () => {
		const doc = documentOf(4);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("first");
		await h.settle();
		search.setQuery("second");
		await h.settle();
		// A transport that honours the signal rejects with CANCELLED. That is
		// not a failure and must not reach `onError`, or every keystroke in a
		// slow document would raise an error toast.
		(h.scans[0] as PendingScan).fail(AdapterError.cancelled());
		await h.settle();
		expect(h.errors).toEqual([]);
		expect(search.state().status).toBe("searching");
	});

	it("clear() aborts the scan, drops the results, and returns to idle", async () => {
		const doc = documentOf(4);
		const h = harness({ doc, layers: { 0: layerOf(0, ["alpha"]) } });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0] });
		search.setQuery("alpha");
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 0, end: 5 }],
			progress: 0.5,
			done: false,
		});
		await h.settle();
		expect(search.state().matchCount).toBe(1);

		const state = search.clear();
		expect(state.status).toBe("idle");
		expect(state.query).toBe("");
		expect(state.matchCount).toBe(0);
		expect(state.highlights).toEqual([]);
		expect(state.announcement).toBe("Type to search");
		expect((h.scans[0] as PendingScan).aborted()).toBe(true);

		// A batch already on its way when the reader closed search cannot
		// resurrect it.
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 0, end: 5 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(search.state().matchCount).toBe(0);
		expect(search.state().status).toBe("idle");
	});

	it("treats an empty query as a clear, never as a rejected search", async () => {
		const doc = documentOf(4);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("alpha");
		await h.settle();
		search.setQuery("");
		await h.settle();
		expect(h.scans).toHaveLength(1);
		expect(h.errors).toEqual([]);
		expect(search.state().status).toBe("idle");
	});

	it("stops publishing after dispose", async () => {
		const doc = documentOf(4);
		const h = harness({ doc });
		const { search, published } = controllerFor(h, doc);
		search.setQuery("alpha");
		await h.settle();
		published.length = 0;
		search.dispose();
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 0, start: 0, end: 5 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(published).toEqual([]);
	});
});

describe("keyboard and announcements", () => {
	it("claims Enter, Shift+Enter, F3, Shift+F3 and Escape - and nothing else", () => {
		// The map is total, and it declines everything it should decline, which
		// is what lets a shell hand it every key without arbitrating.
		expect(resolveSearchKey("Enter", { hasQuery: true, shift: false })?.action).toBe("next");
		expect(resolveSearchKey("Enter", { hasQuery: true, shift: true })?.action).toBe("previous");
		expect(resolveSearchKey("F3", { hasQuery: true, shift: false })?.action).toBe("next");
		expect(resolveSearchKey("F3", { hasQuery: true, shift: true })?.action).toBe("previous");
		expect(resolveSearchKey("Escape", { hasQuery: true, shift: false })?.action).toBe("close");
		expect(resolveSearchKey("Escape", { hasQuery: true, shift: true })?.action).toBe("close");
		// The page list's keys stay the page list's.
		expect(resolveSearchKey("ArrowDown", { hasQuery: true, shift: false })).toBeNull();
		expect(resolveSearchKey("Home", { hasQuery: true, shift: false })).toBeNull();
		// With no query, search claims nothing at all - including Escape, which
		// the shell may need for a dialog.
		for (const key of ["Enter", "F3", "Escape"]) {
			expect(resolveSearchKey(key, { hasQuery: false, shift: false })).toBeNull();
		}
	});

	it("names the key that produced the command, for UI.07's audit trail", () => {
		expect(resolveSearchKey("F3", { hasQuery: true, shift: true })).toEqual({
			action: "previous",
			key: "F3",
		});
	});

	it("drives next, previous and close from the key map", async () => {
		const doc = documentOf(4);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("alpha");
		(h.scans[0] as PendingScan).push({
			matches: [
				{ page: 0, start: 0, end: 5 },
				{ page: 1, start: 0, end: 5 },
			],
			progress: 1,
			done: true,
		});
		await h.settle();
		expect(search.handleKey("Enter")?.currentIndex).toBe(0);
		expect(search.handleKey("Enter")?.currentIndex).toBe(1);
		expect(search.handleKey("Enter", true)?.currentIndex).toBe(0);
		expect(search.handleKey("Escape")?.status).toBe("idle");
		// Closed, so nothing is claimed any more.
		expect(search.handleKey("Enter")).toBeNull();
	});

	it("announces the current match as a sentence, once per move", async () => {
		const doc = documentOf(4);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.setQuery("alpha");
		const scan = h.scans[0] as PendingScan;
		scan.push({ matches: [{ page: 2, start: 0, end: 5 }], progress: 0.5, done: false });
		await h.settle();
		search.next();
		expect(search.state().announcement).toBe("Match 1 of 1, Page 3 of 4");
		// A later batch grows the count but must not re-announce the same match:
		// a polite live region that interrupts itself once per page is unusable.
		scan.push({ matches: [{ page: 3, start: 0, end: 5 }], progress: 1, done: true });
		await h.settle();
		expect(search.state().matchCount).toBe(2);
		expect(search.state().announcement).toBe("Match 1 of 1, Page 3 of 4");
		search.next();
		expect(search.state().announcement).toBe("Match 2 of 2, Page 4 of 4");
	});

	it("words the page with the page list's catalogue, so one page is worded once", async () => {
		const doc = documentOf(4);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc, {
			pageStrings: { "pageList.page.label": "Seite {page} von {total}" },
		});
		search.setQuery("alpha");
		(h.scans[0] as PendingScan).push({
			matches: [{ page: 2, start: 0, end: 5 }],
			progress: 1,
			done: true,
		});
		await h.settle();
		search.next();
		expect(search.state().announcement).toBe("Match 1 of 1, Seite 3 von 4");
	});

	it("words its own strings from the catalogue, never from a literal", async () => {
		const doc = documentOf(4);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc, {
			strings: { "search.status.none": "Nichts gefunden" },
		});
		search.setQuery("alpha");
		expect(search.state().fieldLabel).toBe("Find in document");
		expect(search.state().fieldPlaceholder).toBe("Find");
		expect(search.state().regionLabel).toBe("Find in document");
		expect(search.state().nextLabel).toBe("Next match");
		expect(search.state().previousLabel).toBe("Previous match");
		expect(search.state().closeLabel).toBe("Close search");
		expect(search.state().caseLabel).toBe("Match case");
		expect(search.state().wordLabel).toBe("Match whole word");
		(h.scans[0] as PendingScan).push({ matches: [], progress: 1, done: true });
		await h.settle();
		expect(search.state().announcement).toBe("Nichts gefunden");
	});
});

describe("the offset mapping the whole thing rests on", () => {
	const frame = buildTextLayer(layerOf(0, ["abc", "de"]), PLACED_1);

	it("walks the page text, newline by newline", () => {
		// "abc\nde": 0..3 on line 0, 4..6 on line 1.
		expect(caretAtTextIndex(frame, 0)).toEqual({ line: 0, offset: 0 });
		expect(caretAtTextIndex(frame, 3)).toEqual({ line: 0, offset: 3 });
		expect(caretAtTextIndex(frame, 4)).toEqual({ line: 1, offset: 0 });
		expect(caretAtTextIndex(frame, 6)).toEqual({ line: 1, offset: 2 });
	});

	it("resolves the very end of the page to the last character, not past it", () => {
		// A match running to the end of a page is ordinary - a heading, a
		// footnote - and losing its highlight because the index equals the text
		// length would be a bug that only ever shows on the last line.
		expect(caretAtTextIndex(frame, 6)).toEqual({ line: 1, offset: 2 });
	});

	it("returns null for an index the page does not have", () => {
		expect(caretAtTextIndex(frame, 7)).toBeNull();
		expect(caretAtTextIndex(frame, -1)).toBeNull();
		expect(caretAtTextIndex(frame, 1.5)).toBeNull();
	});
});

describe("per-batch cost", () => {
	it("does not grow with the number of matches found so far", async () => {
		// The claim in the module doc is O(window) per batch, not O(document). A
		// document-wide common word is the case that would break it, so this
		// pushes a five-thousandth-match batch and asserts it still lands inside
		// a bound. A bound, not a benchmark: it is there to catch a regression
		// into document-proportional work, with room for a slow machine.
		const doc = documentOf(5000);
		const h = harness({ doc });
		const { search } = controllerFor(h, doc);
		search.update({ currentPage: 0, pages: [0, 1] });

		const push = async (count: number, done: boolean): Promise<void> => {
			search.setQuery(`q${count}`);
			const scan = h.scans[h.scans.length - 1] as PendingScan;
			scan.push({
				matches: Array.from({ length: count }, (_unused, index) => ({
					page: index % 5000,
					start: 0,
					end: 1,
				})),
				progress: 1,
				done,
			});
			await h.settle();
		};

		await push(1, false);
		const started = process.hrtime.bigint();
		await push(5000, true);
		const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6;
		expect(search.state().matchCount).toBe(5000);
		expect(elapsedMs).toBeLessThan(250);
	});
});

describe("the highlight stylesheet", () => {
	const appCss = readFileSync(join(here, "page-list.css"), "utf8");
	const baseCss = readFileSync(
		join(here, "..", "..", "..", "..", "packages", "ui-kit", "css", "base.css"),
		"utf8",
	);
	const controllerSource = readFileSync(join(here, "search.ts"), "utf8");

	it("styles every class the controller emits", () => {
		// The same discipline `page-list.css.test.ts` applies to the page list:
		// a highlight cannot ship with a modifier the stylesheet forgot, because
		// the current match would then be indistinguishable and the test would
		// not notice.
		const emitted = [...controllerSource.matchAll(/"(selis-search[a-z-]*)"/g)].flatMap((match) =>
			match[1] !== undefined ? [match[1]] : [],
		);
		expect(emitted.length).toBeGreaterThan(1);
		const styled = new Set(
			[...`${appCss}\n${baseCss}`.matchAll(/\.(selis-[a-zA-Z0-9_-]+)/g)].flatMap((match) =>
				match[1] !== undefined ? [match[1]] : [],
			),
		);
		const unstyled = [...new Set(emitted)].filter((name) => !styled.has(name));
		expect(unstyled, `class names with no styles: ${unstyled.join(", ")}`).toEqual([]);
	});

	it("positions highlights absolutely from data carriers, like a placed tile", () => {
		// A highlight is positioned geometry, so it obeys the same rule the
		// placed tile does: the box comes from custom properties the shell sets,
		// never from a hard-coded size, and the carriers stay out of the token
		// namespace because they are document data rather than design tokens.
		const rules = appCss.replace(/\/\*[\s\S]*?\*\//g, "");
		const highlight = /\.selis-search-highlight\s*\{([^}]*)\}/.exec(rules)?.[1] ?? "";
		expect(highlight).not.toBe("");
		expect(highlight).toContain("position: absolute");
		for (const carrier of ["--match-x", "--match-y", "--match-w", "--match-h"]) {
			expect(highlight, carrier).toContain(`var(${carrier}`);
		}
		expect(rules).not.toContain("--selis-match-");
	});

	it("takes no pointer event, so a match is still selectable text", () => {
		// The highlight sits under the text layer. A box that swallowed the
		// click would make every match unselectable, which is a worse bug than
		// an ugly one and is invisible in a screenshot.
		const rules = appCss.replace(/\/\*[\s\S]*?\*\//g, "");
		const highlight = /\.selis-search-highlight\s*\{([^}]*)\}/.exec(rules)?.[1] ?? "";
		expect(highlight).toContain("pointer-events: none");
	});

	it("uses the highlight tokens, and both states are distinguishable", () => {
		// Two different tokens rather than one colour at two opacities: opacity
		// is not available in the high-contrast theme, and the current match
		// would stop being distinguishable exactly where it matters most.
		const rules = appCss.replace(/\/\*[\s\S]*?\*\//g, "");
		const highlight = /\.selis-search-highlight\s*\{([^}]*)\}/.exec(rules)?.[1] ?? "";
		const current = /\.selis-search-highlight--current\s*\{([^}]*)\}/.exec(rules)?.[1] ?? "";
		expect(current).not.toBe("");
		expect(highlight).toContain("var(--selis-color-highlight-bg)");
		expect(current).toContain("var(--selis-color-highlight-active-bg)");
		expect(rules).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
		expect(rules).not.toMatch(/\b(?:rgb|rgba|hsl|hsla)\(/);
	});

	it("references only tokens the generator defines", () => {
		// Checked against the *generated* token set rather than the committed
		// file, so a token that exists only in the source cannot pass.
		const defined = new Set(
			[...generateTokensCss().matchAll(/(--selis-[a-z0-9-]+):/g)].flatMap((match) =>
				match[1] !== undefined ? [match[1]] : [],
			),
		);
		const used = [...appCss.matchAll(/var\(\s*(--selis-[a-z0-9-]+)/g)].flatMap((match) =>
			match[1] !== undefined ? [match[1]] : [],
		);
		const unknown = [...new Set(used)].filter((name) => !defined.has(name));
		expect(unknown, `unknown token references: ${unknown.join(", ")}`).toEqual([]);
	});
});
