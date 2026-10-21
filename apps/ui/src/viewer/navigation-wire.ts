/**
 * The engine's navigation wire shapes, mapped onto the viewer's vocabulary
 * (SL-4.UI.06).
 *
 * ## Why this file exists
 *
 * The WASM.01 protocol answers four navigation questions — `outline`,
 * `pageLabels`, `destinations`, `pageLinks` — and every answer arrives as JSON
 * whose field names are the **engine's** (`destKind`, `x0`, `target`), not the
 * viewer's (`kind`, `x`/`width`, `destination`). A host that maps those answers
 * inline writes the mapping twice: once in the web shell and once in the
 * extension, and the two copies drift. This file is the one copy.
 *
 * It is deliberately pure — no DOM, no platform global, no engine — so it is
 * asserted in plain Node like the rest of `apps/ui` (ADR-0044, SL-4.UI.01).
 *
 * ## The rules this mapping follows
 *
 * - **The engine reports; the viewer decides.** Every action class crosses the
 *   seam, including the ones ADR-0020 disables. A mapping that dropped
 *   `/Launch` here would make "is the viewer refusing this, or does the engine
 *   not know about it?" unanswerable from the outside — which is the whole
 *   question `viewer/links.ts` exists to answer.
 * - **A malformed answer is a refusal, not a shrug.** Every field is checked.
 *   A `Rect` missing a component is a highlight somewhere arbitrary that still
 *   type-checks, and a text layer that is subtly wrong is worse than one that
 *   is refused.
 * - **Nothing is invented.** Where the wire cannot express what the UI's type
 *   requires — a link with no `/Rect`, a named destination that resolves to
 *   another name — the entry is **skipped and the skip is recorded in the
 *   reason**, rather than filled in with a zero or a guess.
 *
 * ## The three places a value is deliberately lost
 *
 * 1. **An unknown destination class becomes an `xyz` hop with no
 *    parameters.** `PdfDestination`'s own doc says so: "Anything the viewer
 *    does not recognise is carried as an `xyz` destination with no parameters,
 *    which navigates to the page and nothing more; that is a loss of fidelity,
 *    not an error". The `/XYZ` coordinates are dropped too, because an
 *    unrecognised class has no agreed meaning for them.
 * 2. **An inverted `/Rect` is normalised.** The engine crosses a rectangle
 *    verbatim and says so: "An inverted rectangle is a document defect a viewer
 *    may usefully normalise for hit-testing; the guest is not the layer that
 *    decides what 'where' means". This is that layer.
 * 3. **A link with no `/Rect` is dropped.** A zero box would "silently swallow
 *    clicks in the corner" — the engine says that explicitly — and a link with
 *    no position is not one a reader can activate.
 */

import type {
	DestinationKind,
	LinkAction,
	LinkActionKind,
	LinkAnnotation,
	NamedDestination,
	OutlineNode,
	PageLabelRange,
	PageLabelStyle,
	PdfDestination,
	Rect,
} from "../platform/types.js";

/**
 * The answer to a navigation op that could not be understood.
 *
 * A distinct type rather than a bare `Error`, so a host can tell "the engine
 * said something I cannot read" from "the engine refused" — the first is a
 * transport bug, the second is a fact about the document.
 *
 * ## Why the messages are not in the catalogue
 *
 * These are **diagnostics for whoever is debugging the transport**, not prose
 * a reader is shown, and they are written in the same style as
 * `viewer/health.ts`'s — an identifier prefix, then what was wrong. The i18n
 * gate (`viewer/strings.test.ts`) exists to keep *user-facing* prose in the
 * catalogue; a wire validator's complaint about a malformed answer is not
 * that. If one of these ever reaches a reader, the caller must word it — the
 * host's own failure panel is where that sentence belongs.
 */
export class NavigationWireError extends Error {
	constructor(
		/** Which op produced the unreadable answer. */
		readonly op: string,
		/** What was wrong with it, in one clause. */
		readonly detail: string,
	) {
		super(`the ${op} answer was not readable: ${detail}`);
		this.name = "NavigationWireError";
	}
}

/** The outcome of reading an `outline` answer. */
export interface OutlineReport {
	/** The document HAS an outline. `false` is "this document has none". */
	readonly present: boolean;
	/** The outline exists but could not be read in full. */
	readonly truncated: boolean;
	/** The outline was pruned — the engine dropped items it could not read. */
	readonly pruned: boolean;
	/** The bookmark tree, in document order. */
	readonly items: readonly OutlineNode[];
}

/**
 * The action classes the viewer models.
 *
 * Exported because a **transport** needs the same closed set: a host that
 * receives an action class over a wire has to decide whether it is one the
 * viewer models or one it does not, and "decide" needs the list rather than
 * a copy of it. A shell that kept its own list would be the second copy this
 * module exists to prevent.
 */
export const ACTION_KINDS: readonly LinkActionKind[] = [
	"goTo",
	"uri",
	"launch",
	"goToR",
	"submitForm",
	"importData",
	"javascript",
	"named",
	"none",
	"unknown",
];

/**
 * The destination classes the viewer names.
 *
 * The same reasoning as {@link ACTION_KINDS}: the closed set is shared
 * vocabulary, and a transport that has to validate a destination over a wire
 * needs it in one place.
 */
export const DESTINATION_KINDS: readonly DestinationKind[] = [
	"fit",
	"fitH",
	"fitV",
	"fitR",
	"fitB",
	"xyz",
];

/** The `/PageLabels` styles (PDF 32000-2:2020 §7.7.3.4, Table 165). */
export const LABEL_STYLES: readonly PageLabelStyle[] = ["D", "R", "r", "A", "a"];

/**
 * How far a named destination chain is chased.
 *
 * A `/Dests` name tree may point at another name, and the engine passes that
 * through deliberately ("so the viewer can chase the chain itself rather than
 * the engine deciding where it ends"). Eight hops is far more than a real
 * document uses and short enough that a cycle terminates: the chain is walked
 * with the visited set, so a cycle is detected rather than merely bounded.
 */
const MAX_DESTINATION_CHAIN = 8;

/** Read a plain object, or `null` for anything else. */
function objectOf(value: unknown): Record<string, unknown> | null {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		return null;
	}
	return value as Record<string, unknown>;
}

/** Read a string, or `null`. */
function stringOf(value: unknown): string | null {
	return typeof value === "string" ? value : null;
}

/** Read a finite number, or `null`. */
function numberOf(value: unknown): number | null {
	return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/**
 * The engine's destination class, as the viewer names it.
 *
 * The engine crosses the **spec's** spelling (`Fit`, `FitH`, `XYZ`), because
 * "the class's own name, for a transport that speaks names" is what lets a
 * viewer tell `/FitH` from `/FitV` without guessing.
 */
export function destinationKindOf(name: string): DestinationKind {
	switch (name) {
		case "Fit":
			return "fit";
		case "FitH":
			return "fitH";
		case "FitV":
			return "fitV";
		case "FitR":
			return "fitR";
		case "FitB":
			return "fitB";
		default:
			// `/XYZ` and every class the viewer does not model.
			return "xyz";
	}
}

/** True when the engine's class is one the viewer names exactly. */
function isKnownDestinationClass(name: string): boolean {
	return (
		name === "Fit" ||
		name === "FitH" ||
		name === "FitV" ||
		name === "FitR" ||
		name === "FitB" ||
		name === "XYZ"
	);
}

/**
 * The engine's `NavTarget`, as the viewer's `PdfDestination` — or `null` when
 * the target is not a page.
 *
 * `null` is a real answer here: a named target is resolved by the caller
 * (against the document's name tree) and an unresolved one has no destination
 * at all. Both are *not* a page, and pretending otherwise would put a
 * destination on a bookmark that has none.
 */
function destinationOf(target: unknown): PdfDestination | null {
	const object = objectOf(target);
	if (object === null) {
		return null;
	}
	if (object.kind !== "page") {
		return null;
	}
	const page = numberOf(object.page);
	if (page === null) {
		return null;
	}
	const className = stringOf(object.destKind) ?? "";
	const kind = destinationKindOf(className);
	if (!isKnownDestinationClass(className)) {
		// The documented loss: an unrecognised class navigates to the page and
		// nothing more. The `/XYZ` coordinates go with it, because an
		// unrecognised class has no agreed meaning for them.
		return { page, kind };
	}
	const left = numberOf(object.left);
	const top = numberOf(object.top);
	const zoom = numberOf(object.zoom);
	return {
		page,
		kind,
		...(left === null ? {} : { left }),
		...(top === null ? {} : { top }),
		...(zoom === null ? {} : { zoom }),
	};
}

/** The name a target carries, when it is a named target. */
function namedTargetOf(target: unknown): string | null {
	const object = objectOf(target);
	if (object === null || object.kind !== "named") {
		return null;
	}
	return stringOf(object.name);
}

/**
 * The engine's `/Rect`, as the viewer's `Rect`.
 *
 * Normalised, because the guest crosses the rectangle verbatim and says the
 * viewer is the layer that decides what "where" means. An inverted rectangle
 * is a document defect; a negative width is not a shape.
 */
function rectOf(value: unknown): Rect | null {
	const object = objectOf(value);
	if (object === null) {
		return null;
	}
	const x0 = numberOf(object.x0);
	const y0 = numberOf(object.y0);
	const x1 = numberOf(object.x1);
	const y1 = numberOf(object.y1);
	if (x0 === null || y0 === null || x1 === null || y1 === null) {
		return null;
	}
	return {
		x: Math.min(x0, x1),
		y: Math.min(y0, y1),
		width: Math.abs(x1 - x0),
		height: Math.abs(y1 - y0),
	};
}

/**
 * The engine's action, as the viewer's `LinkAction`.
 *
 * `fallbackTarget` is the annotation's own `/Dest`, which is what a link
 * without an `/A` action has.
 */
function actionOf(value: unknown, fallbackTarget: unknown): LinkAction {
	const object = objectOf(value);
	if (object === null) {
		// No action at all: the annotation carries a `/Dest` (an implicit
		// `/GoTo`) or nothing.
		const destination = destinationOf(fallbackTarget);
		if (destination !== null) {
			return { kind: "goTo", destination };
		}
		return { kind: "none" };
	}
	const rawKind = stringOf(object.kind);
	const kind: LinkActionKind =
		rawKind !== null && ACTION_KINDS.includes(rawKind as LinkActionKind)
			? (rawKind as LinkActionKind)
			: "unknown";
	const uri = stringOf(object.uri);
	const name = stringOf(object.name);
	const destination = destinationOf(object.target) ?? destinationOf(fallbackTarget);
	return {
		kind,
		...(uri === null ? {} : { uri }),
		...(name === null ? {} : { name }),
		...(destination === null ? {} : { destination }),
	};
}

/**
 * Read the engine's `outline` answer.
 *
 * @throws {NavigationWireError} when the answer is not an outline at all.
 */
export function mapOutline(value: unknown): OutlineReport {
	const object = objectOf(value);
	if (object === null) {
		throw new NavigationWireError("outline", `outline: the answer was not an object`);
	}
	const rawItems = object.items;
	if (!Array.isArray(rawItems)) {
		throw new NavigationWireError("outline", `outline: the answer carried no item list`);
	}
	return {
		present: object.present === true,
		truncated: object.truncated === true,
		pruned: object.pruned === true,
		items: rawItems.map((item) => mapOutlineItem(item)),
	};
}

/** One bookmark, as the viewer names it. */
function mapOutlineItem(value: unknown): OutlineNode {
	const object = objectOf(value);
	if (object === null) {
		throw new NavigationWireError("outline", `outline: an item was not an object`);
	}
	const title = stringOf(object.title);
	if (title === null) {
		throw new NavigationWireError("outline", `outline: an item carried no title`);
	}
	// The item's own target, or the one its `/A` action carries. A bookmark
	// with an `/A /GoTo` and no `/Dest` is the common shape.
	const action = objectOf(object.action);
	const target = object.target ?? (action === null ? null : action.target);
	const destination = destinationOf(target);
	const named = namedTargetOf(target) ?? (action === null ? null : stringOf(action.name));
	const count = numberOf(object.count);
	const children = Array.isArray(object.children) ? object.children : [];
	return {
		title,
		...(destination === null ? {} : { destination }),
		...(named === null ? {} : { namedDestination: named }),
		...(count === null ? {} : { descendantCount: count }),
		...(children.length === 0 ? {} : { children: children.map(mapOutlineItem) }),
	};
}

/**
 * Read the engine's `pageLabels` answer.
 *
 * @throws {NavigationWireError} when the answer is not a range list.
 */
export function mapPageLabels(value: unknown): readonly PageLabelRange[] {
	const object = objectOf(value);
	if (object === null) {
		throw new NavigationWireError("pageLabels", `pageLabels: the answer was not an object`);
	}
	const ranges = object.ranges;
	if (!Array.isArray(ranges)) {
		throw new NavigationWireError("pageLabels", `pageLabels: the answer carried no range list`);
	}
	return ranges.map((range) => {
		const entry = objectOf(range);
		if (entry === null) {
			throw new NavigationWireError("pageLabels", `pageLabels: a range was not an object`);
		}
		const firstPage = numberOf(entry.firstPage);
		if (firstPage === null) {
			throw new NavigationWireError("pageLabels", `pageLabels: a range carried no first page`);
		}
		const style = stringOf(entry.style);
		const prefix = stringOf(entry.prefix);
		const firstValue = numberOf(entry.firstValue);
		return {
			firstPage,
			...(style !== null && LABEL_STYLES.includes(style as PageLabelStyle)
				? { style: style as PageLabelStyle }
				: {}),
			...(prefix === null ? {} : { prefix }),
			...(firstValue === null ? {} : { firstValue }),
		};
	});
}

/**
 * Read the engine's `destinations` answer.
 *
 * A name whose target is another name is **chased**: the engine passes the
 * chain through so the viewer resolves it, and the viewer is this layer. A
 * chain that loops, or that ends somewhere that is not a page, is dropped —
 * a destination that does not resolve is not a destination.
 *
 * @throws {NavigationWireError} when the answer is not a destination list.
 */
export function mapDestinations(value: unknown): readonly NamedDestination[] {
	const object = objectOf(value);
	if (object === null) {
		throw new NavigationWireError("destinations", `destinations: the answer was not an object`);
	}
	const list = object.destinations;
	if (!Array.isArray(list)) {
		throw new NavigationWireError("destinations", `destinations: the answer carried no destination list`);
	}
	/** name → the target it points at, for the chain chase. */
	const byName = new Map<string, unknown>();
	for (const entry of list) {
		const item = objectOf(entry);
		if (item === null) {
			continue;
		}
		const name = stringOf(item.name);
		if (name === null) {
			continue;
		}
		byName.set(name, item.target);
	}
	const out: NamedDestination[] = [];
	for (const entry of list) {
		const item = objectOf(entry);
		if (item === null) {
			continue;
		}
		const name = stringOf(item.name);
		if (name === null) {
			continue;
		}
		const destination = resolveDestination(byName, item.target);
		if (destination === null) {
			continue;
		}
		out.push({ name, destination });
	}
	return out;
}

/**
 * Follow a destination chain to the page it ends at.
 *
 * Returns `null` for a chain that loops, that is longer than
 * {@link MAX_DESTINATION_CHAIN}, or that does not end at a page.
 */
function resolveDestination(
	byName: ReadonlyMap<string, unknown>,
	target: unknown,
): PdfDestination | null {
	const seen = new Set<string>();
	let current = target;
	for (let hop = 0; hop <= MAX_DESTINATION_CHAIN; hop += 1) {
		const direct = destinationOf(current);
		if (direct !== null) {
			return direct;
		}
		const name = namedTargetOf(current);
		if (name === null) {
			return null;
		}
		if (seen.has(name)) {
			// A cycle. There is no page at the end of it.
			return null;
		}
		seen.add(name);
		const next = byName.get(name);
		if (next === undefined) {
			// A name the document's own tree does not contain.
			return null;
		}
		current = next;
	}
	return null;
}

/**
 * Read the engine's `pageLinks` answer.
 *
 * A link with no `/Rect` is **dropped**: the engine crosses an absent `/Rect`
 * as `null` rather than a zero box that "would silently swallow clicks in the
 * corner", and a link with no position is not one a reader can activate.
 *
 * @throws {NavigationWireError} when the answer is not a link list.
 */
export function mapPageLinks(value: unknown): readonly LinkAnnotation[] {
	const object = objectOf(value);
	if (object === null) {
		throw new NavigationWireError("pageLinks", `pageLinks: the answer was not an object`);
	}
	const links = object.links;
	if (!Array.isArray(links)) {
		throw new NavigationWireError("pageLinks", `pageLinks: the answer carried no link list`);
	}
	const out: LinkAnnotation[] = [];
	for (const entry of links) {
		const item = objectOf(entry);
		if (item === null) {
			continue;
		}
		const rect = rectOf(item.rect);
		if (rect === null) {
			continue;
		}
		const index = numberOf(item.index);
		const object_ = numberOf(item.object);
		const contents = stringOf(item.contents);
		out.push({
			// Unique within the page: the annotation's array index is, and the
			// object number is carried alongside it so a host can trace a link
			// back to the object it came from.
			id: `${object_ ?? "?"}#${index ?? out.length}`,
			rect,
			action: actionOf(item.action, item.target),
			...(contents === null ? {} : { contents }),
		});
	}
	return out;
}
