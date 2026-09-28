/**
 * The outline (bookmark) tree as a flat, navigable list (SL-4.UI.06).
 *
 * A document's `/Outlines` is a tree; a sidebar is a list. This module is the
 * only place that knows both, and it is pure — tree plus a set of expanded ids
 * in, rows out. The controller in `navigation.ts` owns the set; this file never
 * mutates anything.
 *
 * ## Why the ids are paths
 *
 * A row's id is its position in the tree — `"0"`, `"0.2"`, `"0.2.1"` — and not
 * an array index or a generated counter. Three properties follow, and all three
 * are wanted:
 * - Expansion survives a reload of the outline, because the id comes from the
 *   document's own structure rather than from the order we happened to walk it.
 * - Two rows can never collide, because a path is unique in a tree.
 * - The ids are readable in a test failure, which matters more than it sounds
 *   when the thing being debugged is a tree.
 *
 * ## The two decisions the spec pushes to the viewer
 *
 * 1. **What starts expanded.** §12.3.3 says a negative `/Count` means the
 *    subtree starts closed, and a positive one means it starts open; absent
 *    `/Count`, the viewer decides. {@link initialExpansion} closes exactly the
 *    items the document asked to be closed, and nothing else — a viewer that
 *    opened everything would produce a 2 000-row sidebar for a 2 000-page
 *    document, and a viewer that obeyed `/Count` as a *number of descendants*
 *    would be reading a value that is not a visibility flag.
 * 2. **How much a collapsed item costs.** A collapsed item contributes one row.
 *    Its children are in the tree and in the ids; they are simply not in the
 *    list, so expanding is a set membership test and not a re-walk.
 */

import type { OutlineNode, PdfDestination } from "../platform/types.js";

/** One visible row of the outline panel. */
export interface OutlineRow {
	/** Path id; stable for a given document structure. See the module doc. */
	readonly id: string;
	readonly title: string;
	/** 0 for a top-level item. */
	readonly depth: number;
	/** The id of the parent row, or `null` at the top level. */
	readonly parentId: string | null;
	readonly hasChildren: boolean;
	readonly expanded: boolean;
	/**
	 * The page this row goes to, or `null`.
	 *
	 * `null` is a real case, not an error: an outline item may name a
	 * destination the document does not define, and may have none at all (a
	 * heading the author never linked). Such a row is still shown — it is in
	 * the document — and selecting it moves nowhere, which the controller
	 * reports rather than swallowing.
	 */
	readonly page: number | null;
	/** The item's named destination, when it has one. See `OutlineNode`. */
	readonly namedDestination: string | null;
	/** Position in the visible list, for `aria-posinset`. */
	readonly index: number;
	/** Sibling count at this depth, for `aria-setsize`. */
	readonly setSize: number;
}

/** The outcome of walking the tree, enough to draw and to navigate. */
export interface FlatOutline {
	readonly rows: readonly OutlineRow[];
	/** Ids of every item with children, whether or not it is expanded. */
	readonly parentIds: ReadonlySet<string>;
	/** Total item count including collapsed ones; `rows.length` is not this. */
	readonly itemCount: number;
	/** The deepest nesting level in the tree, 0 for a flat one. */
	readonly maxDepth: number;
	/** True when the row cap stopped the walk. See {@link MAX_OUTLINE_ROWS}. */
	readonly truncated: boolean;
}

/**
 * Cap on the number of rows published.
 *
 * A hostile or merely eccentric document can nest ten thousand items deep, and


/**
 * Cap on the number of rows published.
 *
 * A hostile or merely eccentric document can hold tens of thousands of outline
 * items, and `flattenOutline` runs inside a state build. The cap bounds the
 * *published* list; the rest of the document is still reachable through search
 * and the page box, and `FlatOutline.truncated` says the panel stopped rather
 * than letting it imply the outline ended there.
 */
export const MAX_OUTLINE_ROWS = 2000;

/**
 * Cap on outline nesting.
 *
 * Both walks here are depth-bounded by this, for the reason the conventions
 * give for the engine: a document that nests outlines ten thousand deep must
 * not be able to turn a panel render into a stack overflow. A stack overflow is
 * a crash primitive; a truncated panel is a sentence in the viewer's state.
 */
export const MAX_OUTLINE_DEPTH = 256;

/**
 * Items the survey walk will count before it gives up.
 *
 * The panel can only draw {@link MAX_OUTLINE_ROWS} of them, so counting far past
 * that buys nothing — except the "N of M" sentence, and M is a number the reader
 * would rather have approximately and immediately than exactly and late.
 */
export const MAX_OUTLINE_ITEMS = 20_000;

/**
 * Which items start **expanded**, from the document's own `/Count` values.
 *
 * Only items with a **negative** `/Count` start closed. An item with no `/Count`
 * starts open, because the author wrote children they expected to be reachable
 * and said nothing about hiding them.
 *
 * The name says "expanded" because that is what the caller wants: the
 * controller's expansion set is a whitelist, so handing it the *collapsed* ids
 * would open exactly the chapters the document asked to have closed — an
 * inversion that is invisible in a flat outline and obvious in a real one.
 */
export function initialExpansion(nodes: readonly OutlineNode[]): ReadonlySet<string> {
	const collapsed = new Set<string>();
	walkIds(nodes, (id, node) => {
		const children = node.children ?? [];
		if (
			children.length > 0 &&
			typeof node.descendantCount === "number" &&
			node.descendantCount < 0
		) {
			collapsed.add(id);
		}
	});
	// A collapsed parent's descendants are irrelevant until it opens, so only the
	// ids that can be *shown* need to be in the expanded set.
	const expanded = new Set<string>();
	walkIds(nodes, (id) => {
		expanded.add(id);
	});
	for (const id of collapsed) {
		expanded.delete(id);
	}
	return expanded;
}

/** Ids of every item that has children, expanded or not. */
export function parentIdsOf(nodes: readonly OutlineNode[]): ReadonlySet<string> {
	const parents = new Set<string>();
	walkIds(nodes, (id, node) => {
		if ((node.children?.length ?? 0) > 0) {
			parents.add(id);
		}
	});
	return parents;
}

/**
 * Walk every item, depth-first, handing the visitor its path id.
 *
 * The walk is iterative rather than recursive for the same reason the engine
 * avoids native recursion (ADR-P0006 / the L3 rule): a document can nest
 * outlines ten thousand deep, and a recursive flatten would turn a hostile
 * document into a stack overflow — which is a crash primitive, not a graceful
 * "this outline is too deep". An explicit stack with a depth budget is the
 * escape hatch the conventions ask for.
 */
function walkIds(
	nodes: readonly OutlineNode[],
	visit: (id: string, node: OutlineNode) => void,
): void {
	// A pathologically deep outline is cut off rather than allowed to grow the
	// stack without bound. Real outlines are three or four deep; the budget is
	// two orders of magnitude above that.
	const stack: OutlineFrame[] = [{ entries: nodes, prefix: "", depth: 0 }];
	while (stack.length > 0) {
		const frame = stack.pop();
		if (frame === undefined) {
			break;
		}
		frame.entries.forEach((node, index) => {
			const id = frame.depth === 0 ? String(index) : `${frame.prefix}.${index}`;
			visit(id, node);
			const children = node.children ?? [];
			if (children.length > 0 && frame.depth + 1 < MAX_OUTLINE_DEPTH) {
				stack.push({ entries: children, prefix: id, depth: frame.depth + 1 });
			}
		});
	}
}

/** One frame of the iterative walk: a sibling list, its path prefix and depth. */
interface OutlineFrame {
	readonly entries: readonly OutlineNode[];
	readonly prefix: string;
	readonly depth: number;
}

export interface FlattenRequest {
	readonly nodes: readonly OutlineNode[];
	/** Ids whose children are visible. Everything else is one row. */
	readonly expanded: ReadonlySet<string>;
	/** Row cap; defaults to {@link MAX_OUTLINE_ROWS}. */
	readonly maxRows?: number;
}

/**
 * The ids the document's own `/Count` closes, plus how many items it has and
 * which of them are parents — one iterative walk for all three.
 *
 * The counts are needed *independently* of the visible rows, because a panel
 * that has collapsed everything still has to be able to say "showing 3 of 900
 * items", and because a row's expander must be offered for a parent that is not
 * currently on screen. Both properties are of the whole document, not of the
 * current expansion, so they are computed from the tree and the flatten walks
 * only what is visible.
 *
 * {@link MAX_OUTLINE_ITEMS} bounds that walk. A tree larger than the budget is
 * a document the viewer will not fully enumerate, and it says so through
 * `truncated` rather than spending a keystroke's budget counting what it cannot
 * draw.
 */
function surveyTree(nodes: readonly OutlineNode[]): {
	parentIds: Set<string>;
	itemCount: number;
	scannedAll: boolean;
} {
	const parentIds = new Set<string>();
	let itemCount = 0;
	let scannedAll = true;
	walkIds(nodes, (id, node) => {
		if (itemCount >= MAX_OUTLINE_ITEMS) {
			scannedAll = false;
			return;
		}
		itemCount += 1;
		if ((node.children?.length ?? 0) > 0) {
			parentIds.add(id);
		}
	});
	return { parentIds, itemCount, scannedAll };
}

/**
 * Flatten a tree into the rows a panel shows.
 *
 * `expanded` is a *whitelist*, not a blacklist: an id absent from it is closed
 * even if its children exist. That is what makes the controller's state
 * reproducible — the tree does not carry its own expansion, so the same inputs
 * always give the same rows, and a test can assert a row count without
 * reproducing a click sequence.
 */
export function flattenOutline(request: FlattenRequest): FlatOutline {
	const maxRows = Math.max(0, request.maxRows ?? MAX_OUTLINE_ROWS);
	const rows: OutlineRow[] = [];
	const survey = surveyTree(request.nodes);
	let maxDepth = 0;
	let truncated = false;

	const walk = (
		entries: readonly OutlineNode[],
		prefix: string,
		depth: number,
		parentId: string | null,
		open: boolean,
	): void => {
		entries.forEach((node, index) => {
			const id = depth === 0 ? String(index) : `${prefix}.${index}`;
			if (depth + 1 > maxDepth) {
				maxDepth = depth + 1;
			}
			const children = node.children ?? [];
			const hasChildren = children.length > 0;
			// A row is emitted only when every ancestor is open, which is the
			// whole meaning of "collapsed" and the reason this is one walk
			// rather than a filter over a pre-flattened list.
			if (!open) {
				return;
			}
			if (rows.length >= maxRows) {
				truncated = true;
				return;
			}
			const expanded = hasChildren && request.expanded.has(id);
			rows.push({
				id,
				title: node.title,
				depth,
				parentId,
				hasChildren,
				expanded,
				page: node.destination?.page ?? null,
				namedDestination: node.namedDestination ?? null,
				index: rows.length,
				// `aria-setsize` counts the siblings at this depth. It comes from
				// the tree rather than from the published rows, so a collapsed
				// sibling does not shrink its neighbours' sets.
				setSize: entries.length,
			});
			if (expanded && depth + 1 < MAX_OUTLINE_DEPTH) {
				// Depth-bounded for the same reason `walkIds` is: the expansion
				// set comes from the document, so the recursion depth is the
				// document's to choose up to the cap.
				walk(children, id, depth + 1, id, true);
			}
		});
	};

	walk(request.nodes, "", 0, null, true);
	return {
		rows,
		parentIds: survey.parentIds,
		itemCount: survey.itemCount,
		maxDepth,
		// Either the row cap or the item budget stopped us, and a reader told
		// "3 of 900" deserves to know the 900 is itself a lower bound.
		truncated: truncated || !survey.scannedAll,
	};
}

/** Whether the outline is worth showing at all. */
export function hasOutline(nodes: readonly OutlineNode[]): boolean {
	return nodes.length > 0;
}

/**
 * The ids to add when a collapsed parent is expanded: the subtree.
 *
 * "Expand the subtree" is what a reader means by clicking the triangle on a
 * chapter that has sections. A shell that only ever added the clicked id would
 * make a second click on the same triangle necessary, which is not what a
 * triangle does anywhere else.
 */
export function expandSubtree(nodes: readonly OutlineNode[], targetId: string): Set<string> {
	const out = new Set<string>();
	walkIds(nodes, (id) => {
		if (isUnder(targetId, id)) {
			out.add(id);
		}
	});
	return out;
}

/**
 * The keys the outline's own key map claims. See `resolveNavigationKey` in
 * `keyboard.ts`, which decides whether the navigation panel owns a key at all
 * before any of this is consulted.
 */
export type OutlineKey = "ArrowDown" | "ArrowUp" | "ArrowRight" | "ArrowLeft" | "Home" | "End";

/**
 * Where a key press moves the selection, over the *visible* rows.
 *
 * The WAI-ARIA treeview keyboard contract, restricted to these six keys:
 * - `ArrowDown`/`ArrowUp` move one visible row and stop at the ends rather than
 *   wrapping — a tree that wraps surprises a reader walking it with a screen
 *   reader counting rows.
 * - `ArrowRight` expands a collapsed parent, or moves to its first child.
 * - `ArrowLeft` collapses an expanded parent, or moves to the parent.
 * - `Home`/`End` go to the first/last visible row.
 *
 * Returns the id to focus, or `null` when there is nowhere to go. A pure
 * function of the rows, so the same press on the same tree always gives the
 * same answer and the controller re-derives none of it.
 */
export function moveOutlineFocus(
	rows: readonly OutlineRow[],
	currentId: string | null,
	key: OutlineKey,
): string | null {
	if (rows.length === 0) {
		return null;
	}
	const index = rows.findIndex((row) => row.id === currentId);
	const at = index === -1 ? 0 : index;
	const row = rows[at];
	switch (key) {
		case "ArrowDown":
			return rows[Math.min(rows.length - 1, at + 1)]?.id ?? null;
		case "ArrowUp":
			return rows[Math.max(0, at - 1)]?.id ?? null;
		case "Home":
			return rows[0]?.id ?? null;
		case "End":
			return rows[rows.length - 1]?.id ?? null;
		case "ArrowRight": {
			if (row === undefined) {
				return null;
			}
			// A collapsed parent opens; an open one steps into its children; a
			// childless row has nowhere to go, and the treeview contract says
			// the key does nothing rather than moving somewhere surprising.
			return rows[at + 1]?.id ?? row.id;
		}
		case "ArrowLeft": {
			if (row === undefined) {
				return null;
			}
			if (row.hasChildren && row.expanded) {
				return row.id;
			}
			return row.parentId ?? row.id;
		}
		default:
			return null;
	}
}

/**
 * The page a row goes to, resolving a named destination against `/Dests`.
 *
 * Resolution lives here rather than in the transport because the *name tree* is
 * a document-wide structure and the *destination* it names is what a click
 * needs; a transport that resolved names would have to load the whole name tree
 * to answer a question about one outline item, and a viewer that resolved them
 * could not ask for the tree at all on a host that splits the two calls.
 */
export function rowPage(
	row: OutlineRow,
	destinations: ReadonlyMap<string, PdfDestination>,
): number | null {
	if (row.page !== null) {
		return row.page;
	}
	if (row.namedDestination === null) {
		return null;
	}
	return destinations.get(row.namedDestination)?.page ?? null;
}

/** True when `candidate` is `root` or sits beneath it in the path id space. */
function isUnder(root: string, candidate: string): boolean {
	return candidate === root || candidate.startsWith(`${root}.`);
}
