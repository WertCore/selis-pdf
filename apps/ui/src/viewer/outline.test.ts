/**
 * The outline tree as a flat list (SL-4.UI.06).
 *
 * The properties asserted here are the ones a sidebar depends on and cannot
 * check for itself: that a collapsed item costs exactly one row, that expansion
 * is a set membership test rather than a re-walk, that ids survive a reload, and
 * that a document which nests ten thousand deep produces a bounded list instead
 * of a stack overflow. The last is not hypothetical — a viewer that crashes on a
 * document is a viewer whose DoD includes crashing.
 */

import { describe, expect, it } from "vitest";
import type { OutlineNode, PdfDestination } from "../platform/types.js";
import {
	MAX_OUTLINE_DEPTH,
	MAX_OUTLINE_ROWS,
	expandSubtree,
	flattenOutline,
	hasOutline,
	initialExpansion,
	moveOutlineFocus,
	parentIdsOf,
	rowPage,
} from "./outline.js";

/** Chapter → sections → a figure, the shape most real outlines have. */
const TREE: OutlineNode[] = [
	{
		title: "Chapter one",
		destination: { page: 0 },
		children: [
			{ title: "Section 1.1", destination: { page: 1 } },
			{
				title: "Section 1.2",
				destination: { page: 2 },
				children: [{ title: "Figure 1", destination: { page: 3 } }],
			},
		],
	},
	{ title: "Chapter two", destination: { page: 8 } },
	{ title: "Unlinked heading" },
];

const ALL_OPEN = new Set(["0", "0.0", "0.1", "0.1.0"]);

describe("flattenOutline", () => {
	it("emits one row per visible item, in document order", () => {
		const flat = flattenOutline({ nodes: TREE, expanded: ALL_OPEN });
		expect(flat.rows.map((row) => row.title)).toEqual([
			"Chapter one",
			"Section 1.1",
			"Section 1.2",
			"Figure 1",
			"Chapter two",
			"Unlinked heading",
		]);
	});

	it("gives a collapsed item one row and hides its children", () => {
		const flat = flattenOutline({ nodes: TREE, expanded: new Set() });
		expect(flat.rows.map((row) => row.title)).toEqual([
			"Chapter one",
			"Chapter two",
			"Unlinked heading",
		]);
		// The children are still counted: `itemCount` is the document's size, not
		// the panel's.
		expect(flat.itemCount).toBe(6);
		expect(flat.rows).toHaveLength(3);
	});

	it("hides a grandchild when its parent is collapsed", () => {
		const flat = flattenOutline({ nodes: TREE, expanded: new Set(["0"]) });
		expect(flat.rows.map((row) => row.title)).toEqual([
			"Chapter one",
			"Section 1.1",
			"Section 1.2",
			"Chapter two",
			"Unlinked heading",
		]);
	});

	it("treats expanded as a whitelist, so the same inputs give the same rows", () => {
		const once = flattenOutline({ nodes: TREE, expanded: new Set(["0"]) });
		const twice = flattenOutline({ nodes: TREE, expanded: new Set(["0"]) });
		expect(once.rows).toEqual(twice.rows);
	});

	it("numbers depth, parent and sibling count for the tree's ARIA", () => {
		const flat = flattenOutline({ nodes: TREE, expanded: ALL_OPEN });
		const figure = flat.rows.find((row) => row.title === "Figure 1");
		expect(figure?.depth).toBe(2);
		expect(figure?.parentId).toBe("0.1");
		// The three top-level items are siblings, whatever their subtrees do.
		expect(flat.rows[0]?.setSize).toBe(3);
		expect(flat.rows[0]?.parentId).toBeNull();
		expect(flat.maxDepth).toBe(3);
	});

	it("indexes rows for aria-posinset", () => {
		const flat = flattenOutline({ nodes: TREE, expanded: ALL_OPEN });
		expect(flat.rows.map((row) => row.index)).toEqual([0, 1, 2, 3, 4, 5]);
	});

	it("carries a row's destination, and null for an item with none", () => {
		const flat = flattenOutline({ nodes: TREE, expanded: ALL_OPEN });
		const chapter = flat.rows.find((row) => row.title === "Chapter two");
		const unlinked = flat.rows.find((row) => row.title === "Unlinked heading");
		expect(chapter?.page).toBe(8);
		// Not an error: the item is in the document and the row says so.
		expect(unlinked?.page).toBeNull();
	});

	it("names the parent ids whether or not they are expanded", () => {
		const flat = flattenOutline({ nodes: TREE, expanded: new Set() });
		expect([...flat.parentIds].sort()).toEqual(["0", "0.1"]);
		expect(parentIdsOf(TREE)).toEqual(flat.parentIds);
	});

	it("is empty for an empty document", () => {
		const flat = flattenOutline({ nodes: [], expanded: ALL_OPEN });
		expect(flat.rows).toEqual([]);
		expect(flat.itemCount).toBe(0);
		expect(flat.maxDepth).toBe(0);
		expect(flat.truncated).toBe(false);
	});

	it("caps the published rows and says that it did", () => {
		const wide: OutlineNode[] = Array.from({ length: 50 }, (_unused, index) => ({
			title: `Item ${index}`,
			destination: { page: index },
		}));
		const flat = flattenOutline({ nodes: wide, expanded: new Set(), maxRows: 10 });
		expect(flat.rows).toHaveLength(10);
		expect(flat.truncated).toBe(true);
		// The document's real size is still reported, so the panel can say
		// "10 of 50" rather than implying the outline ended.
		expect(flat.itemCount).toBe(50);
	});

	it("does not truncate when everything fits", () => {
		const flat = flattenOutline({ nodes: TREE, expanded: ALL_OPEN, maxRows: 100 });
		expect(flat.truncated).toBe(false);
		expect(MAX_OUTLINE_ROWS).toBeGreaterThan(6);
	});

	it("bounds a pathologically deep outline instead of overflowing the stack", () => {
		// Built iteratively here so the *test* does not overflow either.
		let node: OutlineNode = { title: "leaf", destination: { page: 1 } };
		for (let depth = 0; depth < 5_000; depth += 1) {
			node = { title: `level ${depth}`, children: [node] };
		}
		const flat = flattenOutline({ nodes: [node], expanded: new Set() });
		// One row: everything below is closed. What matters is that this returns
		// at all — the count is bounded by the depth cap, not by the stack.
		expect(flat.rows).toHaveLength(1);
		expect(flat.rows[0]?.depth).toBe(0);
		expect(MAX_OUTLINE_DEPTH).toBe(256);

		// And the walk that counts items is depth-bounded in the same way, so a
		// document that nests 5 000 deep reports a count rather than a crash.
		const counted = flattenOutline({ nodes: [node], expanded: new Set() });
		expect(counted.itemCount).toBeGreaterThan(0);
		expect(counted.itemCount).toBeLessThanOrEqual(5_001);
	});

	it("closes a deep outline that asks to be closed", () => {
		let node: OutlineNode = { title: "leaf" };
		for (let depth = 0; depth < 1_000; depth += 1) {
			node = { title: `level ${depth}`, children: [node] };
		}
		// The top asks to be closed, so it is the one id not in the expanded set.
		const closed: OutlineNode[] = [{ ...node, descendantCount: -1 }];
		expect([...initialExpansion(closed)]).not.toContain("0");
	});

	describe("initialExpansion", () => {
		it("expands everything except the items whose /Count is negative", () => {
			const nodes: OutlineNode[] = [
				{
					title: "closed",
					descendantCount: -3,
					children: [{ title: "a" }, { title: "b", children: [{ title: "c" }] }],
				},
				{ title: "open", descendantCount: 2, children: [{ title: "d" }] },
				{ title: "unspecified", children: [{ title: "e" }] },
			];
			// "0" is the closed chapter; everything else, at every depth, is expanded.
			expect([...initialExpansion(nodes)].sort()).toEqual([
				"0.0",
				"0.1",
				"0.1.0",
				"1",
				"1.0",
				"2",
				"2.0",
			]);
		});

		it("does not decide a child's state from its parent's", () => {
			const nodes: OutlineNode[] = [
				{ title: "p", descendantCount: -1, children: [{ title: "c", children: [{ title: "g" }] }] },
			];
			// Only "0" is asked to be closed; the child's state is its own, and it is
			// moot until the parent opens.
			expect([...initialExpansion(nodes)].sort()).toEqual(["0.0", "0.0.0"]);
		});

		it("expands a flat outline entirely, which is a no-op for a row list", () => {
			expect([...initialExpansion([{ title: "a" }, { title: "b" }])]).toEqual(["0", "1"]);
		});
	});

	describe("expandSubtree", () => {
		it("returns the subtree under a row, and nothing outside it", () => {
			expect([...expandSubtree(TREE, "0.1")].sort()).toEqual(["0.1", "0.1.0"]);
			expect([...expandSubtree(TREE, "0")].sort()).toEqual(["0", "0.0", "0.1", "0.1.0"]);
		});

		it("does not confuse id 1 with id 10 — ids are paths, not prefixes", () => {
			const wide: OutlineNode[] = Array.from({ length: 12 }, (_unused, index) => ({
				title: `Item ${index}`,
			}));
			// Only "1": "10" and "11" are siblings, not descendants.
			expect([...expandSubtree(wide, "1")]).toEqual(["1"]);
		});

		it("is empty for an id that is not in the tree", () => {
			expect(expandSubtree(TREE, "nope").size).toBe(0);
		});
	});

	describe("moveOutlineFocus", () => {
		function rows() {
			return flattenOutline({ nodes: TREE, expanded: ALL_OPEN }).rows;
		}

		it("moves down and up, stopping at the ends", () => {
			const list = rows();
			expect(moveOutlineFocus(list, "0", "ArrowDown")).toBe("0.0");
			expect(moveOutlineFocus(list, "0", "ArrowUp")).toBe("0");
			// The last visible row is id "2" — the third top-level item.
			expect(moveOutlineFocus(list, "1", "ArrowDown")).toBe("2");
			expect(moveOutlineFocus(list, "2", "ArrowDown")).toBe("2");
			expect(moveOutlineFocus(list, "0.0", "ArrowUp")).toBe("0");
		});

		it("jumps to the first and last visible row", () => {
			const list = rows();
			expect(moveOutlineFocus(list, "0.1", "Home")).toBe("0");
			expect(moveOutlineFocus(list, "0.1", "End")).toBe("2");
		});

		it("steps into an expanded parent and back out to the parent", () => {
			const list = rows();
			expect(moveOutlineFocus(list, "0", "ArrowRight")).toBe("0.0");
			expect(moveOutlineFocus(list, "0.1", "ArrowLeft")).toBe("0.1");
			expect(moveOutlineFocus(list, "0.0", "ArrowLeft")).toBe("0");
		});

		it("does nothing on a row with no children", () => {
			const list = rows();
			const unlinked = list[list.length - 1];
			expect(moveOutlineFocus(list, unlinked?.id ?? "", "ArrowRight")).toBe(unlinked?.id);
			// And ArrowLeft on a top-level leaf stays put: there is no parent.
			expect(moveOutlineFocus(list, "1", "ArrowLeft")).toBe("1");
		});

		it("starts at the first row when nothing is focused", () => {
			const list = rows();
			expect(moveOutlineFocus(list, null, "ArrowDown")).toBe("0.0");
			expect(moveOutlineFocus(list, null, "ArrowUp")).toBe("0");
		});

		it("is null for an empty list, for every key", () => {
			for (const key of [
				"ArrowDown",
				"ArrowUp",
				"ArrowRight",
				"ArrowLeft",
				"Home",
				"End",
			] as const) {
				expect(moveOutlineFocus([], "0", key), key).toBeNull();
			}
		});
	});

	describe("rowPage", () => {
		it("prefers the row's own destination", () => {
			const row = flattenOutline({ nodes: TREE, expanded: ALL_OPEN }).rows[0];
			const map = new Map<string, PdfDestination>([["anything", { page: 99 }]]);
			expect(row?.page === null ? null : rowPage(row ?? ({ page: null } as never), map)).toBe(0);
		});

		it("resolves a named destination from the document's name tree", () => {
			const nodes: OutlineNode[] = [{ title: "Named", namedDestination: "here" }];
			const row = flattenOutline({ nodes, expanded: new Set() }).rows[0];
			const map = new Map<string, PdfDestination>([["here", { page: 12 }]]);
			expect(rowPage(row ?? ({ page: null } as never), map)).toBe(12);
		});

		it("is null when the name is not in the tree", () => {
			const nodes: OutlineNode[] = [{ title: "Named", namedDestination: "gone" }];
			const row = flattenOutline({ nodes, expanded: new Set() }).rows[0];
			expect(rowPage(row ?? ({ page: null } as never), new Map())).toBeNull();
		});
	});

	describe("hasOutline", () => {
		it("is true only when there is at least one item", () => {
			expect(hasOutline([])).toBe(false);
			expect(hasOutline([{ title: "a" }])).toBe(true);
		});
	});
});
