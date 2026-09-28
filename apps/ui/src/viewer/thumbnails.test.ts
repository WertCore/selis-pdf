/**
 * The thumbnail rail (SL-4.UI.06).
 *
 * The claim under test is the one the viewer README makes: **a thumbnail is the
 * page list's ladder at a fixed width**. So the tests here are not "do the
 * thumbnails look right" — nothing is looked at, under ADR-P0021's no-jsdom
 * rule — but three things that can be asserted honestly about a rasterisation
 * path:
 *
 * 1. It asks the engine for *whole pages* at the rail's scale, through
 *    `EnginePort.renderTile` with the `thumbnail` hint, and never for a rect.
 * 2. A preview's box does not move when its bitmap lands — UI.02's no-layout-
 *    shift rule, in a smaller costume.
 * 3. It is bounded: a 2 000-page document's rail does not ask for 2 000 renders,
 *    and the pages it skips are the ones nobody can see.
 *
 * What is owed a manual browser pass: that the rail *looks* like the document.
 */

import { describe, expect, it } from "vitest";
import { createMockAdapter } from "../platform/mock-adapter.js";
import type { RenderTileRequest, Size } from "../platform/types.js";
import {
	MAX_THUMBNAIL_ROWS,
	MAX_THUMBNAIL_WIDTH,
	MIN_THUMBNAIL_WIDTH,
	createThumbnailRail,
	thumbnailClassName,
} from "./thumbnails.js";

const LETTER: Size = { width: 612, height: 792 };

/** A rail 112 px wide with an 8 px gap: the default desktop shape. */
const VIEWPORT = { width: 112, height: 400, scrollTop: 0, devicePixelRatio: 1 };

async function fixture(pageCount = 12, pageSize: Size = LETTER) {
	const adapter = createMockAdapter();
	const requested: RenderTileRequest[] = [];
	// Wrap `renderTile` so the test can see exactly what the rail asked for. The
	// wrapper is a *spy*, not a stub: the real mock still does the work. The
	// original is bound first, because `adapter.engine` is the same object the
	// wrapper replaces — capturing the method and calling that would recurse.
	const original = adapter.engine.renderTile.bind(adapter.engine);
	adapter.engine.renderTile = async (request, options) => {
		requested.push(request);
		return original(request, options);
	};
	const descriptor = adapter.addDocument({ name: "thumb.pdf", pageCount, pageSize });
	const doc = await adapter.engine.open(descriptor);
	const rail = createThumbnailRail({ adapter, doc, width: VIEWPORT.width, gap: 8 });
	return { adapter, doc, rail, requested };
}

/** Let the scheduler's microtask-driven renders land. */
async function settle(): Promise<void> {
	for (let tick = 0; tick < 8; tick += 1) {
		await Promise.resolve();
	}
}

/** The rail's own scale: 112 − 8 usable px over a 612 pt page. */
const RAIL_SCALE = 104 / 612;

describe("the thumbnail rail", () => {
	it("publishes an empty state before any metrics are reported", async () => {
		const { rail } = await fixture();
		const state = rail.state();
		expect(state.pageCount).toBe(12);
		// No rows, because a rail with no height has nothing in it — but the
		// scroller's content height is known from the document alone, exactly as
		// the page list publishes its own before the first report.
		expect(state.thumbnails).toEqual([]);
		expect(state.contentHeight).toBeGreaterThan(0);
		rail.dispose();
	});

	it("asks the engine for whole pages at the rail's scale, with the thumbnail hint", async () => {
		const { rail, requested } = await fixture();
		rail.update(VIEWPORT);
		await settle();
		expect(requested.length).toBeGreaterThan(0);
		for (const request of requested) {
			// No rect: a thumbnail is the whole page, drawn small.
			expect(request.rect).toBeUndefined();
			expect(request.hint).toBe("thumbnail");
			expect(request.scale).toBeCloseTo(RAIL_SCALE, 5);
		}
		rail.dispose();
	});

	it("keeps a preview's box identical at every rung of the ladder", async () => {
		const { rail } = await fixture();
		const before = rail.update(VIEWPORT);
		const placeholder = before.thumbnails[0];
		expect(placeholder?.stage).toBe("placeholder");
		await settle();
		const after = rail.update(VIEWPORT).thumbnails[0];
		// Same box, whatever the bitmap did. This is the no-shift rule.
		expect(after?.y).toBe(placeholder?.y);
		expect(after?.width).toBe(placeholder?.width);
		expect(after?.height).toBe(placeholder?.height);
		expect(after?.stage).not.toBe("placeholder");
		rail.dispose();
	});

	it("windows the rail: a long document renders a strip, not the whole file", async () => {
		const { rail, requested } = await fixture(2_000);
		rail.update(VIEWPORT);
		await settle();
		const pages = new Set(requested.map((request) => request.page));
		// One visible thumbnail per ~104×135 px plus the overscan band, capped.
		expect(pages.size).toBeLessThanOrEqual(MAX_THUMBNAIL_ROWS);
		expect(pages.has(0)).toBe(true);
		// Page 1 999 is nowhere near the top of a 2 000-page rail.
		expect(pages.has(1_999)).toBe(false);
		rail.dispose();
	});

	it("renders the pages the reader scrolled to", async () => {
		const { rail, requested } = await fixture(2_000);
		rail.update({ ...VIEWPORT, scrollTop: 400_000 });
		await settle();
		expect(Math.max(...requested.map((request) => request.page))).toBeGreaterThan(100);
		rail.dispose();
	});

	it("reaches a fixed point: a settled rail asks for nothing more", async () => {
		const { rail, requested } = await fixture();
		rail.update(VIEWPORT);
		await settle();
		// The ladder climbs over several frames — full rung, then the low-res
		// prefetch rung — and a render already in flight when a frame is submitted
		// can legitimately be asked for again. What must not happen is unbounded
		// asking: run the rail to a fixed point and check it stays there.
		for (let frame = 0; frame < 6; frame += 1) {
			rail.update(VIEWPORT);
			await settle();
		}
		const settled = requested.length;
		for (let frame = 0; frame < 3; frame += 1) {
			rail.update(VIEWPORT);
			await settle();
		}
		expect(requested.length).toBe(settled);
		// And it is bounded: five visible previews plus their overscan, at two
		// rungs each, on a twelve-page document.
		expect(settled).toBeLessThanOrEqual(2 * (MAX_THUMBNAIL_ROWS + 4));
		rail.dispose();
	});

	it("settles: repeated updates stop asking for more", async () => {
		const { rail, requested } = await fixture();
		rail.update(VIEWPORT);
		await settle();
		for (let frame = 0; frame < 4; frame += 1) {
			rail.update(VIEWPORT);
			await settle();
		}
		const settled = requested.length;
		rail.update(VIEWPORT);
		await settle();
		expect(requested.length).toBe(settled);
		rail.dispose();
	});

	describe("labels, focus and geometry", () => {
		it("labels thumbnails with the document's own page labels", async () => {
			const { rail, doc } = await fixture(4);
			rail.setPageLabels([{ firstPage: 0, style: "r" }]);
			const state = rail.update(VIEWPORT);
			expect(state.thumbnails[0]?.label).toBe("i");
			expect(state.thumbnails[3]?.label).toBe("iv");
			expect(doc.pageCount).toBe(4);
			rail.dispose();
		});

		it("keeps the current page in the tab order and no other", async () => {
			const { rail } = await fixture();
			rail.setCurrentPage(2);
			const state = rail.update(VIEWPORT);
			const current = state.thumbnails.filter((thumb) => thumb.current);
			expect(current).toHaveLength(1);
			expect(current[0]?.tabIndex).toBe(0);
			for (const thumb of state.thumbnails) {
				if (!thumb.current) {
					expect(thumb.tabIndex).toBe(-1);
				}
			}
			rail.dispose();
		});

		it("clamps a current page outside the document", async () => {
			const { rail } = await fixture(3);
			expect(rail.setCurrentPage(99).currentPage).toBe(2);
			expect(rail.setCurrentPage(-4).currentPage).toBe(0);
			expect(rail.setCurrentPage(Number.NaN).currentPage).toBe(0);
			rail.dispose();
		});

		it("gives every thumbnail a class the stylesheet can style", async () => {
			const { rail } = await fixture();
			const state = rail.update(VIEWPORT);
			for (const thumb of state.thumbnails) {
				expect(thumb.className).toContain("selis-thumbnail");
				expect(thumb.className).not.toContain("undefined");
			}
			rail.dispose();
		});

		it("sizes a landscape page's preview by its own aspect", async () => {
			const { rail } = await fixture(2, { width: 792, height: 612 });
			const state = rail.update(VIEWPORT);
			const first = state.thumbnails[0];
			const second = state.thumbnails[1];
			// The box is wider than tall for a landscape page, and the second page
			// starts below the first by exactly that height plus the gap.
			expect(first?.height).toBeLessThan(first?.width ?? 0);
			expect(second?.y).toBeCloseTo((first?.y ?? 0) + (first?.height ?? 0) + 8, 6);
			rail.dispose();
		});

		it("bounds the rail's width, because a preview is not a second viewport", async () => {
			const { rail, requested } = await fixture(2);
			rail.update({ ...VIEWPORT, width: 4_000 });
			await settle();
			// Clamped to the maximum, so the scale cannot become a document view.
			expect(requested[0]?.scale).toBeCloseTo((MAX_THUMBNAIL_WIDTH - 8) / LETTER.width, 5);
			expect(MIN_THUMBNAIL_WIDTH).toBeGreaterThan(0);
			rail.dispose();
		});

		it("asks for one scale when every page is the same size", async () => {
			// A landscape and a portrait page share the rail's width but cannot share
			// a scale, which is why `planLadder` is called per page; a uniform
			// document must still produce one scale, or the rail is inventing
			// variation it does not have.
			const { rail, requested } = await fixture(4);
			rail.update(VIEWPORT);
			await settle();
			expect(new Set(requested.map((request) => request.scale)).size).toBe(1);
			rail.dispose();
		});
	});

	describe("subscriptions and disposal", () => {
		it("publishes through subscribe, including when a tile lands", async () => {
			const { rail } = await fixture();
			const seen: number[] = [];
			const unsubscribe = rail.subscribe((state) => seen.push(state.thumbnails.length));
			rail.update(VIEWPORT);
			await settle();
			rail.update(VIEWPORT);
			unsubscribe();
			const after = seen.length;
			rail.update({ ...VIEWPORT, scrollTop: 5_000 });
			expect(seen.length).toBe(after);
			rail.dispose();
		});

		it("hands every subscriber the same state object", async () => {
			const { rail } = await fixture();
			const first: unknown[] = [];
			const second: unknown[] = [];
			rail.subscribe((state) => first.push(state));
			rail.subscribe((state) => second.push(state));
			rail.update(VIEWPORT);
			expect(first[0]).toBe(second[0]);
			rail.dispose();
		});

		it("exposes the cached tile for a page, for a shell painting it directly", async () => {
			const { rail } = await fixture();
			rail.update(VIEWPORT);
			await settle();
			expect(rail.tileAt(0)).not.toBeNull();
			expect(rail.tileAt(11)).toBeNull();
			rail.dispose();
		});

		it("stops asking for renders after dispose", async () => {
			const { rail, requested } = await fixture();
			rail.update(VIEWPORT);
			await settle();
			const before = requested.length;
			rail.dispose();
			rail.update({ ...VIEWPORT, scrollTop: 100_000 });
			await settle();
			expect(requested.length).toBe(before);
		});

		it("reports a render failure through onError and does not throw", async () => {
			const adapter = createMockAdapter();
			const errors: unknown[] = [];
			// A hostile transport: every render fails. The rail's contract is to
			// report it through `onError` — the same seam the page list's ladder
			// reports through — and to keep publishing placeholders rather than
			// throwing at the shell.
			adapter.engine.renderTile = async () => {
				throw new Error("engine unavailable");
			};
			const descriptor = adapter.addDocument({ name: "boom.pdf", pageCount: 2 });
			const doc = await adapter.engine.open(descriptor);
			const rail = createThumbnailRail({
				adapter,
				doc,
				width: VIEWPORT.width,
				onError: (error) => errors.push(error),
			});
			expect(() => rail.update(VIEWPORT)).not.toThrow();
			await settle();
			expect(errors.length).toBeGreaterThan(0);
			// And the rail still publishes: a failed preview is a placeholder, not a
			// missing thumbnail.
			expect(rail.state().thumbnails.length).toBeGreaterThan(0);
			rail.dispose();
		});
	});

	describe("thumbnailClassName", () => {
		it("names the rung and the current state", () => {
			expect(thumbnailClassName("placeholder", false)).toBe(
				"selis-thumbnail selis-thumbnail--placeholder selis-thumbnail--idle",
			);
			expect(thumbnailClassName("full", true)).toBe(
				"selis-thumbnail selis-thumbnail--full selis-thumbnail--current",
			);
		});
	});
});
