/**
 * Page labels (SL-4.UI.06) — the document's own page numbering, made of text.
 *
 * A `/PageLabels` number tree (`selis-pdf-doc`'s `page_labels`, SL-1.DOC.08)
 * maps page ranges to a prefix, a numbering style and a starting value. The
 * reader-facing consequence is that "page 4" is a lie in a document whose
 * front matter is numbered `i, ii, iii` and whose body starts again at 1, and
 * that a page-number box which cannot say "iv" is a page-number box a reader
 * cannot use to find the page they were told about.
 *
 * Everything here is pure: ranges in, strings out. It is the same shape as
 * `layout.ts` — geometry with no knowledge of tiles, the shell, or the host —
 * and the reason is the same: it is testable in Node under ADR-P0021's
 * zero-dependency rule, with no jsdom and no engine.
 *
 * ## The rules, and where each one comes from
 *
 * PDF 32000-2:2020 §7.7.3.4 and Table 165:
 * - A page belongs to the range with the **largest** `firstPage` that is still
 *   `<=` the page. Ranges may arrive out of order, may overlap, and may name a
 *   page past the end of the document; none of that is an error.
 * - The sequence value is `/St` (default **1**) plus the page's offset from the
 *   range's first page.
 * - `/S` defaults to `D`. `/P` defaults to nothing.
 * - Alphabetic labels are **bijective base-26**: A…Z, then AA, AB, … So page
 *   27 is `AA`, not `BA`. This is the single most commonly gotten detail.
 * - Roman labels are ordinary roman numerals for 1…3999. §7.7.3.4 leaves larger
 *   values to the viewer; Acrobat writes them as decimal, and so do we — see
 *   {@link MAX_ROMAN}.
 * - A page **before** the first range is labelled with its own number, in
 *   decimal. That is the spec's "there is no label for this page", and a
 *   document that mixes labelled front matter with unlabelled body pages is
 *   rare enough that guessing a range for it would be worse than saying so.
 *
 * ## Why the strings live elsewhere
 *
 * The numbers here are **data**, not prose: "iv" is a document's own token, and
 * a translation must not change it. Every sentence *around* a label goes
 * through the catalogue (`strings.ts`), which is why this file formats no
 * phrases at all — `pageLabelFor()` returns a bare label and the caller words
 * it. That is the same rule search followed, and the same reason.
 */

import type { PageLabelRange, PageLabelStyle } from "../platform/types.js";

/**
 * The largest value roman labels are rendered for. §7.7.3.4 leaves this to
 * the viewer and Adobe's behaviour is decimal above 3999, which is also the
 * point past which a roman numeral is longer than the decimal it replaced.
 */
export const MAX_ROMAN = 3999;

/** The style a range with no `/S` gets, per Table 165. */
export const DEFAULT_LABEL_STYLE: PageLabelStyle = "D";

/** A label range that is sorted, validated, and safe to search. */
export interface NormalisedLabelRange {
	readonly firstPage: number;
	readonly style: PageLabelStyle;
	readonly prefix: string;
	readonly firstValue: number;
}

function isStyle(value: unknown): value is PageLabelStyle {
	return value === "D" || value === "R" || value === "r" || value === "A" || value === "a";
}

/**
 * Sort and validate a transport's ranges.
 *
 * Three things are dropped, and each for a stated reason rather than by
 * tolerance: a non-integer or negative `firstPage` (no page can be before page
 * 0, so the entry cannot apply to anything), an unknown `/S` name (the spec
 * defines five; anything else is a producer bug, and falling back to `D` would
 * silently mislabel a document), and a non-integer `/St` (the sequence would
 * not be a page number).
 *
 * Sorting is by `firstPage`, and a later entry with the *same* first page wins
 * — which is what the spec's "smallest index ≥ the page" lookup degenerates to
 * when two ranges collide, and what a document that redefines a range expects.
 */
export function normaliseLabelRanges(
	ranges: readonly PageLabelRange[] | undefined,
): readonly NormalisedLabelRange[] {
	const usable = (ranges ?? []).flatMap((range) => {
		if (!Number.isInteger(range.firstPage) || range.firstPage < 0) {
			return [];
		}
		if (range.style !== undefined && !isStyle(range.style)) {
			return [];
		}
		if (range.firstValue !== undefined && !Number.isInteger(range.firstValue)) {
			return [];
		}
		return [
			{
				firstPage: range.firstPage,
				style: range.style ?? DEFAULT_LABEL_STYLE,
				prefix: range.prefix ?? "",
				firstValue: range.firstValue ?? 1,
			} satisfies NormalisedLabelRange,
		];
	});
	return usable.sort((left, right) => left.firstPage - right.firstPage);
}

/** The range that owns `page`, or `null` when the document labels none. */
export function rangeForPage(
	ranges: readonly NormalisedLabelRange[],
	page: number,
): NormalisedLabelRange | null {
	let found: NormalisedLabelRange | null = null;
	for (const range of ranges) {
		if (range.firstPage > page) {
			// Sorted ascending, so the first entry past the page ends the scan.
			break;
		}
		found = range;
	}
	return found;
}


/**
 * Roman numerals for 1…{@link MAX_ROMAN}. `0` and negatives return `null`, so
 * the caller decides the fallback rather than this function inventing one.
 */
export function toRoman(value: number): string | null {
	if (!Number.isInteger(value) || value < 1 || value > MAX_ROMAN) {
		return null;
	}
	const table: readonly (readonly [number, string])[] = [
		[1000, "M"],
		[900, "CM"],
		[500, "D"],
		[400, "CD"],
		[100, "C"],
		[90, "XC"],
		[50, "L"],
		[40, "XL"],
		[10, "X"],
		[9, "IX"],
		[5, "V"],
		[4, "IV"],
		[1, "I"],
	];
	let remaining = value;
	let out = "";
	for (const [amount, numeral] of table) {
		while (remaining >= amount) {
			out += numeral;
			remaining -= amount;
		}
	}
	return out;
}

/**
 * Alphabetic labels, bijective base-26: 1 → `A`, 26 → `Z`, 27 → `AA`.
 *
 * Bijective, not plain base-26: with a plain encoding `A…Z` occupy 0…25, so
 * there is no symbol for 26 and every label after `Z` would be off by one.
 * Subtracting one before each division is what makes the sequence
 * A…Z, AA…AZ, BA…, which is what a reader of "AA" expects.
 */
export function toAlphabetic(value: number, lower = false): string | null {
	if (!Number.isInteger(value) || value < 1) {
		return null;
	}
	let remaining = value;
	let out = "";
	while (remaining > 0) {
		const digit = ((remaining - 1) % 26) + 1;
		out = String.fromCharCode(64 + digit) + out;
		remaining = Math.floor((remaining - 1) / 26);
	}
	return lower ? out.toLowerCase() : out;
}

/** Render one sequence value in one style, or `null` if the style cannot. */
export function formatSequenceValue(value: number, style: PageLabelStyle): string | null {
	switch (style) {
		case "D":
			return String(value);
		case "R":
			return toRoman(value);
		case "r":
			return toRoman(value)?.toLowerCase() ?? null;
		case "A":
			return toAlphabetic(value);
		case "a":
			return toAlphabetic(value, true);
		default:
			return null;
	}
}

/**
 * The document's label for a 0-based page — `"iv"`, `"A-7"`, `"1"`.
 *
 * A value a style cannot express (a roman above 3999, an alphabetic below 1)
 * falls back to the decimal value rather than to an empty string, because a
 * missing label reads as a bug in the viewer and a wrong-style label reads as
 * a bug in the document; the decimal at least counts. The case is documented
 * in the README as a fidelity limit rather than papered over.
 */
export function pageLabelFor(
	page: number,
	ranges: readonly PageLabelRange[] | undefined,
	normalised?: readonly NormalisedLabelRange[],
): string {
	if (!Number.isInteger(page) || page < 0) {
		return "";
	}
	const table = normalised ?? normaliseLabelRanges(ranges);
	const range = rangeForPage(table, page);
	if (range === null) {
		// No range owns this page. The page number is the honest label, and it
		// is the same number the page list would show.
		return String(page + 1);
	}
	const value = range.firstValue + (page - range.firstPage);
	const rendered = formatSequenceValue(value, range.style);
	return `${range.prefix}${rendered ?? String(page + 1)}`;
}

/**
 * True when the document says anything a plain page number would not.
 *
 * This is the predicate a toolbar uses to decide whether to *show* labels at
 * all: a document with no `/PageLabels` tree gets a page box reading "4 of 900",
 * and one whose only range is "decimal from 1" would get the same box, because
 * a label identical to the page number is noise.
 *
 * The coverage test is about the **first** range only, and that is worth
 * spelling out because it is easy to get backwards: a range does not *stop*,
 * it runs until the next one, so the last range covers the rest of the document
 * and no page is ever unlabelled at the end. A per-range check would call a
 * perfectly ordinary document "meaningful" for no reason a reader would thank
 * us for.
 */
export function hasMeaningfulLabels(
	ranges: readonly PageLabelRange[] | undefined,
	pageCount: number,
): boolean {
	const table = normaliseLabelRanges(ranges);
	const first = table[0];
	if (first === undefined || pageCount <= 0) {
		// No ranges, or no pages: there is nothing for a label to differ from.
		return false;
	}
	// Pages before the first range are unlabelled, so some page's label is the
	// page number while another's is not — which is the whole definition of
	// "meaningful" here.
	if (first.firstPage > 0) {
		return true;
	}
	return table.some(
		(range) =>
			range.prefix.length > 0 ||
			range.style !== DEFAULT_LABEL_STYLE ||
			range.firstValue !== 1,
	);
}

/**
 * Labels for a list of pages, in one pass.
 *
 * A thumbnail rail needs a label per *visible* thumbnail, not per document: a
 * 2 000-page document would allocate 2 000 strings per frame to label six
 * boxes. The controller therefore formats on demand, and this helper is for
 * the *outline* case, where a bookmark list is short enough to label up front
 * and doing so once beats doing it per keystroke.
 */
export function pageLabelsFor(
	pages: readonly number[],
	ranges: readonly PageLabelRange[] | undefined,
	normalised?: readonly NormalisedLabelRange[],
): readonly string[] {
	const table = normalised ?? normaliseLabelRanges(ranges);
	return pages.map((page) => pageLabelFor(page, undefined, table));
}
