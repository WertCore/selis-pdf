/**
 * SL-4.UI.06 (the extension's half) — the viewer view the page mounts.
 *
 * The web host's `boot.js` is the shell half of UI.06; this file is the MV3
 * viewer's half, and it is the same shape of seams as the rest of this
 * package: `apps/ui` stays headless (its controllers render nothing, ADR-P0044),
 * so *something* has to build the DOM, hand the browser facts over as
 * parameters, and repaint from published states. That is all this module is —
 * it holds no `chrome.*` call and reaches no remote origin.
 *
 * ## The two panels, and the one page number they share
 *
 * {@link createNavigation} owns the outline, the page labels, the name tree
 * and the external-link prompt; {@link createPageList} owns the window, the
 * scroll offset and the tile ladder. Neither may move the reader by itself —
 * `goToPage` is the only way to move (UI.02's rule) — so the controller's
 * `NavigationState.target` is **published** and consumed exactly once here
 * (by object identity, {@link consumedTarget}), which is what stops a stale
 * target from yanking the reader back on the next scroll that happens not to
 * cross a page boundary.
 *
 * ## What is deliberately *not* here
 *
 * - **The canvas click overlay for links.** Without it nothing in this page
 *   can call `controller.activateLink`, which is why the prompt's wiring is
 *   driven by {@link NavigationState.pendingExternal} and exercised by tests
 *   rather than by a click: the prompt is real, its trigger is future work and
 *   `REUSE.md` says so.
 * - **The thumbnail rail.** `thumbnails.ts` exists in `apps/ui`; the rail is
 *   the next slice and is not pretended into existence here.
 * - **A keyboard map for the page tiles.** Native scrolling is what moves the
 *   pages, so `ResolveNavigationKey` on lines below claims keys *only* inside
 *   the outline tree; the stage region keeps its own key set untouched, and
 *   the tile's roving tabindex puts exactly one page in the tab order.
 */

import type { PlatformAdapter } from "../../ui/src/platform/adapter.js";
import type { AdapterError } from "../../ui/src/platform/errors.js";
import type {
	DocHandle,
	NamedDestination,
	PdfDestination,
} from "../../ui/src/platform/types.js";
import { createTileCompositor } from "../../ui/src/viewer/compositor.js";
import type { NavigationController } from "../../ui/src/viewer/navigation.js";
import type { NavigationState } from "../../ui/src/viewer/navigation.js";
import { createNavigation } from "../../ui/src/viewer/navigation.js";
import type { NavigationKeyContext } from "../../ui/src/viewer/keyboard.js";
import { resolveNavigationKey } from "../../ui/src/viewer/keyboard.js";
import type { OutlineRow } from "../../ui/src/viewer/outline.js";
import { rowPage } from "../../ui/src/viewer/outline.js";
import type { PageList, PageListMetrics, PageListState, PageTileView } from "../../ui/src/viewer/page-list.js";
import { DEFAULT_METRICS, createPageList } from "../../ui/src/viewer/page-list.js";
import type { NavigationStrings, PageListStrings } from "../../ui/src/viewer/strings.js";
import {
	EN_NAVIGATION_CATALOGUE,
	EN_PAGE_LIST_CATALOGUE,
	createNavigationStrings,
	createPageListStrings,
} from "../../ui/src/viewer/strings.js";
import {
	createAnimationFrameClock,
	createCanvasRaster,
	createMainThreadSurface,
} from "./ext/surface.js";

/**
 * ## Pure decisions first
 *
 * Every helper below is total, has no DOM, and is what a repaint read. They
 * are exported because the suite runs in Node, where the only way to reach
 * this module's decisions is to *call* them — there is no DOM to mount against
 * and the repo ships no DOM shim to simulate one.
 */

/** Parse a CSS length in pixels, or hand back `fallback` when it is not one. */
export function parsePxOr(value: string | undefined | null, fallback: number): number {
	if (value === undefined || value === null) {
		return fallback;
	}
	const parsed = Number.parseFloat(value);
	return Number.isFinite(parsed) ? parsed : fallback;
}

/**
 * The two pixel values the page-list controller wants, read from the density
 * tokens the page already carries.
 *
 * Read *here* rather than imported because they are CSS lengths the host
 * resolves at runtime; the fallbacks are {@link DEFAULT_METRICS}'s own, which
 * is what makes a page whose stylesheet failed to arrive still lay out
 * honestly instead of dividing by zero.
 */
export function metricsFromStyle(source: {
	getPropertyValue(name: string): string;
}): PageListMetrics {
	return {
		pageGap: parsePxOr(
			source.getPropertyValue("--selis-density-page-gap"),
			DEFAULT_METRICS.pageGap,
		),
		padding: parsePxOr(source.getPropertyValue("--selis-space-6"), DEFAULT_METRICS.padding),
	};
}

/** What one outline row renders as, after `/Dests` resolution. */
export type OutlineRowPlan =
	| { readonly kind: "target"; readonly page: number }
	| { readonly kind: "unresolved"; readonly named: string }
	| { readonly kind: "inert" };

/**
 * The pure target decider for one row.
 *
 * A row that names a page directly, or a destination the document's name tree
 * defines, is a **target** — a control that moves the reader. A row that names
 * a destination which does not resolve stays visible as **unresolved** (the
 * reader is told the document does not define it), and a row with neither is
 * **inert** text. Text instead of a dead control is the boot.js rule, and the
 * controller's own `followRow` agrees: it refuses and announces rather than
 * moving nowhere.
 */
export function outlineRowPlan(
	row: OutlineRow,
	destinations: ReadonlyMap<string, PdfDestination>,
): OutlineRowPlan {
	const page = rowPage(row, destinations);
	if (page !== null) {
		return { kind: "target", page };
	}
	if (row.namedDestination !== null) {
		return { kind: "unresolved", named: row.namedDestination };
	}
	return { kind: "inert" };
}

/** One visible outline row, plus the subtree the panel nests beneath it. */
export interface OutlineTreeNode {
	readonly row: OutlineRow;
	/** Position among this row's siblings, 1-based for `aria-posinset`. */
	readonly position: number;
	readonly children: readonly OutlineTreeNode[];
}

/**
 * Re-nest the controller's flat rows into the tree the panel paints.
 *
 * The flat list exists so the controller's state is one immutable value, but
 * the panel renders the tree **nested** — a `<ol>` inside its parent's `<li>`
 * — because that is the hierarchy a screen reader navigates (the same shape
 * the web host's `boot.js` ships). `flattenOutline` emits each subtree's rows
 * directly after its parent, so grouping consecutive children by their
 * `parentId` rebuilds the document's own tree in document order.
 */
export function nestOutlineRows(rows: readonly OutlineRow[]): readonly OutlineTreeNode[] {
	const byParent = new Map<string | null, OutlineRow[]>();
	for (const row of rows) {
		const siblings = byParent.get(row.parentId);
		if (siblings === undefined) {
			byParent.set(row.parentId, [row]);
		} else {
			siblings.push(row);
		}
	}
	const nest = (parentId: string | null): OutlineTreeNode[] =>
		(byParent.get(parentId) ?? []).map((row, index) => ({
			row,
			position: index + 1,
			children: nest(row.id),
		}));
	return nest(null);
}

/**
 * The outline row that points at the page the document is showing, or `null`.
 *
 * The controller echoes `currentPage`; the panel marks *something* with
 * `aria-current`, and "the first row whose destination is this page" is the
 * honest reading — several rows may name the same page and the reader needs
 * the panel to pick one consistently rather than not at all.
 */
export function currentOutlineId(
	rows: readonly OutlineRow[],
	currentPage: number,
	destinations: ReadonlyMap<string, PdfDestination>,
): string | null {
	for (const row of rows) {
		if (rowPage(row, destinations) === currentPage) {
			return row.id;
		}
	}
	return null;
}

/** One explanatory line the outline panel shows instead of rows. */
export interface OutlineNote {
	/** The `data-role` the browser check and the class hooks share. */
	readonly dataRole: "no-outline" | "unavailable" | "outline-truncated";
	readonly text: string;
}

export interface OutlineNotesOptions {
	/** The controller's first `load()` has settled. */
	readonly loaded?: boolean;
	/** That `load()` rejected. Distinct from "loaded and found nothing". */
	readonly loadFailed?: boolean;
}

/**
 * The panel's explanatory lines, in the order they show.
 *
 * The `{loaded}` gate exists because the controller publishes a placeholder
 * **before** `load()` resolves — and its placeholder then reads "empty", a
 * claim it cannot yet make. Until the load settles the panel shows nothing
 * rather than "this document has no outline" for a document it has not read.
 *
 * After it settles the three real facts are distinct on purpose: an outline
 * that exists and is empty, a document walk that failed (announced with the
 * *unavailable* sentence even though the host does have the port), and rows
 * that fitted but a cap truncated are three different states published as
 * three different shapes.
 */
export function outlineNotes(
	state: NavigationState,
	strings: NavigationStrings,
	options: OutlineNotesOptions = {},
): readonly OutlineNote[] {
	if (options.loaded !== true && options.loadFailed !== true) {
		return [];
	}
	if (options.loadFailed === true || state.placeholder === "unavailable") {
		return [{ dataRole: "unavailable", text: strings.unavailable() }];
	}
	if (state.placeholder === "empty") {
		return [{ dataRole: "no-outline", text: strings.empty() }];
	}
	// Rows are present; the only note a full list may still carry is the cap's.
	if (state.rows.length > 0) {
		if (state.truncatedNote === null) {
			return [];
		}
		return [{ dataRole: "outline-truncated", text: state.truncatedNote }];
	}
	return [];
}

/**
 * A page as *this* panel is told about it.
 *
 * The document's own label wins ("Page iv"); otherwise the page list's
 * sentence is used ("Page 4 of 900"). Two panels must not word one page two
 * ways, which is why the fallback is the page list's *catalogue* rather than
 * a locally formatted number.
 */
export function outlineRowSpokenLabel(
	page: number,
	state: NavigationState,
	pageStrings: PageListStrings,
	pageCount: number,
): string {
	return state.pageLabel(page) ?? pageStrings.pageLabel(page, pageCount);
}

/** The external-link prompt's shape, open or closed. */
export type ExternalPromptPlan =
	| { readonly open: false }
	| {
			readonly open: true;
			readonly title: string;
			/** The catalogue's sentence, carrying the **verbatim** destination. */
			readonly body: string;
			readonly url: string;
			readonly host: string | null;
			readonly confirm: string;
			readonly cancel: string;
	  };

/** What the prompt renders for the state the shell is holding. */
export function externalPromptPlan(state: NavigationState): ExternalPromptPlan {
	const pending = state.pendingExternal;
	if (pending === null) {
		return { open: false };
	}
	return {
		open: true,
		title: state.externalTitle,
		// The body always carries the full URL (navigation.ts's rule): a prompt
		// showing "example.com" for a redirect is how a click gets earned that
		// nobody made.
		body: state.externalBody(pending),
		url: pending.url,
		host: pending.host,
		confirm: state.externalConfirm,
		cancel: state.externalCancel,
	};
}

/** How a prompt's open state moved between two published states. */
export type PromptTransition = "none" | "open" | "closed";

/**
 * A prompt's *transition* between two navigation states.
 *
 * A second activation **replaces** the first prompt rather than queueing it
 * (the controller publishes only the most recent request), so a fresh pending
 * value renders as another `open`. Nothing changes on `none` — same object,
 * nothing to repaint — which is what keeps a scroll-step repaint from
 * re-focusing the dialog that is already there.
 */
export function promptTransition(
	previous: NavigationState["pendingExternal"],
	next: NavigationState["pendingExternal"],
): PromptTransition {
	if (previous === next) {
		return "none";
	}
	if (previous === null) {
		return "open";
	}
	if (next === null) {
		return "closed";
	}
	return "open";
}

/** The keys the prompt itself claims; the panel's keys stay with the panel. */
export function promptKeyAction(key: string): "cancel" | null {
	return key === "Escape" ? "cancel" : null;
}

/**
 * The key resolver's context, built from the published state.
 *
 * `controller.handleKey` derives exactly this internally; re-deriving it here
 * (from the *same* state) lets the DOM keydown ask "is the key ours?" and
 * `preventDefault()` only then. Both resolvers read one state and nothing
 * mutates in between, so the double-resolve is safe and the key map stays the
 * single authority over which key means what.
 */
export function navigationKeyContext(state: NavigationState): NavigationKeyContext {
	const focusedRow = state.rows.find((row) => row.id === state.focusedId);
	return {
		hasFocus: state.focused,
		hasRows: state.rows.length > 0,
		focusedHasChildren: focusedRow?.hasChildren ?? false,
		focusedExpanded: focusedRow?.expanded ?? false,
	};
}

/**
 * ## The mount
 *
 * Everything above is what the mount replays. The DOM composition itself runs
 * only in a browser page, and it is covered by the type checker plus the
 * browser check leg, which is labelled a future slice.
 */

/** What a shell hands {@link mountViewerView}. */
export interface ViewerViewOptions {
	readonly adapter: PlatformAdapter;
	readonly doc: DocHandle;
	/** The page element the view replaces the content of. */
	readonly container: HTMLElement;
	/** Adapter errors (a failed document walk, a failed tile) reported upward. */
	readonly onError?: (error: AdapterError) => void;
}

/** The mounted view. {@link dispose} is idempotent. */
export interface ViewerView {
	readonly dispose: () => void;
}

/** Row-side data roles, verbatim with the web host's naming. */
const DATA_ROLE = {
	outlineHeading: "outline-heading",
	outlineNotes: "outline-notes",
	outlineLive: "navigation-live",
	outlineItem: "outline-item",
	outlineRowTarget: "outline-target",
	outlineRowLabel: "outline-label",
	outlineRowUnresolved: "outline-unresolved",
	outlineExpander: "outline-expander",
	pageScroll: "page-list-scroll",
	pageTile: "page-tile",
	pageLive: "page-list-live",
	prompt: "external-prompt",
	promptQuestion: "external-question",
	promptBody: "external-url",
	promptConfirm: "external-confirm",
	promptCancel: "external-cancel",
} as const;

/** The ids the accessible-name wiring needs; one mount per page, so stable. */
const ELEMENT_ID = {
	outlineHeading: "selis-viewer-outline-heading",
	promptQuestion: "selis-viewer-prompt-question",
	promptBody: "selis-viewer-prompt-body",
} as const;

/**
 * Mount the viewer view onto the page element.
 *
 * Composition order matters in exactly one place: the canvas must be in the
 * document before the raster reads its computed `--selis-bg`, and the
 * controllers must exist before the skeleton's `aria-label`s read their
 * published state — so the DOM build sits between them on purpose.
 */
export function mountViewerView(options: ViewerViewOptions): ViewerView {
	const { adapter, doc, container } = options;

	// ── 1. The two controllers, over one catalogue object ────────────────────
	//
	// The English catalogues are passed through **unchanged** (not re-bound per
	// component), so the row sentence the outline speaks and the page the page
	// list speaks resolve from the same catalogue object — the exact property
	// "a page is worded one way everywhere" needs.
	const navStrings = createNavigationStrings(EN_NAVIGATION_CATALOGUE);
	const pageStrings = createPageListStrings(EN_PAGE_LIST_CATALOGUE);
	const nav: NavigationController = createNavigation({
		adapter,
		doc,
		strings: EN_NAVIGATION_CATALOGUE,
		pageStrings: EN_PAGE_LIST_CATALOGUE,
		onError: (error) => {
			options.onError?.(error);
		},
	});
	const list: PageList = createPageList({
		adapter,
		doc,
		strings: EN_PAGE_LIST_CATALOGUE,
		metrics: metricsFromStyle(globalThis.getComputedStyle(container)),
	});

	// The name tree, fetched beside the controller's load so named rows resolve
	// without the controller growing a second copy of the destinations. The
	// mount fetches it itself because `NavigationState` deliberately does not
	// re-publish a whole map it only needs for row resolution. A failure here
	// is an empty map — named rows become unresolved, which is the honest
	// answer when the resolution data could not be read.
	const destinations = new Map<string, PdfDestination>();
	let disposed = false;
	// The load's settlement, for the notes host: the pre-load state publishes
	// the "empty" placeholder before `load()` has read the document, so the
	// panel needs to know when it may make the claim it is showing.
	let navLoaded = false;
	let navLoadFailed = false;

	// ── 2. The skeleton ──────────────────────────────────────────────────────

	const view = document.createElement("div");
	view.className = "selis-viewer__mount";

	// The navigation sidebar. `role="region"` is named by the controller's own
	// `regionLabel` so the panel announces what it is rather than where it is.
	const navPanel = document.createElement("div");
	navPanel.className = "selis-navigation";
	navPanel.setAttribute("role", "region");
	navPanel.setAttribute("aria-label", nav.state().regionLabel);

	const section = document.createElement("div");
	section.className = "selis-navigation__section";
	navPanel.append(section);

	const outlineHeading = document.createElement("h2");
	outlineHeading.className = "selis-navigation__heading";
	outlineHeading.id = ELEMENT_ID.outlineHeading;
	outlineHeading.setAttribute("data-role", DATA_ROLE.outlineHeading);
	outlineHeading.textContent = nav.state().outlineLabel;
	section.append(outlineHeading);

	const notesHost = document.createElement("div");
	notesHost.className = "selis-viewer__notes";
	notesHost.dataset.role = DATA_ROLE.outlineNotes;
	section.append(notesHost);

	const outlineTree = document.createElement("ol");
	outlineTree.className = "selis-outline";
	outlineTree.setAttribute("role", "list");
	outlineTree.setAttribute("aria-labelledby", ELEMENT_ID.outlineHeading);
	section.append(outlineTree);

	// The nav panel's live line. It sits **outside** the outline scroller so a
	// repaint cannot march it out of view mid-scroll, and it doubles as the
	// links surface: activation announcements and refusals arrive here. There
	// are no sidebar link rows — without a canvas click overlay there is no
	// link to list, and an empty list that looks real would claim otherwise.
	const navLive = document.createElement("p");
	navLive.className = "selis-visually-hidden";
	navLive.setAttribute("data-role", DATA_ROLE.outlineLive);
	navLive.setAttribute("role", "status");
	navLive.setAttribute("aria-live", "polite");
	navPanel.append(navLive);

	// The document stage. The canvas paints the window; the scroll region
	// clips it. The canvas is the *first* child so it sits underneath the
	// (transparent) scroll region in paint order, and `page-list.css` keeps
	// it under every pointer with `pointer-events: none`.
	const stage = document.createElement("div");
	stage.className = "selis-viewer__stage";

	const canvas = document.createElement("canvas");
	canvas.className = "selis-page-list__surface";
	canvas.setAttribute("aria-hidden", "true");

	const scroller = document.createElement("div");
	scroller.className = "selis-viewer__scroller";
	scroller.setAttribute("data-role", DATA_ROLE.pageScroll);
	scroller.setAttribute("role", "region");
	scroller.setAttribute("aria-label", list.state().regionLabel);
	// The stage region itself is the tab stop (not each tile's scroll): native
	// keyboard scrolling is what turns pages here, and the page list's own key
	// map stays unclaimed — recorded as a limit in REUSE.md.
	scroller.tabIndex = 0;

	const plane = document.createElement("ol");
	plane.className = "selis-page-list--virtual";
	plane.setAttribute("role", "list");
	plane.setAttribute("aria-label", list.state().listLabel);
	scroller.append(plane);

	stage.append(canvas, scroller);

	const pageLive = document.createElement("p");
	pageLive.className = "selis-visually-hidden";
	pageLive.setAttribute("data-role", DATA_ROLE.pageLive);
	pageLive.setAttribute("role", "status");
	pageLive.setAttribute("aria-live", "polite");

	view.append(navPanel, stage, pageLive);
	container.replaceChildren(view);

	// ── 3. The compositor and surface, over the canvas now in the document ───

	// `createCanvasRaster` named a real 2d context only for a page-level
	// canvas that *has* one; a Node import of this module never reaches here.
	const surface = createMainThreadSurface({ raster: createCanvasRaster(canvas) });
	const clock = createAnimationFrameClock({
		request: (callback) => globalThis.requestAnimationFrame(callback),
		cancel: (id) => globalThis.cancelAnimationFrame(id),
	});
	const compositor = createTileCompositor({ list, surface, clock });

	// ── 4. Repainting: published states in, DOM patches out ─────────────────

	/** Title/focusable element of one outline row, by row id. */
	const rowEls = new Map<string, HTMLElement>();
	/** Elements of the visible tile window, by page. */
	const tileEls = new Map<number, HTMLElement>();
	/** Signature of the rows currently painted; a change forces a rebuild. */
	let treeSignature = "";
	/** The tile window signature, so a repaint only moves what moved. */
	let tileSignature = "";
	/** The last page-list plane sizes set, to skip idempotent style writes. */
	let planeWidth = -1;
	let planeHeight = -1;
	/** The navigation state to consume a target from, once. */
	let consumedTarget: NavigationState["target"] = null;
	/** The previously rendered pending external, for the prompt transition. */
	let renderedPending: NavigationState["pendingExternal"] = null;
	/** The focusable element a prompt stole focus from. */
	let promptTrigger: HTMLElement | null = null;
	/** Parked focus row, so a tree rebuild can put the reader back. */
	let parkedRowId: string | null = null;

	/** One placeholder tile, exactly the window's box for every ladder rung. */
	function tileElement(tile: PageTileView): HTMLElement {
		const el = tileEls.get(tile.page);
		if (el !== undefined) {
			return el;
		}
		const created = document.createElement("li");
		created.setAttribute("role", "listitem");
		created.setAttribute("data-role", DATA_ROLE.pageTile);
		tileEls.set(tile.page, created);
		return created;
	}

	function patchTile(el: HTMLElement, tile: PageTileView): void {
		el.className = tile.className;
		// Geometry is *document data* (`page-list.css`'s carrier contract), set
		// as custom properties rather than an inline style string.
		el.style.setProperty("--tile-x", `${tile.x}px`);
		el.style.setProperty("--tile-y", `${tile.y}px`);
		el.style.setProperty("--tile-w", `${tile.width}px`);
		el.style.setProperty("--tile-h", `${tile.height}px`);
		el.setAttribute("aria-label", tile.label);
		if (tile.current) {
			el.setAttribute("aria-current", "page");
		} else {
			el.removeAttribute("aria-current");
		}
		// Roving tabindex: the state already decided which page is the one in
		// the tab order. A tile claims none of the key map — native scrolling
		// on the stage region is what moves the reader.
		el.tabIndex = tile.tabIndex;
	}

	/** Repaint one page-list state: the plane, the window, the announcement. */
	function paintPageList(state: PageListState): void {
		if (disposed) {
			return;
		}
		if (planeWidth !== state.contentWidth || planeHeight !== state.contentHeight) {
			planeWidth = state.contentWidth;
			planeHeight = state.contentHeight;
			plane.style.setProperty("--page-list-width", `${planeWidth}px`);
			plane.style.setProperty("--page-list-height", `${planeHeight}px`);
		}
		const signature = state.tiles.map((tile) => tile.page).join(",");
		if (signature !== tileSignature) {
			tileSignature = signature;
			const kept = new Set(state.tiles.map((tile) => tile.page));
			for (const [page, el] of [...tileEls]) {
				if (!kept.has(page)) {
					el.remove();
					tileEls.delete(page);
				}
			}
		}
		// Insert-before walk so DOM order — the reading order — matches the
		// window even though the elements themselves are kept across frames.
		let previous: Element | null = null;
		for (const tile of state.tiles) {
			const el = tileElement(tile);
			if (previous === null) {
				if (plane.firstChild !== el) {
					plane.insertBefore(el, plane.firstChild);
				}
			} else if (previous.nextElementSibling !== el) {
				plane.insertBefore(el, previous.nextElementSibling);
			}
			previous = el;
			patchTile(el, tile);
		}
		pageLive.textContent = state.announcement;
		// The controller echoes the page it is showing, for `aria-current`. The
		// publish this triggers repaints the outline (an interaction-rate path),
		// not the page list, so the loop cannot chase itself.
		if (state.currentPage !== nav.state().currentPage) {
			nav.setCurrentPage(state.currentPage);
		}
		// The scroll offset the controller decided is authoritative. While the
		// reader scrolls, the value equals their own and the write is a no-op;
		// after `goToPage` it is what puts the reader where the pointer said.
		if (scroller.scrollTop !== state.scrollTop) {
			scroller.scrollTop = state.scrollTop;
		}
	}

	function buildRow(
		row: OutlineRow,
		plan: OutlineRowPlan,
		state: NavigationState,
		isCurrent: boolean,
		isTabbed: boolean,
	): HTMLElement {
		const name =
			plan.kind === "target"
				? navStrings.row(
						row.title,
						outlineRowSpokenLabel(plan.page, state, pageStrings, doc.pageCount),
					)
				: plan.kind === "unresolved"
					? navStrings.row(row.title, null)
					: row.title;
		const el =
			plan.kind === "target"
				? document.createElement("button")
				: document.createElement("span");
		if (plan.kind === "target") {
			const button = el as HTMLButtonElement;
			button.type = "button";
			button.setAttribute("data-role", DATA_ROLE.outlineRowTarget);
			// The machine-readable page, exactly as boot.js carries it, so a
			// browser check can read what the row points at without trusting the
			// label's prose.
			button.dataset.page = String(plan.page);
		} else if (plan.kind === "unresolved") {
			el.setAttribute("data-role", DATA_ROLE.outlineRowUnresolved);
			el.setAttribute("data-named-destination", plan.named);
		} else {
			el.setAttribute("data-role", DATA_ROLE.outlineRowLabel);
		}
		const classes = ["selis-outline__row"];
		if (isCurrent) {
			classes.push("selis-outline__row--current");
		}
		if (plan.kind === "unresolved") {
			classes.push("selis-outline__row--unresolved");
		}
		el.className = classes.join(" ");
		el.setAttribute("data-row-id", row.id);
		if (isCurrent) {
			el.setAttribute("aria-current", "true");
		}
		// Roving tabindex: one row in the tab order falls back to the tree's
		// first when no row holds focus, so Tab always has somewhere to land.
		el.tabIndex = isTabbed ? 0 : -1;
		el.setAttribute("aria-label", name);
		const title = document.createElement("span");
		title.className = "selis-outline__title";
		title.textContent = row.title;
		el.append(title);
		return el;
	}

	/** Build one subtree of li > row (+ expander) + nested ol, recursively. */
	function nodeElement(
		node: OutlineTreeNode,
		destinations: ReadonlyMap<string, PdfDestination>,
		state: NavigationState,
		currentId: string | null,
		tabOwner: string | null,
	): HTMLElement {
		const { row, position } = node;
		const plan = outlineRowPlan(row, destinations);
		const li = document.createElement("li");
		li.setAttribute("role", "listitem");
		li.setAttribute("data-role", DATA_ROLE.outlineItem);
		li.setAttribute("aria-level", String(row.depth + 1));
		li.setAttribute("aria-posinset", String(position));
		li.setAttribute("aria-setsize", String(row.setSize));
		if (row.hasChildren) {
			li.setAttribute("aria-expanded", String(row.expanded));
		}
		const rowEl = buildRow(row, plan, state, currentId === row.id, tabOwner === row.id);
		li.append(rowEl);
		rowEls.set(row.id, rowEl);
		if (row.hasChildren) {
			// The expander is a pointer affordance *inside* the row: click the
			// triangle to open, click the text to go. Keyboard readers already
			// own expansion (ArrowRight/ArrowLeft are the tree map), so the
			// triangle is `aria-hidden` — one tab stop per row is the main
			// reason; a second would eat the roving tabindex the rows form.
			const expander = document.createElement("span");
			expander.className = `selis-outline__expander selis-outline__expander--${
				row.expanded ? "expanded" : "collapsed"
			}`;
			expander.setAttribute("data-role", DATA_ROLE.outlineExpander);
			expander.setAttribute("aria-hidden", "true");
			expander.setAttribute("data-row-id", row.id);
			li.insertBefore(expander, rowEl);
		}
		if (node.children.length > 0) {
			// Nested, the way `boot.js` says: a reader hears the hierarchy, not
			// an indentation number nobody announced.
			const subtree = document.createElement("ol");
			subtree.className = "selis-outline__subtree";
			subtree.setAttribute("role", "list");
			for (const child of node.children) {
				subtree.append(nodeElement(child, destinations, state, currentId, tabOwner));
			}
			li.append(subtree);
		}
		return li;
	}

	/** Repaint the outline panel from one state. */
	function paintNav(state: NavigationState): void {
		if (disposed) {
			return;
		}
		// The current row and the tab owner are whole-state decisions, read
		// once per repaint: every pass below answers from the same value rather
		// than re-deriving it per row.
		const currentId = currentOutlineId(state.rows, state.currentPage, destinations);
		const tabOwner = state.focusedId ?? state.rows[0]?.id ?? null;
		// The focus tree moved (or is about to). Park the affected row so a
		// rebuild below can restore the reader's place instead of dropping it.
		parkedRowId = state.focusedId ?? parkedRowId;
		const signature = `${state.rows
			.map((row) => `${row.id}:${row.expanded ? "1" : "0"}`)
			.join(",")}|nav:${state.currentPage}`;
		if (signature !== treeSignature) {
			treeSignature = signature;
			rowEls.clear();
			// Was the reader's focus inside the tree *before* the rebuild? It
			// cannot be read afterwards — replaceChildren empties the tree and
			// drops focus to the body — so this pre-read is the only witness.
			const hadTreeFocus = outlineTree.contains(document.activeElement);
			const nodes = nestOutlineRows(state.rows);
			outlineTree.replaceChildren(
				...nodes.map((node) =>
					nodeElement(node, destinations, state, currentId, tabOwner),
				),
			);
			if (hadTreeFocus && parkedRowId !== null) {
				rowEls.get(parkedRowId)?.focus({ preventScroll: true });
			}
		}
		// The cheap pass: current row and roving tabindex move on every state,
		// without a rebuild.
		for (const row of state.rows) {
			const el = rowEls.get(row.id);
			if (el === undefined) {
				continue;
			}
			const isCurrent = row.id === currentId;
			el.classList.toggle("selis-outline__row--current", isCurrent);
			if (isCurrent) {
				el.setAttribute("aria-current", "true");
			} else {
				el.removeAttribute("aria-current");
			}
			el.tabIndex = row.id === tabOwner ? 0 : -1;
		}
		navLive.textContent = state.announcement;
		paintNotes(state);
	}

	/** Repaint the outline panel's explanatory lines from one state. */
	function paintNotes(state: NavigationState): void {
		if (disposed) {
			return;
		}
		// The same decision the tests read directly, replayed onto its host:
		// until the load settles the host is empty (the "empty" placeholder is
		// a claim the controller cannot make yet), and after it the three
		// notes — none, no-outline and truncated — are the only lines shown.
		notesHost.replaceChildren(
			...outlineNotes(state, navStrings, {
				loaded: navLoaded,
				loadFailed: navLoadFailed,
			}).map((note) => {
				const el = document.createElement("p");
				el.className = "selis-viewer__note";
				el.setAttribute("data-role", note.dataRole);
				el.textContent = note.text;
				return el;
			}),
		);
	}

	/** Repaint the external-link prompt from one state. */
	function paintPrompt(state: NavigationState): void {
		if (disposed) {
			return;
		}
		const plan = externalPromptPlan(state);
		const transition = promptTransition(renderedPending, state.pendingExternal);
		renderedPending = state.pendingExternal;
		const existing = view.querySelector<HTMLElement>(
			`[data-role="${DATA_ROLE.prompt}"]`,
		);
		if (!plan.open) {
			if (transition === "closed") {
				existing?.remove();
				// Focus returns to whatever the trigger was, when it is still in
				// the document: leaving the reader at the document without their
				// place is how a dialog becomes a dead end.
				if (promptTrigger !== null && promptTrigger.isConnected) {
					promptTrigger.focus();
				}
				promptTrigger = null;
			}
			return;
		}
		if (transition === "none" && existing !== null) {
			return;
		}
		// Captured on the transition that *opened* the prompt, before focus
		// moves into it. A replacement — a second activation while a prompt is
		// already painted — keeps the first trigger: focus is already inside a
		// dialog this branch is about to remove, and re-capturing would park
		// the reader on its own outgoing button.
		if (existing === null) {
			promptTrigger =
				document.activeElement instanceof HTMLElement ? document.activeElement : null;
		}
		existing?.remove();
		const prompt = document.createElement("div");
		prompt.className = "selis-navigation__dialog";
		prompt.setAttribute("data-role", DATA_ROLE.prompt);
		prompt.setAttribute("role", "dialog");
		prompt.setAttribute("aria-labelledby", ELEMENT_ID.promptQuestion);
		prompt.setAttribute("aria-describedby", ELEMENT_ID.promptBody);

		// The question carries the catalogue's title; the body sentence carries
		// the destination verbatim and IS the described-by target: the prompt's
		// description is the URL a reader must be able to check.
		const question = document.createElement("p");
		question.id = ELEMENT_ID.promptQuestion;
		question.className = "selis-navigation__heading";
		question.setAttribute("data-role", DATA_ROLE.promptQuestion);
		question.textContent = plan.title;
		const body = document.createElement("p");
		body.id = ELEMENT_ID.promptBody;
		body.className = "selis-navigation__url";
		body.setAttribute("data-role", DATA_ROLE.promptBody);
		body.textContent = plan.body;
		if (plan.host !== null) {
			prompt.setAttribute("data-host", plan.host);
		}
		prompt.append(question, body);

		const confirm = document.createElement("button");
		confirm.type = "button";
		confirm.className = "selis-viewer__btn";
		confirm.setAttribute("data-role", DATA_ROLE.promptConfirm);
		confirm.textContent = plan.confirm;
		confirm.addEventListener("click", () => {
			// THE only path to openExternal, and only this controller route can
			// produce a pending prompt. Fire-and-let-publish: the answered state
			// is what repaints a line we read from the state, not our return value.
			void nav.confirmExternal();
		});
		const cancel = document.createElement("button");
		cancel.type = "button";
		cancel.className = "selis-viewer__btn";
		cancel.setAttribute("data-role", DATA_ROLE.promptCancel);
		cancel.textContent = plan.cancel;
		cancel.addEventListener("click", () => {
			nav.cancelExternal();
		});
		prompt.addEventListener("keydown", (event) => {
			const action = promptKeyAction(event.key);
			if (action === null) {
				return;
			}
			// A dialog consumes Escape. `stopPropagation` keeps the shell's own
			// Escape (a failure panel) from also reacting behind the prompt.
			event.preventDefault();
			event.stopPropagation();
			nav.cancelExternal();
		});
		prompt.append(confirm, cancel);
		// The safe answer holds focus: Enter on "open an external link?" must
		// not leave the document. Open stays one Tab away.
		navPanel.append(prompt);
		cancel.focus();
	}

	// ── 5. Measurement, scroll and device-scale ─────────────────────────────

	/** Report the current stage facts; idempotent, and safe before it loads. */
	function measure(): void {
		if (disposed) {
			return;
		}
		compositor.update({
			width: scroller.clientWidth,
			height: scroller.clientHeight,
			scrollTop: scroller.scrollTop,
			devicePixelRatio: globalThis.devicePixelRatio,
		});
	}

	/** One-time stage focus, guarded so a late reveal cannot steal focus. */
	let stageFocused = false;

	function focusStageOnce(): void {
		if (disposed || stageFocused) {
			return;
		}
		const active = document.activeElement;
		// Only a reader who has focus "nowhere" (no element, the body, or
		// inside this view) gets moved. Anything else was aimed at something
		// outside our box deliberately, and is not ours to grab.
		const ours =
			active === null || active === globalThis.document.body || container.contains(active);
		if (!ours) {
			return;
		}
		stageFocused = true;
		scroller.focus({ preventScroll: true });
	}

	scroller.addEventListener("scroll", () => {
		measure();
	});

	const resizeObserver = new ResizeObserver(() => {
		measure();
		// The dust settles before the tab stop is offered; a first measure with
		// size 0 lays out nothing and the next one earns the focus.
		if (scroller.clientWidth > 0 && scroller.clientHeight > 0) {
			focusStageOnce();
		}
	});
	resizeObserver.observe(scroller);
	resizeObserver.observe(stage);

	/**
	 * Device-pixel-ratio changes re-measure: the ladder computes its request
	 * scale from what the surface will actually draw, and a stale ratio is the
	 * zoomed-in case a reader *sees* as softness. One-shot, then re-armed: a
	 * changed ratio means a new query for the new value.
	 */
	let dppxCleanup: (() => void) | null = null;
	function watchDppx(): void {
		if (disposed || typeof globalThis.matchMedia !== "function") {
			return;
		}
		dppxCleanup?.();
		dppxCleanup = null;
		const query = globalThis.matchMedia(
			`(resolution: ${globalThis.devicePixelRatio}dppx)`,
		);
		const listener = (): void => {
			if (disposed) {
				return;
			}
			watchDppx();
			measure();
		};
		query.addEventListener("change", listener);
		dppxCleanup = () => {
			query.removeEventListener("change", listener);
		};
	}
	watchDppx();

	// ── 6. The outline tree's key map, focus, expander and activation ────────

	/**
	 * Tree keyboard: the controller owns the map and the panel merely asks.
	 *
	 * `preventDefault` happens only when the resolver actually claimed the
	 * key — an unclaimed key passes through, which is what keeps Tab, the
	 * stage's own scrolling and a screen-reader virtual cursor alive on the
	 * panel.
	 */
	function onNavKeydown(event: KeyboardEvent): void {
		const state = nav.state();
		const context = navigationKeyContext(state);
		if (resolveNavigationKey(event.key, context) === null) {
			return;
		}
		event.preventDefault();
		const next = nav.handleKey(event.key);
		if (next === null) {
			return;
		}
		// The reader came here with the keyboard; the focus moves to where the
		// row went. `handleKey` is free to return the same state when the key
		// was claimed but did not move (Enter on a leaf, for instance), and no
		// move means no focus dance.
		const id = next.focusedId;
		const el = id === null ? undefined : rowEls.get(id);
		if (el !== undefined) {
			el.focus();
		}
	}

	navPanel.addEventListener("keydown", onNavKeydown);
	// Focus reporting is what "the panel owns the keyboard" means (keyboard.ts
	// distinguishes the claim by focus). Reporting focusout only when focus
	// leaves the panel keeps Edit-in-place states from flickering in between.
	navPanel.addEventListener("focusin", () => {
		nav.setFocused(true);
	});
	navPanel.addEventListener("focusout", (event) => {
		if (disposed) {
			return;
		}
		const related = event.relatedTarget;
		if (related instanceof Node && navPanel.contains(related)) {
			return;
		}
		nav.setFocused(false);
	});

	// Pointer: the expander toggles; the row activates. The expander sits
	// before the row element in the DOM, so click bubbles would toggle AND
	// activate — the stop is what keeps them two gestures.
	navPanel.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		if (target.getAttribute("data-role") !== DATA_ROLE.outlineExpander) {
			return;
		}
		const id = target.getAttribute("data-row-id");
		if (id !== null) {
			nav.toggle(id);
		}
	});

	// Row activation: the button's own click. Target rows only — inert and
	// unresolved rows are spans and have no action to wire.
	navPanel.addEventListener("click", (event) => {
		const target = event.target;
		if (!(target instanceof Element)) {
			return;
		}
		if (target.getAttribute("data-role") === DATA_ROLE.outlineExpander) {
			// Handled above; neither swallow this nor navigate.
			return;
		}
		if (target.getAttribute("data-role") !== DATA_ROLE.outlineRowTarget) {
			return;
		}
		const id = target.getAttribute("data-row-id");
		if (id !== null) {
			nav.activateRow(id);
		}
	});

	// ── 7. Subscriptions: the published flow the mount listens to ────────────

	/**
	 * Consume a navigation target the panel published: the outline's one
	 * request to move, handled through the page list's one mover.
	 *
	 * Consumed **once per target, by object identity**: the state carries the
	 * same target until the next move, and a scroll assuming it would still be
	 * pending would drag the reader back to the last jump point whenever a
	 * repaint lands without one of the nav's own pages crossing a boundary.
	 */
	const unsubscribeNav = nav.subscribe((state) => {
		paintNav(state);
		if (state.target !== null && state.target !== consumedTarget) {
			consumedTarget = state.target;
			list.goToPage(state.target.page, state.target.align);
		}
		paintPrompt(state);
	});
	const unsubscribeList = list.subscribe((state) => {
		paintPageList(state);
	});

	// The destinations are the mount's own read, fetched beside the load, so a
	// failure of the *document walk* cannot decide whether a reader gets to
	// resolve names (the panel says unavailable for that on its own). Empty is
	// the agreed answer for failure here: rows render as unresolved rather
	// than as a lie — which is also what a host without the port gets, an
	// empty list, in place of a TypeError on a `.then` a missing port cannot
	// reach.
	const destinationFetch: Promise<readonly NamedDestination[]> =
		adapter.navigation?.destinations(doc) ?? Promise.resolve([]);
	void destinationFetch.then((entries) => {
		if (disposed) {
			return;
		}
		for (const entry of entries) {
			destinations.set(entry.name, entry.destination);
		}
		// The rows' resolution depends on this map; repaint the tree with it.
		paintNav(nav.state());
	});

	// The controller's first load is the mount's to start: nothing else calls
	// it, and until it runs the panel would sit on the pre-load state forever.
	// `load()` never rejects — a failed walk is reported through `onError` and
	// *published* — so the settlement flags are read off the published state:
	// the catalogue's unavailable sentence on a state whose placeholder still
	// reads "empty" is exactly the failed walk (a host with no port publishes
	// the placeholder *as* "unavailable" instead, via `buildState`).
	void nav.load().then((state) => {
		if (disposed) {
			return;
		}
		navLoaded = true;
		navLoadFailed =
			state.available &&
			state.placeholder === "empty" &&
			state.announcement === navStrings.unavailable();
		// The subscription painted this same state before these flags existed;
		// repaint so the notes host says what can now honestly be said and the
		// rows resolve with the destinations that have landed by now.
		paintNav(state);
	});

	// First paint from the controllers' initial published states, before any
	// async load settles: the reader sees the panel frame and the page plane
	// immediately, rather than a blank area waiting for a network round trip.
	paintNav(nav.state());
	paintPageList(list.state());
	// And the first honest measure: the compositor configures its backing
	// store here, since the plane's own height is what the stage's box is.
	measure();

	// ── 8. Teardown ──────────────────────────────────────────────────────────

	return {
		dispose() {
			if (disposed) {
				return;
			}
			disposed = true;
			resizeObserver.disconnect();
			dppxCleanup?.();
			dppxCleanup = null;
			unsubscribeNav();
			unsubscribeList();
			compositor.dispose();
			list.dispose();
			nav.dispose();
			container.replaceChildren();
		},
	};
}
