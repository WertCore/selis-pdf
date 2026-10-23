/**
 * SL-4.UI.06 (the extension's half) — the decisions the mount replays.
 *
 * `mountViewerView` is DOM composition, and Node cannot run it (the raster
 * needs a real 2d context, a page-level one; see the module's own note), so
 * what this suite pins is everything else: the pure helpers are read directly,
 * and the controller facts those helpers read are proven against the **real
 * transport** — the `createLoopbackLink`/`serveEngineHost` wiring
 * `adapter.test.ts` proves the seam with, so a fixture here and one there
 * cannot quietly diverge.
 *
 * The DOM half is covered by the type checker plus the browser check leg,
 * which this task records as future work in `REUSE.md` rather than pretending
 * into existence here.
 */

import { describe, expect, it } from "vitest";
import { createMockAdapter } from "../../ui/src/platform/mock-adapter.js";
import { createNavigation } from "../../ui/src/viewer/navigation.js";
import type { NavigationState } from "../../ui/src/viewer/navigation.js";
import type { OutlineRow } from "../../ui/src/viewer/outline.js";
import { MAX_OUTLINE_ROWS } from "../../ui/src/viewer/outline.js";
import { DEFAULT_METRICS } from "../../ui/src/viewer/page-list.js";
import {
	EN_NAVIGATION_CATALOGUE,
	EN_PAGE_LIST_CATALOGUE,
	createNavigationStrings,
	createPageListStrings,
} from "../../ui/src/viewer/strings.js";
import { createExtensionAdapter } from "./ext/adapter.js";
import { createFakeHostEnv } from "./ext/fake-host-env.js";
import { createFixtureEngine, sampleBytes } from "./ext/fixture-engine.js";
import { createLoopbackLink } from "./ext/loopback.js";
import { serveEngineHost } from "./ext/offscreen-engine.js";
import { openDocument } from "./ext/viewer-session.js";
import {
	currentOutlineId,
	externalPromptPlan,
	metricsFromStyle,
	navigationKeyContext,
	nestOutlineRows,
	outlineNotes,
	outlineRowPlan,
	outlineRowSpokenLabel,
	parsePxOr,
	promptKeyAction,
	promptTransition,
} from "./viewer-view.js";

const navStrings = createNavigationStrings(EN_NAVIGATION_CATALOGUE);
const pageStrings = createPageListStrings(EN_PAGE_LIST_CATALOGUE);

/** A live adapter over a loopback, with the fixture document open. */
async function connected(options: { navigation?: boolean } = {}) {
	const link = createLoopbackLink();
	const engine = createFixtureEngine();
	if (options.navigation === false) {
		serveEngineHost({ link: link.host, engine });
	} else {
		serveEngineHost({ link: link.host, engine, navigation: engine });
	}
	const host = createFakeHostEnv();
	const adapter = createExtensionAdapter({ env: host.env, link: link.client });
	const { doc } = await openDocument(adapter, {
		kind: "bytes",
		bytes: sampleBytes().buffer as ArrayBuffer,
		name: "report.pdf",
	});
	return { adapter, doc, host };
}

/** The navigation controller over the live transport, loaded. */
async function navigationFixture() {
	const live = await connected();
	const nav = createNavigation({
		adapter: live.adapter,
		doc: live.doc,
		strings: EN_NAVIGATION_CATALOGUE,
		pageStrings: EN_PAGE_LIST_CATALOGUE,
	});
	const state = await nav.load();
	return { ...live, nav, state };
}

/** The helper's own page-number lookup, over a mock document. */
async function navigationOverMock(spec: {
	name?: string;
	pageCount: number;
	outline?: readonly { title: string; destination: { page: number } }[];
}) {
	const adapter = createMockAdapter();
	const registered = adapter.addDocument(spec);
	const doc = await adapter.engine.open(registered);
	const nav = createNavigation({
		adapter,
		doc,
		strings: EN_NAVIGATION_CATALOGUE,
		pageStrings: EN_PAGE_LIST_CATALOGUE,
	});
	return { nav, state: await nav.load() };
}

// ── The pure side first: no page, no controller, just decisions ────────────

describe("parsePxOr", () => {
	it("parses a CSS length in pixels, and falls back otherwise", () => {
		expect(parsePxOr("16px", 8)).toBe(16);
		expect(parsePxOr("12.5px", 8)).toBe(12.5);
		expect(parsePxOr("  3px", 8)).toBe(3);
		expect(parsePxOr(undefined, 8)).toBe(8);
		expect(parsePxOr(null, 8)).toBe(8);
		// An empty declaration or a keyword ("auto", "inherit") is not a length
		// the layout can multiply by, so it takes the fallback too.
		expect(parsePxOr("", 8)).toBe(8);
		expect(parsePxOr("auto", 8)).toBe(8);
	});
});

describe("metricsFromStyle", () => {
	it("reads the two density custom properties the page-list wants", () => {
		const style = (values: Record<string, string>) => ({
			getPropertyValue: (name: string) => values[name] ?? "",
		});
		expect(
			metricsFromStyle(
				style({
					"--selis-density-page-gap": "12px",
					"--selis-space-6": "14px",
				}),
			),
		).toEqual({ pageGap: 12, padding: 14 });
		// A stylesheet that failed to arrive still lays out honestly: the
		// fallbacks are the controller's own defaults, not zero.
		expect(metricsFromStyle(style({}))).toEqual({
			pageGap: DEFAULT_METRICS.pageGap,
			padding: DEFAULT_METRICS.padding,
		});
	});
});

describe("outlineRowPlan", () => {
	it("sends a direct page, or a named destination that resolves, to the target", () => {
		expect(outlineRowPlan(handRow({ page: 4 }), new Map([["here", { page: 2 }]]))).toEqual({
			kind: "target",
			page: 4,
		});
		expect(
			outlineRowPlan(handRow({ namedDestination: "here" }), new Map([["here", { page: 2 }]])),
		).toEqual({ kind: "target", page: 2 });
	});

	it("keeps an unresolvable name visible as text, and inert rows inert", () => {
		expect(outlineRowPlan(handRow({ namedDestination: "gone" }), new Map())).toEqual({
			kind: "unresolved",
			named: "gone",
		});
		expect(outlineRowPlan(handRow(), new Map())).toEqual({ kind: "inert" });
	});
});

describe("nestOutlineRows", () => {
	it("re-nests the flat rows into the tree, in document order", () => {
		const flat = [
			handRow({ id: "0", title: "Chapter one", index: 1, setSize: 2 }),
			handRow({ id: "0.0", title: "Figure 1", depth: 1, parentId: "0", index: 1, setSize: 1 }),
			handRow({ id: "1", title: "Appendix", index: 2, setSize: 2 }),
		];
		const [chapter, appendix] = nestOutlineRows(flat);
		expect(chapter?.row.id).toBe("0");
		expect(chapter?.position).toBe(1);
		expect(chapter?.children).toHaveLength(1);
		expect(chapter?.children[0]?.row.id).toBe("0.0");
		expect(chapter?.children[0]?.position).toBe(1);
		expect(appendix?.row.id).toBe("1");
		expect(appendix?.position).toBe(2);
		expect(appendix?.children).toHaveLength(0);
	});
});

describe("currentOutlineId", () => {
	it("picks the first row that lands on the current page, named or direct", () => {
		const destinations = new Map([
			["here", { page: 2 }],
			["alsoTwo", { page: 2 }],
		]);
		const rows = [
			handRow({ id: "a", page: 0 }),
			handRow({ id: "b", namedDestination: "here" }),
			handRow({ id: "c", namedDestination: "alsoTwo" }),
		];
		expect(currentOutlineId(rows, 2, destinations)).toBe("b");
		expect(currentOutlineId(rows, 0, destinations)).toBe("a");
	});

	it("answers null when no row lands on the page", () => {
		expect(currentOutlineId([handRow({ page: 0 })], 1, new Map())).toBeNull();
		expect(currentOutlineId([], 0, new Map())).toBeNull();
	});
});

describe("promptKeyAction", () => {
	it("claims Escape and nothing else", () => {
		expect(promptKeyAction("Escape")).toBe("cancel");
		expect(promptKeyAction("Enter")).toBeNull();
		expect(promptKeyAction("Tab")).toBeNull();
	});
});

// ── The controller side: real transport, real published states ─────────────

describe("navigationKeyContext (over the real publish)", () => {
	it("answers the four facts the key resolver asks about", async () => {
		const { nav, state } = await navigationFixture();
		// The fixture outline loads collapsed: rows exist, focus does not.
		expect(state.rows.length).toBeGreaterThan(0);
		expect(navigationKeyContext(state)).toEqual({
			hasFocus: false,
			hasRows: true,
			focusedHasChildren: false,
			focusedExpanded: false,
		});
		nav.setFocused(true);
		nav.focusRow("0");
		expect(navigationKeyContext(nav.state())).toEqual({
			hasFocus: true,
			hasRows: true,
			focusedHasChildren: true,
			focusedExpanded: false,
		});
		nav.toggle("0");
		expect(navigationKeyContext(nav.state())).toEqual({
			hasFocus: true,
			hasRows: true,
			focusedHasChildren: true,
			focusedExpanded: true,
		});
	});
});

describe("outlineRowSpokenLabel", () => {
	it("uses the document's own labels once the load has read them", async () => {
		const { nav, state, doc } = await navigationFixture();
		// The fixture's `/PageLabels`: roman front matter, decimal from 7.
		expect(outlineRowSpokenLabel(0, state, pageStrings, doc.pageCount)).toBe("pi");
		expect(outlineRowSpokenLabel(2, state, pageStrings, doc.pageCount)).toBe("7");
		expect(nav.state().currentPage).toBe(0);
	});

	it("falls back to the page list's wording when the state carries no labels", () => {
		// Before the controller has read `/PageLabels` the published lookup is
		// null, and the page list's sentence is the one wording that remains.
		const preLoad = preLoadState();
		expect(preLoad.pageLabel(0)).toBeNull();
		expect(outlineRowSpokenLabel(0, preLoad, pageStrings, 3)).toBe(pageStrings.pageLabel(0, 3));
		expect(outlineRowSpokenLabel(2, preLoad, pageStrings, 3)).toBe(pageStrings.pageLabel(2, 3));
	});
});

describe("the loaded fixture's rows", () => {
	it("resolve the named destination and mark the current page through the helpers", async () => {
		const { nav, adapter, doc } = await navigationFixture();
		const entries = (await adapter.navigation?.destinations(doc)) ?? [];
		const destinations = new Map(entries.map((entry) => [entry.name, entry.destination]));
		// The fixture: "Chapter one" carries page 0 directly; "Appendix" names
		// "here", which resolves to page 2.
		expect(currentOutlineId(nav.state().rows, 2, destinations)).toBe("1");
		expect(currentOutlineId(nav.state().rows, 0, destinations)).toBe("0");
	});

	it("nest back into the tree the panel draws", async () => {
		const { nav } = await navigationFixture();
		// Pre-toggle the child row is not published at all: the parent starts
		// collapsed, from the document's own negative `/Count`.
		expect(nav.state().rows.map((row) => row.id)).toEqual(["0", "1"]);
		nav.toggle("0");
		const [chapter] = nestOutlineRows(nav.state().rows);
		expect(chapter?.row.id).toBe("0");
		expect(chapter?.children).toHaveLength(1);
		expect(chapter?.children[0]?.row.id).toBe("0.0");
	});
});

describe("outlineNotes", () => {
	it("says nothing before the load settles, whatever the pre-load state looks like", () => {
		// The pre-load placeholder reads "empty" — a claim about a document the
		// controller has not read yet. Without the gate every open would flash
		// "no outline" before the real one could arrive.
		expect(outlineNotes(preLoadState(), navStrings)).toEqual([]);
		expect(outlineNotes(preLoadState(), navStrings, { loaded: false })).toEqual([]);
	});

	it("reads a document with no outline as the one real empty case", async () => {
		const { state } = await navigationOverMock({ name: "plain.pdf", pageCount: 1 });
		expect(state.rows).toHaveLength(0);
		expect(outlineNotes(state, navStrings, { loaded: true })).toEqual([
			{ dataRole: "no-outline", text: navStrings.empty() },
		]);
	});

	it("reports a failed walk and a portless host with the unavailable sentence", async () => {
		const { nav, state } = await navigationFixture();
		// The helper's own flag, for whatever reports the walk failed.
		expect(outlineNotes(state, navStrings, { loaded: true, loadFailed: true })).toEqual([
			{ dataRole: "unavailable", text: navStrings.unavailable() },
		]);
		// A host with no navigation port, spelled as a spread for the pure test.
		const portless = {
			...state,
			available: false,
			placeholder: "unavailable",
			rows: [],
		} as NavigationState;
		expect(outlineNotes(portless, navStrings, { loaded: true })).toEqual([
			{ dataRole: "unavailable", text: navStrings.unavailable() },
		]);
		expect(nav.state().rows.length).toBeGreaterThan(0);
	});

	it("says nothing extra about a full list that fitted the cap", async () => {
		const { state } = await navigationFixture();
		expect(state.rows.length).toBeGreaterThan(0);
		expect(state.truncatedNote).toBeNull();
		expect(outlineNotes(state, navStrings, { loaded: true })).toEqual([]);
	});
});

describe("the row cap", () => {
	it("stops at MAX_OUTLINE_ROWS and repeats the cap's sentence as a note", async () => {
		// The mock never truncates by itself, so the cap is reached the honest
		// way: a document that genuinely holds one row more than it.
		const { state } = await navigationOverMock({
			name: "long.pdf",
			pageCount: 1,
			outline: Array.from({ length: MAX_OUTLINE_ROWS + 1 }, (_, index) => ({
				title: `Entry ${index}`,
				destination: { page: 0 },
			})),
		});
		expect(state.rows).toHaveLength(MAX_OUTLINE_ROWS);
		expect(state.truncated).toBe(true);
		expect(state.truncatedNote).not.toBeNull();
		expect(state.rows[0]?.id).toBe("0");
		expect(state.rows[MAX_OUTLINE_ROWS - 1]?.id).toBe(String(MAX_OUTLINE_ROWS - 1));
		const notes = outlineNotes(state, navStrings, { loaded: true });
		expect(notes).toEqual([
			{ dataRole: "outline-truncated", text: state.truncatedNote ?? "" },
		]);
	});
});

describe("the external-link prompt plan", () => {
	it("opens with the destination verbatim and the catalogue's own labels", async () => {
		const { nav, adapter, doc } = await navigationFixture();
		const uriLink = await firstUriLink(adapter, doc);
		expect(nav.state().pendingExternal).toBeNull();
		nav.activateLink(uriLink);
		const plan = externalPromptPlan(nav.state());
		expect(plan.open).toBe(true);
		if (!plan.open) {
			return;
		}
		// The URL appears exactly as the document wrote it; a shortened form is
		// what ADR-P0020's prompt requirement exists to prevent.
		expect(plan.url).toBe("https://example.com/a");
		expect(plan.host).toBe("example.com");
		expect(plan.body).toContain("https://example.com/a");
		expect(plan.title).toBe(navStrings.externalTitle);
		expect(plan.confirm).toBe(navStrings.externalConfirm);
		expect(plan.cancel).toBe(navStrings.externalCancel);
	});

	it("changes per transition: same pending is none, a new one a replacement", async () => {
		const { nav, adapter, doc } = await navigationFixture();
		const uriLink = await firstUriLink(adapter, doc);
		// Same object twice in a row: a scroll-step repaint. Nothing to do.
		expect(promptTransition(null, null)).toBe("none");
		nav.activateLink(uriLink);
		const first = nav.state().pendingExternal;
		expect(first).not.toBeNull();
		expect(promptTransition(null, first)).toBe("open");
		expect(promptTransition(first, first)).toBe("none");
		// A second activation while the prompt is up replaces it: the
		// controller publishes only the most recent request.
		nav.cancelExternal();
		nav.activateLink(uriLink);
		const second = nav.state().pendingExternal;
		expect(second).not.toBeNull();
		expect(second).not.toBe(first);
		expect(promptTransition(first, second)).toBe("open");
		// And a dismissal is the one closed transition.
		expect(promptTransition(second, null)).toBe("closed");
	});
});

describe("confirmExternal", () => {
	it("opens the URL only through the prompt's confirm, then clears it", async () => {
		const { nav, adapter, doc, host } = await navigationFixture();
		const uriLink = await firstUriLink(adapter, doc);
		nav.activateLink(uriLink);
		await nav.confirmExternal();
		expect(host.recording.externalUrls).toEqual(["https://example.com/a"]);
		expect(nav.state().pendingExternal).toBeNull();
	});

	it("never becomes a prompt for a class the viewer refuses", async () => {
		const { nav, adapter, doc, host } = await navigationFixture();
		const links = (await adapter.navigation?.pageLinks(doc, 0)) ?? [];
		const launch = links.find((entry) => entry.action.kind === "launch");
		expect(launch?.action.kind).toBe("launch");
		if (launch === undefined) {
			return;
		}
		nav.activateLink(launch);
		// A refusal publishes a live-region line, never a pending dialog: the
		// confirm button is the one road to `openExternal` in the house, and a
		// prompt here would be the road taken without a question asked.
		expect(nav.state().pendingExternal).toBeNull();
		await nav.confirmExternal();
		expect(host.recording.externalUrls).toEqual([]);
	});
});

describe("a host without the navigation capability", () => {
	it("reports as a failed document walk, not as a portless state", async () => {
		// The extension adapter always carries the port; the capability lives a
		// wire call below it, and a host that was served without it answers
		// every navigation read with BINDING_UNSUPPORTED_OP. So the honest
		// shape here is the failed walk: the panel EXISTS — what failed is
		// reading this document — and the catalogue spells exactly that.
		const walked: unknown[] = [];
		const loopback = await connected({ navigation: false });
		const nav = createNavigation({
			adapter: loopback.adapter,
			doc: loopback.doc,
			strings: EN_NAVIGATION_CATALOGUE,
			pageStrings: EN_PAGE_LIST_CATALOGUE,
			onError: (error) => {
				walked.push(error);
			},
		});
		const state = await nav.load();
		expect(walked.length).toBeGreaterThan(0);
		expect(state.available).toBe(true);
		expect(state.placeholder).toBe("empty");
		expect(state.announcement).toBe(navStrings.unavailable());
		// Which is also the shape the mount's own flag derivation reads: the
		// unavailable sentence on a state whose placeholder still says "empty"
		// is exactly a failed walk, and the notes host says so.
		const loadFailed =
			state.available &&
			state.placeholder === "empty" &&
			state.announcement === navStrings.unavailable();
		expect(loadFailed).toBe(true);
		expect(
			outlineNotes(state, navStrings, { loaded: true, loadFailed: loadFailed }),
		).toEqual([{ dataRole: "unavailable", text: navStrings.unavailable() }]);
	});
});

// ── Private fixtures ────────────────────────────────────────────────────────

/** The controller's pre-load state shape, by hand. */
function preLoadState(): NavigationState {
	return {
		available: true,
		hasOutline: false,
		rows: [],
		focusedId: null,
		currentPage: 0,
		focused: false,
		parentIds: new Set(),
		truncated: false,
		pageLabel: () => null,
		target: null,
		pendingExternal: null,
		lastActivation: null,
		refusal: null,
		announcement: "",
		regionLabel: navStrings.regionLabel,
		outlineLabel: navStrings.outlineLabel,
		placeholder: "empty",
		truncatedNote: null,
		externalTitle: navStrings.externalTitle,
		externalConfirm: navStrings.externalConfirm,
		externalCancel: navStrings.externalCancel,
		externalBody: () => "",
	};
}

/** One outline row by hand, for the pure deciders. */
function handRow(overrides: Partial<OutlineRow> = {}): OutlineRow {
	return {
		id: overrides.id ?? "r",
		title: overrides.title ?? "Section",
		depth: 0,
		parentId: null,
		hasChildren: false,
		expanded: false,
		page: null,
		namedDestination: null,
		index: 1,
		setSize: 1,
		...overrides,
	};
}

/** The fixture's page-0 `/URI` link, retrieved over the real transport. */
async function firstUriLink(
	adapter: Awaited<ReturnType<typeof connected>>["adapter"],
	doc: Awaited<ReturnType<typeof connected>>["doc"],
) {
	const links = (await adapter.navigation?.pageLinks(doc, 0)) ?? [];
	const link = links.find((entry) => entry.action.kind === "uri");
	if (link === undefined) {
		throw new Error("the fixture document has no /URI link on page 0");
	}
	return link;
}
