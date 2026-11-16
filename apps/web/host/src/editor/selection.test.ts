import { describe, expect, it } from "vitest";

import {
	characterAt,
	compareEnds,
	runsToPaint,
	textBetween,
	type SelectionLine,
} from "./selection.js";

/** One line of five characters, each 10 wide and 20 tall, on a baseline. */
function line(text: string, top: number): SelectionLine {
	return {
		text,
		chars: [...text].map((_, index) => ({
			x: 10 + index * 10,
			y: top,
			width: 10,
			height: 20,
			inked: text[index] !== " ",
		})),
	};
}

const LINES: readonly SelectionLine[] = [line("Hello", 100), line("World", 140)];

describe("the selection over a page's text", () => {
	it("finds the character under a point", () => {
		// The second character of the first line.
		const found = characterAt(LINES, { x: 25, y: 110 });
		expect(found).toEqual({ line: 0, char: 1 });
	});

	it("finds the nearest character when the point is past the end", () => {
		// Past the last character of the first line.
		const found = characterAt(LINES, { x: 80, y: 110 });
		expect(found).toEqual({ line: 0, char: 4 });
	});

	it("takes the text between two ends, in reading order", () => {
		expect(textBetween(LINES, { line: 0, char: 0 }, { line: 1, char: 4 })).toBe(
			"Hello World",
		);
	});

	it("takes the same text when the selection is dragged backwards", () => {
		expect(textBetween(LINES, { line: 1, char: 4 }, { line: 0, char: 0 })).toBe(
			"Hello World",
		);
	});

	it("orders the ends by the reading order", () => {
		expect(compareEnds({ line: 0, char: 4 }, { line: 1, char: 0 })).toBeLessThan(0);
		expect(compareEnds({ line: 1, char: 0 }, { line: 0, char: 4 })).toBeGreaterThan(0);
	});

	it("paints the ink's box, not the character's whole box", () => {
		// The character box is 20 tall (twice the type size); the ink of a
		// line is the ascent and the descent: three quarters of the type
		// above the baseline and a quarter below it.
		const runs = runsToPaint(LINES, { line: 0, char: 0 }, { line: 0, char: 4 });
		expect(runs).toHaveLength(1);
		const run = runs[0];
		expect(run?.width).toBe(50);
		// The baseline is the box's centre (110), and the ink runs from
		// three quarters of the type above it to a quarter below it. The
		// page's y axis points UP, so the box starts a quarter below the
		// baseline and reaches the ascent.
		expect(run?.y).toBe(107.5);
		expect(run?.height).toBe(10);
	});

	it("leaves the spaces out of the painted run", () => {
		const spaced: readonly SelectionLine[] = [line("a b", 100)];
		const runs = runsToPaint(spaced, { line: 0, char: 0 }, { line: 0, char: 2 });
		// The space has no ink, so the run covers the two words and the gap
		// between them - the words, not the space.
		expect(runs[0]?.width).toBe(30);
	});
});
