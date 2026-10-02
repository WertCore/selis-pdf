/**
 * SL-4.UI.07: the structure the viewer exposes, and what it refuses to invent.
 *
 * The negative tests are the point. This module's job is to be the layer where
 * "I do not know" is allowed, so a gate that only checked the happy path would
 * pass an implementation that reconstructs headings from font sizes and invents
 * alt text - the two things this item exists to stop.
 */
import { describe, expect, it } from "vitest";
import {
	type StructureNode,
	hasStructure,
	outlineOf,
	semanticsOf,
	structureOf,
	structureSummary,
} from "./a11y.js";

const heading = (title: string, children: StructureNode[] = []): StructureNode => ({
	role: "H1",
	title,
	children,
});

describe("headings", () => {
	it("maps a heading role to a real heading element at the document's level", () => {
		const s = semanticsOf(heading("Introduction"));
		expect(s.element).toBe("h1");
		expect(s.level).toBe(1);
		expect(s.name).toBe("Introduction");
		// A real `<h1>` needs no ARIA role on top; adding one is noise.
		expect(s.role).toBeNull();
	});

	it("takes the level from the ROLE, never from nesting depth", () => {
		// Deeply nested, but the document called it an H1. Depth-derived levels
		// would report an <h5> and put it in the outline at the wrong depth.
		const deep = heading("Chapter", [
			heading("Section", [heading("Subsection", [heading("Still H1")])]),
		]);
		expect(outlineOf([deep]).map((h) => h.level)).toEqual([1, 1, 1, 1]);
	});

	it("does not turn a paragraph into a heading", () => {
		const s = semanticsOf({ role: "P", title: "Looks big", children: [] });
		expect(s.level).toBeNull();
		expect(s.element).toBe("p");
	});

	it("reports no name for a heading the document gave no title", () => {
		// Its text is in the marked content, which this layer was not given. An
		// empty name is a blank button a reader tries three times to hear.
		const s = semanticsOf({ role: "H1", title: "   ", children: [] });
		expect(s.name).toBeNull();
	});
});

describe("untagged documents", () => {
	it("says a document with no structure tree has none", () => {
		expect(structureOf(null)).toBeNull();
		expect(structureOf(undefined)).toBeNull();
		expect(hasStructure(null)).toBe(false);
		expect(structureSummary(null).tagged).toBe(false);
	});

	it("never reports an untagged document as one with zero headings", () => {
		// `tagged:false, headings:0` and `tagged:true, headings:0` are different
		// facts and the shell words them differently. A "0 headings" status line on
		// an untagged document claims it was measured and found none.
		expect(structureSummary(null)).toEqual({
			tagged: false,
			headings: 0,
			figures: 0,
			needsDescription: 0,
		});
	});

	it("treats an empty structure root as not navigable", () => {
		expect(hasStructure([])).toBe(false);
		expect(outlineOf([])).toEqual([]);
	});
});

describe("figures and alt text", () => {
	it("uses the document's /Alt as the figure's name", () => {
		const s = semanticsOf({ role: "Figure", alt: "A bar chart of revenue", children: [] });
		expect(s.element).toBe("figure");
		expect(s.name).toBe("A bar chart of revenue");
		expect(s.needsDescription).toBe(false);
	});

	it("flags a figure with no /Alt instead of inventing a description", () => {
		const s = semanticsOf({ role: "Figure", children: [] });
		expect(s.needsDescription).toBe(true);
		// Crucially: NO name. The host supplies a generic word; this layer must
		// not write a sentence about what the picture appears to show.
		expect(s.name).toBeNull();
	});

	it("treats an empty or whitespace /Alt as no /Alt", () => {
		expect(semanticsOf({ role: "Figure", alt: "   ", children: [] }).needsDescription).toBe(true);
	});
});

describe("roles this module does not know", () => {
	it("keeps an unmapped custom role and flags it, rather than dropping it", () => {
		const s = semanticsOf({ role: "Chart", title: "Q3", children: [] });
		expect(s.unknown).toBe(true);
		expect(s.name).toBe("Q3");
		// A group, not silence: a reader hears there is something there.
		expect(s.role).toBe("group");
	});

	it("lets the DOCUMENT's own /RoleMap win over the built-in defaults", () => {
		// The document says its custom `Chart` role means a figure. That is the
		// document's call about its own content, not this module's.
		const s = semanticsOf(
			{ role: "Chart", alt: "Sales by quarter", children: [] },
			{ Chart: "Figure" },
		);
		expect(s.unknown).toBe(false);
		expect(s.element).toBe("figure");
		expect(s.needsDescription).toBe(false);
	});

	it("is not fooled by a RoleMap that renames a known role", () => {
		const s = semanticsOf(heading("Intro"), { H1: "Figure" });
		expect(s.element).toBe("figure");
		expect(s.level).toBeNull();
	});
});

describe("artifacts", () => {
	it("does not announce content the author marked as decoration", () => {
		const s = semanticsOf({ role: "Artifact", title: "Watermark", children: [] });
		expect(s.role).toBeNull();
	});

	it("skips an artifact's whole subtree, headings included", () => {
		// A heading inside something the author marked decorative is decoration.
		// Exposing it puts a phantom entry in the outline.
		const roots: StructureNode[] = [
			{
				role: "Artifact",
				children: [heading("Hidden chapter"), heading("Also hidden")],
			},
			heading("Real chapter"),
		];
		expect(outlineOf(roots).map((h) => h.name)).toEqual(["Real chapter"]);
	});
});

describe("tables", () => {
	it("gives table semantics a reader can navigate cell by cell", () => {
		const table: StructureNode = {
			role: "Table",
			children: [
				{ role: "TR", children: [{ role: "TH", title: "Quarter", children: [] }] },
				{ role: "TR", children: [{ role: "TD", title: "Q1", children: [] }] },
			],
		};
		const s = semanticsOf(table);
		expect(s.element).toBe("table");
		// Compared as whole arrays rather than by indexing: `noUncheckedIndexedAccess`
		// makes `children[0]` possibly-undefined, and the shape of a table's
		// children is exactly what this test is about.
		expect(s.children.map((row) => row.element)).toEqual(["tr", "tr"]);
		expect(s.children.map((row) => row.children.map((cell) => cell.element))).toEqual([
			["th"],
			["td"],
		]);
	});
});

describe("structureSummary", () => {
	it("counts only what is actually there", () => {
		const summary = structureSummary([
			heading("One"),
			{ role: "Figure", alt: "described", children: [] },
			{ role: "Figure", children: [] },
		]);
		expect(summary).toEqual({
			tagged: true,
			headings: 1,
			figures: 2,
			needsDescription: 1,
		});
	});

	it("counts headings inside a figure, because a reader still hears them", () => {
		const summary = structureSummary([
			{ role: "Figure", alt: "x", children: [heading("Caption-ish")] },
		]);
		expect(summary.headings).toBe(1);
	});
});
