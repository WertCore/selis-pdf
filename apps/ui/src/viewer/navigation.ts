/**
 * The navigation controller (SL-4.UI.06).
 *
 * Owns what the sidebar and the link layer need: the document's outline, its
 * page labels, its named destinations, and the state of a link the reader
 * clicked. It renders nothing, reads no viewport and reaches for no platform
 * global — the same contract `page-list.ts` and `search.ts` keep, and the
 * reason all three are testable in Node with no DOM (ADR-P0044; SL-4.UI.01 puts
 * the seam — no platform globals, and the repo ships no jsdom to simulate them).
 *
 * ## What it does *not* do
 *
 * It does not move the reader. `goToPage` on the page list is the only way to
 * move (UI.02's rule), so this controller's job is to *say where to*: every
 * navigation request is published as {@link NavigationState.target} and the
 * shell hands it to the page list. A controller that computed a scroll offset
 * would be a second definition of "go to page", and two definitions is how
 * "the bookmark went to the wrong place" happens.
 *
 * ## Three states a reader can be in, and why they are different
 *
 * - **Host has no navigation port** (`adapter.navigation === undefined`):
 *   `available` is false and the panel says the host cannot read outlines. It
 *   does not fall back to parsing bytes, and it does not show an empty panel
 *   that looks like a document with no bookmarks.
 * - **Document has no outline**: `available` is true, `hasOutline` is false, and
 *   the panel says *this document* has none. That is a fact about a document,
 *   and a reader who opens a different file should see it change.
 * - **Document has one**: the rows are published, and the reader can move,
 *   expand and activate them.
 *
 * ## The external-link handshake
 *
 * ADR-P0020: nothing in a document acts without the user. Activating a `/URI`
 * link does *not* open anything — it publishes {@link NavigationState.pendingExternal},
 * the shell renders a dialog, and only `confirmExternal()` calls
 * `adapter.window.openExternal`. So there is exactly one call site of
 * `openExternal` in this file, it is behind a confirmation, and a test asserts
 * the URL never reaches the host without one. A controller that opened the URL
 * and showed a dialog afterwards would satisfy a DoD written loosely and defeat
 * its purpose.
 *
 * One pending prompt at a time. A second click while a prompt is open replaces
 * the first rather than queueing: the reader asked for two things, and the
 * honest state is the one they asked for most recently.
 */

import type { PlatformAdapter } from "../platform/adapter.js";
import type { AdapterError } from "../platform/errors.js";
import type { DocHandle, LinkAnnotation, OutlineNode, PdfDestination } from "../platform/types.js";
import type { NavigationCommand } from "./keyboard.js";
import { resolveNavigationKey } from "./keyboard.js";
import type { LinkDecision, LinkRefusal } from "./links.js";
import { alignmentForDestination, decideLinkAction } from "./links.js";
import type { OutlineRow } from "./outline.js";
import {
	expandSubtree,
	flattenOutline,
	initialExpansion,
	moveOutlineFocus,
	rowPage,
} from "./outline.js";
import type { NormalisedLabelRange } from "./page-labels.js";
import { hasMeaningfulLabels, normaliseLabelRanges, pageLabelFor } from "./page-labels.js";
import type { NavigationCatalogue, PageListCatalogue } from "./strings.js";
import { createNavigationStrings, createPageListStrings } from "./strings.js";

/** A link the reader clicked, and what the viewer decided about it. */
export interface LinkActivation {
	readonly link: LinkAnnotation;
	readonly decision: LinkDecision;
}

/** The external destination awaiting the reader's confirmation. */
export interface PendingExternal {
	/** Verbatim, never shortened. See `links.ts`. */
	readonly url: string;
	/** The host shown beside it, or `null` when the URI has no authority. */
	readonly host: string | null;
	/** The link's `/Contents`, when the document gave it one. */
	readonly title: string | null;
}

/** Where a navigation request wants to go. */
export interface NavigationTarget {
	readonly page: number;
	readonly align: "start" | "centre";
	/**
	 * What asked for it, so a shell can scroll *and* focus: activating a
	 * bookmark should move the document and leave the reader's focus where they
	 * can keep reading, and a target carrying only a page could not say.
	 */
	readonly from: "outline" | "thumbnail" | "link";
}

/** Everything the shell paints for navigation, in one immutable value. */
export interface NavigationState {
	/** False when the host has no navigation port at all. */
	readonly available: boolean;
	/** True when the document has an outline with at least one item. */
	readonly hasOutline: boolean;
	/** The visible outline rows, in document order. */
	readonly rows: readonly OutlineRow[];
	/** The focused row's id, or `null`. Roving tabindex follows this. */
	readonly focusedId: string | null;
	/**
	 * The page the document is showing, 0-based. Echoed so a shell can mark the
	 * current outline row (`aria-current`) without keeping a second copy of the
	 * reader's position — the same reason the page list and search both echo it.
	 */
	readonly currentPage: number;
	/** True while the panel owns the keyboard. */
	readonly focused: boolean;
	/** Ids the reader can expand or collapse. */
	readonly parentIds: ReadonlySet<string>;
	/** True when the row cap stopped the walk; the panel must say so. */
	readonly truncated: boolean;
	/**
	 * The document's own label for a page, or `null` when it has none worth
	 * showing. A label identical to the page number is noise, so a document whose
	 * only range is "decimal from 1" gets `null` and the page list's own wording
	 * — see `hasMeaningfulLabels`.
	 */
	readonly pageLabel: (page: number) => string | null;
	/** The page a navigation request wants, or `null`. The shell calls `goToPage`. */
	readonly target: NavigationTarget | null;
	/** The awaiting-confirmation external destination, or `null`. */
	readonly pendingExternal: PendingExternal | null;
	/** The last link the reader activated and what happened, or `null`. */
	readonly lastActivation: LinkActivation | null;
	/** Why the last activation was refused, or `null`. Drives the live region. */
	readonly refusal: LinkRefusal | null;
	/** Polite live-region text. */
	readonly announcement: string;
	/** `aria-label` for the navigation region. */
	readonly regionLabel: string;
	/** The outline panel's heading. */
	readonly outlineLabel: string;
	/** What to show instead of rows: none, "no outline", or "unavailable". */
	readonly placeholder: "none" | "empty" | "unavailable";
	/** The row cap's sentence, or `null` when the outline fitted. */
	readonly truncatedNote: string | null;
	/** The prompt's title, confirm and cancel labels. */
	readonly externalTitle: string;
	readonly externalConfirm: string;
	readonly externalCancel: string;
	/** The prompt's body, with the full destination. */
	readonly externalBody: (pending: PendingExternal) => string;
}

export interface NavigationOptions {
	readonly adapter: PlatformAdapter;
	readonly doc: DocHandle;
	/** Navigation catalogue override, merged over English. */
	readonly strings?: Partial<NavigationCatalogue>;
	/** Page-list catalogue, so a page is worded one way everywhere. */
	readonly pageStrings?: Partial<PageListCatalogue>;
	readonly onState?: (state: NavigationState) => void;
	readonly onError?: (error: AdapterError) => void;
}

/** The controller the shell programs against. */
export interface NavigationController {
	/** Load the outline, labels and destinations. Idempotent while loading. */
	load(): Promise<NavigationState>;
	/** Report which page the document is showing, for `aria-current`. */
	setCurrentPage(page: number): NavigationState;
	/** Move the roving focus to a row by id; unknown ids are ignored. */
	focusRow(id: string): NavigationState;
	/** Report whether the panel owns the keyboard. */
	setFocused(focused: boolean): NavigationState;
	/** Expand or collapse a row, expanding its subtree. */
	toggle(id: string): NavigationState;
	/** Expand a row's subtree. */
	expand(id: string): NavigationState;
	/** Collapse a row's subtree. */
	collapse(id: string): NavigationState;
	/** Activate a row: the reader pressed Enter on it. */
	activateRow(id: string): NavigationState;
	/**
	 * Activate a link annotation. The only path to `openExternal`, and only
	 * after {@link NavigationController.confirmExternal}.
	 */
	activateLink(link: LinkAnnotation): NavigationState;
	/** The reader confirmed the pending external destination. */
	confirmExternal(): Promise<NavigationState>;
	/** The reader dismissed the prompt. */
	cancelExternal(): NavigationState;
	/** Resolve a key press through the navigation key map, or `null`. */
	handleKey(key: string): NavigationState | null;
	/** The last state. */
	state(): NavigationState;
	subscribe(listener: (state: NavigationState) => void): () => void;
	/** Abort in-flight work. Idempotent. */
	dispose(): void;
}

/** Create the navigation controller. */
export function createNavigation(options: NavigationOptions): NavigationController {
	const doc = options.doc;
	const strings = createNavigationStrings(options.strings);
	const pageStrings = createPageListStrings(options.pageStrings);
	const port = options.adapter.navigation;

	let nodes: readonly OutlineNode[] = [];
	let destinations = new Map<string, PdfDestination>();
	let labels: readonly NormalisedLabelRange[] = [];
	let expanded = new Set<string>();
	let currentPage = 0;
	let focusedId: string | null = null;
	let focused = false;
	let target: NavigationTarget | null = null;
	let pendingExternal: PendingExternal | null = null;
	let lastActivation: LinkActivation | null = null;
	let refusal: LinkRefusal | null = null;
	let announced: string | null = null;
	let loading: Promise<NavigationState> | null = null;
	let disposed = false;
	let lastState: NavigationState | null = null;
	const subscribers = new Set<(state: NavigationState) => void>();

	/** The `/Dests` lookup the pure decider and the outline both resolve with. */
	function resolveName(name: string): PdfDestination | undefined {
		return destinations.get(name);
	}

	/**
	 * The label ranges worth showing.
	 *
	 * A document whose only range is "decimal from 1" labels its pages exactly
	 * as the page list already does, so showing it would add a second way to say
	 * the same thing and nothing else.
	 */
	function meaningfulLabels(): readonly NormalisedLabelRange[] {
		return hasMeaningfulLabels(labels, doc.pageCount) ? labels : [];
	}

	/**
	 * A page as the reader is told about it, in one wording.
	 *
	 * The document's own label wins when it exists ("Page iv"); otherwise the
	 * page list's sentence is used ("Page 4 of 900"). Two panels must not be able
	 * to word the same page two ways, and passing the page list's catalogue in —
	 * rather than re-deriving a number here — is what buys that.
	 */
	function spokenPageLabel(page: number): string {
		const table = meaningfulLabels();
		return table.length === 0
			? pageStrings.pageLabel(page, doc.pageCount)
			: pageLabelFor(page, undefined, table);
	}

	function buildState(): NavigationState {
		const flat = flattenOutline({ nodes, expanded });
		const rows = flat.rows;
		if (focusedId !== null && !rows.some((row) => row.id === focusedId)) {
			// The focused row went away because a collapse or the row cap hid it.
			// Focus follows the tree: keeping a stale id would leave the roving
			// tabindex pointing at a row that is not on screen.
			focusedId = null;
		}
		const table = meaningfulLabels();
		return {
			available: port !== undefined,
			hasOutline: nodes.length > 0,
			rows,
			focusedId,
			currentPage,
			focused,
			parentIds: flat.parentIds,
			truncated: flat.truncated,
			pageLabel: (page) => (table.length === 0 ? null : pageLabelFor(page, undefined, table)),
			target,
			pendingExternal,
			lastActivation,
			refusal,
			announcement: announced ?? "",
			regionLabel: strings.regionLabel,
			outlineLabel: strings.outlineLabel,
			placeholder: port === undefined ? "unavailable" : nodes.length === 0 ? "empty" : "none",
			truncatedNote: flat.truncated ? strings.truncated(rows.length, flat.itemCount) : null,
			externalTitle: strings.externalTitle,
			externalConfirm: strings.externalConfirm,
			externalCancel: strings.externalCancel,
			// The prompt's body always carries the **full** destination. A
			// shortened form in the dialog is the exact failure ADR-P0020's prompt
			// requirement exists to prevent.
			externalBody: (pending) => strings.externalBody(pending.url, pending.host ?? pending.url),
		};
	}

	function publish(): NavigationState {
		lastState = buildState();
		for (const listener of subscribers) {
			listener(lastState);
		}
		return lastState;
	}

	/** Announce something, once, in the panel's live region. */
	function say(text: string): void {
		announced = text;
	}

	/**
	 * Load the three document structures.
	 *
	 * One failure does not lose the others: a document whose outline walk times
	 * out should still have working page labels, and an error that took the whole
	 * panel down would be a worse answer than a panel with a hole in it. The error
	 * goes to `onError`, and `available` stays true — the host *does* have the
	 * port; what failed is this document's walk.
	 *
	 * A superseded run cannot write. `load()` is idempotent while one is in
	 * flight, and `dispose()` makes any late result inert — the same
	 * belt-and-braces pattern `search.ts` uses, for the same reason: an
	 * `AbortSignal` is only ever a promise the transport keeps.
	 */
	async function runLoad(): Promise<NavigationState> {
		if (port === undefined) {
			say(strings.unavailable());
			return publish();
		}
		try {
			const [outline, labelRanges, named] = await Promise.all([
				port.outline(doc),
				port.pageLabels(doc),
				port.destinations(doc),
			]);
			if (disposed) {
				return lastState ?? buildState();
			}
			nodes = outline;
			labels = normaliseLabelRanges(labelRanges);
			destinations = new Map(named.map((entry) => [entry.name, entry.destination]));
			// Initial expansion is the document's own: only items with a negative
			// `/Count` start closed. See `initialExpansion`.
			expanded = new Set(initialExpansion(outline));
			focusedId = null;
			say(nodes.length === 0 ? strings.empty() : "");
			return publish();
		} catch (error) {
			if (disposed) {
				return lastState ?? buildState();
			}
			options.onError?.(error as AdapterError);
			// The *unavailable* sentence, because what failed is the document
			// walk and the honest thing to tell the reader is that the navigation
			// could not be read. It is a different claim from "this host has no
			// navigation port", and the two keys stay separate.
			say(strings.unavailable());
			return publish();
		}
	}

	/** The key each row movement is expressed in, for `moveOutlineFocus`. */
	const MOVEMENT_KEY = {
		next: "ArrowDown",
		previous: "ArrowUp",
		first: "Home",
		last: "End",
		"open-child": "ArrowRight",
		"open-parent": "ArrowLeft",
	} as const;

	/**
	 * Apply a tree keyboard command to the current state.
	 *
	 * Every action is a pure function of the rows and the expansion set, and each
	 * ends in `publish()`, so the state after a key press is reconstructible from
	 * the same inputs — which is what makes the key map testable without a shell
	 * standing next to it.
	 */
	function applyCommand(command: NavigationCommand): NavigationState {
		const state = lastState ?? buildState();
		const rows = state.rows;
		const from = state.focusedId ?? rows[0]?.id ?? "";
		switch (command.action) {
			case "next":
			case "previous":
			case "first":
			case "last":
			case "open-child":
			case "open-parent": {
				const moved = moveOutlineFocus(rows, from, MOVEMENT_KEY[command.action]);
				return moved === null ? publish() : controller.focusRow(moved);
			}
			case "expand":
				return controller.expand(from);
			case "collapse":
				return controller.collapse(from);
			case "open":
				return controller.activateRow(from);
			default:
				return publish();
		}
	}

	/** Parent ids from the current flattening, so expand/collapse stay in sync. */
	function parentIds(): ReadonlySet<string> {
		return flattenOutline({ nodes, expanded }).parentIds;
	}

	/**
	 * Follow a row's destination, or explain why it cannot.
	 *
	 * A row with no resolvable destination is *not* an error to hide: the row says
	 * so in its accessible name (`strings.row(title, null)`) and the live region
	 * repeats the reason, so a reader who pressed Enter on a broken bookmark
	 * learns that the document's destination is missing rather than concluding
	 * that the viewer ignored them.
	 */
	function followRow(id: string, from: NavigationTarget["from"]): NavigationState {
		const row = (lastState ?? buildState()).rows.find((candidate) => candidate.id === id);
		if (row === undefined) {
			return publish();
		}
		const page = rowPage(row, destinations);
		if (page === null) {
			refusal = "missing-destination";
			say(strings.row(row.title, null));
			return publish();
		}
		const clamped = Math.min(doc.pageCount - 1, Math.max(0, page));
		target = { page: clamped, align: "start", from };
		say(strings.moved(spokenPageLabel(clamped)));
		return publish();
	}

	/**
	 * Activate a link annotation.
	 *
	 * The decision comes from `links.ts` and is not second-guessed here. The three
	 * outcomes, and what this function does with each:
	 * - `internal`: publish a target; the shell moves the page list.
	 * - `external`: publish a prompt. **Nothing is opened.**
	 * - `blocked`: record the refusal and announce it.
	 *
	 * Every activation clears the previous prompt, because a reader who clicks a
	 * second link has answered the first question by moving on.
	 */
	function activate(link: LinkAnnotation): NavigationState {
		const decision = decideLinkAction(link.action, resolveName);
		lastActivation = { link, decision };
		refusal = decision.kind === "blocked" ? decision.reason : null;
		pendingExternal = null;
		if (decision.kind === "blocked") {
			say(strings.refusal(decision.reason));
			return publish();
		}
		if (decision.kind === "external") {
			pendingExternal = {
				url: decision.url,
				host: decision.host.length === 0 ? null : decision.host,
				title: link.contents ?? null,
			};
			say(strings.externalBody(decision.url, decision.host));
			return publish();
		}
		const page = Math.min(doc.pageCount - 1, Math.max(0, decision.destination.page));
		target = { page, align: alignmentForDestination(decision.destination), from: "link" };
		say(strings.moved(spokenPageLabel(page)));
		return publish();
	}

	const controller: NavigationController = {
		load() {
			// Idempotent while a load is in flight: a shell that calls `load()`
			// on every focus event must not walk the outline tree forty times.
			loading ??= runLoad();
			return loading;
		},

		setCurrentPage(page) {
			const next = Number.isFinite(page)
				? Math.min(doc.pageCount - 1, Math.max(0, Math.trunc(page)))
				: 0;
			if (next === currentPage) {
				return lastState ?? buildState();
			}
			currentPage = next;
			return publish();
		},

		focusRow(id) {
			if (!(lastState ?? buildState()).rows.some((row) => row.id === id)) {
				return lastState ?? buildState();
			}
			focusedId = id;
			return publish();
		},

		setFocused(next) {
			if (next === focused) {
				return lastState ?? buildState();
			}
			focused = next;
			return publish();
		},

		toggle(id) {
			const row = (lastState ?? buildState()).rows.find((candidate) => candidate.id === id);
			if (row === undefined || !row.hasChildren) {
				return lastState ?? buildState();
			}
			return row.expanded ? controller.collapse(id) : controller.expand(id);
		},

		expand(id) {
			if (!parentIds().has(id)) {
				return lastState ?? buildState();
			}
			for (const child of expandSubtree(nodes, id)) {
				expanded.add(child);
			}
			return publish();
		},

		collapse(id) {
			// Collapsing removes the id and every id beneath it, so a parent's
			// collapse also closes its open children. That is what a reader means
			// by collapsing a chapter.
			for (const key of [...expanded]) {
				if (key === id || key.startsWith(`${id}.`)) {
					expanded.delete(key);
				}
			}
			return publish();
		},

		activateRow(id) {
			return followRow(id, "outline");
		},

		activateLink: activate,

		async confirmExternal() {
			const pending = pendingExternal;
			if (pending === null) {
				return lastState ?? buildState();
			}
			pendingExternal = null;
			try {
				// The one and only call site of `openExternal` in this file, and it
				// is unreachable without a pending prompt, which only
				// `activateLink` can create. The test asserts both halves.
				await options.adapter.window.openExternal(pending.url);
				say(strings.externalTitle);
			} catch (error) {
				options.onError?.(error as AdapterError);
			}
			return publish();
		},

		cancelExternal() {
			if (pendingExternal === null) {
				return lastState ?? buildState();
			}
			pendingExternal = null;
			say("");
			return publish();
		},

		handleKey(key) {
			const state = lastState ?? buildState();
			const focusedRow = state.rows.find((row) => row.id === state.focusedId);
			const command = resolveNavigationKey(key, {
				hasFocus: state.focused,
				hasRows: state.rows.length > 0,
				focusedHasChildren: focusedRow?.hasChildren ?? false,
				focusedExpanded: focusedRow?.expanded ?? false,
			});
			// `null` means the key is not ours and the shell must keep it: the page
			// list claims the same arrows, and this resolver is the reason both
			// can.
			return command === null ? null : applyCommand(command);
		},

		state() {
			return lastState ?? buildState();
		},

		subscribe(listener) {
			subscribers.add(listener);
			return () => {
				subscribers.delete(listener);
			};
		},

		dispose() {
			disposed = true;
			loading = null;
			subscribers.clear();
		},
	};

	// Publish the initial state so a shell can paint the panel frame before the
	// first `load()`; no engine work is scheduled from it.
	publish();
	return controller;
}
