/**
 * SL-4.UI.11 — the pseudo-locale, and why it is a build gate and not a setting.
 *
 * UI.11's DoD is "Ship English; wire pseudo-locale into CI". The first half is
 * a catalogue in this repo; the second half is this file, and the reason it is
 * worth a module is that the pseudo-locale is not a *translation*, it is a
 * **detector**. It answers one question about a build:
 *
 * > Is every string a user can see a string this build can translate?
 *
 * A build rendered in `en-XA` shows accented, bracketed text wherever a message
 * came from a catalogue. Anything still readable as English did not come from
 * one — it is a literal somebody typed beside the geometry, which is precisely
 * what ADR-P0034 says must not exist. That is the check `strings.test.ts` does
 * by scanning source for prose; this is the same property checked from the
 * other end, by rendering. Neither can see the other's blind spot, which is why
 * both exist.
 *
 * ## What the transform must not do
 *
 * - **Not touch a placeholder name.** `{page}` has to stay `{page}` or the
 *   pseudo build fails to format, and a pseudo build that cannot format tells
 *   you nothing about layout.
 * - **Not touch a digit or a non-Latin letter.** The point is to make Latin text
 *   visibly *not Latin*, not to mangle a CJK catalogue into something that
 *   measures differently for an unrelated reason.
 * - **Not be reversible-by-accident.** The bracket is the signal; a screenshot
 *   in a bug report should be unambiguous about which build produced it.
 *
 * ## Why it is a locale and not a flag
 *
 * `en-XA` goes through the same `createMessageRuntime` as any other catalogue,
 * with real `Intl` data behind it. A `--pseudo` boolean would have to
 * reimplement the resolution order to know whether a string was translated, and
 * a detector that reimplements the thing it detects is a detector that agrees
 * with a bug.
 */

import type { MessageTemplate } from "./message.js";
import { PSEUDO_LOCALE, type Catalogue } from "./runtime.js";

/** The tag a pseudo-locale catalogue registers under. */
export { PSEUDO_LOCALE };

/** Accented replacements, chosen to be one glyph and stay readable. */
const ACCENTS: Readonly<Record<string, string>> = {
	a: "á",
	b: "ƀ",
	c: "ç",
	d: "ď",
	e: "é",
	f: "ƒ",
	g: "ġ",
	h: "ĥ",
	i: "í",
	j: "ĵ",
	k: "ķ",
	l: "ľ",
	m: "ɱ",
	n: "ñ",
	o: "ó",
	p: "þ",
	q: "ǫ",
	r: "ŕ",
	s: "š",
	t: "ť",
	u: "ú",
	v: "ṽ",
	w: "ŵ",
	x: "ẋ",
	y: "ý",
	z: "ž",
	A: "Á",
	B: "Ɓ",
	C: "Ç",
	D: "Ď",
	E: "É",
	F: "Ƒ",
	G: "Ġ",
	H: "Ĥ",
	I: "Í",
	J: "Ĵ",
	K: "Ķ",
	L: "Ľ",
	M: "Ṁ",
	N: "Ñ",
	O: "Ó",
	P: "Þ",
	Q: "Ǫ",
	R: "Ŕ",
	S: "Š",
	T: "Ť",
	U: "Ú",
	V: "Ṽ",
	W: "Ŵ",
	X: "Ẋ",
	Y: "Ý",
	Z: "Ž",
};

/** The wrapper that makes a pseudo string unmistakable in a screenshot. */
const OPEN = "⟦";
const CLOSE = "⟧";

	/**
	 * Appended to a literal run that has no letter to accent, so a punctuation-only
	 * run still reads as a translation. Taken from the same map as every other accent, and
	 * string carries.
	 */
	const WIDEN_MARKER = "á";

/** A placeholder, matched so its name is never transformed. */
const PLACEHOLDER = /(\{\w+(?:,\s*\w+)?\})/g;

	/**
	 * Accent one literal run of a template, leaving placeholders alone.
	 *
	 * A run with no ASCII letter to accent still has to come out visibly
	 * non-English: `{pageLabel} - {mode}` is entirely placeholders and an em
	 * dash, so a letters-only transform leaves it byte-identical to the English
	 * and the pseudo string could pass for a real translation -- which is
	 * exactly the failure the pseudo-locale exists to catch. Such a run is widened
	 * with a marker instead; the em-dash keeps its place, so the shape survives.
	 */
	function accent(text: string): string {
		return text
			.split(PLACEHOLDER)
			.map((part) => {
				if (part.startsWith("{")) {
					return part;
				}
				if (part.length === 0 || !/[A-Za-z]/.test(part)) {
					return part.length === 0 ? part : `${part}${WIDEN_MARKER}`;
				}
				return part.replace(/[A-Za-z]/g, (letter) => ACCENTS[letter] ?? letter);
			})
			.join("");
	}

/** Accent and bracket, so a pseudo string cannot pass for a real one. */
function wrap(template: string): string {
	return `${OPEN}${accent(template)}${CLOSE}`;
}

/** One pseudo template: accented, bracketed, and no longer than it was. */
function pseudoTemplate(template: MessageTemplate): MessageTemplate {
	if (typeof template === "string") {
		return wrap(template);
	}
	const variants: {
		count: string;
		zero?: string;
		one?: string;
		two?: string;
		few?: string;
		many?: string;
		other: string;
	} = { count: template.count, other: wrap(template.other) };
	// `other` is the only required variant, so it is set above and the rest are
	// copied only when the source carries them: a pseudo locale must not
	// *invent* a category, or the detector would be asserting a translation
	// exists where the source has none.
	for (const category of ["zero", "one", "two", "few", "many"] as const) {
		const variant = template[category];
		if (variant !== undefined) {
			variants[category] = wrap(variant);
		}
	}
	return variants;
}

/**
 * Derive a pseudo-locale catalogue from a source one.
 *
 * Total by construction: every key in `source` gets a pseudo template, so a
 * pseudo build can never report a missing message. That is deliberate — a gap
 * in the *detector* would be indistinguishable from a gap in the product, and
 * the pseudo-locale's only job is to be complete so that what it reports about
 * anything else is trustworthy.
 */
export function createPseudoCatalogue(source: Catalogue): Catalogue {
	const pseudo: Record<string, MessageTemplate> = {};
	for (const [key, template] of Object.entries(source)) {
		pseudo[key] = pseudoTemplate(template);
	}
	return pseudo;
}
