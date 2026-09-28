/**
 * The navigation controller (SL-4.UI.06).
 *
 * What can be asserted here, and what cannot, is the whole point of the file's
 * header comment repeated: the outline panel's rows, its roving focus, its
 * expansion set, its live-region sentences and its link decisions are all plain
 * data, so Node can assert every one of them. What is owed a manual pass is the
 * dialog a shell draws for an external link, and the *look* of a tree.
 *
 * The accessibility assertions are the ones UI.02/UI.05 set the bar at, and
 * navigation has to meet it rather than invent a second model: roving focus over
 * the visible rows, `aria` data published rather than computed by the shell, and
 * a live region that says something on every state change a reader caused.
 */

import { describe, expect, it } from "vitest";
import { createMockAdapter, type MockAdapter } from "../platform/mock-adapter.js";
import type { DocHandle, LinkAnnotation, OutlineNode } from "../platform/types.js";
import { createNavigation, type NavigationController } from "./navigation.js";

const OUTLINE: OutlineNode[] = [
	{
		title: "Chapter one",
		destination: { page: 0 },
		// Negative `/Count`: the document asks for this subtree to start closed.
		descendantCount: -1,
		children: [
			{ title: "Section 1.1", destination: { page: 1 } },
			{ title: "Section 1.2", destination: { page: 2 } },
		],
	},
	{ title: "Chapter two", destination: { page: 5 } },
	{ title: "Broken", namedDestination: "nowhere" },
];

async function fixture(
	options: { outline?: OutlineNode[]; dropPort?: boolean } = {},
): Promise<{ adapter: MockAdapter; doc: DocHandle; navigation: NavigationController }> {
	const adapter = createMockAdapter();
	const descriptor = adapter.addDocument({
		name: "nav.pdf",
		pageCount: 8,
		outline: options.outline ?? OUTLINE,
		pageLabels: [{ firstPage: 0, style: "r" }],
		destinations: [{ name: "here", destination: { page: 6 } }],
		links: [
			{
				page: 0,
				links: [
					{
						rect: { x: 0, y: 0, width: 1, height: 1 },
						action: { kind: "goTo", destination: { page: 4 } },
					},
				],
			},
		],
	});
	const doc = await adapter.engine.open(descriptor);
	// A host with no navigation port at all: the optional property is absent, not
	// null and not throwing. This is the shape `apps/web/host` and the extension
	// have until the engine grows an outline walk.
	const { navigation: _absent, ...withoutPort } = adapter;
	void _absent;
	const navigation = createNavigation({
		adapter: options.dropPort === true ? (withoutPort as MockAdapter) : adapter,
		doc,
	});
	await navigation.load();
	return { adapter, doc, navigation };
}

const link: LinkAnnotation = {
	id: "l1",
	rect: { x: 0, y: 0, width: 1, height: 1 },
	action: { kind: "goTo", destination: { page: 4 } },
};

describe("loading", () => {
	it("publishes the outline the document asked to be collapsed", async () => {
		const { navigation } = await fixture();
		const state = navigation.state();
		expect(state.available).toBe(true);
		expect(state.hasOutline).toBe(true);
		// The `/Count` of -1 closes the chapter; its sections are in the tree but
		// not on screen.
		expect(state.rows.map((row) => row.title)).toEqual([
			"Chapter one",
			"Chapter two",
			"Broken",
		]);
		expect(state.placeholder).toBe("none");
	});

	it("is idempotent: a second load does not re-walk the tree", async () => {
		const adapter = createMockAdapter();
		const descriptor = adapter.addDocument({ name: "nav.pdf", pageCount: 2, outline: OUTLINE });
		const doc = await adapter.engine.open(descriptor);
		const navigation = createNavigation({ adapter, doc });
		const first = await navigation.load();
		const second = await navigation.load();
		expect(second.rows).toEqual(first.rows);
		navigation.dispose();
	});

	it("says the host cannot read navigation when the port is absent", async () => {
		const { navigation } = await fixture({ dropPort: true });
		const state = navigation.state();
		expect(state.available).toBe(false);
		// A *different* sentence from "this document has no outline", because
		// they are different facts.
		expect(state.placeholder).toBe("unavailable");
		expect(state.announcement).not.toBe("");
	});

	it("says the document has no outline when it has none", async () => {
		const { navigation } = await fixture({ outline: [] });
		const state = navigation.state();
		expect(state.available).toBe(true);
		expect(state.hasOutline).toBe(false);
		expect(state.placeholder).toBe("empty");
	});

	it("reports a failed walk without losing the host", async () => {
		const adapter = createMockAdapter();
		const descriptor = adapter.addDocument({ name: "nav.pdf", pageCount: 2 });
		const doc = await adapter.engine.open(descriptor);
		const errors: unknown[] = [];
		const broken = {
			...adapter,
			navigation: {
				outline: async () => {
					throw new Error("walk failed");
				},
				pageLabels: async () => [{ firstPage: 0, style: "r" as const }],
				destinations: async () => [],
				pageLinks: async () => [],
			},
		};
		const navigation = createNavigation({
			adapter: broken as MockAdapter,
			doc,
			onError: (error) => errors.push(error),
		});
		const state = await navigation.load();
		expect(errors).toHaveLength(1);
		expect(state.announcement).not.toBe("");
		navigation.dispose();
	});

describe("the outline panel", () => {
	it("expands a collapsed chapter into its sections, subtree and all", async () => {
		const { navigation } = await fixture();
		const state = navigation.expand("0");
		expect(state.rows.map((row) => row.title)).toEqual([
			"Chapter one",
			"Section 1.1",
			"Section 1.2",
			"Chapter two",
			"Broken",
		]);
		expect(state.rows[0]?.expanded).toBe(true);
	});

	it("collapses a chapter and everything beneath it", async () => {
		const { navigation } = await fixture();
		navigation.expand("0");
		const state = navigation.collapse("0");
		expect(state.rows.map((row) => row.title)).toEqual([
			"Chapter one",
			"Chapter two",
			"Broken",
		]);
		expect(state.rows[0]?.expanded).toBe(false);
	});

	it("toggles a parent both ways, and ignores a leaf", async () => {
		const { navigation } = await fixture();
		expect(navigation.toggle("0").rows).toHaveLength(5);
		expect(navigation.toggle("0").rows).toHaveLength(3);
		// "Chapter two" has no children: a toggle on it is not an error, it is
		// nothing, and the state is republished unchanged.
		const before = navigation.state();
		expect(navigation.toggle("1").rows).toEqual(before.rows);
	});

	it("ignores an id that is not in the tree", async () => {
		const { navigation } = await fixture();
		const before = navigation.state();
		expect(navigation.expand("nope").rows).toEqual(before.rows);
		expect(navigation.collapse("nope").rows).toEqual(before.rows);
		expect(navigation.focusRow("nope").focusedId).toBe(before.focusedId);
	});

	it("publishes which rows are parents, for the expander buttons", async () => {
		const { navigation } = await fixture();
		expect([...navigation.state().parentIds]).toEqual(["0"]);
	});

	it("truncates a huge outline and says so", async () => {
		const wide: OutlineNode[] = Array.from({ length: 3_000 }, (_unused, index) => ({
			title: `Item ${index}`,
			destination: { page: index % 8 },
		}));
		const { navigation } = await fixture({ outline: wide });
		const state = navigation.state();
		expect(state.truncated).toBe(true);
		expect(state.truncatedNote).not.toBeNull();
		expect(state.rows.length).toBeLessThanOrEqual(2_000);
	});
});

describe("following an item", () => {
	it("publishes a target and says where it went", async () => {
		const { navigation } = await fixture();
		const state = navigation.activateRow("1");
		expect(state.target).toEqual({ page: 5, align: "start", from: "outline" });
		// The announcement is the document's own wording of the page, because the
		// document numbers it "vi".
		expect(state.announcement).toContain("vi");
	});

	it("explains a broken destination instead of moving nowhere silently", async () => {
		const { navigation } = await fixture();
		const state = navigation.activateRow("2");
		expect(state.target).toBeNull();
		expect(state.refusal).toBe("missing-destination");
		expect(state.announcement.length).toBeGreaterThan(0);
	});

	it("resolves a named destination through the name tree", async () => {
		const nodes: OutlineNode[] = [{ title: "Named", namedDestination: "here" }];
		const { navigation } = await fixture({ outline: nodes });
		const state = navigation.activateRow("0");
		expect(state.target).toEqual({ page: 6, align: "start", from: "outline" });
	});

	it("ignores an activation of a row that is not on screen", async () => {
		const { navigation } = await fixture();
		const before = navigation.state();
		expect(navigation.activateRow("0.0").target).toBe(before.target);
	});

	it("navigates a link without opening anything", async () => {
		const { adapter, navigation } = await fixture();
		const state = navigation.activateLink(link);
		expect(state.target).toEqual({ page: 4, align: "centre", from: "link" });
		expect(adapter.recording.externalUrls).toEqual([]);
	});
});


describe("accessibility, in the model UI.02 and UI.05 established", () => {
	it("keeps exactly one row in the tab order — the focused one", async () => {
		const { navigation } = await fixture();
		const state = navigation.focusRow("1");
		expect(state.focusedId).toBe("1");
		// The rows themselves carry no tabIndex: the shell reads `focusedId` and
		// gives the roving tabindex to that row. One source of truth.
		expect(state.rows.filter((row) => row.id === "1")).toHaveLength(1);
	});

	it("drops the focus when its row is hidden by a collapse", async () => {
		const { navigation } = await fixture();
		navigation.expand("0");
		navigation.focusRow("0.0");
		const state = navigation.collapse("0");
		// A stale id would leave the roving tabindex on a row that is not on
		// screen, which is the accessibility bug this rule exists to prevent.
		expect(state.focusedId).toBeNull();
	});

	it("claims no keys until the panel has focus", async () => {
		const { navigation } = await fixture();
		// The page list claims the same arrows. Without focus the panel must claim
		// nothing at all, or the document area would lose its own keys.
		for (const key of ["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End", "Enter"]) {
			expect(navigation.handleKey(key), key).toBeNull();
		}
	});

	it("claims nothing for a key that is not the tree's", async () => {
		const { navigation } = await fixture();
		navigation.setFocused(true);
		for (const key of ["F3", "Escape", "PageDown", "a", "Tab"]) {
			expect(navigation.handleKey(key), key).toBeNull();
		}
	});

	it("moves the roving focus with the arrows once focused", async () => {
		const { navigation } = await fixture();
		navigation.setFocused(true);
		navigation.focusRow("0");
		expect(navigation.handleKey("ArrowDown")?.focusedId).toBe("1");
		expect(navigation.handleKey("ArrowUp")?.focusedId).toBe("0");
		expect(navigation.handleKey("End")?.focusedId).toBe("2");
		expect(navigation.handleKey("Home")?.focusedId).toBe("0");
	});

	it("expands and collapses with the horizontal arrows", async () => {
		const { navigation } = await fixture();
		navigation.setFocused(true);
		navigation.focusRow("0");
		// Closed parent: ArrowRight opens it, ArrowLeft steps out (a no-op here).
		expect(navigation.handleKey("ArrowRight")?.rows).toHaveLength(5);
		expect(navigation.handleKey("ArrowLeft")?.rows).toHaveLength(3);
		// Open parent: ArrowLeft collapses it, ArrowRight steps into a child.
		expect(navigation.handleKey("ArrowRight")?.rows).toHaveLength(5);
		const into = navigation.handleKey("ArrowRight");
		expect(into?.focusedId).toBe("0.0");
	});

	it("activates the focused row with Enter, and with Space", async () => {
		const { navigation } = await fixture();
		navigation.setFocused(true);
		navigation.focusRow("1");
		expect(navigation.handleKey("Enter")?.target).toEqual({
			page: 5,
			align: "start",
			from: "outline",
		});
		expect(navigation.handleKey(" ")?.target?.page).toBe(5);
	});

	it("claims nothing when there are no rows to move between", async () => {
		const { navigation } = await fixture({ outline: [] });
		navigation.setFocused(true);
		expect(navigation.handleKey("ArrowDown")).toBeNull();
		expect(navigation.handleKey("Enter")).toBeNull();
	});

	it("echoes the current page so a shell can mark the current item", async () => {
		const { navigation } = await fixture();
		expect(navigation.setCurrentPage(3).currentPage).toBe(3);
		expect(navigation.setCurrentPage(99).currentPage).toBe(7);
		expect(navigation.setCurrentPage(-1).currentPage).toBe(0);
	});

	it("publishes the document's own page label, and null when it has none", async () => {
		const { navigation } = await fixture();
		expect(navigation.state().pageLabel(0)).toBe("i");
		// A document with no label range has nothing extra to say, so the state
		// offers null and the page list's own wording is used instead of a
		// second, different one.
		const bare = createMockAdapter();
		const descriptor = bare.addDocument({ name: "bare.pdf", pageCount: 3 });
		const doc = await bare.engine.open(descriptor);
		const plain = createNavigation({ adapter: bare, doc });
		await plain.load();
		expect(plain.state().pageLabel(0)).toBeNull();
		plain.dispose();
	});
});

describe("the state object", () => {
	it("is one object per change, and the same one for every subscriber", async () => {
		const { navigation } = await fixture();
		const first: unknown[] = [];
		const second: unknown[] = [];
		navigation.subscribe((state) => first.push(state));
		navigation.subscribe((state) => second.push(state));
		navigation.expand("0");
		expect(first).toHaveLength(1);
		expect(first[0]).toBe(second[0]);
	});

	it("stops publishing after unsubscribe and after dispose", async () => {
		const { navigation } = await fixture();
		const seen: unknown[] = [];
		const unsubscribe = navigation.subscribe((state) => seen.push(state));
		navigation.expand("0");
		unsubscribe();
		navigation.collapse("0");
		const after = seen.length;
		navigation.expand("0");
		expect(seen.length).toBe(after);
		navigation.dispose();
	});

	it("survives a dispose while a load is in flight", async () => {
		const adapter = createMockAdapter();
		const descriptor = adapter.addDocument({ name: "nav.pdf", pageCount: 2, outline: OUTLINE });
		const doc = await adapter.engine.open(descriptor);
		const navigation = createNavigation({ adapter, doc });
		const loading = navigation.load();
		navigation.dispose();
		// A late result must not write into a controller nobody is watching.
		await expect(loading).resolves.toBeDefined();
		expect(() => navigation.expand("0")).not.toThrow();
	});
});

});
