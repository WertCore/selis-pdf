/**
 * The operations the editor can run, as pure request builders.
 *
 * Every one of them is a request to the **tools chunk** — the same engine
 * operations the one-shot cards use, driven against the open document instead
 * of against a file the user picked. That is deliberate: the editor does not
 * have its own mutation primitives, so nothing it does can disagree with what
 * the equivalent card does to the same document.
 *
 * The builders are pure so the request shape — the page numbering, the option
 * names, the label the history records — can be tested without an engine.
 */

import type { ToolOp } from "../tools/protocol.js";

/** One operation, ready to run against the open document. */
export interface EditorOperation {
	/** The tools-chunk operation. */
	readonly op: ToolOp;
	/** The operation's options, in the chunk's own vocabulary. */
	readonly options: Readonly<Record<string, unknown>>;
	/** The label the history records the change under. */
	readonly label: string;
}

/**
 * The engine's page-selection syntax for one page.
 *
 * The engine numbers pages from **zero** (`0,2,5-7`); the editor's chrome
 * numbers them from one, the way a reader counts. Every builder here takes a
 * zero-based index and writes it straight into the request, so the shift
 * happens in exactly one place — this function.
 */
export function pageSelection(page: number): string {
	if (!Number.isInteger(page) || page < 0) {
		throw new RangeError("a page index is a whole number, 0 or greater");
	}
	return String(page);
}

/** Turn a page by a quarter, a half or three quarters. */
export function rotatePage(page: number, degrees: 90 | 180 | 270): EditorOperation {
	return {
		op: "rotate",
		options: { angle: degrees, pages: pageSelection(page) },
		label: `Rotated page ${page + 1} by ${degrees}°`,
	};
}

/** Remove a page. */
export function deletePage(page: number): EditorOperation {
	return {
		op: "delete",
		options: { pages: pageSelection(page) },
		label: `Deleted page ${page + 1}`,
	};
}

/** Copy a page and put the copy immediately after it. */
export function duplicatePage(page: number): EditorOperation {
	return {
		op: "reorder",
		options: { mode: "duplicate", pages: pageSelection(page) },
		label: `Duplicated page ${page + 1}`,
	};
}

/**
 * Insert blank pages.
 *
 * `after` is the engine's own convention: the **one-based** page the blanks
 * follow, where `0` puts them before the first page. It is one-based because
 * that is what `insert_blank_pages` documents, and a boundary is a position
 * between two pages rather than a page.
 */
export function insertBlankPages(after: number, count = 1): EditorOperation {
	if (!Number.isInteger(after) || after < 0) {
		throw new RangeError("a page boundary is a whole number, 0 or greater");
	}
	if (!Number.isInteger(count) || count < 1) {
		throw new RangeError("the number of blank pages is 1 or greater");
	}
	return {
		op: "reorder",
		options: { mode: "blank", after, count },
		label: `Inserted ${count} blank page${count === 1 ? "" : "s"}`,
	};
}

/** Put the pages in a new order. `order` lists zero-based page indices. */
export function reorderPages(order: readonly number[]): EditorOperation {
	if (order.length === 0) {
		throw new RangeError("a new order needs at least one page");
	}
	const selection = order.map((page) => pageSelection(page)).join(",");
	return {
		op: "reorder",
		options: { mode: "reorder", order: selection },
		label: "Reordered the pages",
	};
}

/**
 * Move a page one place earlier or later.
 *
 * The engine reorders by listing every page, so a move is a full order rather
 * than a special case — and a full order is checked against the document's
 * page count before it is sent.
 */
export function movePage(
	order: readonly number[],
	page: number,
	direction: "up" | "down",
): EditorOperation {
	const from = order.indexOf(page);
	if (from < 0) {
		throw new RangeError("the page is not in the current order");
	}
	const to = direction === "up" ? from - 1 : from + 1;
	if (to < 0 || to >= order.length) {
		throw new RangeError(
			direction === "up" ? "the page is already first" : "the page is already last",
		);
	}
	const next = order.slice();
	const [moved] = next.splice(from, 1);
	if (moved === undefined) {
		throw new RangeError("the page is not in the current order");
	}
	next.splice(to, 0, moved);
	return reorderPages(next);
}

/**
 * Replace a run of text on a page.
 *
 * The engine replaces the first show-text operand on the page that **carries**
 * the search text — a substring match, with the rest of the operand kept. That
 * is the primitive the engine has, and it is the one the editor drives.
 */
export function editText(page: number, find: string, replace: string): EditorOperation {
	if (!Number.isInteger(page) || page < 0) {
		throw new RangeError("a page index is a whole number, 0 or greater");
	}
	if (find === "") {
		throw new RangeError("the text to find cannot be empty");
	}
	return {
		op: "edit_text",
		options: { page, find, replace },
		label: `Edited the text on page ${page + 1}`,
	};
}

/**
 * The search strings to try for a clicked line, most specific first.
 *
 * The engine finds the first show-text operand on the page that carries the
 * search string, so the search string has to be something one operand actually
 * contains. A line's recovered text is **assembled** from several operands
 * joined by synthesised spaces, so the whole line is often not inside any one
 * of them — a justified paragraph draws each word separately.
 *
 * So the ladder is: the whole line first (the user's literal intent, and the
 * right answer whenever the line is one run), then the line's words, longest
 * first (a word is usually a whole operand). The caller tries them in order
 * and keeps the first that actually changed the line the user clicked.
 *
 * The list is capped: a line of thirty words must not become thirty engine
 * runs, and the longest words are the ones most likely to be a whole operand.
 */
export function searchCandidates(line: string, limit = 4): string[] {
	const trimmed = line.trim();
	if (trimmed === "") {
		return [];
	}
	const candidates: string[] = [];
	const seen = new Set<string>();
	const add = (value: string): void => {
		if (value === "" || seen.has(value) || candidates.length >= limit) {
			return;
		}
		seen.add(value);
		candidates.push(value);
	};
	add(trimmed);
	const words = trimmed.split(/\s+/).filter((word) => word !== "");
	// Longest first: a long word is more likely to be a whole operand than a
	// short one, and a short word is more likely to occur earlier in the page.
	words.sort((left, right) => right.length - left.length);
	for (const word of words) {
		add(word);
	}
	return candidates;
}

/**
 * Whether an edit changed the line the user clicked.
 *
 * The engine edits the **first** operand on the page that carries the search
 * text, and the page may carry the same words earlier — a title repeated at
 * the top of every page. An edit that landed on an earlier line leaves the
 * clicked line untouched, and that is the case this catches: the editor
 * reports it instead of showing a document that changed somewhere else.
 *
 * @param before the line's text before the edit
 * @param after the line's text after the edit
 * @param candidate the search string the edit used
 * @param replacement the text the edit put in its place
 */
export function editLandedOnLine(
	before: string,
	after: string,
	candidate: string,
	replacement: string,
): boolean {
	if (after === before) {
		return false;
	}
	const expected = before.replace(candidate, replacement);
	return after === expected || after.includes(replacement);
}
