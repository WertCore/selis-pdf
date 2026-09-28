/**
 * Keyboard/AT tests (SL-4.UI.02's share of SL-4.UI.07's accessibility pass).
 * The key maps are pure, so they are pinned here; UI.06 and UI.07 extend this
 * file rather than re-inventing it.
 *
 * The three maps and the argument for keeping them apart:
 * - `resolvePageKey` (UI.02) — the document's arrows, home and end.
 * - `resolveSearchKey` (UI.05) — Enter, Escape, F3, and only with a query.
 * - `resolveNavigationKey` (UI.06) — the outline tree's arrows, home and end,
 *   and **only while the panel has focus**.
 *
 * The first two claim disjoint keys, so both can be consulted unconditionally.
 * The third claims the *same key names* as the first, which is why it is
 * focus-scoped: without focus it returns `null` for everything, and that is the
 * assertion below.
 */

import { describe, expect, it } from "vitest";
import type { KeyboardContext, NavigationKeyContext } from "./keyboard.js";
import {
	announcement,
	pageLabel,
	resolveNavigationKey,
	resolvePageKey,
	resolveSearchKey,
	stepFor,
} from "./keyboard.js";

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

function navContext(overrides: Partial<NavigationKeyContext> = {}): NavigationKeyContext {
	return {
		hasFocus: true,
		hasRows: true,
		focusedHasChildren: false,
		focusedExpanded: false,
		...overrides,
	};
}

describe("the navigation key map (SL-4.UI.06)", () => {
	it("claims nothing at all without focus — the page list keeps its arrows", () => {
		// The whole overlap argument in one assertion: the outline and the page
		// list claim the same key *names*, and focus is what separates them.
		for (const key of [
			"ArrowDown",
			"ArrowUp",
			"ArrowLeft",
			"ArrowRight",
			"Home",
			"End",
			"Enter",
			" ",
		]) {
			expect(resolveNavigationKey(key, navContext({ hasFocus: false })), key).toBeNull();
		}
		// And with no focus the document area still moves, which is the point.
		expect(resolvePageKey("ArrowDown", context())?.page).toBe(11);
	});

	it("claims nothing when the panel is empty", () => {
		for (const key of ["ArrowDown", "End", "Enter"]) {
			expect(resolveNavigationKey(key, navContext({ hasRows: false })), key).toBeNull();
		}
	});

	it("moves between rows with the arrows and the ends", () => {
		expect(resolveNavigationKey("ArrowDown", navContext())).toEqual({
			action: "next",
			key: "ArrowDown",
		});
		expect(resolveNavigationKey("ArrowUp", navContext())?.action).toBe("previous");
		expect(resolveNavigationKey("Home", navContext())?.action).toBe("first");
		expect(resolveNavigationKey("End", navContext())?.action).toBe("last");
	});

	it("opens a closed parent with ArrowRight and steps into an open one", () => {
		const closed = navContext({ focusedHasChildren: true, focusedExpanded: false });
		expect(resolveNavigationKey("ArrowRight", closed)?.action).toBe("expand");
		const open = navContext({ focusedHasChildren: true, focusedExpanded: true });
		expect(resolveNavigationKey("ArrowRight", open)?.action).toBe("open-child");
	});

	it("collapses an open parent with ArrowLeft and steps out of a leaf", () => {
		const open = navContext({ focusedHasChildren: true, focusedExpanded: true });
		expect(resolveNavigationKey("ArrowLeft", open)?.action).toBe("collapse");
		const closed = navContext({ focusedHasChildren: true, focusedExpanded: false });
		expect(resolveNavigationKey("ArrowLeft", closed)?.action).toBe("open-parent");
		// A leaf: stepping out is the only meaning ArrowLeft has.
		expect(resolveNavigationKey("ArrowLeft", navContext())?.action).toBe("open-parent");
		// And a leaf has nowhere to step *into*.
		expect(resolveNavigationKey("ArrowRight", navContext())).toBeNull();
	});

	it("activates the focused row with Enter and Space", () => {
		expect(resolveNavigationKey("Enter", navContext())?.action).toBe("open");
		expect(resolveNavigationKey(" ", navContext())?.action).toBe("open");
	});

	it("claims no key that belongs to search or the shell", () => {
		for (const key of ["F3", "Escape", "PageDown", "PageUp", "Tab", "a", "Shift"]) {
			expect(resolveNavigationKey(key, navContext()), key).toBeNull();
		}
		// And the search map keeps its own: Escape closes search, not the panel.
		expect(resolveSearchKey("Escape", { hasQuery: true, shift: false })?.action).toBe("close");
		expect(resolveSearchKey("Escape", { hasQuery: false, shift: false })).toBeNull();
	});

	it("and the page list keeps its own, so the three maps compose", () => {
		// With the panel focused, the document area's arrows are the panel's; the
		// page list is not consulted at all, and the page list's own resolver is
		// untouched by that. This is the composition a shell performs by focus.
		const panel = resolveNavigationKey("ArrowDown", navContext());
		const document = resolvePageKey("ArrowDown", context());
		expect(panel?.action).toBe("next");
		expect(document?.page).toBe(11);
	});
});
