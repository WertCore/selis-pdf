/**
 * SL-4.UI.11 — the pseudo-locale gate: "Ship English; wire pseudo-locale into CI".
 *
 * This is the DoD's second half, and it is a *build* gate rather than a setting,
 * so it runs in `pnpm -r test` and fails in the same pass everything else fails
 * in. What it asserts, and what each one prevents:
 *
 * | Assertion | The failure it prevents |
 * |---|---|
 * | the pseudo catalogue covers every declared key | a gap in the detector, which would be indistinguishable from a gap in the product |
 * | every rendered viewer string is bracketed and accented | a user-visible literal typed beside the geometry — what ADR-P0034 forbids and `strings.test.ts` hunts for in source; this sees it from the rendered side |
 * | placeholders survive the transform | a pseudo build that cannot format, which would tell you nothing about layout |
 * | a pseudo string is never shorter than the English | a detector that only changes glyphs cannot expose a label that no longer fits |
 * | `gaps()` is empty | an English string reaching a "translated" build by a route the transform did not touch |
 * | nothing renders as the missing marker | a key no locale has, which strict mode would already have thrown on |
 *
 * What it does **not** assert, stated plainly because a gate that overstates
 * itself is worse than no gate: it cannot see a string built by string
 * concatenation out of translated fragments, and it cannot see a component that
 * never asks the runtime at all. The source scan in `viewer/strings.test.ts`
 * covers the first; the second is the argument for a closed key union per
 * component, which is a type rather than a test.
 */

import { describe, expect, it } from "vitest";
import {
	EN_VIEWER_CATALOGUE,
	SEARCH_MESSAGE_KEYS,
	VIEWER_MESSAGE_KEYS,
	createPseudoViewerStrings,
	createViewerMessageRuntime,
} from "../viewer/strings.js";
import { placeholdersIn } from "./message.js";
import { createPseudoCatalogue } from "./pseudo-locale.js";
import { PSEUDO_LOCALE, createMessageRuntime, isMissingMessage } from "./runtime.js";

const PSEUDO = createPseudoCatalogue(EN_VIEWER_CATALOGUE);

/**
 * The template type, derived from the data rather than imported:
 * `MessageTemplate` is declared inside `runtime.ts` and not re-exported, and
 * widening the module's public surface for a test's convenience is the wrong
 * trade. `NonNullable` cancels the `| undefined` that `noUncheckedIndexedAccess`
 * puts on an index access — the catalogues are keyed by *literal* key unions,
 * so the element type is reached through `keyof`, not through `[string]`.
 */
type Template = NonNullable<(typeof PSEUDO)[keyof typeof PSEUDO]>;

/** Values that fill every placeholder the viewer's catalogues declare. */
const VALUES: Readonly<Record<string, string | number>> = {
	count: 1,
	current: 1,
	host: "example.com",
	mode: "fit page",
	page: 1,
	pageLabel: "Page 1 of 9",
	shown: 1,
	title: "Chapter 1",
	total: 9,
	url: "https://example.com/a.pdf",
};

/** Every string the three components can put in front of a user. */
function renderedStrings(): readonly string[] {
	const viewer = createPseudoViewerStrings();
	return [
		viewer.pageList.pageLabel(0, 12),
		viewer.pageList.announcement(0, 12, "page"),
		viewer.pageList.announcement(4, 12, "width"),
		viewer.pageList.announcement(4, 12, "spread"),
		viewer.pageList.regionLabel,
		viewer.pageList.listLabel,
		viewer.search.fieldLabel,
		viewer.search.fieldPlaceholder,
		viewer.search.regionLabel,
		viewer.search.idle(),
		viewer.search.scanning(3),
		viewer.search.found(0),
		viewer.search.found(1),
		viewer.search.found(12345),
		viewer.search.none(),
		viewer.search.match(3, 12, "Page 7 of 900"),
		viewer.search.failed(),
		viewer.search.nextLabel,
		viewer.search.previousLabel,
		viewer.search.closeLabel,
		viewer.search.caseLabel,
		viewer.search.wordLabel,
		viewer.navigation.regionLabel,
		viewer.navigation.outlineLabel,
		viewer.navigation.empty(),
		viewer.navigation.unavailable(),
		viewer.navigation.truncated(10, 400),
		viewer.navigation.row("Chapter 1", "Page 3 of 900"),
		viewer.navigation.row("Chapter 1", null),
		viewer.navigation.moved("Page iv"),
		viewer.navigation.thumbnailsLabel,
		viewer.navigation.thumbnailItem("Page 2 of 900"),
		viewer.navigation.externalTitle,
		viewer.navigation.externalBody("https://example.com/a.pdf", "example.com"),
		viewer.navigation.externalConfirm,
		viewer.navigation.externalCancel,
		viewer.navigation.refusal("launch"),
		viewer.navigation.refusal("scheme"),
		viewer.navigation.refusal("no-target"),
		// A reason `links.ts` does not define yet: the generic refusal, which
		// must itself be a real sentence rather than a blank.
		viewer.navigation.refusal("something-new"),
	];
}

describe("the pseudo catalogue", () => {
	it("covers every key the viewer declares", () => {
		// Total by construction, and that is the point: a gap in the *detector*
		// would look exactly like a gap in the product.
		expect(Object.keys(PSEUDO).sort()).toEqual([...VIEWER_MESSAGE_KEYS].sort());
	});

	it("makes every literal visibly non-English, and brackets it", () => {
		for (const [key, template] of Object.entries(PSEUDO)) {
			const text = typeof template === "string" ? template : template.other;
			expect(text.startsWith("⟦"), key).toBe(true);
			expect(text.endsWith("⟧"), key).toBe(true);
			// At least one accented letter: a template of only digits, or of only
			// characters the map does not touch, would pass a bracket check while
			// telling the reader nothing.
			expect(text, key).toMatch(/[áéíóúñšžçÁÉÍÓÚ]/);
		}
	});

	it("is never shorter than the English it came from", () => {
		// The point of a pseudo-locale is to be *wider*: a label that no longer
		// fits is the defect it exists to surface, and a transform that shortened
		// text would hide exactly that.
		for (const key of VIEWER_MESSAGE_KEYS) {
			const source = EN_VIEWER_CATALOGUE[key];
			const pseudo = PSEUDO[key];
			// `noUncheckedIndexedAccess` is right that an index may be missing,
			// and a missing entry is exactly what this runtime is built to make
			// loud rather than render as an empty string. So assert presence here
			// instead of asserting `| undefined` is a string: a key that vanished
			// from the pseudo catalogue should fail this test by name, not pass
			// a width comparison of 0 against 0.
			expect(pseudo, `pseudo catalogue is missing ${key}`).toBeDefined();
			expect(source, `English catalogue is missing ${key}`).toBeDefined();
			const width = (value: string | { other: string }): number =>
				(typeof value === "string" ? value : value.other).length;
			expect(width(pseudo as string | { other: string }), key).toBeGreaterThanOrEqual(
				width(source as string | { other: string }),
			);
		}
	});

	it("keeps every placeholder name, so a pseudo build still formats", () => {
		for (const key of VIEWER_MESSAGE_KEYS) {
			// Same reasoning as the width test above: assert presence rather
			// than letting `| undefined` through, so a key missing from either
			// catalogue fails this test by name.
			const pseudo = PSEUDO[key];
			const source = EN_VIEWER_CATALOGUE[key];
			expect(pseudo, `pseudo catalogue is missing ${key}`).toBeDefined();
			expect(source, `English catalogue is missing ${key}`).toBeDefined();
			expect(placeholdersIn(pseudo as Template).sort(), key).toEqual(
				placeholdersIn(source as Template).sort(),
			);
		}
	});
});


describe("the pseudo build — CI's half of the DoD", () => {
	it("renders every string through a catalogue", () => {
		const strings = renderedStrings();
		expect(strings.length).toBeGreaterThan(30);
		for (const text of strings) {
			expect(text.length, text).toBeGreaterThan(0);
			expect(text.startsWith("⟦"), `not from a catalogue: ${text}`).toBe(true);
		}
	});

	it("leaves no string reading as English", () => {
		// Spot phrases rather than a general "differs from English" check: the
		// transform is per-character, so the bracketing above is what proves
		// provenance, and this proves the *words* are gone too — a catalogue whose
		// text were somehow passed through would still be bracketed.
		for (const text of renderedStrings()) {
			expect(/Document pages|fit page|Next match|No matches|Moved to/.test(text), text).toBe(
				false,
			);
		}
	});

	it("renders no missing message, because the pseudo locale is total", () => {
		for (const text of renderedStrings()) {
			expect(isMissingMessage(text), text).toBe(false);
		}
	});

	it("reports no gaps at all", () => {
		// The strongest single assertion here: a strict runtime that rendered
		// every string without throwing, plus an empty report, together say that
		// every user-visible string in the viewer is translatable *and* fillable.
		const viewer = createPseudoViewerStrings();
		renderedStrings();
		expect(viewer.runtime.gaps()).toEqual([]);
		expect(viewer.runtime.hasGap()).toBe(false);
	});

	it("resolves every declared key in strict mode without throwing", () => {
		// Every key, not just the ones the helpers above happen to touch: a key
		// nobody renders today is still a key the next task will render, and this
		// is where its absence is found.
		const runtime = createViewerMessageRuntime({
			locale: PSEUDO_LOCALE,
			catalogues: { [PSEUDO_LOCALE]: PSEUDO },
			declaredKeys: VIEWER_MESSAGE_KEYS,
			strict: true,
		});
		for (const key of VIEWER_MESSAGE_KEYS) {
			expect(runtime.t(key, VALUES).length, key).toBeGreaterThan(0);
		}
		expect(runtime.gaps()).toEqual([]);
	});

	it("keeps the runtime's key set equal to the union of the three closed sets", () => {
		// A guard on the guard. `VIEWER_MESSAGE_KEYS` is assembled from three
		// unions, and a key added to one component and not the union would make
		// every test above pass over a key it never looked at.
		const runtime = createMessageRuntime({ catalogues: { en: EN_VIEWER_CATALOGUE } });
		expect(runtime.keys).toHaveLength(VIEWER_MESSAGE_KEYS.length);
		expect(runtime.namespaces).toEqual(["navigation", "pageList", "search"]);
		for (const key of SEARCH_MESSAGE_KEYS) {
			expect(VIEWER_MESSAGE_KEYS).toContain(key);
		}
	});

	it("says the pseudo locale is the one it registered under", () => {
		// `en-XA` is a real CLDR tag, so the pseudo build resolves through real
		// `Intl` data rather than a special case beside the runtime.
		expect(PSEUDO_LOCALE).toBe("en-XA");
		expect(createPseudoViewerStrings().locale).toBe("en-XA");
	});
});

