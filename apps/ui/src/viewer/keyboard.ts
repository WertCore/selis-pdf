/**
 * Keyboard and assistive-technology contract for the page list (SL-4.UI.02).
 *
 * The full viewer a11y pass is SL-4.UI.07 (axe-core, screen-reader script,
 * keyboard operation of every control); this file is UI.02's part of it and it
 * is written to be *extended*, not replaced: the page list is a single widget
 * with a documented key map, and the later tasks attach to the same commands.
 *
 * The model is the one assistive tech already understands: a scrollable
 * document with a list of pages, where the topmost page is "current"
 * (`aria-current="page"`), each page is announced as "Page N of M", and the
 * arrow/page/home/end keys move between pages the way every document viewer
 * does. The list is a *list* (`role="list"` / `listitem`), not a
 * `listbox` — pages are not options, and a listbox would promise selection
 * semantics the viewer does not have.
 *
 * ## The two key maps, and why they are separate
 *
 * UI.05 (search) attaches to this file rather than beside it, because the
 * contract it needs is the same contract: a *pure, total* resolver that a shell
 * calls from a key handler and that returns `null` for anything that is not
 * ours, so the shell keeps the keys it wants. But the maps are separate
 * functions with separate context types, because the two claim different keys
 * and must not be able to claim each other's: Enter belongs to the search field
 * and Escape closes search, while the arrows belong to the list. A combined
 * resolver would need a precedence rule, and precedence is where "the arrow key
 * did nothing" bugs live.
 *
 * The search map is *complete* rather than partial in one respect worth
 * naming: `F3` is the platform convention for "find next" and `Shift+F3` for
 * "find previous", and a viewer that only binds Enter sends a reader who has
 * learned one viewer to a dead key in the next.
 */

import type { PageFitMode } from "./layout.js";
import { DEFAULT_STRINGS, type PageListStrings } from "./strings.js";

/** Keys the page list claims. Anything else belongs to the shell. */
export type PageListKey =
	| "ArrowDown"
	| "ArrowUp"
	| "ArrowRight"
	| "ArrowLeft"
	| "PageDown"
	| "PageUp"
	| "Home"
	| "End";

/** What a key press asked the list to do. `null` = not ours. */
export interface PageNavigationCommand {
	/** Target page, 0-based. */
	readonly page: number;
	/** How the shell should place it. */
	readonly align: "start" | "centre";
	/** The key that produced it — for tests and for UI.07's audit trail. */
	readonly key: PageListKey;
}

export interface KeyboardContext {
	/** Current page, 0-based. */
	readonly currentPage: number;
	readonly pageCount: number;
	readonly mode: PageFitMode;
	/**
	 * How many pages a PageUp/PageDown should move — the number of pages the
	 * viewport can actually show, which the controller derives from the window.
	 */
	readonly pagesPerView: number;
	/**
	 * Where a step lands: in spread mode an arrow key crosses a whole spread,
	 * so it steps two pages and then snaps to the first page of the next row.
	 */
	readonly step: number;
}

/** Step size for a mode: a spread is two pages wide. */
export function stepFor(mode: PageFitMode): number {
	return mode === "spread" ? 2 : 1;
}

/**
 * Resolve a key press to a navigation command, or `null` if the key is not
 * the page list's. Pure and total: it clamps rather than throwing, and it
 * returns `null` for an empty document so the shell can decide what an empty
 * list does with focus.
 */
export function resolvePageKey(
	key: string,
	context: KeyboardContext,
): PageNavigationCommand | null {
	if (context.pageCount <= 0) {
		return null;
	}
	const last = context.pageCount - 1;
	const step = Math.max(1, context.step);
	const perView = Math.max(1, context.pagesPerView);
	const current = clamp(context.currentPage, 0, last);
	const page = (candidate: number, align: PageNavigationCommand["align"] = "start") =>
		candidate < 0 || candidate > last
			? null
			: { page: clamp(candidate, 0, last), align, key: key as PageListKey };

	switch (key) {
		case "ArrowDown":
		case "ArrowRight":
			return page(current + step);
		case "ArrowUp":
		case "ArrowLeft":
			return page(current - step);
		case "PageDown":
			// A screen of pages, not a page: `perView` is at least one step.
			return page(current + Math.max(step, perView));
		case "PageUp":
			return page(current - Math.max(step, perView));
		case "Home":
			return { page: 0, align: "start", key };
		case "End":
			return { page: last, align: "start", key };
		default:
			return null;
	}
}

function clamp(value: number, low: number, high: number): number {
	return Math.min(high, Math.max(low, value));
}

/**
 * Human label for a page, used by the tile `aria-label` and the live region.
 *
 * The text comes from `strings.ts` (every user-visible string is a key), so
 * this is a thin binding rather than a template — see {@link PageListStrings}.
 * `strings` defaults to English; the controller passes the host's catalogue.
 */
export function pageLabel(
	page: number,
	pageCount: number,
	strings: PageListStrings = DEFAULT_STRINGS,
): string {
	return strings.pageLabel(page, pageCount);
}

/** The polite live-region text: which page the reader is on, and the mode. */
export function announcement(
	page: number,
	pageCount: number,
	mode: PageFitMode,
	strings: PageListStrings = DEFAULT_STRINGS,
): string {
	return strings.announcement(page, pageCount, mode);
}

/** Keys the search controls claim. Anything else belongs to the shell. */
export type SearchKey = "Enter" | "Escape" | "F3";

/** What a key press asked search to do. */
export type SearchCommandAction = "next" | "previous" | "close";

/** A resolved search key press. */
export interface SearchCommand {
	readonly action: SearchCommandAction;
	/** The key that produced it, for tests and for UI.07's audit trail. */
	readonly key: SearchKey;
}

/** What the search resolver needs to know to decide whether a key means anything. */
export interface SearchKeyContext {
	/**
	 * The reader is searching: there is a non-empty query. `next`/`previous` on
	 * an empty query would have nothing to step through, and `close` on an
	 * already-closed search would steal the shell's Escape (a dialog, a
	 * sidebar) to do nothing.
	 */
	readonly hasQuery: boolean;
	/**
	 * The shift key was held. It is in the context rather than the key name
	 * because DOM `KeyboardEvent.key` is `"F3"` for both, and inventing
	 * `"F3+shift"` would be a key name no host reports.
	 */
	readonly shift: boolean;
}

/**
 * Resolve a key press against the search controls, or `null` if it is not ours.
 *
 * Pure and total, and *context-sensitive in one direction only*: with no query
 * every key returns `null`, so search never swallows a key it has no business
 * acting on. With a query, Enter and F3 are "next" and their shifted forms are
 * "previous" — the same two commands on two conventions, which is what makes
 * both worth binding.
 */
export function resolveSearchKey(key: string, context: SearchKeyContext): SearchCommand | null {
	if (!context.hasQuery) {
		return null;
	}
	const command = (action: SearchCommandAction): SearchCommand => ({
		action,
		key: key as SearchKey,
	});
	switch (key) {
		case "Enter":
		case "F3":
			return command(context.shift ? "previous" : "next");
		case "Escape":
			return command("close");
		default:
			return null;
	}
}
