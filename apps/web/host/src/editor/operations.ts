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

/**
 * The width of a line of text in the stamp's font, in points.
 *
 * The engine MEASURES the text before it draws it, and refuses a line that
 * does not fit the box it was given. It measures with a coarse per-character
 * table rather than the font's real metrics — every letter that is not one of
 * a handful of exceptions is charged as 556/1000 em — and that table is
 * WIDER than the real Helvetica for most text. A box sized from the browser's
 * font metrics would therefore be too small, and the engine would refuse it.
 *
 * So the table is the same one here. It is a duplication, deliberately: the
 * two copies can only disagree by a character width, and when they do the
 * engine refuses the box with a sentence that says so — which is a loud
 * failure, not a misplaced text.
 */
export function stampTextWidth(text: string, fontSize: number): number {
	let units = 0;
	for (const byte of new TextEncoder().encode(text)) {
		if (byte === 0x20) {
			units += 278;
		} else if (
			byte === 0x69 ||
			byte === 0x6c ||
			byte === 0x2e ||
			byte === 0x2c ||
			byte === 0x27 ||
			byte === 0x21
		) {
			units += 278;
		} else if (byte === 0x6d || byte === 0x77) {
			units += 833;
		} else if (byte === 0x4d || byte === 0x57) {
			units += 944;
		} else if (byte >= 0x30 && byte <= 0x39) {
			units += 556;
		} else if (byte >= 0x41 && byte <= 0x5a) {
			units += 667;
		} else {
			units += 556;
		}
	}
	return (units * fontSize) / 1000;
}

// ── placing new content ─────────────────────────────────────────────────────

/** A rectangle in display points: the page as it is displayed, y-up. */
export interface DisplayRect {
	readonly x0: number;
	readonly y0: number;
	readonly x1: number;
	readonly y1: number;
}

/** A rectangle in the page's own user space: points, y-up, at the media box's origin. */
export interface UserRect {
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
}

/**
 * The options for a text box.
 *
 * The engine draws it with its visual stamp: typed lines inside a rectangle,
 * appended to the page's content stream. The rectangle is in display points
 * (`x0, y0, x1, y1`, origin at the bottom-left of the displayed page) and the
 * page is **one-based**, because the stamp's own option is.
 *
 * The font is the base-14 Helvetica with WinAnsi encoding, so a character
 * outside WinAnsi is refused by the engine rather than drawn as a substitute.
 */
export function textBoxOptions(
	page: number,
	rect: DisplayRect,
	text: string,
	fontSize: number,
): Record<string, unknown> {
	return {
		page: page + 1,
		rect: [rect.x0, rect.y0, rect.x1, rect.y1],
		lines: [{ text, size: fontSize }],
		strokes: [],
		color: [0.05, 0.05, 0.12],
		strokeWidth: 1.5,
		fontSize,
		border: false,
	};
}

/**
 * The options for placing an image.
 *
 * The image is the operation's second input file (the browser has no
 * filesystem, so the bytes are handed over like any other input). `x` and `y`
 * are the image's lower-left corner in display points, and `scale` is the
 * points-per-pixel factor: at 1 an image is as wide, in points, as it is in
 * pixels.
 *
 * The page is **one-based**, because the overlay writer's page selection is.
 */
export function imageOptions(
	page: number,
	x: number,
	y: number,
	scale: number,
): Record<string, unknown> {
	return {
		opacity: 1,
		scale,
		x,
		y,
		pages: String(page + 1),
		tiled: false,
	};
}

/**
 * The options for a redaction.
 *
 * The page is **zero-based**, like every other page selection in the tools
 * chunk, and each region is `x, y, width, height` in the page's own user
 * space — the space the content stream is written in.
 */
export function redactOptions(page: number, rect: UserRect): Record<string, unknown> {
	return {
		page,
		rects: [[rect.x, rect.y, rect.width, rect.height]],
	};
}

/**
 * The options for cropping a page by its margins.
 *
 * The margins are in points, measured on the page as the reader sees it (the
 * rotation applied), and the page selection is **zero-based**.
 */
export function cropOptions(
	margins: {
		readonly left: number;
		readonly top: number;
		readonly right: number;
		readonly bottom: number;
	},
	page: number,
): Record<string, unknown> {
	return {
		left: margins.left,
		top: margins.top,
		right: margins.right,
		bottom: margins.bottom,
		pages: String(page),
	};
}

/**
 * The options for the document's properties.
 *
 * The keys are the property names the engine writes (`Title`, `Author`,
 * `Subject`, `Keywords`), and only the ones the reader filled in are sent: an
 * empty field is left as it is rather than cleared.
 */
export function metadataOptions(fields: Readonly<Record<string, string>>): Record<string, unknown> {
	return { fields: { ...fields } };
}

/**
 * The options for filling the document's form fields.
 *
 * Every field is sent with its value, in the order the fields were read: the
 * engine writes the values into the document's fields, and a field the reader
 * left alone keeps the value it had.
 */
export function fillFormsOptions(
	fields: readonly { readonly name: string; readonly value: string }[],
): Record<string, unknown> {
	return { fields: fields.map((field) => ({ name: field.name, value: field.value })) };
}

/** The page sizes the editor offers, in points, as the tools page offers them. */
export const PAGE_SIZES: Readonly<Record<string, readonly [number, number]>> = {
	a3: [841.89, 1190.55],
	a4: [595.28, 841.89],
	a5: [419.53, 595.28],
	letter: [612, 792],
	legal: [612, 1008],
};

/**
 * The options for resizing a page.
 *
 * The size is in points, and `fit` says whether the content is scaled to the
 * new page or left alone (and clipped). The page selection is **zero-based**.
 */
export function resizeOptions(
	width: number,
	height: number,
	fit: boolean,
	page: number,
): Record<string, unknown> {
	return { width, height, fit, pages: String(page) };
}

/** One bookmark, as the editor holds it. */
export interface OutlineEntry {
	/** The title a reader sees. */
	readonly title: string;
	/** The **one-based** page the bookmark points at. */
	readonly page: number;
	/** The nesting depth: 0 is a top-level bookmark. */
	readonly level: number;
}

/**
 * The options for the document's bookmarks.
 *
 * The engine replaces the WHOLE outline with the list, so the editor sends
 * every entry it read, in reading order, with its nesting depth.
 */
export function outlineOptions(entries: readonly OutlineEntry[]): Record<string, unknown> {
	return {
		entries: entries.map((entry) => ({
			title: entry.title,
			page: entry.page,
			level: entry.level,
		})),
	};
}

// ── the document's own furniture ────────────────────────────────────────────

/**
 * The options for the pages' labels.
 *
 * The label is the number a reader sees in the page box, and it is not the
 * same thing as the page's number: a front matter is labelled `i, ii, iii`
 * while the pages themselves are `1, 2, 3`. The style is the engine's own
 * one-letter spelling (`D`, `r`, `R`, `a`, `A`, `None`).
 */
export function pageLabelsOptions(
	style: string,
	start: number,
	prefix: string,
): Record<string, unknown> {
	return { style, start, prefix };
}

/**
 * The options for numbering the pages.
 *
 * The position is the engine's own spelling (`bottom_center`, `top_right`,
 * …), the number starts at `start`, and the size is in points. No page
 * selection is sent, so every page is numbered.
 */
export function pageNumbersOptions(
	position: string,
	start: number,
	fontSize: number,
): Record<string, unknown> {
	return {
		position,
		margin_x: 36,
		margin_y: 24,
		start,
		format: "plain",
		font_size: fontSize,
		color: { r: 0, g: 0, b: 0 },
	};
}

/**
 * The options for a header and a footer.
 *
 * The three slots are the left, the centre and the right of each band, and
 * each may carry `{page}` (the page's number) and `{pages}` (the page count).
 * The engine REFUSES an unknown placeholder rather than printing it literally,
 * so a typo in a placeholder is a visible failure rather than a surprise in
 * the finished document.
 */
export function headerFooterOptions(
	header: { readonly left: string; readonly center: string; readonly right: string },
	footer: { readonly left: string; readonly center: string; readonly right: string },
	fontSize: number,
): Record<string, unknown> {
	return {
		header: { ...header },
		footer: { ...footer },
		margin_x: 36,
		header_margin: 36,
		footer_margin: 36,
		font_size: fontSize,
		color: { r: 0, g: 0, b: 0 },
	};
}

/**
 * The options for a watermark.
 *
 * The watermark is one line of text centred on every page, drawn at an
 * opacity so the page shows through it. The orientation is `diagonal` (up
 * across the page) or `horizontal`.
 */
export function watermarkOptions(
	text: string,
	fontSize: number,
	opacity: number,
	orientation: "diagonal" | "horizontal",
): Record<string, unknown> {
	return {
		text,
		orientation,
		opacity,
		font_size: fontSize,
		color: { r: 0, g: 0, b: 0 },
		tiled: false,
	};
}
