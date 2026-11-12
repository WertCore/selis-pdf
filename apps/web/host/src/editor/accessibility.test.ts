/**
 * The accessibility report's own arithmetic.
 *
 * The rule the module follows is the engine's: nothing is invented. An
 * untagged document is untagged, not "a document with no headings", and an
 * image with no alternative text is listed as one rather than quietly
 * counted as fine.
 */

import { describe, expect, it } from "vitest";
import { reportStructure } from "./accessibility.js";
import type { Structure, StructureNode } from "./engine.js";

function node(role: string, extra: Partial<StructureNode> = {}): StructureNode {
	return { role, children: [], ...extra };
}

const TAGGED: Structure = {
	tagged: true,
	root: node("Document", {
		children: [
			node("H1", { title: "Invoice", children: [node("P", { title: "Hello" })] }),
			node("Figure", { title: "The logo" }),
			node("Figure", { alt: "A bar chart" }),
			node("Link", { title: "Terms" }),
		],
	}),
};

describe("the accessibility report", () => {
	it("says an untagged document is untagged", () => {
		const report = reportStructure({ tagged: false });
		expect(report.tagged).toBe(false);
		expect(report.headings).toBe(0);
		expect(report.images).toBe(0);
	});

	it("counts the headings, the images and the links", () => {
		const report = reportStructure(TAGGED);
		expect(report.tagged).toBe(true);
		expect(report.headings).toBe(1);
		expect(report.images).toBe(2);
		expect(report.links).toBe(1);
	});

	it("lists the images that have no alternative text", () => {
		const report = reportStructure(TAGGED);
		expect(report.imagesWithoutAlt).toBe(1);
		expect(report.undescribed).toEqual(["The logo"]);
	});

	it("does not count a role it does not know as a heading", () => {
		const report = reportStructure({
			tagged: true,
			root: node("Document", { children: [node("Div"), node("Span")] }),
		});
		expect(report.headings).toBe(0);
		expect(report.images).toBe(0);
	});

	it("matches the role whatever its case", () => {
		const report = reportStructure({
			tagged: true,
			root: node("Document", { children: [node("h2"), node("figure")] }),
		});
		expect(report.headings).toBe(1);
		expect(report.images).toBe(1);
	});

	it("walks a deep tree without losing its way", () => {
		let deep = node("H1");
		for (let i = 0; i < 200; i += 1) {
			deep = node("Div", { children: [deep] });
		}
		const report = reportStructure({ tagged: true, root: deep });
		// The walk is depth-bounded, so the deep heading is not counted. The
		// bound is the point: an untrusted tree must not spin the reader.
		expect(report.headings).toBe(0);
	});
});
