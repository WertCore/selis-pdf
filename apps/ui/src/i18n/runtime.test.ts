/**
 * SL-4.UI.11 — the runtime, asserted against the three questions it answers.
 *
 * The tests are grouped by the question, not by the function, because the
 * questions are what a reviewer and a translator both need answered and the
 * functions are an implementation detail of the answers:
 *
 * 1. **How do the per-task catalogues relate to the runtime?** One flat key
 *    space, namespace derived from the key's first segment, a host's `Partial`
 *    merged over the requested locale.
 * 2. **What does a key with no translation do?** Three tiers, and the middle one
 *    (fall back to the source, record it) is the one a partial locale lives in.
 *    The tier-3 marker is asserted *character for character* on purpose: it is a
 *    documented, user-visible contract, and "roughly the key in brackets" is not
 *    a contract.
 * 3. **How does a host supply another locale?** A tag in, a shipped locale out,
 *    and a build that ships nothing in the reader's language still boots.
 *
 * Everything here is DOM-free and clock-free: no jsdom, no `Intl`-format
 * dependence beyond what Node ships, no platform globals (03-CONVENTIONS §6).
 */

import { describe, expect, it } from "vitest";
import { createMockAdapter } from "../platform/mock-adapter.js";
import type { Catalogue } from "./runtime.js";
import {
	I18nConfigurationError,
	MISSING_MESSAGE_PREFIX,
	MissingMessageError,
	SOURCE_LOCALE,
	createFormatter,
	createMessageRuntime,
	isMissingMessage,
	missingMessageText,
	namespaceOf,
	negotiateLocale,
} from "./runtime.js";

/** A two-namespace source catalogue, small enough to read in a failure. */
const SOURCE: Catalogue = {
	"pageList.page.label": "Page {page} of {total}",
	"pageList.list.label": "Pages",
	"search.status.found": "found",
};

/** A partial German: one key translated, one blank, one absent. */
const GERMAN: Catalogue = {
	"pageList.page.label": "Seite {page} von {total}",
	"pageList.list.label": "   ",
};

function runtime(options: Partial<Parameters<typeof createMessageRuntime>[0]> = {}) {
	return createMessageRuntime({
		catalogues: { en: SOURCE, de: GERMAN },
		declaredKeys: Object.keys(SOURCE),
		...options,
	});
}

describe("namespaces are derived from the key, not declared beside it", () => {
	it("takes the namespace from the first dotted segment", () => {
		expect(namespaceOf("pageList.page.label")).toBe("pageList");
		expect(namespaceOf("options.permission.storage")).toBe("options");
		// A key with no dot is a configuration error, not a namespace of one.
		expect(namespaceOf("orphan")).toBe("orphan");
	});

	it("refuses a key that is not namespace.segment", () => {
		// Caught at construction rather than at first render, because coverage
		// reporting and per-component shipping are both derived from the shape.
		expect(() => createMessageRuntime({ catalogues: { en: { orphan: "text" } } })).toThrow(
			I18nConfigurationError,
		);
		expect(() => createMessageRuntime({ catalogues: { en: { orphan: "text" } } })).toThrow(
			/not namespace.segment/,
		);
	});
});

describe("tier 1 — the locale has the key", () => {
	it("renders its own text and says so", () => {
		const de = runtime({ locale: "de" });
		expect(de.t("pageList.page.label", { page: 1, total: 9 })).toBe("Seite 1 von 9");
		const resolved = de.resolve("pageList.page.label", { page: 1, total: 9 });
		expect(resolved.origin).toBe("requested");
		expect(resolved.namespace).toBe("pageList");
		expect(resolved.locale).toBe("de");
		// A key the locale has, filled with the values it was given, is not a
		// gap of any kind.
		expect(de.hasGap()).toBe(false);
	});

	it("wants an exact tag, and leaves the fuzzy matching to `negotiateLocale`", () => {
		// One layer does the matching, not two. The runtime resolves a tag it was
		// given or refuses it, naming what it has; `negotiateLocale` is the one
		// place that decides `de-AT` means `de`.
		expect(() => runtime({ locale: "DE_at" })).toThrow(I18nConfigurationError);
		expect(runtime({ locale: "DE" }).locale).toBe("de");
	});
});

describe("tier 2 — the locale does not, and that is normal", () => {
	it("falls back to the source text rather than to a blank", () => {
		const de = runtime({ locale: "de" });
		// Absent entirely: a translator who has not got to it yet.
		expect(de.t("search.status.found")).toBe("found");
		expect(de.resolve("search.status.found").origin).toBe("source");
	});

	it("treats a blank translation as no translation at all", () => {
		// `"   "` is not a German sentence, it is a key nobody filled in, and
		// rendering it would put an empty `aria-label` in front of a user. This
		// is the one place a partial locale is allowed to be incomplete, so it
		// has to be handled as incompleteness rather than as content.
		const de = runtime({ locale: "de" });
		expect(de.t("pageList.list.label")).toBe("Pages");
		expect(de.resolve("pageList.list.label").origin).toBe("source");
	});

	it("records the gap once per key however many times it is rendered", () => {
		// A page list re-renders its label on every scroll frame. A gap list that
		// grew with the frame count would be useless as a report and would cost
		// more than the string it describes.
		const de = runtime({ locale: "de" });
		for (let i = 0; i < 50; i += 1) {
			de.t("search.status.found");
		}
		expect(de.gaps().map((gap) => gap.key)).toEqual(["search.status.found"]);
	});

	it("reports the source locale's own gaps as none, so a shipped build is clean", () => {
		expect(runtime().gaps()).toEqual([]);
	});

	it("sorts the report, so two runs produce the same file", () => {
		const de = runtime({ locale: "de" });
		de.t("search.status.found");
		de.t("pageList.list.label");
		expect(de.gaps().map((gap) => `${gap.locale} ${gap.kind} ${gap.key}`)).toEqual([
			"de untranslated pageList.list.label",
			"de untranslated search.status.found",
		]);
	});

	it("hands every gap to the host's reporter as it is found", () => {
		const seen: string[] = [];
		const de = runtime({ locale: "de", onGap: (gap) => seen.push(gap.key) });
		de.t("search.status.found");
		de.t("search.status.found");
		expect(seen).toEqual(["search.status.found"]);
	});
});

describe("coverage: what a locale has actually translated", () => {
	it("counts per namespace against the declared key set", () => {
		// The question a translator asks, asked per component rather than per
		// product: the page list is done, search is not.
		const de = runtime({ locale: "de" });
		const coverage = de.coverage();
		expect(coverage.locale).toBe("de");
		expect(coverage.total).toBe(3);
		expect(coverage.translated).toBe(1);
		expect(coverage.namespaces).toEqual([
			{ namespace: "pageList", declared: 2, translated: 1, untranslated: ["pageList.list.label"] },
			{ namespace: "search", declared: 1, translated: 0, untranslated: ["search.status.found"] },
		]);
	});

	it("reports the source locale as fully translated by definition", () => {
		expect(runtime().coverage().translated).toBe(3);
	});
});

describe("tier 3 — a key nothing has, the documented answer", () => {
	const orphan = "pageList.page.doomed";

	it("renders a marked token that is not empty and not the bare key", () => {
		const en = runtime();
		const text = en.t(orphan);
		expect(text).toBe(`⟨missing message: ${orphan}⟩`);
		expect(text).not.toBe("");
		expect(text).not.toBe(orphan);
		expect(isMissingMessage(text)).toBe(true);
		expect(text.startsWith(MISSING_MESSAGE_PREFIX)).toBe(true);
	});

	it("marks the same way in every locale, on purpose", () => {
		// Deliberate: a defect marker a translation could turn into a
		// real-looking sentence stops being a defect marker, and the string has
		// to stay recognisable in a screenshot of any language for the report it
		// produces to be worth reading.
		expect(runtime({ locale: "de" }).t(orphan)).toBe(`⟨missing message: ${orphan}⟩`);
	});

	it("records the key, its namespace and the locale it was asked for", () => {
		const de = runtime({ locale: "de" });
		de.t(orphan);
		expect(de.gaps()).toEqual([
			{ kind: "missing", key: orphan, namespace: "pageList", locale: "de" },
		]);
		expect(de.hasGap("missing")).toBe(true);
		expect(de.hasGap("untranslated")).toBe(false);
	});

	it("throws instead, when the build asked to be strict", () => {
		// The same defect, split by who is asking: a shell renders the marker
		// because throwing inside a render path is a blank page, and CI throws
		// because a blank page in a build is a failure nobody was looking for.
		const strict = runtime({ strict: true });
		expect(() => strict.t(orphan)).toThrow(MissingMessageError);
		try {
			strict.t(orphan);
		} catch (error) {
			expect(error).toBeInstanceOf(MissingMessageError);
			const missing = error as MissingMessageError;
			expect(missing.key).toBe(orphan);
			// The namespaces the build *does* ship, so the message says what was
			// available rather than only what was not.
			expect(missing.available).toEqual(["pageList", "search"]);
			expect(missing.message).toContain(orphan);
		}
	});

	it("exposes the marker as one function, so nothing re-implements it", () => {
		expect(missingMessageText("a.b")).toBe("⟨missing message: a.b⟩");
		expect(isMissingMessage("Page 1 of 12")).toBe(false);
	});
});

describe("construction refuses what a render must not discover", () => {
	it("refuses an empty catalogue set", () => {
		expect(() => createMessageRuntime({ catalogues: {} })).toThrow(I18nConfigurationError);
	});

	it("names the locales it does have when asked for one it does not", () => {
		// "Locale not found" with no list is a guessing game; the tags are right
		// there in the object the caller built.
		expect(() => runtime({ locale: "fr" })).toThrow(/fr.*en, de/s);
		expect(() => runtime({ sourceLocale: "fr" })).toThrow(/fr.*en, de/s);
	});

	it("refuses a catalogue value that is not a template", () => {
		expect(() =>
			createMessageRuntime({
				catalogues: { en: { "a.b": 7 as unknown as string } },
			}),
		).toThrow(/not a string or a plural message/);
	});

	it("refuses a blank template in the source locale", () => {
		// The one blankness it will not fall back from, because there is nothing
		// to fall back to: this key would render as nothing in every locale.
		expect(() => createMessageRuntime({ catalogues: { en: { "a.b": "  " } } })).toThrow(
			/blank in the source locale/,
		);
	});

	it("allows a blank template anywhere else", () => {
		expect(() =>
			createMessageRuntime({ catalogues: { en: { "a.b": "text" }, de: { "a.b": "" } } }),
		).not.toThrow();
	});
});

describe("stale keys: a rename that missed a file", () => {
	it("reports a key a translation still carries and the build has dropped", () => {
		// Only detectable because the build declared its key set. Without one the
		// union *is* the key set and this is a host adding a string.
		const en = createMessageRuntime({
			catalogues: { en: { "a.b": "text" }, de: { "a.b": "text", "a.renamed": "alt" } },
			declaredKeys: ["a.b"],
		});
		expect(en.gaps()).toEqual([
			{
				kind: "stale",
				key: "a.renamed",
				namespace: "a",
				locale: "de",
				detail: "not declared by this build",
			},
		]);
	});

	it("reports nothing for a key that only the source declares", () => {
		const en = createMessageRuntime({
			catalogues: { en: { "a.b": "text" }, de: {} },
			declaredKeys: ["a.b"],
		});
		expect(en.gaps()).toEqual([]);
	});

	it("reports nothing at all when the build declares no key set", () => {
		const en = createMessageRuntime({
			catalogues: { en: { "a.b": "text" }, de: { "a.extra": "extra" } },
		});
		expect(en.gaps()).toEqual([]);
		expect(en.keys).toEqual(["a.b", "a.extra"]);
	});
});

describe("other gaps the runtime records rather than swallows", () => {
	const COUNTED: Catalogue = {
		"search.status.found": { count: "n", other: "{n} matches" },
		"search.status.match": "Match {current} of {count}",
	};

	it("reports a plural form the locale needs and the translation lacks", () => {
		// Correct output, wrong translation: Polish needs `few` for 2 and this
		// catalogue only has `other`, so the reader gets a sentence in the wrong
		// number and nobody notices until a Polish reviewer does.
		const pl = createMessageRuntime({
			locale: "pl",
			catalogues: { en: COUNTED, pl: { ...COUNTED } },
		});
		expect(pl.t("search.status.found", { n: 2 })).toBe("2 matches");
		expect(pl.gaps()).toEqual([
			{
				kind: "no-plural-variant",
				key: "search.status.found",
				namespace: "search",
				locale: "pl",
				detail: "few",
			},
		]);
	});

	it("does not report it for a category the translation has", () => {
		const pl = createMessageRuntime({
			locale: "pl",
			catalogues: {
				en: COUNTED,
				pl: {
					...COUNTED,
					"search.status.found": { count: "n", few: "{n} mecze", other: "{n} meczu" },
				},
			},
		});
		expect(pl.t("search.status.found", { n: 2 })).toBe("2 mecze");
		expect(pl.gaps()).toEqual([]);
	});

	it("reports a placeholder the caller could not fill, and says which", () => {
		const en = createMessageRuntime({ catalogues: { en: COUNTED } });
		expect(en.t("search.status.match", { current: 1 })).toBe("Match 1 of {count}");
		expect(en.gaps()).toEqual([
			{
				kind: "unfillable",
				key: "search.status.match",
				namespace: "search",
				locale: SOURCE_LOCALE,
				detail: "{count}",
			},
		]);
	});
});

describe("a host supplies another locale", () => {
	it("maps what the host asked for onto what the build ships", () => {
		const available = ["en", "de", "pt-BR"];
		// Exact tag first: pt-BR is not pt-PT, and a reader who asked for one
		// should not silently be given the other.
		expect(negotiateLocale("pt-BR", available)).toBe("pt-BR");
		expect(negotiateLocale("de-AT", available)).toBe("de");
		expect(negotiateLocale("DE", available)).toBe("de");
		expect(negotiateLocale("de_at", available)).toBe("de");
	});

	it("falls back to the source rather than throwing, and says which it chose", () => {
		// A build that ships one language is not broken when a reader asks for
		// another, and the caller learns what it got from the return value.
		expect(negotiateLocale("fr", ["en", "de"])).toBe("en");
		expect(negotiateLocale("fr", ["en", "de"], "de")).toBe("de");
		expect(negotiateLocale(undefined, ["en", "de"])).toBe("en");
		expect(negotiateLocale("   ", ["en"])).toBe("en");
		expect(negotiateLocale("fr", [])).toBe("en");
	});

	it("renders the same catalogues in another locale without rebuilding them", () => {
		const en = runtime();
		const de = en.withLocale("de");
		expect(en.t("pageList.page.label", { page: 1, total: 9 })).toBe("Page 1 of 9");
		expect(de.t("pageList.page.label", { page: 1, total: 9 })).toBe("Seite 1 von 9");
		expect(de.t("pageList.page.label", { page: 1, total: 9 })).toBe("Seite 1 von 9");
		expect(de.sourceLocale).toBe(SOURCE_LOCALE);
		expect(de.locales).toEqual(["en", "de"]);
	});

	it("merges a host's overrides over the requested locale, not over English", () => {
		// The change from the pre-UI.11 factories, and the reason a host with two
		// languages does not have to remember which one its overrides belong to.
		const de = runtime({ locale: "de" });
		const corrected = de.withOverrides({ "pageList.page.label": "Blatt {page} / {total}" });
		expect(corrected.t("pageList.page.label", { page: 1, total: 9 })).toBe("Blatt 1 / 9");
		// The base runtime is untouched: an override is a new runtime, not a
		// mutation, so two panels can disagree on purpose.
		expect(de.t("pageList.page.label", { page: 1, total: 9 })).toBe("Seite 1 von 9");
	});

	it("formats numbers in the runtime's locale", () => {
		expect(runtime({ locale: "de" }).number(12345)).toBe("12.345");
		expect(runtime().number(12345)).toBe("12,345");
	});

	it("binds a key -> text function, which is all a DOM fill needs", () => {
		const format = createFormatter(runtime({ locale: "de" }));
		expect(format("pageList.page.label", { page: 2, total: 9 })).toBe("Seite 2 von 9");
		expect(format("pageList.nope")).toBe("⟨missing message: pageList.nope⟩");
	});
});

describe("the host port (SL-4.UI.11's `PlatformAdapter.locale`)", () => {
	it("reports the host's own preference when the user has chosen nothing", () => {
		const adapter = createMockAdapter({ locale: "de-AT" });
		expect(adapter.locale?.current()).toBe("de-AT");
		// And the shell's answer to that is the two lines this seam exists for:
		// read the port, negotiate it against what the build ships.
		expect(negotiateLocale(adapter.locale?.current(), ["en", "de"])).toBe("de");
	});

	it("lets a user's choice override the host, and tells its subscribers", async () => {
		// The choice is a *setting* (UI.10's rule) and the port is read-only, so
		// the two cannot end up disagreeing about what the reader sees.
		const adapter = createMockAdapter({ locale: "en" });
		const seen: string[] = [];
		const stop = adapter.locale?.onChange((tag) => seen.push(tag));
		await adapter.storage.set("ui.locale", "pl");
		expect(adapter.locale?.current()).toBe("pl");
		expect(seen).toEqual(["pl"]);
		stop?.();
		// Unsubscribed: a later change is the shell's business again, not ours.
		await adapter.storage.set("ui.locale", "de");
		expect(seen).toEqual(["pl"]);
	});

	it("hands the decision back to the host when the setting is deleted", async () => {
		const adapter = createMockAdapter({ locale: "en-GB" });
		await adapter.storage.set("ui.locale", "fr");
		expect(adapter.locale?.current()).toBe("fr");
		await adapter.storage.delete("ui.locale");
		expect(adapter.locale?.current()).toBe("en-GB");
	});

	it("is a port a host may simply not have", () => {
		// Absence is a real state, and it is the same state the contract suite
		// already treats as legitimate for `navigation`. A harness with no answer
		// reports no port, and the viewer ships English.
		//
		// Built by omission rather than `delete`: the property is genuinely
		// absent, which is what a host that never implemented the port looks
		// like, and it keeps the fixture free of a mutation.
		const { locale: _neverDeclared, ...withoutLocale } = createMockAdapter();
		const adapter: { locale?: unknown } = withoutLocale;
		expect(adapter.locale).toBeUndefined();
		expect(negotiateLocale(undefined, ["en"])).toBe("en");
	});
});
