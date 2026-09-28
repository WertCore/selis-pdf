/**
 * Selection, hit testing, and copy (SL-4.UI.04), half two.
 *
 * Everything here is a **pure function of a {@link TextLayerFrame}**. There is
 * no controller, no stored pixel, and no DOM read: a caret is a `(line, offset)`
 * pair, and every question — what is under the pointer, what should be
 * highlighted, what does the clipboard get — is answered by recomputing from
 * the current frame.
 *
 * ## Why positions rather than a selection object
 *
 * Three properties fall out of storing `(line, offset)` and nothing else, and
 * all three are the DoD:
 *
 * 1. **Zoom survival.** The frame is rebuilt at the new scale on every zoom and
 *    the caret is still a `(line, offset)`, so it is still exactly on the same
 *    character. Had a selection been stored as a pair of pixel rectangles — the
 *    obvious thing to do when the highlight is a rectangle — it would have had
 *    to be rescaled by hand, and any disagreement between the rescale and the
 *    new layout is a selection that is subtly off at some zoom levels and not
 *    others. That is the bug this shape makes unrepresentable.
 * 2. **Reading order.** Ordering a range is a lexicographic compare on
 *    `(line, offset)`, and the engine already put the lines in reading order
 *    (SL-3.TEXT.04, structure-tree-first). So a drag that goes *up and to the
 *    right* on a two-column page still copies column one before column two,
 *    because lines are numbered by reading order and not by their y. Visual
 *    order is never consulted, which is precisely what "copy preserves reading
 *    order, not visual order" means.
 * 3. **Copy is a slice.** Because the engine's `chars` are index-aligned with
 *    its `text`, the copied string is `line.text.slice(a, b)`. There is no
 *    re-extraction, no re-joining, and no second code path that could disagree
 *    with `selis extract`.
 *
 * ## RTL
 *
 * Two places, and they are different questions:
 *
 * - **Hit testing** is *visual*. A pointer at an x lands between the two
 *   character boundaries nearest to that x, whichever way the line runs. The
 *   hit test mirrors the line's coordinate system and then forgets about it.
 * - **Arrow keys** are *visual* too, which is what a browser does and what a
 *   reader's muscle memory expects: on an RTL line, ArrowRight moves the caret
 *   right, which is *backwards* through the text. Home/End are the exception —
 *   they are logical, the start and end of the line in reading order, because
 *   that is what the keys are named after.
 *
 * A mixed-direction line (an Arabic word inside an English sentence) is the
 * known limit: the engine reports one direction per line, read from the quads,
 * and a visual move is resolved against that single answer. Recorded in the
 * viewer README rather than papered over.
 *
 * ## The seam
 *
 * No platform globals, no `OffscreenCanvas`/`Worker`/`requestAnimationFrame`, no
 * clipboard access — the host writes the string through `ClipboardPort`.
 * ADR-P0035 says selection state ultimately belongs in the `selis-viewmodel`
 * crate; that crate does not exist yet, so this is the pure geometry half of
 * it, deliberately written as free functions over plain data so that moving
 * the *state* across the language boundary later is a matter of calling these
 * from Rust's side rather than a rewrite.
 */

import type { CharBox, LayerLineBox, TextLayerFrame } from "./text-layer.js";
import { lineLength } from "./text-layer.js";

/** A position between characters. `offset` is a UTF-16 code-unit index. */
export interface Caret {
	readonly line: number;
	readonly offset: number;
}

/** A selection: a fixed `anchor` and a moving `head`, which may precede it. */
export interface SelectionRange {
	readonly anchor: Caret;
	readonly head: Caret;
}

/** A range with `start` no later than `end` in reading order. */
export interface OrderedRange {
	readonly start: Caret;
	readonly end: Caret;
}

/** Total order on carets, in **reading** order: line first, then offset. */
function compareCarets(a: Caret, b: Caret): number {
	return a.line - b.line || a.offset - b.offset;
}

/**
 * Clamp a caret into the frame, so no input can address a character that is not
 * there. Every entry point below assumes a clamped caret.
 */
export function clampCaret(frame: TextLayerFrame, caret: Caret): Caret {
	if (frame.lines.length === 0) {
		return { line: 0, offset: 0 };
	}
	const line = Math.min(Math.max(0, caret.line), frame.lines.length - 1);
	return { line, offset: Math.min(Math.max(0, caret.offset), lineLength(frame, line)) };
}

/** Put a range's ends in reading order. */

/**
 * Visual x of boundary `offset` on a line: its position in the writing
 * direction. Boundary `o` is the leading edge of character `o`; for the boundary
 * past the last character it is that character's trailing edge.
 *
 * This is `caretX` specialised to a line the caller already holds, so the arrow
 * keys can sweep boundaries without going back through the frame.
 */
function boundaryX(line: LayerLineBox, offset: number): number {
	const chars = line.chars;
	if (chars.length === 0) {
		return 0;
	}
	const rtl = line.direction === "rtl";
	const last = chars[chars.length - 1];
	if (last === undefined) {
		return 0;
	}
	const endEdge = rtl ? last.x : last.x + last.width;
	if (offset >= chars.length) {
		return endEdge;
	}
	const box = chars[Math.max(0, offset)];
	if (box === undefined) {
		return endEdge;
	}
	return rtl ? box.x + box.width : box.x;
}

/**
 * The line whose box contains the point, else the nearest one.
 *
 * Containment in **both** axes, not "nearest by y". That distinction is the
 * multi-column case: a two-column page's reading-ordered lines put column one's
 * lower lines and column two's upper lines at the *same* y, so nearest-by-y
 * would send a click in the right-hand column to a line in the left one. The
 * frame already numbers lines by reading order, so picking by geometry and then
 * keeping the index is what makes a drag across columns come out in reading
 * order for free.
 */
function lineAtPoint(frame: TextLayerFrame, x: number, y: number): LayerLineBox | null {
	let best: LayerLineBox | null = null;
	let bestDistance = Number.POSITIVE_INFINITY;
	for (const line of frame.lines) {
		const box = line.box;
		const insideX = x >= box.x && x <= box.x + box.width;
		const insideY = y >= box.y && y <= box.y + box.height;
		if (insideX && insideY) {
			return line;
		}
		// Distance to the box, so a click in the margin still finds the page.
		const dx = Math.max(box.x - x, 0, x - (box.x + box.width));
		const dy = Math.max(box.y - y, 0, y - (box.y + box.height));
		const distance = Math.hypot(dx, dy);
		if (distance < bestDistance) {
			best = line;
			bestDistance = distance;
		}
	}
	return best;
}

/**
 * The offset on `line` nearest a visual x.
 *
 * The line is mirrored into a left-to-right coordinate system first, which is
 * the whole of the RTL handling here: after mirroring, "the boundary to the
 * left" and "the boundary to the right" are the same arithmetic for both
 * directions, and the index arithmetic is undone at the end.
 *
 * A click inside a character splits it at its midpoint, which is what a reader
 * expects and what makes hit testing agree with the highlight it will produce.
 */
function offsetAtX(line: LayerLineBox, x: number): number {
	const rtl = line.direction === "rtl";
	if (line.chars.length === 0) {
		return 0;
	}
	const mirror = (value: number): number => (rtl ? -value : value);
	const target = mirror(x);
	let bestIndex = 0;
	let bestDistance = Number.POSITIVE_INFINITY;
	let bestLow = 0;
	let bestHigh = 0;
	for (const [index, box] of line.chars.entries()) {
		// The character's interval in mirrored space: `low` is the boundary
		// before it, `high` the boundary after it.
		const leading = mirror(rtl ? box.x + box.width : box.x);
		const trailing = mirror(rtl ? box.x : box.x + box.width);
		const low = Math.min(leading, trailing);
		const high = Math.max(leading, trailing);
		const distance = target < low ? low - target : target > high ? target - high : 0;
		if (distance < bestDistance) {
			bestDistance = distance;
			bestIndex = index;
			bestLow = low;
			bestHigh = high;
		}
	}
	return target <= (bestLow + bestHigh) / 2 ? bestIndex : bestIndex + 1;
}

/**
 * Hit-test a point in page-element CSS pixels to a caret.
 *
 * Returns `null` only for a page with no lines at all, which a caller should
 * treat as "nothing to select" rather than as an error.
 */
export function caretFromPoint(frame: TextLayerFrame, x: number, y: number): Caret | null {
	const line = lineAtPoint(frame, x, y);
	if (line === null) {
		return null;
	}
	return { line: frame.lines.indexOf(line), offset: offsetAtX(line, x) };
}

/** The caret at the very start of the document. */
export function documentStart(frame: TextLayerFrame): Caret {
	return { line: 0, offset: 0 };
}

/** The caret at the very end of the document. */
export function documentEnd(frame: TextLayerFrame): Caret {
	const last = frame.lines.length - 1;
	return last < 0 ? { line: 0, offset: 0 } : { line: last, offset: lineLength(frame, last) };
}

export function orderedRange(range: SelectionRange): OrderedRange {
	return compareCarets(range.anchor, range.head) <= 0
		? { start: range.anchor, end: range.head }
		: { start: range.head, end: range.anchor };
}

/** True when the range covers no characters. */
export function isCollapsed(range: SelectionRange): boolean {
	return compareCarets(range.anchor, range.head) === 0;
}

/** A range from one caret to itself. */
export function collapsedAt(caret: Caret): SelectionRange {
	return { anchor: caret, head: caret };
}

/**
 * The boxes to paint for a selection, one per line the range touches, in CSS
 * pixels relative to the page element.
 *
 * A range that starts mid-line and ends mid-line yields two boxes, not one
 * spanning rectangle: a highlight that ran the full width of both lines would
 * cover the rest of the line the reader did not select. The first box starts at
 * the caret's x and the last ends at the caret's x, and both come from
 * `boundaryX` — the same function that decides where a click lands, so the
 * highlight and the hit test cannot disagree.
 *
 * Lines touched only at their end (the range ends at offset 0) contribute no
 * box, because nothing of that line is selected. The text still contributes its
 * newline, which is what keeps a selection ending at the start of a line copying
 * identically to `selis extract`.
 */
export function selectionRects(frame: TextLayerFrame, range: SelectionRange): CharBox[] {
	const { start, end } = orderedRange(range);
	if (compareCarets(start, end) === 0) {
		return [];
	}
	const boxes: CharBox[] = [];
	for (let index = start.line; index <= end.line; index += 1) {
		const line = frame.lines[index];
		if (line === undefined) {
			continue;
		}
		const from = index === start.line ? start.offset : 0;
		const to = index === end.line ? end.offset : line.chars.length;
		if (to <= from) {
			continue;
		}
		const left = boundaryX(line, from);
		const right = boundaryX(line, to);
		// `from`/`to` are in reading order, so on an RTL line `right` is the
		// smaller x. Sorting is what keeps the box positive-width; taking the
		// difference directly would produce a negative width that CSS silently
		// drops, and the RTL selection would highlight nothing at all.
		boxes.push({
			x: Math.min(left, right),
			y: line.box.y,
			width: Math.abs(right - left),
			height: line.box.height,
		});
	}
	return boxes;
}

/**
 * The text a range covers, in reading order.
 *
 * This is the copy DoD's whole mechanism: a slice of the engine's own strings.
 * The engine built `chars` index-aligned with `text` and told us so, so
 * `slice(start.offset, end.offset)` is exactly the characters whose quads are
 * being highlighted. There is no re-extraction and no re-joining, which is why
 * this cannot drift from what `selis extract` prints.
 */
export function selectedText(frame: TextLayerFrame, range: SelectionRange): string {
	const { start, end } = orderedRange(range);
	if (compareCarets(start, end) === 0) {
		return "";
	}
	const parts: string[] = [];
	for (let index = start.line; index <= end.line; index += 1) {
		const line = frame.lines[index];
		if (line === undefined) {
			continue;
		}
		const from = index === start.line ? start.offset : 0;
		const to = index === end.line ? end.offset : line.text.length;
		parts.push(line.text.slice(from, to));
	}
	return parts.join("\n");
}

/** What a copy command should put on the clipboard. */
export type CopyRequest =
	| { readonly kind: "selection"; readonly page: number; readonly range: SelectionRange }
	| { readonly kind: "document" };

/**
 * The string a copy request resolves to, across pages.
 *
 * **The inter-page join is a pinned fact, not a guess.** `selis extract` prints
 * each page's `to_text` and separates consecutive pages with exactly one
 * newline (`apps/cli/src/extract.rs`: `if !first_output { println!() }` before
 * every page but the first, and `to_text` emits no trailing newline of its
 * own). So `pages.join("\n")` reproduces the CLI's whole-document output byte
 * for byte — which is the DoD, stated as a function rather than as an
 * aspiration — and a single-page selection is the same rule applied to one page.
 *
 * `to_text`'s own newline between lines and the SL-3.TEXT.10 low-confidence
 * marker are already inside `PageTextLayer.text`, carried through the engine, so
 * this function adds no formatting of its own. A copy of a page the engine
 * could not read therefore still says so, rather than handing back an empty
 * string that reads as a blank page.
 */
export function copyText(frames: readonly TextLayerFrame[], request: CopyRequest): string {
	if (request.kind === "selection") {
		const frame = frames.find((candidate) => candidate.page === request.page);
		return frame === undefined ? "" : selectedText(frame, request.range);
	}
	return frames.map((frame) => frame.text).join("\n");
}

/**
 * The word around a caret: the maximal run of inked characters.
 *
 * A word is defined by *ink*, not by a Unicode word-break, because that is what
 * the geometry supports: the assembler inserts the inter-word spaces and strips
 * the space glyphs, so a run of inked characters is exactly one word and the
 * uninked code units between runs are exactly the spaces. Using
 * `Intl.Segmenter` or a regex here would classify by script rules the PDF never
 * expressed, and could group characters the engine drew as separate words.
 *
 * A caret sitting on the gap itself selects just the gap, which is what a
 * double-click on a space does everywhere else.
 */
export function expandToWord(frame: TextLayerFrame, caret: Caret): OrderedRange {
	const line = frame.lines[caret.line];
	if (line === undefined || line.chars.length === 0) {
		return { start: caret, end: caret };
	}
	const at = Math.min(Math.max(0, caret.offset), line.chars.length - 1);
	if (line.inked[at] !== true) {
		return {
			start: { line: caret.line, offset: at },
			end: { line: caret.line, offset: at + 1 },
		};
	}
	let start = at;
	let end = at + 1;
	while (start > 0 && line.inked[start - 1] === true) {
		start -= 1;
	}
	while (end < line.chars.length && line.inked[end] === true) {
		end += 1;
	}
	return {
		start: { line: caret.line, offset: start },
		end: { line: caret.line, offset: end },
	};
}


/**
 * A caret movement, in the vocabulary a key handler produces.
 *
 * Horizontal moves are **visual** and vertical moves are **line-relative**; see
 * the module doc for why, and for the Home/End exception.
 */
export type CaretMove =
	| "left"
	| "right"
	| "up"
	| "down"
	| "lineStart"
	| "lineEnd"
	| "wordLeft"
	| "wordRight"
	| "pageStart"
	| "pageEnd";

/** Half a pixel: the threshold a visual move uses to pick a side. */
const EPSILON = 0.01;

/**
 * The boundary on `line` whose visual x is nearest `goalX`.
 *
 * Used by the vertical moves, which carry a *desired x* rather than an offset:
 * moving down one line from the middle of a short word should land near the
 * middle of the new line, not at its start. The goal x is remembered by the
 * caller across consecutive up/down presses — a caret that jumped to the line's
 * start would otherwise lose the column, and arrowing back up would not return
 * to where the reader was.
 */
function offsetNearestX(line: LayerLineBox, goalX: number): number {
	let best = 0;
	let bestDistance = Number.POSITIVE_INFINITY;
	for (let offset = 0; offset <= line.chars.length; offset += 1) {
		const distance = Math.abs(boundaryX(line, offset) - goalX);
		if (distance < bestDistance) {
			bestDistance = distance;
			best = offset;
		}
	}
	return best;
}

/**
 * The boundary one step visually left of `offset`, or `null` at the line's edge.
 *
 * "Visually left" is resolved against boundary positions, not against indices,
 * which is the entire RTL correction: on a right-to-left line the boundaries
 * descend in x as the offset rises, so this returns a *larger* offset. The
 * result is that ArrowLeft walks the caret leftwards on screen in both
 * directions, which is what a browser does and what the reader's hand expects.
 */
function stepVisual(line: LayerLineBox, offset: number, towardsLeft: boolean): number | null {
	const here = boundaryX(line, offset);
	let best: number | null = null;
	let bestDistance = Number.POSITIVE_INFINITY;
	for (let candidate = 0; candidate <= line.chars.length; candidate += 1) {
		if (candidate === offset) {
			continue;
		}
		const x = boundaryX(line, candidate);
		const toLeft = x < here - EPSILON;
		if (toLeft !== towardsLeft) {
			continue;
		}
		const distance = Math.abs(here - x);
		if (distance < bestDistance) {
			bestDistance = distance;
			best = candidate;
		}
	}
	return best;
}

/**
 * The nearest word boundary in the given visual direction, or `null` at the edge.
 *
 * Boundaries that begin a word (the one just before an inked run) are the
 * candidates, so a word move skips the whole run rather than one character —
 * which is what Ctrl+Arrow does, and what makes the move reversible by
 * repeating it in the other direction.
 */
function stepWord(line: LayerLineBox, offset: number, towardsLeft: boolean): number | null {
	const here = boundaryX(line, offset);
	let best: number | null = null;
	let bestDistance = Number.POSITIVE_INFINITY;
	for (let candidate = 0; candidate <= line.chars.length; candidate += 1) {
		const x = boundaryX(line, candidate);
		if ((x < here - EPSILON) !== towardsLeft) {
			continue;
		}
		if (line.inked[candidate] !== false) {
			continue;
		}
		const distance = Math.abs(here - x);
		if (distance < bestDistance) {
			bestDistance = distance;
			best = candidate;
		}
	}
	return best;
}

/**
 * Move a caret.
 *
 * `goalX` is the caller's remembered column for the vertical moves; every other
 * command ignores it, so a handler can pass the same value unconditionally.
 * When a vertical move lands on a line with no characters — a
 * drawn-but-unrecovered line, which keeps its reading-order slot but has nothing
 * selectable — the caret takes that line's start rather than refusing to move,
 * so arrowing down a page with a gap in it does not stall.
 */
export function moveCaret(
	frame: TextLayerFrame,
	caret: Caret,
	move: CaretMove,
	goalX = Number.NaN,
): Caret {
	const start = clampCaret(frame, caret);
	const line = frame.lines[start.line];
	if (line === undefined) {
		return start;
	}
	const target = Number.isFinite(goalX) ? goalX : boundaryX(line, start.offset);
	switch (move) {
		case "left":
		case "right": {
			const next = stepVisual(line, start.offset, move === "left");
			return next === null ? start : { line: start.line, offset: next };
		}
		case "wordLeft":
		case "wordRight": {
			const next = stepWord(line, start.offset, move === "wordLeft");
			return next === null ? start : { line: start.line, offset: next };
		}
		case "lineStart":
			// Logical, not visual: the keys are named for the start and end of
			// the line in reading order, and on an RTL line those are the right
			// and left edges respectively.
			return { line: start.line, offset: 0 };
		case "lineEnd":
			return { line: start.line, offset: line.chars.length };
		case "up":
		case "down": {
			const nextIndex = start.line + (move === "up" ? -1 : 1);
			const nextLine = frame.lines[nextIndex];
			if (nextLine === undefined) {
				return start;
			}
			return { line: nextIndex, offset: offsetNearestX(nextLine, target) };
		}
		case "pageStart":
			return documentStart(frame);
		case "pageEnd":
			return documentEnd(frame);
	}
}

/**
 * Apply a move to a range, honouring the shift key.
 *
 * Without shift the caret becomes both ends — the selection collapses to where
 * the caret went, which is what every editor does and what stops a stray arrow
 * key from leaving a highlight behind. With shift the `anchor` is pinned and
 * only the `head` moves, so a shift-arrow extends and a reverse shift-arrow
 * shrinks back through the same characters.
 */
export function moveRange(
	frame: TextLayerFrame,
	range: SelectionRange,
	move: CaretMove,
	extend: boolean,
	goalX?: number,
): SelectionRange {
	const from = extend ? range.head : orderedRange(range).end;
	const head = moveCaret(frame, from, move, goalX);
	return { anchor: extend ? range.anchor : head, head };
}

