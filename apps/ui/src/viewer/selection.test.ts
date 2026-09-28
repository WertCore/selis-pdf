/**
 * Selection, hit testing, and copy (SL-4.UI.04), the other half of the DoD:
 * *selection accuracy tested against known quads; copy output matches
 * `selis extract`.*
 *
 * As in `text-layer.test.ts` every quad here is hand-authored, so a failure
 * names a number rather than disagreeing with a model. The layer builders are
 * deliberately explicit about reading order and direction, because those are
 * the two properties the copy and RTL claims rest on.
 *
 * ## What "copy matches selis extract" can honestly mean here
 *
 * The engine already asserts, in `selis-pdf-text`, that `PageTextLayer::text`
 * is byte-identical to `to_text` for the same page -- and `to_text` is what
 * `apps/cli/src/extract.rs` prints. That half of the equality is pinned in
 * Rust, where the formatter lives, and it is not re-asserted here against a
 * second implementation of it.
 *
 * What is pinned here is the half this package owns, and it is the half that
 * could actually drift:
 *
 *  1. `copyText(frames, {kind:"document"})` reproduces the CLI's whole-document
 *     bytes: each page's `text`, separated by exactly one newline, because
 *     `extract.rs` prints one blank-line separator between pages and no page
 *     text carries a trailing newline of its own.
 *  2. A **whole-page selection** copies exactly `frame.text`. This is the
 *     property that connects selection to the CLI: if selecting everything on a
 *     page did not yield the page's own `text`, then no partial selection could
 *     either, and the two halves would be free to disagree.
 *  3. Every partial selection is a pure slice, so it cannot introduce a
 *     separator, a reordering, or a re-joining of its own.
 *
 * ADR-P0021's no-jsdom rule means there is no DOM here to assert against; the
 * layer and the range are plain data, which is the whole of what the host hands
 * to the browser.
 */

import { describe, expect, it } from "vitest";
import type { PageTextLayer, Rect, TextLayerChar, TextLayerLine } from "../platform/types.js";
import type { PlacedPage } from "./layout.js";
import type { Caret, SelectionRange } from "./selection.js";
import {
	caretFromPoint,
	clampCaret,
	collapsedAt,
	copyText,
	documentEnd,
	documentStart,
	expandToWord,
	isCollapsed,
	moveCaret,
	moveRange,
	orderedRange,
	selectedText,
	selectionRects,
} from "./selection.js";
import type { TextLayerFrame } from "./text-layer.js";
import { buildTextLayer } from "./text-layer.js";

const PAGE_WIDTH = 612;
const PAGE_HEIGHT = 792;

/**
 * Baseline 700 in user space is CSS y 80: the page is 792pt tall and the glyph
 * box is 12pt tall, so its top edge is 792 - (700 + 12) = 80pt below the top.
 * The tests assert against these names rather than repeating 80, so the flip is
 * stated once and cannot drift from the numbers.
 */
const CSS_TOP = PAGE_HEIGHT - (700 + 12);
const CSS_TOP_2 = PAGE_HEIGHT - (660 + 12);

/** Placed at 1 CSS px per point, so user-space numbers are readable as CSS px. */
const PLACED_1X: PlacedPage = {
	page: 0,
	row: 0,
	column: 0,
	x: 0,
	y: 0,
	width: PAGE_WIDTH,
	height: PAGE_HEIGHT,
	scale: 1,
};

function rect(x: number, y: number, width: number, height: number): Rect {
	return { x, y, width, height };
}

/** One inked glyph: 10pt wide, 12pt tall, baseline at its bottom. */
function inked(x: number, y: number): TextLayerChar {
	return { rect: rect(x, y, 10, 12), advance: 10, inked: true };
}

/** A synthesised inter-word gap: the space glyph is stripped by the assembler. */
function gap(x: number, y: number): TextLayerChar {
	return { rect: rect(x, y, 10, 12), advance: 0, inked: false };
}

function layerOf(lines: TextLayerLine[], text: string, lowConfidence = false): PageTextLayer {
	return { page: 0, width: PAGE_WIDTH, height: PAGE_HEIGHT, lines, text, lowConfidence };
}
/** A left-to-right line: characters ascend in x from `startX`. */
function ltrLine(text: string, startX: number, baselineY: number): TextLayerLine {
	const chars: TextLayerChar[] = [];
	let pen = startX;
	for (const unit of text) {
		if (unit === " ") {
			chars.push(gap(pen, baselineY));
			pen += 10;
			continue;
		}
		chars.push(inked(pen, baselineY));
		pen += 10;
	}
	return { text, rect: rect(startX, baselineY, pen - startX, 12), direction: "ltr", chars };
}

/**
 * A right-to-left line: character 0 is the rightmost, so the quads descend in x
 * as the offset rises. This is the only evidence a line runs backwards.
 */
function rtlLine(text: string, rightX: number, baselineY: number): TextLayerLine {
	const chars: TextLayerChar[] = [];
	let pen = rightX;
	for (const unit of text) {
		if (unit === " ") {
			chars.push(gap(pen - 10, baselineY));
			pen -= 10;
			continue;
		}
		chars.push(inked(pen - 10, baselineY));
		pen -= 10;
	}
	return { text, rect: rect(pen, baselineY, rightX - pen, 12), direction: "rtl", chars };
}

/** A frame from hand-authored lines, placed at 1 CSS px per point. */
function frameOf(lines: TextLayerLine[], text: string, lowConfidence = false): TextLayerFrame {
	return buildTextLayer(layerOf(lines, text, lowConfidence), PLACED_1X);
}

function at(line: number, offset: number): Caret {
	return { line, offset };
}

function range(
	anchorLine: number,
	anchorOffset: number,
	headLine: number,
	headOffset: number,
): SelectionRange {
	return { anchor: at(anchorLine, anchorOffset), head: at(headLine, headOffset) };
}
describe("hit testing against known quads", () => {
	// A glyph at x=100 occupies [100,110); its midpoint is 105. These quads are
	// literal, so every expectation below is arithmetic on them rather than a
	// recorded output.
	const line = ltrLine("abcd", 100, 700);
	const frame = frameOf([line], "abcd");

	it("puts a click in a glyph's first half before that glyph", () => {
		expect(caretFromPoint(frame, 100, CSS_TOP + 6)).toEqual(at(0, 0));
		expect(caretFromPoint(frame, 104.9, CSS_TOP + 6)).toEqual(at(0, 0));
	});

	it("puts a click in a glyph's second half after that glyph", () => {
		expect(caretFromPoint(frame, 105.1, CSS_TOP + 6)).toEqual(at(0, 1));
		expect(caretFromPoint(frame, 109.9, CSS_TOP + 6)).toEqual(at(0, 1));
	});

	it("round-trips every boundary through the quads it came from", () => {
		// The strongest form of "accurate": for each boundary x, clicking there
		// must return that boundary. If the hit test and the quads disagreed
		// about where a boundary is, this fails for some offset.
		for (let offset = 0; offset <= 4; offset += 1) {
			expect(caretFromPoint(frame, 100 + offset * 10, CSS_TOP + 6)).toEqual(at(0, offset));
		}
	});

	it("clamps a click past the end of the line to its last boundary", () => {
		expect(caretFromPoint(frame, 900, CSS_TOP + 6)).toEqual(at(0, 4));
	});

	it("clamps a click before the start of the line to its first", () => {
		expect(caretFromPoint(frame, 0, CSS_TOP + 6)).toEqual(at(0, 0));
	});

	it("picks the line whose box contains the point, in both axes", () => {
		// Two lines 40pt apart vertically. Containment in y is what separates
		// them; nearest-by-x alone would send a click to either.
		const two = frameOf([ltrLine("first", 100, 700), ltrLine("other", 100, 660)], "first\nother");
		expect(caretFromPoint(two, 100, CSS_TOP + 6)).toEqual(at(0, 0));
		expect(caretFromPoint(two, 100, 666)).toEqual(at(1, 0));
	});

	it("still finds a line for a click in the margin", () => {
		// Below the last line and right of the text: there is no containing box,
		// and a click that selected nothing would be a dead zone.
		expect(caretFromPoint(frame, 560, PAGE_HEIGHT - 12)).toEqual(at(0, 4));
	});

	it("returns null for a page with no lines at all", () => {
		expect(caretFromPoint(frameOf([], ""), 100, 700)).toBeNull();
	});

	it("splits an RTL glyph at its midpoint, mirrored", () => {
		// The same visual rule as a left-to-right line, resolved against the
		// mirrored geometry: character 0 is the rightmost, at [190,200).
		const mirrored = frameOf([rtlLine("abc", 200, 700)], "abc");
		expect(caretFromPoint(mirrored, 199, CSS_TOP + 6)).toEqual(at(0, 0));
		expect(caretFromPoint(mirrored, 191, CSS_TOP + 6)).toEqual(at(0, 1));
		expect(caretFromPoint(mirrored, 180, CSS_TOP + 6)).toEqual(at(0, 2));
	});
});

describe("selection rectangles", () => {
	const frame = frameOf([ltrLine("hello world", 100, 700)], "hello world");

	it("covers exactly the clicked characters, not the whole line", () => {
		// "hello" is offsets 0..5, so [100,150). A highlight running the full
		// line width would cover the rest of the line the reader did not select.
		expect(selectionRects(frame, range(0, 0, 0, 5))).toEqual([
			{ x: 100, y: CSS_TOP, width: 50, height: 12 },
		]);
	});

	it("pads an interior word to both of its gaps", () => {
		// "world" is offsets 6..11, and the click that starts it landed inside
		// the space, so the box must start at the space's left edge (offset 6).
		expect(selectionRects(frame, range(0, 6, 0, 11))).toEqual([
			{ x: 160, y: CSS_TOP, width: 50, height: 12 },
		]);
	});

	it("emits one box per line for a multi-line range", () => {
		const two = frameOf([ltrLine("first", 100, 700), ltrLine("other", 100, 660)], "first\nother");
		const boxes = selectionRects(two, range(0, 2, 1, 2));
		expect(boxes).toEqual([
			{ x: 120, y: CSS_TOP, width: 30, height: 12 },
			{ x: 100, y: CSS_TOP_2, width: 20, height: 12 },
		]);
	});

	it("has positive width on an RTL line", () => {
		// Reading order runs right to left, so the end caret's x is the smaller
		// one. Taking the difference directly would give a negative width, which
		// CSS silently drops -- the selection would highlight nothing at all.
		const mirrored = frameOf([rtlLine("abc", 200, 700)], "abc");
		expect(selectionRects(mirrored, range(0, 0, 0, 2))).toEqual([
			{ x: 180, y: CSS_TOP, width: 20, height: 12 },
		]);
	});

	it("emits nothing for a collapsed range or a line touched only at its end", () => {
		expect(selectionRects(frame, range(0, 2, 0, 2))).toEqual([]);
		const two = frameOf([ltrLine("first", 100, 700), ltrLine("other", 100, 660)], "first\nother");
		expect(selectionRects(two, range(0, 1, 1, 0))).toEqual([
			{ x: 110, y: CSS_TOP, width: 40, height: 12 },
		]);
	});

	it("agrees with the hit test that produced it", () => {
		// The DoD's accuracy claim, closed: click, then highlight. If the hit
		// test and the boxes disagreed, the reader would see one thing and get
		// another.
		const start = caretFromPoint(frame, 102, CSS_TOP + 6);
		const end = caretFromPoint(frame, 142, CSS_TOP + 6);
		if (start === null || end === null) {
			throw new Error("expected both clicks to hit the line");
		}
		const boxes = selectionRects(frame, { anchor: start, head: end });
		expect(boxes).toEqual([{ x: 100, y: CSS_TOP, width: 40, height: 12 }]);
		expect(selectedText(frame, { anchor: start, head: end })).toBe("hell");
	});
});
describe("copy preserves reading order, not visual order", () => {
	// A two-column page. The engine hands over lines already in reading order
	// (SL-3.TEXT.04, structure-tree-first), so column one is lines 0..1 even
	// though column two's *upper* line is higher on the page than column one's
	// lower one. This is the arrangement that makes "reading order, not visual
	// order" a testable claim rather than a slogan: a visual-order copy would
	// interleave the columns and read "Right top, Left top, Right bottom,
	// Left bottom".
	const twoColumn = frameOf(
		[ltrLine("Left one", 72, 700), ltrLine("Left two", 72, 660), rtlLine("Right one", 400, 700)],
		"Left one\nLeft two\nRight one",
	);

	it("numbers the lines by reading order, not by y", () => {
		// Line 1 sits above line 2's neighbour in the right column, so a naive
		// "sort by y then x" would reorder them. The frame must not.
		expect(twoColumn.lines[0]?.text).toBe("Left one");
		expect(twoColumn.lines[1]?.text).toBe("Left two");
		expect(twoColumn.lines[2]?.text).toBe("Right one");
	});

	it("copies column one before column two whatever the drag direction", () => {
		// Dragged from the bottom-left up to the top-right: the anchor is later
		// in reading order than the head, so the range is inverted and must be
		// reordered before slicing.
		// start (0,4) is inside the space of "Left one", so the copy is that
		// line's tail, its newline, then "Left two" up to offset 4.
		expect(selectedText(twoColumn, range(1, 4, 0, 4))).toBe(" one\nLeft");
	});

	it("copies the same text whichever end the drag started from", () => {
		// start (0,5) is the space before "one", so the copy is "one", the
		// newline, then "Lef" of the next line: the same bytes whichever end led.
		const forward = selectedText(twoColumn, range(0, 5, 1, 3));
		const backward = selectedText(twoColumn, range(1, 3, 0, 5));
		expect(forward).toBe("one\nLef");
		expect(backward).toBe(forward);
	});

	it("does not reorder an RTL line's own text when copying it", () => {
		// Reading order within a mirrored line is still the logical string, so a
		// copy is "Right one" and not "eno thgiR". Direction moves the caret and
		// the highlight; it does not reverse the text.
		expect(selectedText(twoColumn, range(2, 0, 2, 9))).toBe("Right one");
	});
});

describe("copy output matches selis extract", () => {
	it("reproduces a whole-page selection as the page's own text", () => {
		// The link between the two halves of the DoD. `PageTextLayer.text` is
		// pinned in Rust to be byte-identical to `to_text`, which is what
		// `apps/cli/src/extract.rs` prints. If selecting a whole page did not
		// yield exactly that string, no partial selection could be trusted
		// either.
		const text = "First line\nSecond line";
		const frame = frameOf([ltrLine("First line", 72, 700), ltrLine("Second line", 72, 680)], text);
		expect(selectedText(frame, range(0, 0, 1, 11))).toBe(frame.text);
		expect(selectedText(frame, range(0, 0, 1, 11))).toBe("First line\nSecond line");
	});

	it("reproduces a whole document as the pages joined by one newline", () => {
		// The separator is a pinned fact, not a guess: `extract.rs` prints one
		// `println!()` between pages and `to_text` emits no trailing newline, so
		// the CLI's whole-document output is exactly `pages.join("\n")`.
		const pages = [
			frameOf([ltrLine("Page one text", 72, 700)], "Page one text"),
			frameOf([ltrLine("Page two text", 72, 700)], "Page two text"),
			frameOf([ltrLine("Page three", 72, 700)], "Page three"),
		];
		expect(copyText(pages, { kind: "document" })).toBe("Page one text\nPage two text\nPage three");
	});

	it("joins a single page with no separator, so one page is the same rule", () => {
		const pages = [frameOf([ltrLine("Only page", 72, 700)], "Only page")];
		expect(copyText(pages, { kind: "document" })).toBe("Only page");
	});

	it("adds no trailing newline, because the CLI adds none", () => {
		// A trailing "\n" here would be invisible in a text editor and would
		// fail a byte comparison against the CLI.
		const pages = [frameOf([ltrLine("End", 72, 700)], "End")];
		expect(copyText(pages, { kind: "document" }).endsWith("\n")).toBe(false);
	});

	it("copies a partial selection as a pure slice of the page's own text", () => {
		// A partial selection introduces no separator, no re-joining, and no
		// second code path, which is why it cannot drift from the whole-page
		// case above.
		const text = "alpha beta gamma";
		const frame = frameOf([ltrLine(text, 100, 700)], text);
		const whole = text.length;
		expect(selectedText(frame, range(0, 0, 0, whole))).toBe(text);
		expect(selectedText(frame, range(0, 6, 0, 10))).toBe("beta");
		expect(selectedText(frame, range(0, 0, 0, 5))).toBe("alpha");
	});

	it("keeps a multi-line partial selection's newline and nothing else", () => {
		const frame = frameOf(
			[ltrLine("one two", 100, 700), ltrLine("three four", 100, 680)],
			"one two\nthree four",
		);
		// From inside "two" to inside "three": the newline between the lines is
		// part of the answer, and the untouched words are not.
		expect(selectedText(frame, range(0, 4, 1, 5))).toBe("two\nthree");
	});

	it("carries a low-confidence page's marker, as the CLI does", () => {
		// SL-3.TEXT.10: a page the engine could not read must copy as the
		// marker, not as an empty string that reads as a blank page.
		const marker = "[text could not be recovered]";
		const frame = frameOf([], marker, true);
		expect(copyText([frame], { kind: "document" })).toBe(marker);
		expect(frame.lowConfidence).toBe(true);
	});

	it("returns an empty string for a collapsed range and for a missing page", () => {
		const frame = frameOf([ltrLine("text", 100, 700)], "text");
		expect(selectedText(frame, range(0, 2, 0, 2))).toBe("");
		expect(copyText([frame], { kind: "selection", page: 99, range: range(0, 0, 0, 4) })).toBe("");
	});
});
describe("range ordering and clamping", () => {
	const frame = frameOf([ltrLine("abcd", 100, 700)], "abcd");

	it("puts an inverted range's ends in reading order", () => {
		expect(orderedRange(range(0, 3, 0, 1))).toEqual({ start: at(0, 1), end: at(0, 3) });
	});

	it("orders by line before offset", () => {
		// A caret at line 0 offset 99 precedes one at line 1 offset 0, which a
		// flat compare on the pair gets right only by accident if it forgets the
		// line is the major key.
		expect(orderedRange(range(1, 0, 0, 99))).toEqual({ start: at(0, 99), end: at(1, 0) });
	});

	it("reports a collapsed range, and builds one", () => {
		expect(isCollapsed(range(0, 2, 0, 2))).toBe(true);
		expect(isCollapsed(range(0, 2, 0, 3))).toBe(false);
		expect(isCollapsed(collapsedAt(at(1, 0)))).toBe(true);
	});

	it("clamps a caret that addresses a character which is not there", () => {
		// Every entry point assumes a clamped caret, so this is the guard that
		// makes "no input can address a missing character" true.
		expect(clampCaret(frame, at(0, 99))).toEqual(at(0, 4));
		expect(clampCaret(frame, at(-5, -5))).toEqual(at(0, 0));
		expect(clampCaret(frame, at(99, 0))).toEqual(at(0, 0));
	});

	it("collapses an empty frame to the origin", () => {
		expect(clampCaret(frameOf([], ""), at(3, 3))).toEqual(at(0, 0));
	});

	it("reaches both ends of the document", () => {
		expect(documentStart(frame)).toEqual(at(0, 0));
		expect(documentEnd(frame)).toEqual(at(0, 4));
		expect(documentEnd(frameOf([], ""))).toEqual(at(0, 0));
	});
});

describe("keyboard navigation", () => {
	const frame = frameOf([ltrLine("abcd", 100, 700), ltrLine("efgh", 100, 660)], "abcd\nefgh");

	it("moves one character at a time and stops at the line edge", () => {
		expect(moveCaret(frame, at(0, 0), "right")).toEqual(at(0, 1));
		// "abcd" is four characters, so offset 4 is the last boundary.
		expect(moveCaret(frame, at(0, 3), "right")).toEqual(at(0, 4));
		expect(moveCaret(frame, at(0, 4), "right")).toEqual(at(0, 4));
		expect(moveCaret(frame, at(0, 0), "left")).toEqual(at(0, 0));
	});

	it("treats Home and End as logical, not visual", () => {
		// The keys are named for the start and end of the line in reading
		// order, which on a mirrored line are the right and left edges.
		const mirrored = frameOf([rtlLine("abc", 200, 700)], "abc");
		expect(moveCaret(mirrored, at(0, 2), "lineStart")).toEqual(at(0, 0));
		expect(moveCaret(mirrored, at(0, 0), "lineEnd")).toEqual(at(0, 3));
	});

	it("moves leftwards on screen on an RTL line, as a browser does", () => {
		// Arrow keys are visual, which on a mirrored line means ArrowLeft walks
		// the caret left on screen and therefore *backwards* through the text.
		const mirrored = frameOf([rtlLine("abc", 200, 700)], "abc");
		const start = at(0, 0);
		const left = moveCaret(mirrored, start, "left");
		expect(left).toEqual(at(0, 1));
		const right = moveCaret(mirrored, left, "right");
		expect(right).toEqual(start);
	});

	it("keeps the column across a vertical move", () => {
		// Arrowing down from the middle of a 3-character line lands in the middle
		// of the next, not at its start, and arrowing back up returns.
		const down = moveCaret(frame, at(0, 2), "down", 120);
		expect(down).toEqual(at(1, 2));
		expect(moveCaret(frame, down, "up", 120)).toEqual(at(0, 2));
	});

	it("does not stall when a vertical move crosses an unrecovered line", () => {
		// The line keeps its reading-order slot but has nothing selectable, so
		// the caret takes its start rather than refusing to move.
		const gapped = frameOf(
			[
				ltrLine("abc", 100, 700),
				{ text: "", rect: rect(0, 0, 0, 0), direction: "ltr", chars: [] },
				ltrLine("def", 100, 620),
			],
			"abc\n\ndef",
		);
		expect(moveCaret(gapped, at(0, 1), "down", 110)).toEqual(at(1, 0));
		expect(moveCaret(gapped, at(1, 0), "down", 110)).toEqual(at(2, 1));
	});

	it("skips a whole word, and back again", () => {
		const words = frameOf([ltrLine("alpha beta", 100, 700)], "alpha beta");
		const moved = moveCaret(words, at(0, 0), "wordRight");
		expect(moved).toEqual(at(0, 6));
		expect(moveCaret(words, moved, "wordLeft")).toEqual(at(0, 0));
	});

	it("collapses the selection without shift, and extends it with shift", () => {
		const selected = range(0, 1, 0, 3);
		expect(isCollapsed(moveRange(frame, selected, "right", false))).toBe(true);
		const extended = moveRange(frame, selected, "right", true);
		expect(extended.anchor).toEqual(at(0, 1));
		expect(extended.head).toEqual(at(0, 4));
	});

	it("shrinks back through the same characters when shift is reversed", () => {
		const extended = range(0, 1, 0, 4);
		const shrunk = moveRange(frame, extended, "left", true);
		expect(shrunk.anchor).toEqual(at(0, 1));
		expect(shrunk.head).toEqual(at(0, 3));
	});

	it("expands a double-click to the word around the caret", () => {
		// A word is a run of *inked* characters, because that is what the
		// geometry supports: the assembler inserts the inter-word spaces and
		// strips the space glyphs.
		const words = frameOf([ltrLine("alpha beta", 100, 700)], "alpha beta");
		expect(expandToWord(words, at(0, 2))).toEqual({ start: at(0, 0), end: at(0, 5) });
		expect(expandToWord(words, at(0, 8))).toEqual({ start: at(0, 6), end: at(0, 10) });
		// A caret on the gap selects just the gap, as a double-click on a space
		// does everywhere else.
		expect(expandToWord(words, at(0, 5))).toEqual({ start: at(0, 5), end: at(0, 6) });
	});
});
