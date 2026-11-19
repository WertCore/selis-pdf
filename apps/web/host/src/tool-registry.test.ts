/**
 * The registry is the one table the page, the generator and the worker read.
 *
 * These tests are the drift alarm: a card with no group, a slug that cannot be
 * a path, a related id that points at nothing, or two pages carrying the same
 * description is a build failure rather than a page discovered after deploy.
 */

import { describe, expect, it } from "vitest";
import {
	SITE_ORIGIN,
	TOOLS,
	TOOL_BY_ID,
	TOOL_BY_SLUG,
	TOOL_GROUPS,
	TOOL_PAGE_SLUGS,
	spellOut,
} from "./tool-registry.js";

const SLUG_PATTERN = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

describe("registry shape", () => {
	it("holds every card the grid renders", () => {
		// The browser harness names the same count. A change here without the
		// harness and the offline precache changing is a tool nobody exercises.
		expect(TOOLS.length).toBe(51);
		expect(TOOL_PAGE_SLUGS.length).toBe(TOOLS.length);
	});

	it("has unique ids and unique, path-safe slugs", () => {
		const ids = TOOLS.map((tool) => tool.id);
		const slugs = TOOLS.map((tool) => tool.slug);
		expect(new Set(ids).size).toBe(ids.length);
		expect(new Set(slugs).size).toBe(slugs.length);
		for (const tool of TOOLS) {
			expect(tool.slug, tool.id).toMatch(SLUG_PATTERN);
			// The in-app id and the public slug are different names for a
			// reason; an underscore in a slug is the mistake this catches.
			expect(tool.slug.includes("_"), tool.id).toBe(false);
		}
	});

	it("maps both directions without losing an entry", () => {
		expect(TOOL_BY_ID.size).toBe(TOOLS.length);
		expect(TOOL_BY_SLUG.size).toBe(TOOLS.length);
		for (const tool of TOOLS) {
			expect(TOOL_BY_ID.get(tool.id)).toBe(tool);
			expect(TOOL_BY_SLUG.get(tool.slug)).toBe(tool);
		}
	});

	it("has a group for every card, and a card for every section", () => {
		const groupIds = TOOL_GROUPS.map((group) => group.id);
		expect(new Set(groupIds).size).toBe(groupIds.length);
		for (const tool of TOOLS) {
			expect(groupIds, tool.id).toContain(tool.group);
		}
		for (const group of TOOL_GROUPS) {
			expect(
				TOOLS.some((tool) => tool.group === group.id),
				`empty section: ${group.id}`,
			).toBe(true);
		}
	});

	it("points every related link at a real tool, never at itself", () => {
		for (const tool of TOOLS) {
			for (const related of tool.related ?? []) {
				expect(related, tool.id).not.toBe(tool.id);
				expect(TOOL_BY_ID.has(related), `${tool.id} -> ${related}`).toBe(true);
			}
		}
	});

	it("gives every page a title and a description that is not another page's", () => {
		const titles = TOOLS.map((tool) => tool.title);
		const descriptions = TOOLS.map((tool) => tool.blurb);
		expect(new Set(titles).size).toBe(titles.length);
		expect(new Set(descriptions).size).toBe(descriptions.length);
		for (const tool of TOOLS) {
			expect(tool.title.trim(), tool.id).not.toBe("");
			expect(tool.blurb.trim(), tool.id).not.toBe("");
			expect(tool.note.trim(), tool.id).not.toBe("");
		}
		expect(SITE_ORIGIN).toBe("https://pdf.selis.com");
	});
});

describe("the compare card", () => {
	it("is registered as a two-PDF, local-only card that runs in its own chunk", () => {
		const card = TOOL_BY_ID.get("compare");
		expect(card).toBeDefined();
		expect(card?.slug).toBe("compare-pdfs");
		expect(TOOL_BY_SLUG.get("compare-pdfs")).toBe(card);
		expect(card?.group).toBe("organise");
		expect(card?.accepts).toBe("pdf");
		expect(card?.multiple).toBe(true);
		expect(card?.minimum).toBe(2);
		expect(card?.related).toContain("merge");
		expect(card?.blurb).toContain("on this device");
		expect(TOOL_PAGE_SLUGS).toContain("compare-pdfs");
	});
});

describe("the subset-fonts card", () => {
	it("is registered as a single-PDF, local-only repair card on op 41", () => {
		const card = TOOL_BY_ID.get("subset_fonts");
		expect(card).toBeDefined();
		expect(card?.slug).toBe("subset-pdf-fonts");
		expect(TOOL_BY_SLUG.get("subset-pdf-fonts")).toBe(card);
		expect(card?.group).toBe("repair");
		expect(card?.accepts).toBe("pdf");
		expect(card?.multiple).toBe(false);
		expect(card?.minimum).toBeUndefined();
		// The card's own description says the file stays on the device; the
		// request recorder is the mechanical check behind that claim.
		expect(card?.blurb).toContain("never leaves it");
		expect(TOOL_PAGE_SLUGS).toContain("subset-pdf-fonts");
	});
});

describe("the linearize card", () => {
	it("is registered as a single-PDF, local-only repair card on op 43", () => {
		const card = TOOL_BY_ID.get("linearize");
		expect(card).toBeDefined();
		expect(card?.slug).toBe("linearize");
		expect(TOOL_BY_SLUG.get("linearize")).toBe(card);
		expect(card?.group).toBe("repair");
		expect(card?.accepts).toBe("pdf");
		expect(card?.multiple).toBe(false);
		expect(card?.minimum).toBeUndefined();
		// The card's own description says the file stays on the device; the
		// request recorder is the mechanical check behind that claim.
		expect(card?.blurb).toContain("never leaves it");
		expect(card?.note).toContain("not a qpdf conformance verdict");
		expect(TOOL_PAGE_SLUGS).toContain("linearize");
	});
});

describe("the flatten-forms card", () => {
	it("is registered as a single-PDF, local-only security card on op 44", () => {
		const card = TOOL_BY_ID.get("flatten_forms");
		expect(card).toBeDefined();
		expect(card?.slug).toBe("flatten-pdf-forms");
		expect(TOOL_BY_SLUG.get("flatten-pdf-forms")).toBe(card);
		expect(card?.group).toBe("security");
		expect(card?.accepts).toBe("pdf");
		expect(card?.multiple).toBe(false);
		expect(card?.minimum).toBeUndefined();
		expect(card?.related).toContain("fill_forms");
		expect(TOOL_PAGE_SLUGS).toContain("flatten-pdf-forms");
	});
});

describe("spellOut", () => {
	it("spells the counts a heading is likely to need", () => {
		expect(spellOut(0)).toBe("zero");
		expect(spellOut(43)).toBe("43");
		expect(spellOut(20)).toBe("twenty");
	});
});
