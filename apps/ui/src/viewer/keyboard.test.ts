/**
 * Keyboard/AT tests (SL-4.UI.02's share of SL-4.UI.07's accessibility pass).
 * The key map is pure, so it is pinned here; UI.07 extends it rather than
 * re-inventing it.
 */

import { describe, expect, it } from "vitest";
import type { KeyboardContext } from "./keyboard.js";
import { announcement, pageLabel, resolvePageKey, stepFor } from "./keyboard.js";

function context(overrides: Partial<KeyboardContext> = {}): KeyboardContext {
	return {
		currentPage: 10,
		pageCount: 100,
		mode: "page",
		pagesPerView: 2,
		step: stepFor("page"),
		...overrides,
	};
}

describe("key map", () => {
	it("moves one page with the arrow keys", () => {
		expect(resolvePageKey("ArrowDown", context())).toEqual({
			page: 11,
			align: "start",
			key: "ArrowDown",
		});
		expect(resolvePageKey("ArrowUp", context())?.page).toBe(9);
		expect(resolvePageKey("ArrowRight", context())?.page).toBe(11);
		expect(resolvePageKey("ArrowLeft", context())?.page).toBe(9);
	});

	it("steps a whole spread in two-up mode", () => {
		const spread = context({ mode: "spread", step: stepFor("spread") });
		expect(stepFor("spread")).toBe(2);
		expect(resolvePageKey("ArrowDown", spread)?.page).toBe(12);
		expect(resolvePageKey("ArrowUp", spread)?.page).toBe(8);
	});

	it("moves a screen of pages with PageUp/PageDown", () => {
		expect(resolvePageKey("PageDown", context({ pagesPerView: 3 }))?.page).toBe(13);
		expect(resolvePageKey("PageUp", context({ pagesPerView: 3 }))?.page).toBe(7);
		// Never less than a step, however small the viewport claims to be.
		expect(resolvePageKey("PageDown", context({ pagesPerView: 0, step: 2 }))?.page).toBe(12);
	});

	it("jumps to the ends with Home/End", () => {
		expect(resolvePageKey("Home", context())?.page).toBe(0);
		expect(resolvePageKey("End", context())?.page).toBe(99);
	});

	it("declines at the ends instead of wrapping or throwing", () => {
		expect(resolvePageKey("ArrowDown", context({ currentPage: 99 }))).toBeNull();
		expect(resolvePageKey("ArrowUp", context({ currentPage: 0 }))).toBeNull();
		expect(resolvePageKey("PageUp", context({ currentPage: 0 }))).toBeNull();
	});

	it("clamps a current page that is out of range", () => {
		// 500 in a 100-page document reads as the last page, so Up lands on 98.
		expect(resolvePageKey("ArrowUp", context({ currentPage: 500 }))?.page).toBe(98);
	});

	it("ignores keys that are not its own", () => {
		for (const key of ["a", "Enter", "Escape", "Tab", "ArrowDown ", "shift"]) {
			expect(resolvePageKey(key, context()), key).toBeNull();
		}
	});

	it("has nothing to say about an empty document", () => {
		expect(resolvePageKey("ArrowDown", context({ pageCount: 0, currentPage: 0 }))).toBeNull();
		expect(resolvePageKey("End", context({ pageCount: 0, currentPage: 0 }))).toBeNull();
	});
});

describe("labels", () => {
	it("numbers pages from one, for humans", () => {
		expect(pageLabel(0, 12)).toBe("Page 1 of 12");
		expect(pageLabel(11, 12)).toBe("Page 12 of 12");
	});

	it("announces the page and the fit mode in plain words", () => {
		expect(announcement(0, 12, "page")).toBe("Page 1 of 12 — fit page");
		expect(announcement(4, 12, "width")).toBe("Page 5 of 12 — fit width");
		expect(announcement(4, 12, "spread")).toBe("Page 5 of 12 — two-up");
	});
});
