/**
 * The static-page generator, driven directly.
 *
 * The positive cases pin the shape every generated page must ship; the
 * negative cases pin the build failures the plan asks for — a duplicated slug,
 * a slug no host can serve, a reserved path, a duplicate description, a
 * dangling related id, and a document with no title or heading.
 */

import { describe, expect, it } from "vitest";
import {
	RESERVED_TOP_LEVEL,
	assertPageShape,
	escapeHtml,
	renderOutputs,
	renderRobots,
	renderSitemap,
	validateRegistry,
} from "../tools/build-pages.mjs";
import { SITE_ORIGIN, TOOLS, TOOL_GROUPS } from "./tool-registry.js";

/** A minimal publishable entry; individual tests break one field at a time. */
function tool(overrides = {}) {
	return {
		id: "merge",
		slug: "merge-pdf",
		group: "organise",
		mark: "⇄",
		title: "Merge PDF",
		blurb: "Combine several PDFs into one.",
		note: "Merge whole documents, in the order you pick them.",
		verb: "Merge PDFs",
		multiple: true,
		...overrides,
	};
}

describe("generated output", () => {
	const files = renderOutputs();

	it("writes one page per tool plus the manifest, sitemap and robots file", () => {
		expect(files.size).toBe(TOOLS.length + 3);
		for (const tool of TOOLS) {
			expect(files.has(`${tool.slug}/index.html`), tool.slug).toBe(true);
		}
	});

	it("gives every page a unique title, description, canonical and h1", () => {
		const titles = new Set();
		const descriptions = new Set();
		for (const tool of TOOLS) {
			const page = files.get(`${tool.slug}/index.html`) ?? "";
			expect(page).toContain('data-tool="');
			expect(page).toContain(`<link rel="canonical" href="${SITE_ORIGIN}/${tool.slug}/" />`);
			expect(page).toContain(`<h1 id="workspace-title">${escapeHtml(tool.title)}</h1>`);
			expect(page).toContain(escapeHtml(tool.note));
			titles.add(tool.title);
			descriptions.add(tool.blurb);
		}
		expect(titles.size).toBe(TOOLS.length);
		expect(descriptions.size).toBe(TOOLS.length);
	});

	it("does not duplicate the card grid on a tool page", () => {
		for (const [name, page] of files) {
			if (!name.endsWith("/index.html")) continue;
			expect(page, name).not.toContain('id="tool-grid"');
			expect(page, name).not.toContain('id="tool-jump"');
		}
	});

	it("links related tools with real anchors", () => {
		const merge = files.get("merge-pdf/index.html") ?? "";
		expect(merge).toContain('href="/mix-pdf-pages/"');
		const mix = files.get("mix-pdf-pages/index.html") ?? "";
		expect(mix).toContain('href="/merge-pdf/"');
	});

	it("writes an absolute sitemap straight from the registry", () => {
		const sitemap = files.get("sitemap.xml") ?? "";
		for (const tool of TOOLS) {
			expect(sitemap).toContain(`<loc>${SITE_ORIGIN}/${tool.slug}/</loc>`);
		}
		expect(sitemap).not.toContain("<loc>/");
		expect(renderSitemap([tool()])).toContain(`<loc>${SITE_ORIGIN}/merge-pdf/</loc>`);
	});

	it("points robots.txt at the sitemap", () => {
		expect(files.get("robots.txt")).toBe(renderRobots());
		expect(renderRobots()).toContain(`Sitemap: ${SITE_ORIGIN}/sitemap.xml`);
	});
});

describe("the registry checks are real failures", () => {
	it("accepts the shipped registry", () => {
		expect(validateRegistry(TOOLS, TOOL_GROUPS)).toEqual([]);
	});

	it("rejects a duplicate slug", () => {
		const problems = validateRegistry(
			[tool(), tool({ id: "mix", title: "Mix", blurb: "Interleave." })],
			TOOL_GROUPS,
		);
		expect(problems.join("\n")).toContain("duplicate slug: merge-pdf");
	});

	it("rejects a slug that is not lower-case ASCII hyphenated", () => {
		const problems = validateRegistry([tool({ slug: "Merge_PDF" })], TOOL_GROUPS);
		expect(problems.join("\n")).toContain('slug "Merge_PDF"');
	});

	it("rejects a slug that collides with a top-level path", () => {
		expect(Object.hasOwn(RESERVED_TOP_LEVEL, "assets")).toBe(true);
		const problems = validateRegistry([tool({ slug: "assets" })], TOOL_GROUPS);
		expect(problems.join("\n")).toContain("collides with a top-level path");
	});

	it("rejects a description shared by two pages", () => {
		const problems = validateRegistry(
			[
				tool(),
				tool({
					id: "mix",
					slug: "mix-pdf-pages",
					title: "Mix pages",
					blurb: "Combine several PDFs into one.",
				}),
			],
			TOOL_GROUPS,
		);
		expect(problems.join("\n")).toContain("description is identical");
	});

	it("rejects a group or related id that does not exist", () => {
		expect(validateRegistry([tool({ group: "nope" })], TOOL_GROUPS).join("\n")).toContain(
			'group "nope"',
		);
		expect(validateRegistry([tool({ related: ["mix"] })], TOOL_GROUPS).join("\n")).toContain(
			'related tool "mix" does not exist',
		);
	});

	it("rejects a document with no title or heading", () => {
		expect(() => assertPageShape("<p>hi</p>", "x")).toThrow(/lacks a <title> and an <h1>/);
		expect(() => assertPageShape("<title>X</title>", "x")).toThrow(/lacks a <title>/);
		expect(() => assertPageShape("<h1>X</h1>", "x")).toThrow(/lacks a <title>/);
		expect(() => assertPageShape("<!doctype html><title>X</title><h1>X</h1>", "x")).not.toThrow();
	});
});
