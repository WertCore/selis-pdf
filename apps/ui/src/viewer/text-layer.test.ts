/**
 * Text layer geometry (SL-4.UI.04), half the DoD: *selection accuracy tested
 * against known quads*.
 *
 * Every quad in this file is **hand-authored**, not synthesised. The mock
 * adapter's `synthesiseTextLayer` is a convenient model, but asserting against
 * a model only proves the code agrees with the model; the DoD asks for known
 * quads, so the numbers here are stated literally and the projection is
 * checked against arithmetic done in the test body. If the projection's y-flip
 * or its scale ever changes, these fail with a number rather than a diff.
 *
 * What a Node test can honestly assert is the layer's contract as data, which
 * is the whole of what a host consumes. The repo ships no jsdom, so
 * there is no DOM to assert against; the host paints `TextLayerFrame` and the
 * browser's own text layout is what puts the glyphs there. Asserting the boxes
 * is therefore asserting everything the viewer is responsible for.
 */

import { describe, expect, it } from "vitest";
import type { PageTextLayer, Rect, TextLayerChar, TextLayerLine } from "../platform/types.js";
import type { PlacedPage } from "./layout.js";
import type { LayerLineBox, TextLayerFrame } from "./text-layer.js";
import { buildTextLayer, caretX, lineLength } from "./text-layer.js";

/** US Letter, the size the plan's fixtures use. */
const PAGE_WIDTH = 612;
const PAGE_HEIGHT = 792;

/** A page placed at 1.5 CSS px per point, i.e. 918 x 1188 CSS px. */
const PLACED_1_5: PlacedPage = {
	page: 0,
	row: 0,
	column: 0,
	x: 0,
	y: 0,
	width: PAGE_WIDTH * 1.5,
	height: PAGE_HEIGHT * 1.5,
	scale: 1.5,
};

function rect(x: number, y: number, width: number, height: number): Rect {
	return { x, y, width, height };
}

/** One inked glyph: a 10pt-wide, 12pt-tall box with the baseline at its bottom. */
function inked(x: number, y: number): TextLayerChar {
	return { rect: rect(x, y, 10, 12), advance: 10, inked: true };
}

/** A synthesised inter-word gap: zero advance, and not a glyph of its own. */
function gap(x: number, y: number, width = 10): TextLayerChar {
	return { rect: rect(x, y, width, 12), advance: 0, inked: false };
}

function layerOf(lines: TextLayerLine[], text: string, lowConfidence = false): PageTextLayer {
	return { page: 0, width: PAGE_WIDTH, height: PAGE_HEIGHT, lines, text, lowConfidence };
}

/** A line of the given text whose characters ascend in x from `startX`. */
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
		for (let extra = 1; extra < unit.length; extra += 1) {
			// The trailing half of an astral scalar: same quad, no advance.
			chars.push({ rect: rect(pen, baselineY, 10, 12), advance: 0, inked: false });
		}
		pen += 10;
	}
	return { text, rect: rect(startX, baselineY, pen - startX, 12), direction: "ltr", chars };
}

/**
 * A right-to-left line: character 0 is the *rightmost*, so the quads descend in
 * x as the offset rises. This is what the engine reports for a mirrored run, and
 * it is the only evidence the text layer has that the line runs backwards.
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
	return {
		text,
		rect: rect(pen, baselineY, rightX - pen, 12),
		direction: "rtl",
		chars,
	};
}

function line0(frame: TextLayerFrame): LayerLineBox {
	const line = frame.lines[0];
	if (line === undefined) {
		throw new Error("frame has no line 0");
	}
	return line;
}
describe("the text layer's projection into a page box", () => {
	it("flips y about the page height, not about the origin", () => {
		// A glyph 72pt from the top of the page: user-space y is 792 - 72 - 12
		// = 708, so the box's top edge is 72pt below the element's top.
		const frame = buildTextLayer(layerOf([ltrLine("A", 72, 708)], "A"), PLACED_1_5);
		expect(line0(frame).chars[0]).toEqual({ x: 108, y: 108, width: 15, height: 18 });
	});

	it("is not the classic pageHeight - y", () => {
		// The whole reason the flip lives in one function: `792 - 708` is 84,
		// which is right for a zero-height box and wrong by the full box height
		// (12pt, so 18 CSS px) for every real one. A mirror-image selection.
		const frame = buildTextLayer(layerOf([ltrLine("A", 72, 708)], "A"), PLACED_1_5);
		const naive = (792 - 708) * 1.5;
		expect(naive).toBe(126);
		expect(line0(frame).chars[0]?.y).toBe(108);
		expect(line0(frame).chars[0]?.y).not.toBe(naive);
	});

	it("scales by the page's CSS scale and by nothing else", () => {
		const at1x = buildTextLayer(layerOf([ltrLine("A", 72, 708)], "A"), {
			...PLACED_1_5,
			width: PAGE_WIDTH,
			height: PAGE_HEIGHT,
			scale: 1,
		});
		const at2x = buildTextLayer(layerOf([ltrLine("A", 72, 708)], "A"), {
			...PLACED_1_5,
			width: PAGE_WIDTH * 2,
			height: PAGE_HEIGHT * 2,
			scale: 2,
		});
		expect(line0(at1x).chars[0]).toEqual({ x: 72, y: 72, width: 10, height: 12 });
		expect(line0(at2x).chars[0]).toEqual({ x: 144, y: 144, width: 20, height: 24 });
	});

	it("does not apply a device-pixel ratio of its own", () => {
		// UI.03's `SurfaceSize.scale` is the real backing-store factor and
		// `devicePixelRatio` the nominal one; the text layer is DOM in CSS
		// pixels inside the page element, so it multiplies by neither. A layer
		// that applied the DPR would sit at 1.25x its box on a 125% display,
		// and the drift would be proportional: plausible, and wrong.
		const frame = buildTextLayer(layerOf([ltrLine("A", 72, 708)], "A"), PLACED_1_5);
		expect(line0(frame).chars[0]?.x).toBe(72 * 1.5);
	});

	it("sizes the frame to the page element, not to the inked area", () => {
		// A short line must not shrink the frame: the frame is the page box the
		// host positions the layer inside, and the page is the page.
		const frame = buildTextLayer(layerOf([ltrLine("A", 72, 708)], "A"), PLACED_1_5);
		expect(frame.width).toBe(918);
		expect(frame.height).toBe(1188);
		expect(frame.scale).toBe(1.5);
	});
});
describe("the index alignment the copy path depends on", () => {
	it("keeps chars index-aligned with the line text", () => {
		// The contract the whole copy path rests on: the i-th code unit of the
		// text is the i-th char. An astral scalar is two code units and two
		// entries, the second sharing the first's quad.
		const text = "A\u{1F600}B";
		const frame = buildTextLayer(layerOf([ltrLine(text, 72, 708)], text), PLACED_1_5);
		const line = line0(frame);
		expect(line.chars).toHaveLength(line.text.length);
		expect(line.inked).toEqual([true, true, false, true]);
		// Both halves of the astral pair carry the *same* quad, so a consumer
		// reading one box per code unit still highlights the glyph's box.
		expect(line.chars[1]).toEqual(line.chars[2]);
	});

	it("marks a synthesised inter-word space as uninked", () => {
		const frame = buildTextLayer(layerOf([ltrLine("ab cd", 72, 708)], "ab cd"), PLACED_1_5);
		expect(line0(frame).inked).toEqual([true, true, false, true, true]);
	});

	it("projects an empty line to a slot with no characters", () => {
		// A drawn-but-unrecovered line keeps its reading-order slot, because
		// `selis extract` still emits its newline. Dropping it would make a copy
		// stop matching the CLI.
		const frame = buildTextLayer(
			layerOf(
				[ltrLine("ab", 72, 708), { text: "", rect: rect(0, 0, 0, 0), direction: "ltr", chars: [] }],
				"ab\n",
			),
			PLACED_1_5,
		);
		expect(frame.lines).toHaveLength(2);
		expect(lineLength(frame, 1)).toBe(0);
	});
});

describe("caretX", () => {
	it("puts the caret at a character's leading edge on a left-to-right line", () => {
		const frame = buildTextLayer(layerOf([ltrLine("abc", 100, 700)], "abc"), PLACED_1_5);
		expect(caretX(frame, 0, 0)).toBe(100 * 1.5);
		expect(caretX(frame, 0, 1)).toBe(110 * 1.5);
	});

	it("puts the caret at the trailing edge past the last character", () => {
		const frame = buildTextLayer(layerOf([ltrLine("abc", 100, 700)], "abc"), PLACED_1_5);
		expect(caretX(frame, 0, 3)).toBe(130 * 1.5);
	});

	it("mirrors both ends on a right-to-left line", () => {
		// Logical offset 0 is the line's *start*, which on an RTL line is the
		// right edge of character 0, because the pen began at the right. Using
		// the left-to-right rule here would put every caret half a line from
		// where the reader clicked.
		const frame = buildTextLayer(layerOf([rtlLine("abc", 200, 700)], "abc"), PLACED_1_5);
		expect(caretX(frame, 0, 0)).toBe(200 * 1.5);
		expect(caretX(frame, 0, 1)).toBe(190 * 1.5);
		expect(caretX(frame, 0, 3)).toBe(170 * 1.5);
	});

	it("resolves the same offset to the same character in both directions", () => {
		// The two lines are the same run drawn in opposite directions about
		// different anchors, so offset 1 must be the same *character* in both.
		// That is the actual RTL-correctness claim, as opposed to the two carets
		// happening to share an x: the forward run starts at 100pt and the
		// mirrored one ends at 200pt, so the same offset is 90pt further right.
		const forward = buildTextLayer(layerOf([ltrLine("abc", 100, 700)], "abc"), PLACED_1_5);
		const mirrored = buildTextLayer(layerOf([rtlLine("abc", 200, 700)], "abc"), PLACED_1_5);
		const ltrChar = line0(forward).chars[1];
		const rtlChar = line0(mirrored).chars[1];
		expect(ltrChar?.width).toBe(rtlChar?.width);
		// Forward: offset 1 is the *left* edge of character 1. Mirrored: the
		// *right* edge. Same character, opposite edge, which is the whole
		// difference a mirrored caret has to make.
		expect(caretX(forward, 0, 1)).toBe(ltrChar?.x);
		expect(caretX(mirrored, 0, 1)).toBe((rtlChar?.x ?? 0) + (rtlChar?.width ?? 0));
	});

	it("returns 0 for a line with no characters and for a missing line", () => {
		const frame = buildTextLayer(
			layerOf([{ text: "", rect: rect(0, 0, 0, 0), direction: "ltr", chars: [] }], ""),
			PLACED_1_5,
		);
		expect(caretX(frame, 0, 0)).toBe(0);
		expect(caretX(frame, 7, 0)).toBe(0);
	});
});
describe("zoom survival", () => {
	it("is exact at every scale, because nothing here is a pixel", () => {
		// A caret is a (line, offset) pair, so a zoom cannot invalidate it: the
		// frame is rebuilt from the same scale-free user-space layer. What is
		// asserted is that the character under a given offset is unchanged and
		// its x scales exactly linearly.
		const layer = layerOf([ltrLine("abcdef", 100, 700)], "abcdef");
		const scales = [0.5, 1, 1.5, 2, 4.25, 8.3];
		const xs = scales.map((scale) => caretX(buildTextLayer(layer, { ...PLACED_1_5, scale }), 0, 3));
		for (const [index, scale] of scales.entries()) {
			// 100pt margin + 3 characters of 10pt = 130pt to the third boundary.
			expect(xs[index]).toBeCloseTo(130 * scale, 9);
		}
	});

	it("re-projects a selection made at 1x onto the same characters at 8.3x", () => {
		const layer = layerOf([ltrLine("hello world", 100, 700)], "hello world");
		const boxAt = (scale: number): { x: number; width: number } => {
			const frame = buildTextLayer(layer, { ...PLACED_1_5, scale });
			const from = caretX(frame, 0, 0);
			const to = caretX(frame, 0, 5);
			return { x: Math.min(from, to), width: Math.abs(to - from) };
		};
		const small = boxAt(1);
		const large = boxAt(8.3);
		// "hello" is 5 characters of 10pt, so the box is 50pt wide at any scale.
		expect(small.width).toBeCloseTo(50, 9);
		expect(large.width).toBeCloseTo(50 * 8.3, 9);
		expect(large.x / small.x).toBeCloseTo(8.3, 9);
	});
});
