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
 * SL-4.UI.11 has since written the runtime this module was a placeholder for,
 * and this file is now a **registration** of the viewer's three catalogues
 * against it: the keys, the English text and the three factories all stay
 * exactly where they were, and the body of each factory is a lookup in
 * `createViewerMessageRuntime()` rather than a merge written out by hand. The
 * seam's own promise — "UI.11 replaces the body of that factory and nothing
 * else has to move" — is what this file is the evidence for.
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
 * the code changing. The substitution itself is `i18n/message.ts`'s, not this
 * file's: four copies of a grammar is four grammars.
 */

import { interpolate } from "../i18n/message.js";
import { createPseudoCatalogue } from "../i18n/pseudo-locale.js";
import {
	PSEUDO_LOCALE,
	SOURCE_LOCALE,
	type Catalogue,
	type CatalogueSet,
	type MessageGap,
	type MessageRuntime,
	createMessageRuntime,
} from "../i18n/runtime.js";
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
 *
 * A thin call into the runtime's `interpolate`, kept because it is this module's
 * published name and because the rule above is the seam's rule rather than the
 * runtime's. Formatting a bare template here is locale-free; a factory resolves
 * through a {@link MessageRuntime} instead, so the locale that mattered is the
 * one the runtime was built with.
 */
export function formatMessage(template: string, values: PageListMessageValues): string {
	return interpolate(template, values, SOURCE_LOCALE);
}

/**
 * Bind the page list's strings over a runtime.
 *
 * The one implementation that the pre-UI.11 `createPageListStrings` and
 * `createViewerStrings` both call, so a host assembling all three components and
 * a host wanting only the page list resolve a key the same way — including what
 * a missing one looks like.
 */
export function pageListStrings(runtime: MessageRuntime): PageListStrings {
	const label = (page: number, pageCount: number): string =>
		runtime.t("pageList.page.label", { page: page + 1, total: pageCount });
	return {
		pageLabel: label,
		announcement: (page, pageCount, mode) =>
			runtime.t("pageList.page.announcement", {
				page: page + 1,
				total: pageCount,
				pageLabel: label(page, pageCount),
				// The mode name is *resolved* rather than read out of the
				// catalogue, so a countable or missing mode template behaves like
				// every other message instead of being pasted in as raw syntax.
				mode: runtime.t(modeKey(mode)),
			}),
		regionLabel: runtime.t("pageList.region.label"),
		listLabel: runtime.t("pageList.list.label"),
	};
}

/**
 * Merge a partial page-list catalogue over English and bind the strings.
 *
 * The `Partial` is the host's "I have translated these" list, and the runtime
 * resolves each key in the requested locale before falling back to English —
 * which, for the default English runtime, is the same merge this function used
 * to write out by hand.
 */
export function createPageListStrings(overrides: Partial<PageListCatalogue> = {}): PageListStrings {
	return pageListStrings(createViewerMessageRuntime({ overrides }));
}

/** The shared English instance, for callers that override nothing. */

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

/** Bind search's strings over a runtime. See {@link pageListStrings}. */
export function searchStrings(runtime: MessageRuntime): SearchStrings {
	return {
		fieldLabel: runtime.t("search.field.label"),
		fieldPlaceholder: runtime.t("search.field.placeholder"),
		regionLabel: runtime.t("search.region.label"),
		idle: () => runtime.t("search.status.idle"),
		scanning: (count) => runtime.t("search.status.scanning", { count }),
		found: (count) => runtime.t("search.status.found", { count }),
		none: () => runtime.t("search.status.none"),
		match: (current, count, pageLabel) =>
			runtime.t("search.status.match", { current, count, pageLabel }),
		failed: () => runtime.t("search.status.failed"),
		nextLabel: runtime.t("search.button.next"),
		previousLabel: runtime.t("search.button.previous"),
		closeLabel: runtime.t("search.button.close"),
		caseLabel: runtime.t("search.option.case"),
		wordLabel: runtime.t("search.option.word"),
	};
}

/** Merge a partial search catalogue over English and bind the formatted strings. */
export function createSearchStrings(overrides: Partial<SearchCatalogue> = {}): SearchStrings {
	return searchStrings(createViewerMessageRuntime({ overrides }));
}

/** The shared English search strings, for callers that override nothing. */

/**
 * ## Why navigation is a third key set, and not a widening of either of these
 *
 * `PAGE_LIST_MESSAGE_KEYS` and `SEARCH_MESSAGE_KEYS` are both closed unions, and
 * the argument for closing them (UI.05's section above) applies verbatim: a
 * `Partial` of a known set is what a host can be handed safely. What does *not*
 * apply is the "one catalogue, one factory" conclusion — the page list and search
 * share no sentence, and navigation shares with both only the page label, which
 * is *passed in* rather than re-derived so that "Page 7 of 900" cannot be worded
 * two ways in two panels. So UI.06 gets its own closed set, its own catalogue and
 * its own factory, over the same `formatMessage` and the same
 * overrides-merge-over-English rule. `createViewerStrings` is the eventual place
 * to assemble the three; until UI.11 it does not exist, and inventing it now
 * would be a seam with no consumer.
 *
 * The link-refusal strings are the ones worth arguing about. There is one key
 * per refusal reason, not one "link not available" string, because the reader is
 * owed the difference between "this document tried to run a program" and "this
 * link goes somewhere we do not follow", and a single sentence for both would say
 * less than the product actually does.
 */
export const NAVIGATION_MESSAGE_KEYS = [
	/** `aria-label` on the navigation region's wrapper. */
	"navigation.region.label",
	/** The outline panel's heading. */
	"navigation.outline.label",
	/** The outline loaded and the document has none. */
	"navigation.outline.empty",
	/**
	 * The host cannot supply document navigation at all. Distinct from
	 * "empty": one is a fact about the document, the other about the host.
	 */
	"navigation.outline.unavailable",
	/** The outline is longer than the panel will show. Placeholders: `{shown}`, `{total}`. */
	"navigation.outline.truncated",
	/** One outline row's accessible name. Placeholders: `{title}`, `{pageLabel}`. */
	"navigation.outline.row",
	/** A row whose destination the document does not define. Placeholder: `{title}`. */
	"navigation.outline.row.unresolved",
	/** Polite announcement after following an outline item. Placeholder: `{pageLabel}`. */
	"navigation.outline.moved",
	/** The thumbnail rail's `aria-label`. */
	"navigation.thumbnails.label",
	/** One thumbnail's accessible name; the page label is passed in. */
	"navigation.thumbnails.item",
	/** The external-link prompt's title. */
	"navigation.link.external.title",
	/**
	 * The external-link prompt's body. Placeholders: `{url}`, `{host}`.
	 *
	 * The **full** destination is in this string, not a shortened form of it: a
	 * prompt that shows `example.com` for `https://example.com/redirect?to=…`
	 * gets a click it did not earn.
	 */
	"navigation.link.external.body",
	/** The prompt's confirm control. */
	"navigation.link.external.confirm",
	/** The prompt's cancel control. */
	"navigation.link.external.cancel",
	/** Refusal: `/Launch`. */
	"navigation.link.blocked.launch",
	/** Refusal: `/GoToR`. */
	"navigation.link.blocked.remote",
	/** Refusal: `/SubmitForm`. */
	"navigation.link.blocked.submit",
	/** Refusal: `/ImportData`. */
	"navigation.link.blocked.import",
	/** Refusal: document JavaScript. */
	"navigation.link.blocked.javascript",
	/** Refusal: a URI scheme the viewer does not open. Placeholder: `{url}`. */
	"navigation.link.blocked.scheme",
	/** Refusal: the named destination is not in the document's name tree. */
	"navigation.link.blocked.missing",
	/** Refusal: the annotation has no target to act on. */
	"navigation.link.blocked.empty",
	/** Refusal: an action class the viewer does not implement. */
	"navigation.link.blocked.unsupported",
] as const;

/** A key into {@link NAVIGATION_MESSAGE_KEYS}. */
export type NavigationMessageKey = (typeof NAVIGATION_MESSAGE_KEYS)[number];

/** A set of navigation templates keyed by message key. */
export type NavigationCatalogue = Readonly<Record<NavigationMessageKey, string>>;

/**
 * English navigation strings.
 *
 * Two of these are written the long way on purpose. `…blocked.launch` names the
 * mechanism (a program or file the document asked to run) rather than saying
 * "link blocked", because a reader told what the document tried to do can decide
 * whether they need a different tool; a reader told "blocked" only learns the
 * viewer said no. And `navigation.outline.unavailable` is a separate key from
 * `navigation.outline.empty` for the same reason: "this document has no outline"
 * and "this host cannot read outlines" are different facts, and one sentence for
 * both lies about one of them.
 */
export const EN_NAVIGATION_CATALOGUE: NavigationCatalogue = {
	"navigation.region.label": "Document navigation",
	"navigation.outline.label": "Outline",
	"navigation.outline.empty": "This document has no outline.",
	"navigation.outline.unavailable": "This host cannot read the document's outline.",
	"navigation.outline.truncated": "Showing {shown} of {total} outline items.",
	"navigation.outline.row": "{title}, {pageLabel}",
	"navigation.outline.row.unresolved": "{title}, destination not found",
	"navigation.outline.moved": "Moved to {pageLabel}",
	"navigation.thumbnails.label": "Page thumbnails",
	"navigation.thumbnails.item": "Go to {pageLabel}",
	"navigation.link.external.title": "Open an external link?",
	"navigation.link.external.body": "This document links to {url} on {host}.",
	"navigation.link.external.confirm": "Open link",
	"navigation.link.external.cancel": "Cancel",
	"navigation.link.blocked.launch":
		"This document asked to launch a file or program. Selis does not run document content.",
	"navigation.link.blocked.remote":
		"This link points at another document. Selis does not open other documents from a link.",
	"navigation.link.blocked.submit": "This document asked to submit a form. Selis is read-only.",
	"navigation.link.blocked.import": "This document asked to import form data. Selis is read-only.",
	"navigation.link.blocked.javascript":
		"This document asked to run a script. Selis does not run document scripts.",
	"navigation.link.blocked.scheme": "This link uses a kind of address Selis will not open: {url}",
	"navigation.link.blocked.missing": "This link points at a place this document does not define.",
	"navigation.link.blocked.empty": "This link has no destination.",
	"navigation.link.blocked.unsupported": "This link does something Selis does not support.",
};

/** The resolved strings the navigation controller formats against. */
export interface NavigationStrings {
	readonly regionLabel: string;
	readonly outlineLabel: string;
	readonly empty: () => string;
	readonly unavailable: () => string;
	readonly truncated: (shown: number, total: number) => string;
	/** A row's accessible name; `null` for a row with no destination. */
	row(title: string, pageLabel: string | null): string;
	readonly moved: (pageLabel: string) => string;
	readonly thumbnailsLabel: string;
	readonly thumbnailItem: (pageLabel: string) => string;
	readonly externalTitle: string;
	readonly externalBody: (url: string, host: string) => string;
	readonly externalConfirm: string;
	readonly externalCancel: string;
	/** The sentence for one refusal reason; see `LinkRefusal` in `links.ts`. */
	readonly refusal: (reason: string) => string;
}

/** Bind navigation's strings over a runtime. See {@link pageListStrings}. */
export function navigationStrings(runtime: MessageRuntime): NavigationStrings {
	// One table from a refusal reason to its key. A reason with no entry is a
	// bug in `links.ts` rather than a runtime surprise, so the fallback is the
	// generic refusal: a live region is never left blank.
	const refusalKey: Readonly<Record<string, NavigationMessageKey>> = {
		launch: "navigation.link.blocked.launch",
		"remote-destination": "navigation.link.blocked.remote",
		"submit-form": "navigation.link.blocked.submit",
		"import-data": "navigation.link.blocked.import",
		javascript: "navigation.link.blocked.javascript",
		scheme: "navigation.link.blocked.scheme",
		"missing-destination": "navigation.link.blocked.missing",
		"no-target": "navigation.link.blocked.empty",
		unsupported: "navigation.link.blocked.unsupported",
	};
	return {
		regionLabel: runtime.t("navigation.region.label"),
		outlineLabel: runtime.t("navigation.outline.label"),
		empty: () => runtime.t("navigation.outline.empty"),
		unavailable: () => runtime.t("navigation.outline.unavailable"),
		truncated: (shown, total) => runtime.t("navigation.outline.truncated", { shown, total }),
		row: (title, pageLabel) =>
			pageLabel === null
				? runtime.t("navigation.outline.row.unresolved", { title })
				: runtime.t("navigation.outline.row", { title, pageLabel }),
		moved: (pageLabel) => runtime.t("navigation.outline.moved", { pageLabel }),
		thumbnailsLabel: runtime.t("navigation.thumbnails.label"),
		thumbnailItem: (pageLabel) => runtime.t("navigation.thumbnails.item", { pageLabel }),
		externalTitle: runtime.t("navigation.link.external.title"),
		externalBody: (url, host) => runtime.t("navigation.link.external.body", { url, host }),
		externalConfirm: runtime.t("navigation.link.external.confirm"),
		externalCancel: runtime.t("navigation.link.external.cancel"),
		refusal: (reason) => {
			const key = refusalKey[reason] ?? "navigation.link.blocked.unsupported";
			// Only the scheme refusal has a `{url}`; the rest take none. Passing
			// the reason as the URL would print "scheme" in a sentence about an
			// address, so the value is supplied for that one key alone.
			return key === "navigation.link.blocked.scheme"
				? runtime.t(key, { url: reason })
				: runtime.t(key);
		},
	};
}

/**
 * Merge a partial navigation catalogue over English and bind the strings.
 *
 * The `Partial` is the host's "I have translated these" list; the resolution
 * order that turns it into a sentence is the runtime's, and is the same one the
 * page list and search use.
 */
export function createNavigationStrings(
	overrides: Partial<NavigationCatalogue> = {},
): NavigationStrings {
	return navigationStrings(createViewerMessageRuntime({ overrides }));
}

/** The shared English navigation strings, for callers that override nothing. */

/**
 * ## The three catalogues, registered (SL-4.UI.11)
 *
 * Everything below is the assembly UI.02's comment above kept promising. It is
 * a *registration*, not a merge: the three catalogues stay three catalogues with
 * three closed key unions, each next to the component that owns its sentences,
 * and the runtime is handed all three so that one locale decision and one
 * fallback rule apply to the whole viewer.
 *
 * The tempting alternative — one `ViewerCatalogue` a build step generates from
 * the three — was rejected. A generated catalogue is a second place a rename has
 * to be applied, and a rename applied to the source and not the generated file
 * is exactly the failure ADR-P0034 names, arrived at from the other direction.
 */

/** Every key the viewer owns, from the three closed sets above. */
export const VIEWER_MESSAGE_KEYS = [
	...PAGE_LIST_MESSAGE_KEYS,
	...SEARCH_MESSAGE_KEYS,
	...NAVIGATION_MESSAGE_KEYS,
] as const;

/** A key into {@link VIEWER_MESSAGE_KEYS}. */
export type ViewerMessageKey = (typeof VIEWER_MESSAGE_KEYS)[number];

/** The three English catalogues as one source catalogue. */
export const EN_VIEWER_CATALOGUE: Readonly<Record<ViewerMessageKey, string>> = {
	...EN_PAGE_LIST_CATALOGUE,
	...EN_SEARCH_CATALOGUE,
	...EN_NAVIGATION_CATALOGUE,
};

/*
 * The three per-component defaults live HERE, after the catalogues they are built
 * from. They used to sit beside their own factories, which no longer works: each
 * one calls createViewerMessageRuntime, which reads EN_VIEWER_CATALOGUE below, so
 * evaluating one before that const is initialised is a temporal-dead-zone
 * ReferenceError that fails ten test suites at import time. Order is the fix; the
 * values are unchanged.
 */
export const DEFAULT_STRINGS: PageListStrings = createPageListStrings();

export const DEFAULT_SEARCH_STRINGS: SearchStrings = createSearchStrings();

export const DEFAULT_NAVIGATION_STRINGS: NavigationStrings = createNavigationStrings();

/** What a host may pass to {@link createViewerMessageRuntime}. */
export interface ViewerRuntimeOptions {
	/** The locale to render. Defaults to {@link SOURCE_LOCALE}. */
	readonly locale?: string | undefined;
	/**
	 * Extra locales, merged over the English source.
	 *
	 * A `Partial<Catalogue>` per locale, which is the point: a translator ships
	 * the strings they have done, and the runtime resolves the rest from the
	 * source and records them as untranslated rather than failing.
	 */
	readonly catalogues?: Partial<CatalogueSet> | undefined;
	/**
	 * Templates for {@link locale} alone, merged over whatever that locale
	 * already has. This is the pre-UI.11 `Partial` override, still accepted
	 * unchanged by all three `createXStrings` factories.
	 */
	readonly overrides?: Partial<Record<ViewerMessageKey, string>> | undefined;
	/**
	 * Throw on a key no locale has, instead of rendering the missing marker.
	 *
	 * Off by default because a shell must not crash on a bad catalogue, and on
	 * in CI, where the same defect is a build failure instead of a token in a
	 * screenshot.
	 */
	readonly strict?: boolean | undefined;
	/** Called per gap as it is found. */
	readonly onGap?: ((gap: MessageGap) => void) | undefined;
}

/**
 * The viewer's messages as one runtime.
 *
 * The single place a viewer host states a locale, and the single place the three
 * components' keys meet. `VIEWER_MESSAGE_KEYS` is passed as the declared key
 * set, which is what lets the runtime report a translation file still carrying a
 * key the code dropped — a rename that missed a file, which is otherwise
 * invisible until somebody reads the file.
 */
export function createViewerMessageRuntime(
	options: ViewerRuntimeOptions = {},
): MessageRuntime {
	const runtime = createMessageRuntime({
		locale: options.locale ?? SOURCE_LOCALE,
		catalogues: { [SOURCE_LOCALE]: EN_VIEWER_CATALOGUE, ...options.catalogues },
		declaredKeys: VIEWER_MESSAGE_KEYS,
		strict: options.strict,
		onGap: options.onGap,
	});
	return options.overrides === undefined
		? runtime
		: runtime.withOverrides(options.overrides as Catalogue);
}

/** The three components' resolved strings, over one runtime. */
export interface ViewerStrings {
	/** The locale these were resolved in. */
	readonly locale: string;
	readonly pageList: PageListStrings;
	readonly search: SearchStrings;
	readonly navigation: NavigationStrings;
	/**
	 * The runtime behind all three.
	 *
	 * Exposed for the two things only the runtime can answer: `gaps()` for a
	 * report, and `coverage()` for "which of this viewer's strings has this
	 * locale not translated yet".
	 */
	readonly runtime: MessageRuntime;
}

/**
 * Every user-facing string the viewer owns, in one locale.
 *
 * What a shell mounts. The three `DEFAULT_*` constants below remain for a host
 * that overrides nothing and wants one component; this is the one-call form the
 * `i18n/README.md` tells a shell to use, because three separately-constructed
 * runtimes are three chances to disagree about a locale.
 */
export function createViewerStrings(options: ViewerRuntimeOptions = {}): ViewerStrings {
	const runtime = createViewerMessageRuntime(options);
	return {
		locale: runtime.locale,
		runtime,
		pageList: pageListStrings(runtime),
		search: searchStrings(runtime),
		navigation: navigationStrings(runtime),
	};
}

/**
 * The viewer's strings rendered in the pseudo-locale.
 *
 * Exported rather than left for a test to assemble, so "run the UI.11 gate" is a
 * call with no arguments and the gate cannot drift from the way a shell would
 * build it. The pseudo catalogue is total by construction
 * (`pseudo-locale.ts`), so every `GapKind` this can report is one the English
 * catalogue caused.
 */
export function createPseudoViewerStrings(): ViewerStrings {
	return createViewerStrings({
		locale: PSEUDO_LOCALE,
		catalogues: { [PSEUDO_LOCALE]: createPseudoCatalogue(EN_VIEWER_CATALOGUE) },
		strict: true,
	});
}

