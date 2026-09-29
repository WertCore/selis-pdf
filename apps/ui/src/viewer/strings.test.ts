/**
 * The i18n seam (SL-4.UI.02).
 *
 * The ADRs require that "every user-visible string goes through i18n keys from
 * day 1 so a rename is a resource change, not a code change". That is a rule
 * about the *shape* of the code, so the second test here is a gate rather than a
 * behaviour test: it scans the viewer's production sources and fails if a
 * user-facing English literal creeps back in beside the catalogue. It follows
 * the idiom `../platform/platform-globals.test.ts` established for the host seam — a
 * lint-style unit test in the same `pnpm -r test` pass, because Biome has no
 * rule for this and a rule that is only written down is a rule that erodes.
 *
 * SL-4.UI.11 replaces the catalogue lookup with a real runtime; these tests pin
 * the seam it plugs into, not the runtime.
 */

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
	EN_NAVIGATION_CATALOGUE,
	EN_PAGE_LIST_CATALOGUE,
	EN_SEARCH_CATALOGUE,
	NAVIGATION_MESSAGE_KEYS,
	PAGE_LIST_MESSAGE_KEYS,
	SEARCH_MESSAGE_KEYS,
	createNavigationStrings,
	createPageListStrings,
	formatMessage,
	modeKey,
} from "./strings.js";

const here = dirname(fileURLToPath(import.meta.url));

describe("message catalogue", () => {
	it("defaults to English", () => {
		const strings = createPageListStrings();
		expect(strings.pageLabel(0, 12)).toBe("Page 1 of 12");
		expect(strings.pageLabel(11, 12)).toBe("Page 12 of 12");
		expect(strings.regionLabel).toBe("Document pages");
		expect(strings.listLabel).toBe("Pages");
	});

	it("announces the page and the localised fit mode", () => {
		const strings = createPageListStrings();
		expect(strings.announcement(0, 12, "page")).toBe("Page 1 of 12 — fit page");
		expect(strings.announcement(4, 12, "width")).toBe("Page 5 of 12 — fit width");
		expect(strings.announcement(4, 12, "spread")).toBe("Page 5 of 12 — two-up");
	});

	it("lets a host override one string without restating the catalogue", () => {
		const strings = createPageListStrings({ "pageList.mode.spread": "deux pages" });
		expect(strings.announcement(0, 12, "spread")).toBe("Page 1 of 12 — deux pages");
		// The keys it did not override still resolve to English.
		expect(strings.pageLabel(0, 12)).toBe("Page 1 of 12");
		expect(strings.regionLabel).toBe("Document pages");
	});

	it("reorders by name, not by position — the reason placeholders are named", () => {
		// A catalogue that moves {total} first is a language's choice, not a
		// code change; a positional template could not express it.
		const strings = createPageListStrings({
			"pageList.page.label": "{total} / {page}",
			"pageList.page.announcement": "{mode}: {pageLabel}",
		});
		expect(strings.pageLabel(0, 12)).toBe("12 / 1");
		expect(strings.announcement(0, 12, "page")).toBe("fit page: 12 / 1");
	});

	it("leaves an unknown placeholder visible rather than blanking it", () => {
		// A typo'd catalogue should look wrong where a human can see it, not
		// read as an empty label to a screen reader.
		expect(formatMessage("Page {page} of {totl}", { page: 1, total: 9 })).toBe("Page 1 of {totl}");
	});

	it("names every fit mode, and only the three that exist", () => {
		expect(modeKey("page")).toBe("pageList.mode.page");
		expect(modeKey("width")).toBe("pageList.mode.width");
		expect(modeKey("spread")).toBe("pageList.mode.spread");
	});

	it("defines every declared key in the English catalogue", () => {
		const missing = PAGE_LIST_MESSAGE_KEYS.filter(
			(key) => EN_PAGE_LIST_CATALOGUE[key] === undefined,
		);
		expect(missing, `keys with no English text: ${missing.join(", ")}`).toEqual([]);
		const missingSearch = SEARCH_MESSAGE_KEYS.filter(
			(key) => EN_SEARCH_CATALOGUE[key] === undefined,
		);
		expect(missingSearch, `search keys with no English text: ${missingSearch.join(", ")}`).toEqual(
			[],
		);
		const missingNav = NAVIGATION_MESSAGE_KEYS.filter(
			(key) => EN_NAVIGATION_CATALOGUE[key] === undefined,
		);
		expect(missingNav, `navigation keys with no English text: ${missingNav.join(", ")}`).toEqual(
			[],
		);
	});
});

describe("the navigation catalogue (SL-4.UI.06)", () => {
	it("defaults to English", () => {
		const strings = createNavigationStrings();
		expect(strings.regionLabel).toBe("Document navigation");
		expect(strings.outlineLabel).toBe("Outline");
		expect(strings.empty()).toBe("This document has no outline.");
		expect(strings.unavailable()).toContain("host");
		expect(strings.thumbnailsLabel).toBe("Page thumbnails");
	});

	it("says two different things about an absent outline and an absent host", () => {
		const strings = createNavigationStrings();
		// One sentence for both would lie about one of them: "no outline" is a
		// fact about the document, "cannot read" is a fact about the host.
		expect(strings.empty()).not.toBe(strings.unavailable());
	});

	it("puts the document's own label in a row's name, and says so when there is none", () => {
		const strings = createNavigationStrings();
		expect(strings.row("Chapter one", "iv")).toBe("Chapter one, iv");
		const unresolved = strings.row("Broken", null);
		expect(unresolved).toContain("Broken");
		expect(unresolved).not.toBe("Broken");
	});

	it("shows the FULL destination in the prompt, host included", () => {
		const strings = createNavigationStrings();
		const url = "https://example.com/redirect?to=https%3A%2F%2Fevil.example&x=1";
		const body = strings.externalBody(url, "example.com");
		expect(body).toContain(url);
		expect(body).toContain("example.com");
		// No ellipsis, ever: a shortened destination is how a reader approves one
		// URL and is sent to another.
		expect(body).not.toContain("…");
		expect(body).not.toContain("...");
	});

	it("has a distinct sentence for every refusal reason", () => {
		const strings = createNavigationStrings();
		const reasons = [
			"launch",
			"remote-destination",
			"submit-form",
			"import-data",
			"javascript",
			"missing-destination",
			"no-target",
			"unsupported",
		] as const;
		const sentences = reasons.map((reason) => strings.refusal(reason));
		expect(new Set(sentences).size, "two reasons sharing a sentence").toBe(reasons.length);
		for (const sentence of sentences) {
			expect(sentence.length).toBeGreaterThan(0);
		}
	});

	it("never returns a blank refusal, even for a reason it does not know", () => {
		const strings = createNavigationStrings();
		expect(strings.refusal("something-new").length).toBeGreaterThan(0);
	});

	it("names the launch for what it is rather than saying 'blocked'", () => {
		const strings = createNavigationStrings();
		// A reader told what the document tried to do can decide whether they
		// need a different tool. "Link blocked" tells them nothing.
		expect(strings.refusal("launch")).toContain("launch");
	});

	it("lets a host override one string without restating the catalogue", () => {
		const strings = createNavigationStrings({
			"navigation.outline.label": "Inhalt",
			"navigation.link.external.confirm": "Öffnen",
		});
		expect(strings.outlineLabel).toBe("Inhalt");
		expect(strings.externalConfirm).toBe("Öffnen");
		// The keys it did not override still resolve to English.
		expect(strings.regionLabel).toBe("Document navigation");
		expect(strings.thumbnailsLabel).toBe("Page thumbnails");
	});

	it("reorders by name, not by position", () => {
		const strings = createNavigationStrings({
			"navigation.outline.moved": "{pageLabel} — aufgesucht",
		});
		expect(strings.moved("Seite iv")).toBe("Seite iv — aufgesucht");
	});
});

/** Comment-stripped source: prose in a doc comment is not a user-facing string. */
function stripComments(source: string): string {
	return source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/[^\n]*/g, "$1");
}

function collectProductionSources(dir: string): string[] {
	const found: string[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) {
			found.push(...collectProductionSources(path));
		} else if (entry.name.endsWith(".ts") && !entry.name.endsWith(".test.ts")) {
			found.push(path);
		}
	}
	return found;
}

describe("i18n lint gate (SL-4.UI.02)", () => {
	it("keeps user-facing prose out of the viewer's code, in the catalogue", () => {
		const offenders: string[] = [];
		for (const path of collectProductionSources(here)) {
			// `strings.ts` *is* the catalogue; the rule is about everywhere else.
			if (path.endsWith("strings.ts")) {
				continue;
			}
			const source = stripComments(readFileSync(path, "utf8"));
			// A prose literal is a string containing a space and a letter:
			// "fit page" and "Document pages" qualify; "start", "centre",
			// "selis-page-tile--current" and the " " join separator do not.
			for (const match of source.matchAll(/"([^"\n]*)"|'([^'\n]*)'/g)) {
				const literal = match[1] ?? match[2] ?? "";
				if (/ \S*[a-z]/i.test(literal) && /[a-z]/i.test(literal)) {
					offenders.push(`${path.replace(here, "viewer")}: ${literal}`);
				}
			}
		}
		expect(offenders, `inline user-facing strings: ${offenders.join("; ")}`).toEqual([]);
	});

	it("never assigns an aria label from a literal", () => {
		// Single-word labels ("Pages") carry no space, so the scan above cannot
		// see them. This closes that gap structurally: an aria label must come
		// from the catalogue, never from a literal beside the geometry.
		const offenders: string[] = [];
		for (const path of collectProductionSources(here)) {
			if (path.endsWith("strings.ts")) {
				continue;
			}
			const source = stripComments(readFileSync(path, "utf8"));
			for (const match of source.matchAll(/\b\w*[Ll]abel\w*:\s*"/g)) {
				offenders.push(`${path.replace(here, "viewer")}: ${match[0].trim()}`);
			}
		}
		expect(offenders, `aria labels built from literals: ${offenders.join("; ")}`).toEqual([]);
	});
});
