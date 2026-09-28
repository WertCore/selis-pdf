/**
 * SL-4.UI.11 — the template syntax, asserted rule by rule.
 *
 * Every test here pins a rule that a translator or a reviewer would otherwise
 * have to take on trust, and each one names the failure it prevents. The three
 * that matter most:
 *
 * - **An unknown placeholder is left visible.** This is UI.02's rule and UI.11
 *   did not change it, so it is tested here as well as in
 *   `viewer/strings.test.ts`: a rule that moves house and loses its test is a
 *   rule that is about to change by accident.
 * - **`{name, number}` is opt-in.** If plain `{name}` ever started
 *   locale-formatting, every existing catalogue would silently change what it
 *   renders — "Page 2000 of 9000" would become "Page 2,000 of 9,000" in a test
 *   assertion and in a German page alike, and the first person to find out would
 *   be a user.
 * - **The plural category comes from CLDR, not from `n === 1`.** English and
 *   Polish are asserted together, because a runtime that passes the English case
 *   and fails the Polish one is exactly the runtime that ships a translation
 *   nobody can read.
 */

import { describe, expect, it } from "vitest";
import {
	formatNumber,
	interpolate,
	isBlankTemplate,
	isMessageTemplate,
	placeholderNamesIn,
	placeholdersIn,
	pluralCategory,
	selectPluralVariant,
	sourceTextOf,
} from "./message.js";

describe("placeholder substitution", () => {
	it("substitutes by name, so a catalogue can reorder the sentence", () => {
		// The property most languages need, and the reason placeholders are
		// named: German puts the total first and the code must not care.
		expect(interpolate("{total} / {page}", { page: 1, total: 9 })).toBe("9 / 1");
	});

	it("leaves an unknown placeholder visible rather than blanking it", () => {
		// An `aria-label` that renders as "Page 1 of " reads as nothing to a
		// screen reader, and a screenshot of it looks merely terse.
		const seen: string[] = [];
		const text = interpolate("Page {page} of {totl}", { page: 1, total: 9 }, "en", (ph) =>
			seen.push(ph),
		);
		expect(text).toBe("Page 1 of {totl}");
		expect(seen).toEqual(["{totl}"]);
	});

	it("formats a number only when the template asks for it", () => {
		expect(interpolate("{count} matches", { count: 12345 })).toBe("12345 matches");
		expect(interpolate("{count, number} matches", { count: 12345 })).toBe("12,345 matches");
	});

	it("writes numbers the way the locale does", () => {
		// The point of `{name, number}`: 12345 is "12,345" in English and
		// "12.345" in German, and a hand-rolled separator can only ever be right
		// for one of them.
		expect(formatNumber(12345, "en")).toBe("12,345");
		expect(formatNumber(12345, "de")).toBe("12.345");
	});

	it("leaves a non-numeric value alone even where a number was requested", () => {
		// `{pageLabel, number}` would be a catalogue bug, and String() is the
		// honest rendering of "this was not a number" rather than NaN.
		expect(interpolate("{pageLabel, number}", { pageLabel: "iv" })).toBe("iv");
	});
});

describe("plurals", () => {
	const english = { count: "n", one: "{n} match", other: "{n} matches" };
	const polish = {
		count: "n",
		one: "{n} mecz",
		few: "{n} mecze",
		many: "{n} meczów",
		other: "{n} meczu",
	};

	it("selects the category CLDR gives the locale, not `n === 1`", () => {
		expect(pluralCategory(1, "en")).toBe("one");
		expect(pluralCategory(2, "en")).toBe("other");
		// Polish: 2 is `few` and 5 is `many`, and an English rule gets both wrong.
		expect(pluralCategory(1, "pl")).toBe("one");
		expect(pluralCategory(2, "pl")).toBe("few");
		expect(pluralCategory(5, "pl")).toBe("many");
		expect(pluralCategory(1.5, "pl")).toBe("other");
	});

	it("renders the right variant per locale", () => {
		expect(interpolate(english, { n: 1 }, "en")).toBe("1 match");
		expect(interpolate(english, { n: 4 }, "en")).toBe("4 matches");
		expect(interpolate(polish, { n: 2 }, "pl")).toBe("2 mecze");
		expect(interpolate(polish, { n: 5 }, "pl")).toBe("5 meczów");
	});

	it("falls back to `other` for a category the translation has no form for", () => {
		// A Polish translation with only `one`/`other` still renders — a live
		// region is never left blank — and the runtime reports the absent form as
		// a gap, which `runtime.test.ts` asserts.
		const partial = { count: "n", one: "{n} mecz", other: "{n} meczu" };
		expect(interpolate(partial, { n: 2 }, "pl")).toBe("2 meczu");
		expect(selectPluralVariant(partial, 2, "pl")).toBe("{n} meczu");
	});

	it("renders `other` when the count was not supplied at all", () => {
		// "we do not know how many" is what a caller that has not counted yet
		// means, and `PluralRules.select(NaN)` throws — so this is also the case
		// that would take a live region down if it were not handled.
		expect(interpolate(english, {}, "en")).toBe("{n} matches");
		expect(interpolate(english, { n: Number.NaN }, "en")).toBe("{n} matches");
	});

	it("does not throw on a non-finite count", () => {
		expect(pluralCategory(Number.POSITIVE_INFINITY, "en")).toBe("other");
	});
});

describe("introspection", () => {
	it("lists the placeholders a template needs, across every variant", () => {
		// Across every variant, not just `other`: a language that puts the count
		// in `one` and nothing in `few` still has to be able to fill both, and a
		// caller cannot know that from `other` alone.
		const message = { count: "n", one: "{n} of {total}", other: "{n} of {total}" };
		expect(placeholdersIn(message)).toEqual(["{n}", "{total}", "{n}", "{total}"]);
		expect(placeholderNamesIn(message)).toEqual(["n", "total", "n", "total"]);
		expect(placeholdersIn("{a} and {b, number}")).toEqual(["{a}", "{b, number}"]);
		expect(placeholderNamesIn("{b, number}")).toEqual(["b"]);
	});

	it("recognises a template and rejects anything that is not one", () => {
		expect(isMessageTemplate("text")).toBe(true);
		expect(isMessageTemplate({ count: "n", other: "{n}" })).toBe(true);
		expect(isMessageTemplate({ other: "{n}" })).toBe(false);
		expect(isMessageTemplate(null)).toBe(false);
		expect(isMessageTemplate(7)).toBe(false);
	});

	it("calls a template blank only if every variant of it is", () => {
		// "Blank is not a translation" needs a definition, and this is it: a
		// locale that ships an empty string has hidden the key, which is worse
		// than falling back to the source.
		expect(isBlankTemplate("")).toBe(true);
		expect(isBlankTemplate("   ")).toBe(true);
		expect(isBlankTemplate(" x ")).toBe(false);
		expect(isBlankTemplate({ count: "n", one: " ", other: "\n" })).toBe(true);
		expect(isBlankTemplate({ count: "n", one: " ", other: "matches" })).toBe(false);
	});

	it("reads the `other` variant as a template's source text", () => {
		// What a coverage report quotes, and what a pseudo-locale lengthens: `other`
		// is the CLDR-mandated catch-all, so it is the one a fallback can rely on.
		expect(sourceTextOf("plain")).toBe("plain");
		expect(sourceTextOf({ count: "n", one: "one", other: "other" })).toBe("other");
	});
});

