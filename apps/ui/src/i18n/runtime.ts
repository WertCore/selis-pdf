/**
 * SL-4.UI.11 — the i18n runtime the four catalogues were standing in for.
 *
 * `viewer/strings.ts` (page list, search, navigation) and the extension's
 * `options-strings.ts` each built the same four things by hand: a closed key
 * set, an English catalogue, `{name}` interpolation, and a `Partial` override
 * merged over English. UI.02 called that a *seam* and wrote down that UI.11
 * would replace its body with a catalogue lookup. This is that runtime, and
 * this file is where the three questions the seam left open are answered in
 * code rather than in a comment.
 *
 * ## 1. How the per-task catalogues relate to the runtime
 *
 * **One flat key space, and the namespace is the key's own first segment.**
 * `pageList.page.label`, `search.status.found`, `navigation.outline.empty` and
 * `options.welcome.heading` are already namespaced by construction, and they
 * were named that way before this task. Introducing a second level of
 * namespace objects would mean a rename — and a rename is the one thing
 * ADR-P0034 exists to prevent, because it makes it a code change rather than a
 * resource change.
 *
 * So a namespace is not a container, it is a *derived fact*: `namespaceOf(key)`
 * is everything before the first dot, and it exists so that three questions can
 * be asked per component rather than per product —
 *
 * - which keys a locale has not translated yet (`coverage()`);
 * - which keys a component owns, when a shell wants to hand one component's
 *   translations to a page that has no page list;
 * - which namespace a missing key belongs to, in the report.
 *
 * The per-task catalogues stay exactly where they are — next to the component
 * that owns the sentences, with their closed key unions — and a caller
 * *registers* them: `createViewerMessageRuntime()` merges the three into one
 * runtime. Nothing is merged at build time and nothing is generated, because a
 * generated catalogue is a second place for a rename to be applied to one file
 * and not the other.
 *
 * ## 2. What a missing key does
 *
 * Three tiers, in this order, and the middle one is the one that matters:
 *
 * | Tier | Condition | Rendered | Origin | Recorded |
 * |---|---|---|---|---|
 * | 1 | translated in the requested locale | the translation | `requested` | — |
 * | 2 | absent (or blank) there, present in the source locale | the **source** text | `source` | `untranslated` |
 * | 3 | absent everywhere | `⟨missing message: ns.key⟩` | `missing` | `missing` |
 *
 * Tier 2 is the ordinary state of a partially translated locale, and the rule is
 * that a host **never restates English**: a translator overrides the strings
 * they have translated and inherits the rest. That is what the `Partial`
 * overrides the three existing factories already took were doing by hand, and
 * it is now the runtime's resolution order rather than each factory's merge.
 *
 * Tier 3 is the answer to "what does a key with no translation do", and its
 * requirements are in tension: it must be *visible* and it must *identify
 * itself*, it must not be an empty string, and it must not put a raw key in
 * front of a user. The rendering is therefore a marked token —
 * `⟨missing message: search.status.found⟩` — which is none of the four things
 * it must not be:
 *
 * - not empty, so an `aria-label` is never a silent nothing;
 * - not the bare key, so a user is not shown an internal identifier dressed up
 *   as a sentence;
 * - not English, deliberately, and this is the one place the runtime ignores
 *   the locale. A defect marker a translation could turn into a real-looking
 *   sentence stops being a defect marker, and the string has to stay
 *   recognisable in a screenshot of any locale for the report it generates to
 *   be worth anything.
 *
 * Recording it is the other half. `gaps()` returns every key that fell back or
 * vanished, deduped and sorted, and `strict: true` turns a tier-3 lookup into a
 * thrown {@link MissingMessageError} instead. That is not two behaviours for
 * one requirement; it is the requirement split by who is asking. A shipped
 * shell renders the marker, because throwing inside a render path is a blank
 * page and a blank page is the failure this whole design exists to avoid. CI
 * and the tests run `strict`, so the same defect that reaches a user as a
 * marked token reaches a build as a failure.
 *
 * ## 3. How a host supplies another locale
 *
 * In three pieces, each already existing:
 *
 * - **Which locale** — the optional `PlatformAdapter.locale` port
 *   (`platform/adapter.ts`, UI.11), because "what language is this user reading"
 *   is a host fact, and reading it here would mean reading `navigator.language`,
 *   which the host-seam gate forbids. A host with no answer reports no port and
 *   the viewer ships English.
 * - **Which catalogue** — `negotiateLocale()` maps what the host said onto what
 *   the build actually ships: exact tag, then base language, then the source
 *   locale. A build that ships no German and is asked for German gets English,
 *   and *knows it got English* rather than quietly rendering half a page in two
 *   languages.
 * - **Which strings** — the locale's own catalogue, registered as a
 *   `Partial<Catalogue>`. Tier 2 above is what makes partial the normal case
 *   rather than a bug.
 *
 * Persistence is not here: a *chosen* locale is a setting, and settings go
 * through `adapter.storage` like the theme and the density (UI.10's rule), with
 * the port reporting the preference that choice overrides.
 */

import {
	formatNumber,
	interpolate,
	isBlankTemplate,
	isMessageTemplate,
	type MessageTemplate,
	type MessageValues,
	pluralCategory,
} from "./message.js";

/**
 * The locale every build ships and the runtime falls back to.
 *
 * English is a **translation**, not a hardcode, exactly as ERR.04 requires: it
 * is the source catalogue, resolved by the same lookup as any other locale, and
 * a host may register a runtime whose source is a different language.
 */
export const SOURCE_LOCALE = "en";

/**
 * The tag of the pseudo-locale, the one whose whole job is to be wrong.
 *
 * `en-XA` is a real CLDR tag (region `XA`), which is what lets the *same*
 * runtime resolve it with real `Intl` data — plural rules fall back to
 * English's, number formatting to the root locale — so a pseudo build exercises
 * the resolution path rather than a special case beside it.
 */
export const PSEUDO_LOCALE = "en-XA";

/** A message key. Namespaced by its first dotted segment; see the module doc. */
export type MessageKey = string;

/** One locale's templates keyed by message key. A `Partial` is the normal case. */
export type Catalogue = Readonly<Record<MessageKey, MessageTemplate>>;

/** Every locale a build ships, keyed by BCP 47 tag. */
export type CatalogueSet = Readonly<Record<string, Catalogue>>;

/**
 * The shape of a message key: `namespace.segment[.segment…]`, at least one dot.
 *
 * Enforced at construction, loudly, because `namespaceOf` is what coverage
 * reporting and per-component shipping are built on, and a key with no dot
 * silently becomes a namespace of one — a shape no test would notice and no
 * translator could extend.
 */
const KEY_PATTERN = /^[A-Za-z][\w-]*(\.[\w-]+)+$/;

/** Everything before the first dot. */
export function namespaceOf(key: MessageKey): string {
	const dot = key.indexOf(".");
	return dot < 0 ? key : key.slice(0, dot);
}

/** A configuration mistake, found at construction rather than at first render. */
export class I18nConfigurationError extends Error {
	constructor(message: string) {
		super(`[selis:i18n] ${message}`);
		this.name = "I18nConfigurationError";
	}
}

/** A tier-3 lookup, thrown instead of rendered when `strict` is on. */
export class MissingMessageError extends Error {
	/** The key that has no template in any registered locale. */
	readonly key: MessageKey;

	/** The namespaces the build ships, so the report says what *could* have. */
	readonly available: readonly string[];

	constructor(key: MessageKey, locale: string, available: readonly string[]) {
		super(`[selis:i18n] no template for "${key}" in ${locale} or ${SOURCE_LOCALE}`);
		this.name = "MissingMessageError";
		this.key = key;
		this.available = available;
	}
}

/** Where a rendered message came from. */
export type MessageOrigin =
	/** The requested locale's own translation. */
	| "requested"
	/** The source locale's text, because the requested locale has none. */
	| "source"
	/** Nothing had one; the marker was rendered instead. */
	| "missing";

/** One resolved message, with the provenance a caller may want to assert on. */
export interface Resolution {
	readonly key: MessageKey;
	readonly namespace: string;
	/** The locale that was asked for. */
	readonly locale: string;
	readonly text: string;
	readonly origin: MessageOrigin;
}

/** What kind of gap a {@link MessageGap} is. */
export type GapKind =
	/** In the source locale, not in the requested one. Tier 2. */
	| "untranslated"
	/** In no locale at all. Tier 3. */
	| "missing"
	/** Declared in a translation but in no source catalogue: a stale key. */
	| "stale"
	/** The template asks for a value the caller did not pass. */
	| "unfillable"
	/** The locale needs this plural category and the template has no such form. */
	| "no-plural-variant";

/** One thing the runtime could not do, recorded rather than swallowed. */
export interface MessageGap {
	readonly kind: GapKind;
	readonly key: MessageKey;
	readonly namespace: string;
	/** The locale the gap was found in; `SOURCE_LOCALE` for a source gap. */
	readonly locale: string;
	/** Human-readable specifics: the category, or the placeholder. */
	readonly detail?: string;
}

/** How much of one namespace a locale has translated. */
export interface NamespaceCoverage {
	readonly namespace: string;
	/** Keys the source catalogue declares here. */
	readonly declared: number;
	readonly translated: number;
	/** Declared keys this locale has no template for, sorted. */
	readonly untranslated: readonly string[];
}

/** How much a locale has translated, overall and per namespace. */
export interface LocaleCoverage {
	readonly locale: string;
	/** Union of every registered locale's keys: what the build can be asked for. */
	readonly total: number;
	/** Keys with a usable template in this locale. */
	readonly translated: number;
	readonly namespaces: readonly NamespaceCoverage[];
}

/** The runtime: a locale, the catalogues behind it, and what it could not find. */
export interface MessageRuntime {
	/** The locale being rendered. */
	readonly locale: string;
	/** The locale untranslated keys fall back to. */
	readonly sourceLocale: string;
	/** Every locale registered, in the order given. */
	readonly locales: readonly string[];
	/** Every key the build can be asked for: the union of all catalogues. */
	readonly keys: readonly string[];
	/** Every namespace in {@link keys}, sorted. */
	readonly namespaces: readonly string[];
	/** Render a message. The only call a render path needs. */
	t(key: MessageKey, values?: MessageValues): string;
	/** Render a message and report which tier answered. */
	resolve(key: MessageKey, values?: MessageValues): Resolution;
	/** Render a template that is not in any catalogue, under this locale. */
	format(template: MessageTemplate, values?: MessageValues): string;
	/** A number as this locale writes it. */
	number(value: number): string;
	/** Every gap recorded so far, deduped and sorted. */
	gaps(): readonly MessageGap[];
	/** Whether anything was recorded, optionally of one kind only. */
	hasGap(kind?: GapKind): boolean;
	/** How much of the build this locale has translated. */
	coverage(): LocaleCoverage;
	/** The same catalogues, rendered in another locale. */
	withLocale(locale: string): MessageRuntime;
	/**
	 * The same catalogues plus a set of templates for the current locale.
	 *
	 * This is the "host overrides what it has" path, and it merges over the
	 * **requested** locale rather than over the source, so an override handed to
	 * a German runtime corrects the German and not the English.
	 */
	withOverrides(overrides: Catalogue): MessageRuntime;
}

/** Options for {@link createMessageRuntime}. */
export interface MessageRuntimeOptions {
	/**
	 * The locale to render. Must be registered, or construction throws.
	 *
	 * `undefined` is spelled as such rather than left off because the repo
	 * compiles with `exactOptionalPropertyTypes`: a factory forwarding its own
	 * optional options object has to be able to pass "not specified" through as
	 * a value, and an option typed `T` alone cannot carry that.
	 */
	readonly locale?: string | undefined;
	/** The locale to fall back to. Must be registered. Defaults to `en`. */
	readonly sourceLocale?: string | undefined;
	/** Every locale this build ships. */
	readonly catalogues: CatalogueSet;
	/**
	 * The closed key set this build promises, when it has one.
	 *
	 * The viewer has three (`PAGE_LIST_MESSAGE_KEYS` and its siblings), and
	 * declaring them is what makes a **stale** key detectable: a key a
	 * translation file still carries after the code dropped it is reported
	 * instead of sitting there until someone reads the file. With no declared
	 * set the union of the catalogues *is* the key set, and a key that exists in
	 * one locale only is a host adding a string rather than a rename that missed
	 * a file — so nothing is reported.
	 */
	readonly declaredKeys?: readonly MessageKey[] | undefined;
	/**
	 * Throw {@link MissingMessageError} on a tier-3 lookup instead of rendering
	 * the marker. On in CI and in tests; off in a shipped shell.
	 */
	readonly strict?: boolean | undefined;
	/** Called for every gap as it is found, for a host log or a report. */
	readonly onGap?: ((gap: MessageGap) => void) | undefined;
}



/** Normalise a tag for comparison: `de_AT` is `de-at`. */
function canonical(tag: string): string {
	return tag.trim().replaceAll("_", "-").toLowerCase();
}

/** The tag as the catalogue set spells it, or `undefined`. */
function findTag(catalogues: CatalogueSet, wanted: string): string | undefined {
	const target = canonical(wanted);
	return Object.keys(catalogues).find((tag) => canonical(tag) === target);
}

/** How a gap is ordered in `gaps()`: locale, then key, then kind. */
function byGapOrder(left: MessageGap, right: MessageGap): number {
	return (
		left.locale.localeCompare(right.locale) ||
		left.key.localeCompare(right.key) ||
		left.kind.localeCompare(right.kind)
	);
}

/** The dedup key for a gap: everything, including the detail. */
function gapIdentity(gap: MessageGap): string {
	return `${gap.locale} ${gap.kind} ${gap.key} ${gap.detail ?? ""}`;
}

/**
 * Build a runtime over a set of catalogues.
 *
 * Every mistake a caller can make here is a *construction* error rather than a
 * render error, because every one of them is a build-time fact:
 *
 * - a key that is not `namespace.segment`, since coverage and per-component
 *   shipping are derived from that shape;
 * - a catalogue value that is not a template;
 * - a blank template in the **source** locale, which is a catalogue that renders
 *   nothing at all — the one blankness the runtime refuses rather than falls
 *   back from, because there is nothing to fall back to;
 * - a requested or source locale that is not in `catalogues`, reported together
 *   with the tags that are, because "locale not found" with no list is a
 *   guessing game.
 *
 * A blank template in any *other* locale is not an error: it is tier 2, a
 * translation nobody has written yet, and it is recorded as one.
 */
export function createMessageRuntime(options: MessageRuntimeOptions): MessageRuntime {
	const { catalogues, strict = false, onGap } = options;
	const locales = Object.keys(catalogues);
	if (locales.length === 0) {
		throw new I18nConfigurationError("no catalogues were registered");
	}
	const sourceTag = findTag(catalogues, options.sourceLocale ?? SOURCE_LOCALE);
	if (sourceTag === undefined) {
		throw new I18nConfigurationError(
			`source locale "${options.sourceLocale ?? SOURCE_LOCALE}" is not registered; have ${locales.join(", ")}`,
		);
	}
	const requestedTag = findTag(catalogues, options.locale ?? sourceTag);
	if (requestedTag === undefined) {
		throw new I18nConfigurationError(
			`locale "${options.locale ?? sourceTag}" is not registered; have ${locales.join(", ")}`,
		);
	}

	const union = new Set<MessageKey>();
	for (const [tag, catalogue] of Object.entries(catalogues)) {
		for (const [key, template] of Object.entries(catalogue)) {
			if (!KEY_PATTERN.test(key)) {
				throw new I18nConfigurationError(`${tag}: key "${key}" is not namespace.segment`);
			}
			if (!isMessageTemplate(template)) {
				throw new I18nConfigurationError(
					`${tag}: "${key}" is not a string or a plural message`,
				);
			}
			if (tag === sourceTag && isBlankTemplate(template)) {
				throw new I18nConfigurationError(
					`${tag}: "${key}" is blank in the source locale, so it has no text to fall back to`,
				);
			}
			union.add(key);
		}
	}

	// A closed key set, when the caller has one, is what makes a *stale* key
	// detectable at all; see `declaredKeys`.
	const declared = options.declaredKeys;
	const keys = declared === undefined ? [...union].sort() : [...declared].sort();
	const declaredSet = declared === undefined ? null : new Set(declared);
	const namespaces = [...new Set(keys.map(namespaceOf))].sort();

	const seen = new Set<string>();
	const recorded: MessageGap[] = [];
	const record = (gap: MessageGap): void => {
		const identity = gapIdentity(gap);
		if (seen.has(identity)) {
			return;
		}
		seen.add(identity);
		recorded.push(gap);
		onGap?.(gap);
	};

	if (declaredSet !== null) {
		for (const [tag, catalogue] of Object.entries(catalogues)) {
			for (const key of Object.keys(catalogue)) {
				if (tag !== sourceTag && !declaredSet.has(key)) {
					record({
						kind: "stale",
						key,
						namespace: namespaceOf(key),
						locale: tag,
						detail: "not declared by this build",
					});
				}
			}
		}
	}

	/** The template for `key` in `tag`, or `undefined` if absent or blank. */
	const templateIn = (tag: string, key: MessageKey): MessageTemplate | undefined => {
		const template = catalogues[tag]?.[key];
		return template === undefined || isBlankTemplate(template) ? undefined : template;
	};

	const render = (
		tag: string,
		template: MessageTemplate,
		key: MessageKey,
		values: MessageValues,
	): string => {
		// A countable template whose locale needs a category it has no text for
		// renders the `other` form, which is correct output and a translation
		// bug. Recorded here, because the alternative is a language that quietly
		// never shows its `few` sentence and nobody notices until a reviewer who
		// reads that language does.
		if (typeof template !== "string") {
			const raw = values[template.count];
			const count = typeof raw === "number" ? raw : Number.NaN;
			const category = pluralCategory(count, tag);
			if (category !== "other" && (template[category]?.length ?? 0) === 0) {
				record({
					kind: "no-plural-variant",
					key,
					namespace: namespaceOf(key),
					locale: tag,
					detail: category,
				});
			}
		}
		return interpolate(template, values, tag, (placeholder) => {
			record({
				kind: "unfillable",
				key,
				namespace: namespaceOf(key),
				locale: tag,
				detail: placeholder,
			});
		});
	};

	const resolve = (key: MessageKey, values: MessageValues = {}): Resolution => {
		const namespace = namespaceOf(key);
		const own = templateIn(requestedTag, key);
		if (own !== undefined) {
			return {
				key,
				namespace,
				locale: requestedTag,
				text: render(requestedTag, own, key, values),
				origin: "requested",
			};
		}
		const source = templateIn(sourceTag, key);
		if (source !== undefined) {
			// Tier 2. Recorded, because "which strings has this locale not
			// translated" is the question a translator actually asks, and a
			// runtime that answered it by falling back silently would make a half
			// translated build indistinguishable from a whole one.
			if (requestedTag !== sourceTag) {
				record({ kind: "untranslated", key, namespace, locale: requestedTag });
			}
			return {
				key,
				namespace,
				locale: requestedTag,
				text: render(sourceTag, source, key, values),
				origin: "source",
			};
		}
		record({ kind: "missing", key, namespace, locale: requestedTag });
		if (strict) {
			throw new MissingMessageError(key, requestedTag, namespaces);
		}
		return {
			key,
			namespace,
			locale: requestedTag,
			text: missingMessageText(key),
			origin: "missing",
		};
	};

	const coverage = (): LocaleCoverage => {
		const perNamespace = namespaces.map((namespace) => {
			const declaredHere = keys.filter((key) => namespaceOf(key) === namespace);
			const untranslated = declaredHere.filter(
				(key) => templateIn(requestedTag, key) === undefined,
			);
			return {
				namespace,
				declared: declaredHere.length,
				translated: declaredHere.length - untranslated.length,
				untranslated,
			};
		});
		return {
			locale: requestedTag,
			total: keys.length,
			translated: perNamespace.reduce((sum, entry) => sum + entry.translated, 0),
			namespaces: perNamespace,
		};
	};

	const runtime: MessageRuntime = {
		locale: requestedTag,
		sourceLocale: sourceTag,
		locales,
		keys,
		namespaces,
		t: (key, values) => resolve(key, values).text,
		resolve,
		format: (template, values) => interpolate(template, values, requestedTag),
		number: (value) => formatNumber(value, requestedTag),
		gaps: () => [...recorded].sort(byGapOrder),
		hasGap: (kind) => recorded.some((gap) => kind === undefined || gap.kind === kind),
		coverage,
		withLocale: (locale) =>
			createMessageRuntime({
				locale,
				sourceLocale: sourceTag,
				catalogues,
				declaredKeys: declared,
				strict,
				onGap,
			}),
		withOverrides: (overrides) =>
			createMessageRuntime({
				locale: requestedTag,
				sourceLocale: sourceTag,
				// Merged over the *requested* locale, so an override handed to a
				// German runtime corrects the German rather than the English it
				// would have been merged over by the pre-UI.11 factories.
				catalogues: { ...catalogues, [requestedTag]: { ...catalogues[requestedTag], ...overrides } },
				declaredKeys: declared,
				strict,
				onGap,
			}),
	};
	return runtime;
}

/**
 * The marker that opens and closes a missing message.
 *
 * `⟨…⟩` rather than `[…]`, `{…}` or `«…»` for three reasons that all show up in
 * this product: a square bracket collides with the `[hidden]`-style
 * conventions the shipped HTML uses and with a CSS attribute selector a reader
 * might copy; braces collide with the placeholder syntax, so a missing message
 * would look like a template; guillemets are what the engine's own
 * `PseudoMessages` (ERR.04) uses, and this marker must not be confusable with a
 * pseudo-locale string in a screenshot. The angle brackets appear nowhere else
 * in a Selis string.
 */
export const MISSING_MESSAGE_PREFIX = "⟨missing message: ";

/** The `⟩` that closes {@link MISSING_MESSAGE_PREFIX}. */
export const MISSING_MESSAGE_SUFFIX = "⟩";

/**
 * What a key with no template renders as.
 *
 * The documented answer to the question this whole task turns on, and the
 * properties it has to have are argued in the module doc: visible, identifying,
 * never empty, never the bare key, and never localised. Exported so a test, a
 * host log, or a shell that wants to style the marker matches on the marker
 * rather than on a substring of it.
 */
export function missingMessageText(key: MessageKey): string {
	return `${MISSING_MESSAGE_PREFIX}${key}${MISSING_MESSAGE_SUFFIX}`;
}

/** Whether a rendered string is the missing-message marker. */
export function isMissingMessage(text: string): boolean {
	return text.startsWith(MISSING_MESSAGE_PREFIX) && text.endsWith(MISSING_MESSAGE_SUFFIX);
}

/**
 * Map what the host asked for onto a locale this build actually ships.
 *
 * Three steps, in this order, and the order is the argument:
 *
 * 1. **Exact tag**, case- and separator-insensitively: `de-AT` finds `de-AT`.
 * 2. **Base language**: `de-AT` finds `de`. A reader who asked for Austrian
 *    German and is given German is being served their language; a reader given
 *    English is not.
 * 3. **The source locale.** The last step, and it is a real fallback rather than
 *    an error: a build that ships one language is not broken when a user asks
 *    for another, and the caller learns which language it got from the return
 *    value. Returning the source rather than throwing is what lets a shell boot
 *    in every locale and then decide what to say about it.
 *
 * @param requested - the host's tag; `undefined` or empty means "no preference".
 * @param available - the tags the build ships.
 * @param fallback - the tag to return when nothing matches. Defaults to `en`.
 */
export function negotiateLocale(
	requested: string | undefined,
	available: readonly string[],
	fallback: string = SOURCE_LOCALE,
): string {
	if (available.length === 0) {
		return fallback;
	}
	if (requested !== undefined && requested.trim().length > 0) {
		const exact = findTag(Object.fromEntries(available.map((tag) => [tag, {}])), requested);
		if (exact !== undefined) {
			return exact;
		}
		const base = canonical(requested).split("-")[0] ?? "";
		const byLanguage = available.find((tag) => canonical(tag).split("-")[0] === base);
		if (byLanguage !== undefined) {
			return byLanguage;
		}
	}
	return available.find((tag) => canonical(tag) === canonical(fallback)) ?? available[0] ?? fallback;
}

/** A `key -> text` function: the shape a DOM fill and a factory both want. */
export type Formatter = (key: MessageKey, values?: MessageValues) => string;

/**
 * Bind a runtime into a `key -> text` function.
 *
 * This is the seam the extension's options page and every `createXStrings`
 * factory resolve against, and it is deliberately one line: a formatter that
 * carried its own fallback logic would be a second implementation of the
 * resolution order, which is the thing this runtime exists to have exactly one
 * of.
 */
export function createFormatter(runtime: MessageRuntime): Formatter {
	return (key, values) => runtime.t(key, values);
}

