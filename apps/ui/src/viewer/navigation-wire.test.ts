/**
 * The navigation wire mapping (SL-4.UI.06).
 *
 * These tests are about one thing: **what the mapping loses, and whether it
 * loses it on purpose.** A wire→UI mapping is exactly the place where a
 * plausible-looking default turns a document's meaning into a viewer's guess,
 * and every loss below has a reason recorded next to it.
 *
 * The wire shapes here are copied from the engine's own answers
 * (`crates/selis-pdf-wasm/src/worker.rs`), so a change in the engine's JSON is
 * a change in these fixtures — and a mapping that silently accepts a changed
 * shape is a mapping that will read the wrong field without saying so.
 *
 * Headless honesty: the mapping is pure, so Node can assert all of it. What
 * cannot be asserted here is that a reader *sees* the consequence — that is the
 * browser check's leg, and the two are separate gates on purpose.
 */

import { describe, expect, it } from "vitest";
import {
	ACTION_KINDS,
	DESTINATION_KINDS,
	LABEL_STYLES,
	NavigationWireError,
	destinationKindOf,
	mapDestinations,
	mapOutline,
	mapPageLabels,
	mapPageLinks,
} from "./navigation-wire.js";

describe("the shared vocabularies", () => {
	it("publishes the closed sets a transport needs to validate against", () => {
		// A transport that receives an action class or a destination class over
		// a wire has to decide whether the viewer models it, and "decide" needs
		// the list rather than a copy of it. These are the lists.
		expect(ACTION_KINDS).toContain("launch");
		expect(ACTION_KINDS).toContain("unknown");
		expect(DESTINATION_KINDS).toContain("fit");
		expect(DESTINATION_KINDS).toContain("xyz");
		expect(LABEL_STYLES).toEqual(["D", "R", "r", "A", "a"]);
	});

	it("keeps the action classes the ADR-0020 policy is about", () => {
		// The disabled classes are in the vocabulary on purpose: the engine
		// reports them and the viewer decides, so a transport that filtered
		// them would make "is the viewer refusing this?" unanswerable.
		for (const disabled of ["launch", "goToR", "submitForm", "importData", "javascript"]) {
			expect(ACTION_KINDS).toContain(disabled);
		}
	});
});

describe("destinationKindOf", () => {
	it("names the six classes the spec defines", () => {
		expect(destinationKindOf("Fit")).toBe("fit");
		expect(destinationKindOf("FitH")).toBe("fitH");
		expect(destinationKindOf("FitV")).toBe("fitV");
		expect(destinationKindOf("FitR")).toBe("fitR");
		expect(destinationKindOf("FitB")).toBe("fitB");
		expect(destinationKindOf("XYZ")).toBe("xyz");
	});

	it("carries an unknown class as a plain xyz hop", () => {
		// The UI's own doc: "Anything the viewer does not recognise is carried
		// as an `xyz` destination with no parameters". The coordinates go with
		// it, because an unrecognised class has no agreed meaning for them.
		expect(destinationKindOf("Rendition")).toBe("xyz");
		expect(destinationKindOf("")).toBe("xyz");
	});
});

describe("mapOutline", () => {
	it("reads a bookmark with a page destination", () => {
		const report = mapOutline({
			present: true,
			truncated: false,
			pruned: false,
			items: [
				{
					title: "Chapter 1",
					target: { kind: "page", page: 2, destKind: "XYZ", left: 10, top: 20, zoom: null },
					action: null,
					count: 3,
					children: [],
				},
			],
		});
		expect(report.present).toBe(true);
		expect(report.truncated).toBe(false);
		expect(report.items).toHaveLength(1);
		expect(report.items[0]?.title).toBe("Chapter 1");
		expect(report.items[0]?.destination).toEqual({
			page: 2,
			kind: "xyz",
			left: 10,
			top: 20,
		});
		expect(report.items[0]?.descendantCount).toBe(3);
	});

	it("reads a bookmark that names a destination", () => {
		const report = mapOutline({
			present: true,
			items: [
				{
					title: "Appendix",
					target: { kind: "named", name: "appx" },
					action: null,
					count: null,
					children: [],
				},
			],
		});
		expect(report.items[0]?.namedDestination).toBe("appx");
		// A named bookmark has no page yet: the destination is resolved by the
		// caller against the document's name tree.
		expect(report.items[0]?.destination).toBeUndefined();
	});

	it("takes the target from an /A action when the item has no /Dest", () => {
		const report = mapOutline({
			present: true,
			items: [
				{
					title: "GoTo action",
					target: null,
					action: {
						kind: "goTo",
						rawName: "GoTo",
						uri: null,
						name: null,
						target: { kind: "page", page: 7, destKind: "Fit" },
						hasNext: false,
					},
					count: null,
					children: [],
				},
			],
		});
		expect(report.items[0]?.destination).toEqual({ page: 7, kind: "fit" });
	});

	it("keeps the tree nested", () => {
		const report = mapOutline({
			present: true,
			items: [
				{
					title: "Parent",
					target: null,
					action: null,
					count: 1,
					children: [
						{
							title: "Child",
							target: { kind: "page", page: 1, destKind: "Fit" },
							action: null,
							count: null,
							children: [],
						},
					],
				},
			],
		});
		expect(report.items[0]?.children).toHaveLength(1);
		expect(report.items[0]?.children?.[0]?.title).toBe("Child");
	});

	it("reports the three states apart", () => {
		expect(mapOutline({ present: false, truncated: false, pruned: false, items: [] }).present).toBe(
			false,
		);
		expect(mapOutline({ present: true, truncated: true, pruned: false, items: [] }).truncated).toBe(
			true,
		);
		expect(mapOutline({ present: true, truncated: false, pruned: true, items: [] }).pruned).toBe(
			true,
		);
	});

	it("refuses an answer that is not an outline", () => {
		expect(() => mapOutline(null)).toThrow(NavigationWireError);
		expect(() => mapOutline({})).toThrow(/no item list/);
		expect(() => mapOutline({ items: [{ target: null }] })).toThrow(/no title/);
	});
});

describe("mapPageLabels", () => {
	it("reads the ranges the engine sends", () => {
		const ranges = mapPageLabels({
			ranges: [
				{ firstPage: 0, style: "r", prefix: null, firstValue: 1 },
				{ firstPage: 4, style: "D", prefix: "A-", firstValue: 1 },
			],
		});
		expect(ranges).toEqual([
			{ firstPage: 0, style: "r", firstValue: 1 },
			{ firstPage: 4, style: "D", prefix: "A-", firstValue: 1 },
		]);
	});

	it("drops a style the spec does not define", () => {
		// An unknown `/S` is not an error in the document - it is a style this
		// build does not model, and the range still applies with the default.
		const ranges = mapPageLabels({ ranges: [{ firstPage: 0, style: "Z" }] });
		expect(ranges[0]?.style).toBeUndefined();
	});

	it("refuses an answer that is not a range list", () => {
		expect(() => mapPageLabels({})).toThrow(/no range list/);
		expect(() => mapPageLabels({ ranges: [{ style: "D" }] })).toThrow(/no first page/);
	});
});

describe("mapDestinations", () => {
	it("reads a name that points at a page", () => {
		const destinations = mapDestinations({
			destinations: [{ name: "here", target: { kind: "page", page: 5, destKind: "Fit" } }],
		});
		expect(destinations).toEqual([{ name: "here", destination: { page: 5, kind: "fit" } }]);
	});

	it("chases a chain of names to the page it ends at", () => {
		// The engine passes a chain through so the VIEWER resolves it. This is
		// the viewer. Both names are kept: the chain's own end ("second") is a
		// destination in its own right, and dropping it would make a bookmark
		// that names it look missing.
		const destinations = mapDestinations({
			destinations: [
				{ name: "first", target: { kind: "named", name: "second" } },
				{ name: "second", target: { kind: "page", page: 3, destKind: "Fit" } },
			],
		});
		expect(destinations).toEqual([
			{ name: "first", destination: { page: 3, kind: "fit" } },
			{ name: "second", destination: { page: 3, kind: "fit" } },
		]);
	});

	it("drops a chain that loops", () => {
		const destinations = mapDestinations({
			destinations: [
				{ name: "a", target: { kind: "named", name: "b" } },
				{ name: "b", target: { kind: "named", name: "a" } },
			],
		});
		expect(destinations).toEqual([]);
	});

	it("drops a name that resolves to nothing", () => {
		const destinations = mapDestinations({
			destinations: [{ name: "gone", target: { kind: "unresolved" } }],
		});
		expect(destinations).toEqual([]);
	});
});

describe("mapPageLinks", () => {
	it("reads a link with a rectangle and a URI action", () => {
		const links = mapPageLinks({
			page: 0,
			links: [
				{
					index: 0,
					object: 12,
					rect: { x0: 10, y0: 20, x1: 110, y1: 60 },
					action: {
						kind: "uri",
						rawName: "URI",
						uri: "https://example.com/a?b=c",
						name: null,
						target: null,
						hasNext: false,
					},
					target: null,
					contents: "Read more",
				},
			],
		});
		expect(links).toHaveLength(1);
		expect(links[0]?.rect).toEqual({ x: 10, y: 20, width: 100, height: 40 });
		expect(links[0]?.action.kind).toBe("uri");
		expect(links[0]?.action.uri).toBe("https://example.com/a?b=c");
		expect(links[0]?.contents).toBe("Read more");
	});

	it("normalises an inverted rectangle", () => {
		// The guest crosses a rectangle verbatim and says the viewer is the
		// layer that decides what "where" means.
		const links = mapPageLinks({
			page: 0,
			links: [
				{
					index: 0,
					object: 1,
					rect: { x0: 110, y0: 60, x1: 10, y1: 20 },
					action: null,
					target: null,
					contents: null,
				},
			],
		});
		expect(links[0]?.rect).toEqual({ x: 10, y: 20, width: 100, height: 40 });
	});

	it("drops a link with no rectangle", () => {
		// The engine crosses an absent /Rect as null rather than a zero box
		// that "would silently swallow clicks in the corner".
		const links = mapPageLinks({
			page: 0,
			links: [{ index: 0, object: 1, rect: null, action: null, target: null, contents: null }],
		});
		expect(links).toEqual([]);
	});

	it("reads an annotation whose only destination is its own /Dest", () => {
		const links = mapPageLinks({
			page: 0,
			links: [
				{
					index: 0,
					object: 1,
					rect: { x0: 0, y0: 0, x1: 10, y1: 10 },
					action: null,
					target: { kind: "page", page: 4, destKind: "FitB" },
					contents: null,
				},
			],
		});
		expect(links[0]?.action.kind).toBe("goTo");
		expect(links[0]?.action.destination).toEqual({ page: 4, kind: "fitB" });
	});

	it("carries every action class, including the ones ADR-0020 disables", () => {
		const classes = ["launch", "goToR", "submitForm", "importData", "javascript", "named"];
		const links = mapPageLinks({
			page: 0,
			links: classes.map((kind, index) => ({
				index,
				object: index + 1,
				rect: { x0: 0, y0: 0, x1: 10, y1: 10 },
				action: {
					kind,
					rawName: kind,
					uri: null,
					name: kind === "named" ? "NextPage" : null,
					target: null,
					hasNext: false,
				},
				target: null,
				contents: null,
			})),
		});
		expect(links.map((link) => link.action.kind)).toEqual(classes);
	});

	it("refuses an answer that is not a link list", () => {
		expect(() => mapPageLinks({ page: 0 })).toThrow(/no link list/);
	});
});
