/**
 * SL-4.UI.11 — the message *template syntax*, as data.
 *
 * This module is the smallest thing that can honestly be called "one
 * implementation of the i18n rules", and it is deliberately separate from
 * `runtime.ts` for three reasons that all turned out to matter:
 *
 * 1. **It is pure.** `interpolate` and `pluralCategory` are functions of
 *    (template, values, locale) with no catalogue and no state, so every rule
 *    below is unit-testable without constructing a runtime, and a caller that
 *    only needs formatting (a factory binding a catalogue) can take this file
 *    alone.
 * 2. **The extension can ship it.** `apps/extension` reuses `apps/ui` by
 *    relative path and every row it takes is a decision in its ship list
 *    (`REUSE.md`, `PACKAGE_ENTRIES`). A runtime that dragged the viewer's
 *    catalogues in with it would put the page-list and search string sets into a
 *    package with no page list — the trade `REUSE.md` already refused once.
 *    `i18n-boundary.test.ts` is the gate that keeps that true.
 * 3. **The syntax is a decision, and a decision wants its own file.** What the
 *    syntax is, what it refuses, and why it is not ICU are arguments about
 *    *this* file; burying them in the runtime's doc comment would make them the
 *    runtime's opinion rather than the syntax's.
 *
 * ## No dependency, and why that is not a compromise
 *
 * ADR-P0021 (the dependency licence policy) and ADR-P0009 make the dependency
 * set a feature: this repo ships no runtime JavaScript dependency at all. The
 * plan's ERR.04 says user messages resolve through "Fluent (or ICU
 * MessageFormat)" keys — both of which are dependencies — so UI.11 implements
 * the **subset** of that idea the product actually uses, on `Intl`, which is a
 * built-in and therefore not a dependency. What that buys and what it costs is
 * written out below rather than left for a reader to infer.
 *
 * ## The syntax
 *
 * | Form | Meaning |
 * |---|---|
 * | `{name}` | substitute the value, by name, unformatted |
 * | `{name, number}` | substitute it as a locale-formatted number |
 * | `{name, count}` | select a plural variant on it (only inside a {@link PluralMessage}) |
 *
 * Substitution is **by name**, never by position. That is the property the
 * existing catalogues were written for and the reason most languages need: a
 * translator who moves `{total}` in front of `{page}` has expressed their
 * language's word order, and the code does not change.
 *
 * A catalogue value is either a `string` or a {@link PluralMessage}. Both are
 * valid everywhere, so every catalogue that exists today — the three in
 * `viewer/strings.ts`, the extension's in `options-strings.ts` — is a valid
 * catalogue for this syntax unchanged. Nothing had to be rewritten to adopt it.
 *
 * ### What it deliberately does not do
 *
 * - **No nested selects, no offsets, no `#`, no escaping, no rich text.** The
 *   things ICU exists for are the things a viewer chrome does not need, and a
 *   partial ICU parser is a parser with the ICU bug surface and none of the
 *   tooling. A catalogue that needs one of them has outgrown this file, and the
 *   honest response then is a dependency proposal (licence + maintenance
 *   justification), not a fifth special case here.
 * - **No gender or case.** A viewer has no sentence whose grammatical gender is
 *   unknown; the `Intl.PluralRules`-driven form below covers the countable noun
 *   case that actually appears ("{count} matches").
 * - **No translation-time logic.** A template is data; it is not evaluated.
 */

/** A value a `{placeholder}` can take. */
export type MessageValue = string | number;

/** The values a template's placeholders may take. */
export type MessageValues = Readonly<Record<string, MessageValue>>;

/** The plural categories CLDR defines, and the ones `Intl` reports. */
export type PluralCategory = "zero" | "one" | "two" | "few" | "many" | "other";

/**
 * A countable message: one template per plural category the locale uses.
 *
 * `count` names the value the category is selected on, and the variant chosen
 * is the one `Intl.PluralRules` names for the locale — which is why English
 * needs `one`/`other`, Polish needs `one`/`few`/`many`/`other` and Arabic needs
 * all six. Writing the categories a language actually uses is the point: a
 * runtime that hard-coded `count === 1` would be an English plural rule wearing
 * a locale parameter.
 *
 * `other` is required by CLDR for every locale, so it is required here. The
 * other five are optional, and a locale that needs one and does not supply it
 * falls back to `other` — the fallback is reported, never silent (see
 * `runtime.ts`), because "this translation never shows its `few` form" is a bug
 * a translator cannot find by reading the catalogue.
 */
export interface PluralMessage {
	/** The placeholder name the plural is selected on, e.g. `"count"`. */
	readonly count: string;
	/** Every category except `other`, which is required. */
	readonly zero?: string;
	readonly one?: string;
	readonly two?: string;
	readonly few?: string;
	readonly many?: string;
	/** The CLDR-required catch-all, and the fallback for an absent category. */
	readonly other: string;
}

/** One catalogue entry: a plain template, or a countable one. */
export type MessageTemplate = string | PluralMessage;

/** A placeholder, with or without a `, type` suffix. */
const PLACEHOLDER = /\{(\w+)(?:,\s*(\w+))?\}/g;

/** The plural categories in CLDR order, `other` last and always present. */
const PLURAL_CATEGORIES: readonly (keyof PluralMessage)[] = [
	"zero",
	"one",
	"two",
	"few",
	"many",
	"other",
];

/**
 * Every *variant text* of a countable message.
 *
 * `count` is deliberately not one of them: it is the placeholder's **name**, not
 * a sentence, and treating it as text is how a blankness check ends up reporting
 * that a template is not blank because it contains the word `count`.
 */
function* variants(message: PluralMessage): Generator<string> {
	for (const category of PLURAL_CATEGORIES) {
		const variant = message[category];
		if (typeof variant === "string") {
			yield variant;
		}
	}
}

/**
 * Every `{placeholder}` a template asks for, as the text `{name, type}`.
 *
 * Exported because "which values can this key be given" is a question each
 * catalogue's own test asks, and answering it by re-implementing the pattern in
 * every one of them is how four copies of the grammar drift.
 */
export function placeholdersIn(template: MessageTemplate): readonly string[] {
	return typeof template === "string"
		? foundIn(template)
		: [...foundIn(template.count), ...[...variants(template)].flatMap(foundIn)];
}

/** The placeholder names a template names, without the `, type` suffix. */
export function placeholderNamesIn(template: MessageTemplate): readonly string[] {
	const names = (text: string): string[] =>
		[...text.matchAll(PLACEHOLDER)].map((match) => match[1] ?? "");
	return typeof template === "string"
		? names(template)
		: [...names(template.count), ...[...variants(template)].flatMap(names)];
}

/** The `{…}` occurrences of one run of text, in order. */
function foundIn(text: string): string[] {
	return [...text.matchAll(PLACEHOLDER)].map((match) => match[0]);
}


/**
 * The plural category a count falls into, in `locale`.
 *
 * Wraps `Intl.PluralRules` so there is one place that knows a count is selected
 * by *rule* rather than by `n === 1`, and one place the rules object is built.
 * Returns `"other"` for a value that is not a finite number, because
 * `PluralRules.select` throws on `NaN` and a message that renders at all is
 * worth more than a message that is exactly right about `NaN`.
 */
export function pluralCategory(count: number, locale: string): PluralCategory {
	if (!Number.isFinite(count)) {
		return "other";
	}
	return new Intl.PluralRules(locale).select(count) as PluralCategory;
}

/** The variant `locale` uses for `count`, falling back to `other`. */
export function selectPluralVariant(
	message: PluralMessage,
	count: number,
	locale: string,
): string {
	const category = pluralCategory(count, locale);
	if (category !== "other") {
		const variant = message[category];
		if (typeof variant === "string" && variant.length > 0) {
			return variant;
		}
	}
	return message.other;
}

/**
 * A number as `locale` writes it.
 *
 * This is the whole of number localisation: grouping separators, decimal
 * separators and digit shaping belong to the locale, not to the code. A viewer
 * that builds a byte count by string concatenation has English grouping in a
 * German page, and the fix is a resource change only if the runtime owns this.
 */
export function formatNumber(value: number, locale: string): string {
	return new Intl.NumberFormat(locale).format(value);
}


/**
 * Fill a template's placeholders from `values`.
 *
 * The three rules, in the order they matter:
 *
 * 1. **By name.** `values` is keyed, so a template may reorder.
 * 2. **An unknown placeholder is left in place.** `{totl}` renders as `{totl}`,
 *    not as an empty gap. Substituting `""` would produce an `aria-label` that
 *    reads as nothing to a screen reader while looking correct to a reviewer
 *    skimming the DOM; a catalogue typo should be loud in the one place a human
 *    looks. This rule predates UI.11 and is unchanged by it.
 * 3. **`{name, number}` is opt-in.** Plain `{name}` is `String(value)`, exactly
 *    as UI.02/UI.05/UI.06 have always formatted it, so adopting this runtime
 *    changes what no existing catalogue renders. A template that wants the
 *    locale's grouping asks for it by name.
 *
 * @param locale - BCP 47 tag, used for `{name, number}` and for a
 *   {@link PluralMessage}'s category.
 * @param onUnknown - called once per placeholder the template names and
 *   `values` does not carry. The runtime uses it to report a catalogue that
 *   cannot be filled; a caller with no interest in the report passes nothing.
 */
export function interpolate(
	template: MessageTemplate,
	values: MessageValues = {},
	locale = "en",
	onUnknown?: (placeholder: string) => void,
): string {
	if (typeof template !== "string") {
		const raw = values[template.count];
		const count = typeof raw === "number" ? raw : Number.NaN;
		const variant = selectPluralVariant(template, count, locale);
		// A count that is absent, or present but not a finite number, is
		// **unknown** rather than zero. `other` is the right sentence for
		// "however many", but the count itself has nothing honest to print, so it
		// goes through the same unknown-placeholder rule as any other absent
		// value: the live region says "{n} matches" rather than "NaN matches",
		// and `onUnknown` reports the key as unfillable.
		const filled = Number.isFinite(count) ? values : without(values, template.count);
		return fill(variant, filled, locale, onUnknown);
	}
	return fill(template, values, locale, onUnknown);
}

/** `values` with one key dropped, so `fill` treats it as absent. */
function without(values: MessageValues, name: string): MessageValues {
	const copy: Record<string, MessageValue> = { ...values };
	delete copy[name];
	return copy;
}

function fill(
	template: string,
	values: MessageValues,
	locale: string,
	onUnknown: ((placeholder: string) => void) | undefined,
): string {
	return template.replace(PLACEHOLDER, (whole, name: string, kind: string | undefined) => {
		if (!(name in values)) {
			onUnknown?.(whole);
			return whole;
		}
		const value = values[name];
		// `count` inside a variant is the count, formatted like any other number
		// the template asks for by name — its category was already chosen above.
		return kind === "number" && typeof value === "number"
			? formatNumber(value, locale)
			: String(value);
	});
}

/** Narrow an unknown value to a {@link MessageTemplate}. */
export function isMessageTemplate(value: unknown): value is MessageTemplate {
	if (typeof value === "string") {
		return true;
	}
	if (typeof value !== "object" || value === null) {
		return false;
	}
	const candidate = value as Partial<PluralMessage>;
	return typeof candidate.count === "string" && typeof candidate.other === "string";
}

/**
 * Whether a template carries any text at all.
 *
 * "Blank is not a translation" is a rule the runtime enforces, and it needs a
 * definition: a template that is empty, or only whitespace, or whose every
 * plural variant is blank, renders as nothing — and a locale that ships nothing
 * has not translated the key, it has *hidden* it, which is the one outcome
 * worse than falling back to the source text.
 */
export function isBlankTemplate(template: MessageTemplate): boolean {
	if (typeof template === "string") {
		return template.trim().length === 0;
	}
	return [...variants(template)].every((variant) => variant.trim().length === 0);
}

/** The `other` variant of a countable message, or the template itself. */
export function sourceTextOf(template: MessageTemplate): string {
	return typeof template === "string" ? template : template.other;
}

