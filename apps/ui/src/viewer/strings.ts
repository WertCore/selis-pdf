/**
 * User-facing strings for the page list (SL-4.UI.02).
 *
 * Every string the page list shows or announces is a **key** here, and the
 * English text is a catalogue rather than an inline literal. The ADRs are
 * explicit that "every user-visible string goes through i18n keys from day 1 so
 * a rename is a resource change, not a code change" — a literal in
 * `page-list.ts` makes a rename a code change in two files, and it puts English
 * in the geometry layer where a translator cannot reach it.
 *
 * SL-4.UI.11 owns the real i18n runtime (English shipping, pseudo-locale in
 * CI). It is not written yet, so this module deliberately stays a *seam* and
 * not a framework: a key set, one English catalogue, placeholder interpolation,
 * and a `createPageListStrings` factory that takes overrides. UI.11 replaces the
 * body of that factory with a catalogue lookup and nothing else has to move.
 *
 * The page list is the first component to need this, and the right place to
 * prove the shape, because the same catalogue has to carry the strings UI.05
 * (match counts) and UI.06 (outline, destinations) will add. UI.05 is in, below.
 *
 * ## Why search is a second key set and not more page-list keys
 *
 * `PAGE_LIST_MESSAGE_KEYS` is a *closed* union and `createPageListStrings`
 * takes a `Partial` of it, which is what stops a new string appearing without an
 * entry in the catalogue. Widening that union for search would have been cheaper
 * and is the wrong shape: `PageListStrings` is the page list's *resolved*
 * strings, and a search controller forced to accept them would be taking a
 * dependency it does not have. So search gets its own closed key set, its own
 * catalogue and its own factory, over the *same* `formatMessage` interpolation
 * and the same overrides-merge-over-English rule. UI.06 adds a third the same
 * way, and a `createViewerStrings` is the eventual place to assemble them.
 *
 * Placeholders are `{name}` and are substituted by name, not by position, so a
 * catalogue can reorder a sentence — which most languages need to do — without
 * the code changing.
 */

import type { PageFitMode } from "./layout.js";

/**
 * Every user-facing string the page list owns. The union is closed on purpose:
 * `createPageListStrings` takes a `Partial`, so a host can override one string,
 * but a *new* string cannot be introduced without a key here.
 */
export const PAGE_LIST_MESSAGE_KEYS = [
	/** The `aria-label` on a page tile, and the core of the live region. */
	"pageList.page.label",
	/** The polite live-region sentence: the page, and the fit mode. */
	"pageList.page.announcement",
	/** Fit mode: whole page visible. */
	"pageList.mode.page",
	/** Fit mode: page width fills the viewport. */
	"pageList.mode.width",
	/** Fit mode: two pages side by side. */
	"pageList.mode.spread",
	/** `aria-label` on the scrolling region that contains the list. */
	"pageList.region.label",
	/** `aria-label` on the list itself. */
	"pageList.list.label",
] as const;

/** A key into {@link PAGE_LIST_MESSAGE_KEYS}. */
export type PageListMessageKey = (typeof PAGE_LIST_MESSAGE_KEYS)[number];

/**
 * A set of templates keyed by message key. A host may pass a `Partial` and
 * override one string without restating the rest.
 */
export type PageListCatalogue = Readonly<Record<PageListMessageKey, string>>;

/**
 * Values a `{placeholder}` can take. No document data ever passes through here
 * — only a page number, a page count, and an already-localised mode name.
 */
export type PageListMessageValues = Readonly<Record<string, string | number>>;

/**
 * English catalogue. Placeholders: `{page}`, `{total}`, `{pageLabel}`, `{mode}`.
 *
 * "Page 1 of 12" rather than "1 / 12", because assistive tech reads the first
 * as a sentence and announces the second as "one slash twelve".
 */
export const EN_PAGE_LIST_CATALOGUE: PageListCatalogue = {
	"pageList.page.label": "Page {page} of {total}",
	"pageList.page.announcement": "{pageLabel} — {mode}",
	"pageList.mode.page": "fit page",
	"pageList.mode.width": "fit width",
	"pageList.mode.spread": "two-up",
	"pageList.region.label": "Document pages",
	"pageList.list.label": "Pages",
};

/** The resolved strings the controller formats against. */
export interface PageListStrings {
	/** Tile `aria-label`; numbers pages from one. */
	pageLabel(page: number, pageCount: number): string;
	/** Polite live-region text: the page and the fit mode. */
	announcement(page: number, pageCount: number, mode: PageFitMode): string;
	/** `aria-label` for the scrolling region. */
	readonly regionLabel: string;
	/** `aria-label` for the list. */
	readonly listLabel: string;
}

/**
 * The catalogue key naming a fit mode.
 */
export function modeKey(mode: PageFitMode): PageListMessageKey {
	return mode === "page"
		? "pageList.mode.page"
		: mode === "width"
			? "pageList.mode.width"
			: "pageList.mode.spread";
}

/**
 * Substitute `{name}` placeholders. An unknown placeholder is left in place
 * rather than silently blanked: a catalogue with a typo should look wrong in
 * the UI, where someone can see it, not read as an empty label to a screen
 * reader.
 */
export function formatMessage(template: string, values: PageListMessageValues): string {
	return template.replace(/\{(\w+)\}/g, (whole, name: string) =>
		name in values ? String(values[name]) : whole,
	);
}

/** Merge a partial catalogue over English and bind the two formatted strings. */
export function createPageListStrings(overrides: Partial<PageListCatalogue> = {}): PageListStrings {
	const catalogue: PageListCatalogue = { ...EN_PAGE_LIST_CATALOGUE, ...overrides };
	const format = (key: PageListMessageKey, values: PageListMessageValues): string =>
		formatMessage(catalogue[key], values);
	const label = (page: number, pageCount: number): string =>
		format("pageList.page.label", { page: page + 1, total: pageCount });
	return {
		pageLabel: label,
		announcement: (page, pageCount, mode) =>
			format("pageList.page.announcement", {
				page: page + 1,
				total: pageCount,
				pageLabel: label(page, pageCount),
				mode: catalogue[modeKey(mode)],
			}),
		regionLabel: catalogue["pageList.region.label"],
		listLabel: catalogue["pageList.list.label"],
	};
}

/** The shared English instance, for callers that override nothing. */
export const DEFAULT_STRINGS: PageListStrings = createPageListStrings();

/**
 * Every user-facing string search owns (SL-4.UI.05). Closed for the same
 * reason {@link PAGE_LIST_MESSAGE_KEYS} is: a `Partial` of a known set is what a
 * host can safely be handed.
 */
export const SEARCH_MESSAGE_KEYS = [
	/** `aria-label` on the search input. */
	"search.field.label",
	/** The input's placeholder text. */
	"search.field.placeholder",
	/** The toolbar/search region's `aria-label`. */
	"search.region.label",
	/** Nothing typed yet: the field's own state, not a result. */
	"search.status.idle",
	/** Mid-scan, with the count so far. Placeholders: `{count}`. */
	"search.status.scanning",
	/** Scan finished with at least one match. Placeholders: `{count}`. */
	"search.status.found",
	/** Scan finished with none. */
	"search.status.none",
	/**
	 * The current-match sentence, announced on every next/previous.
	 * Placeholders: `{current}`, `{count}`, `{pageLabel}`.
	 */
	"search.status.match",
	/** The scan could not be completed; the shell reports the reason. */
	"search.status.failed",
	/** `aria-label` on the next-match control. */
	"search.button.next",
	/** `aria-label` on the previous-match control. */
	"search.button.previous",
	/** `aria-label` on the control that closes search. */
	"search.button.close",
	/** The match-case modifier's label. */
	"search.option.case",
	/** The whole-word modifier's label. */
	"search.option.word",
] as const;

/** A key into {@link SEARCH_MESSAGE_KEYS}. */
export type SearchMessageKey = (typeof SEARCH_MESSAGE_KEYS)[number];

/** A set of search templates keyed by message key. */
export type SearchCatalogue = Readonly<Record<SearchMessageKey, string>>;

/**
 * English search catalogue.
 *
 * Three things here are worth a word, because they are the sentences a naive
 * implementation gets wrong:
 *
 * - `search.status.scanning` and `search.status.found` are *different keys*, not
 *   one template with a flag. "0 matches" while the scan is still running and
 *   "0 matches" after it finished mean opposite things to a reader, and a live
 *   region that says "No matches" at the first batch of a 2 000-page document is
 *   a lie the UI then has to walk back.
 * - `search.status.none` has no `{count}` in it, so a catalogue that pluralises
 *   cannot accidentally render "No 0 matches".
 * - "Match" is the countable noun and the page names where, so
 *   `search.status.match` reads as a sentence. Assistive tech announces "3 slash
 *   12"; it announces "Match 3 of 12, Page 7 of 900" as a sentence, which is
 *   what the reader needs to hear.
 */
export const EN_SEARCH_CATALOGUE: SearchCatalogue = {
	"search.field.label": "Find in document",
	"search.field.placeholder": "Find",
	"search.region.label": "Find in document",
	"search.status.idle": "Type to search",
	"search.status.scanning": "Searching, {count} so far",
	"search.status.found": "{count} matches found",
	"search.status.none": "No matches",
	"search.status.match": "Match {current} of {count}, {pageLabel}",
	"search.status.failed": "Search could not be completed",
	"search.button.next": "Next match",
	"search.button.previous": "Previous match",
	"search.button.close": "Close search",
	"search.option.case": "Match case",
	"search.option.word": "Match whole word",
};

/**
 * The resolved strings the search controller formats against.
 *
 * A distinct interface from {@link PageListStrings} because the two components
 * genuinely share no sentence: nothing the page list says changes when a search
 * is open, and nothing search says is about fit modes. The one string they do
 * share, the page label, is *passed in* to {@link SearchStrings.match} rather
 * than re-derived, so two catalogues cannot word the same page two ways.
 */
export interface SearchStrings {
	/** `aria-label` for the search input. */
	readonly fieldLabel: string;
	/** Placeholder for the search input. */
	readonly fieldPlaceholder: string;
	/** `aria-label` for the region wrapping the search controls. */
	readonly regionLabel: string;
	/** Live-region text: nothing searched yet. */
	idle(): string;
	/** Live-region text: the scan is running, with the count so far. */
	scanning(count: number): string;
	/** Live-region text: the scan finished with at least one match. */
	found(count: number): string;
	/** Live-region text: the scan finished with no matches. */
	none(): string;
	/** Live-region text for a move to the current match. */
	match(current: number, count: number, pageLabel: string): string;
	/** Live-region text: the scan failed. */
	failed(): string;
	/** `aria-label` for the next-match control. */
	readonly nextLabel: string;
	/** `aria-label` for the previous-match control. */
	readonly previousLabel: string;
	/** `aria-label` for the close control. */
	readonly closeLabel: string;
	/** Label for the match-case modifier. */
	readonly caseLabel: string;
	/** Label for the whole-word modifier. */
	readonly wordLabel: string;
}

/** Merge a partial search catalogue over English and bind the formatted strings. */
export function createSearchStrings(overrides: Partial<SearchCatalogue> = {}): SearchStrings {
	const catalogue: SearchCatalogue = { ...EN_SEARCH_CATALOGUE, ...overrides };
	const format = (key: SearchMessageKey, values: PageListMessageValues): string =>
		formatMessage(catalogue[key], values);
	return {
		fieldLabel: catalogue["search.field.label"],
		fieldPlaceholder: catalogue["search.field.placeholder"],
		regionLabel: catalogue["search.region.label"],
		idle: () => format("search.status.idle", {}),
		scanning: (count) => format("search.status.scanning", { count }),
		found: (count) => format("search.status.found", { count }),
		none: () => format("search.status.none", {}),
		match: (current, count, pageLabel) =>
			format("search.status.match", { current, count, pageLabel }),
		failed: () => format("search.status.failed", {}),
		nextLabel: catalogue["search.button.next"],
		previousLabel: catalogue["search.button.previous"],
		closeLabel: catalogue["search.button.close"],
		caseLabel: catalogue["search.option.case"],
		wordLabel: catalogue["search.option.word"],
	};
}

/** The shared English search strings, for callers that override nothing. */
export const DEFAULT_SEARCH_STRINGS: SearchStrings = createSearchStrings();
