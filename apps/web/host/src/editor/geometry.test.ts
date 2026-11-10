/**
 * The editor's page geometry (pdf-plan/39).
 *
 * These tests are the evidence that the editor puts its clickable text where
 * the engine drew the page. The engine's own matrix arithmetic is the
 * reference: the expected numbers below are the engine's, computed by hand
 * from `crates/selis-pdf-engine/src/page.rs` — the base transform (scale and
 * y-flip, with the `/MediaBox` origin translated away) composed with the
 * quadrant rotation.
 */

import { describe, expect, it } from "vitest";
import { type Matrix, applyMatrix, mapRect, normaliseAngle, placeLine } from "./geometry.js";

/**
 * The transform for a Letter page (612 × 792 points, origin at (0, 0)) at
 * 72 dpi, with the page's `/Rotate` applied.
 *
 * The base transform is `[1 0 0 -1 0 792]` — scale 1, y flipped, the origin
 * translated away — and the rotations are the engine's own matrices.
 */
function letterCtm(rotation: 0 | 90 | 180 | 270): Matrix {
	switch (rotation) {
		case 90:
			return { a: 0, b: 1, c: 1, d: 0, e: 0, f: 0 };
		case 180:
			return { a: -1, b: 0, c: 0, d: 1, e: 612, f: 0 };
		case 270:
			return { a: 0, b: -1, c: -1, d: 0, e: 792, f: 612 };
		default:
			return { a: 1, b: 0, c: 0, d: -1, e: 0, f: 792 };
	}
}

/** A line of text near the top of a Letter page: 200 points wide, 24 tall. */
const LINE = { x: 72, y: 700, width: 200, height: 24 };

describe("the transform a point goes through", () => {
	it("maps a point in PDF's own convention", () => {
		const m: Matrix = { a: 2, b: 0, c: 0, d: -2, e: 10, f: 20 };
		expect(applyMatrix(m, 3, 4)).toEqual({ x: 16, y: 12 });
	});

	it("takes the glyph box's corners, not its edges, for a quarter turn", () => {
		// A quarter turn swaps the axes: the box is 200 × 24 in user space and
		// 24 × 200 on the canvas.
		const box = mapRect(letterCtm(90), LINE);
		expect(box.width).toBeCloseTo(24, 9);
		expect(box.height).toBeCloseTo(200, 9);
	});
});

describe("where a line of text lands on the page", () => {
	it("puts an upright line where the engine drew it", () => {
		const placement = placeLine(letterCtm(0), LINE, 1);
		expect(placement.box.x).toBeCloseTo(72, 9);
		expect(placement.box.y).toBeCloseTo(68, 9);
		expect(placement.box.width).toBeCloseTo(200, 9);
		expect(placement.box.height).toBeCloseTo(24, 9);
		// The glyph box is two em tall, so the font size is half of it…
		expect(placement.em).toBeCloseTo(12, 9);
		// …and the baseline runs through its centre, twelve points below the
		// top of the box.
		expect(placement.origin.x).toBeCloseTo(0, 9);
		expect(placement.origin.y).toBeCloseTo(12, 9);
		expect(placement.angle).toBe(0);
		expect(placement.runWidth).toBeCloseTo(200, 9);
	});

	it("turns the overlay with a page that is on its side", () => {
		const placement = placeLine(letterCtm(90), LINE, 1);
		expect(placement.angle).toBe(90);
		// The text runs DOWN the canvas, so the box's axes are swapped and the
		// run is measured along the box's height.
		expect(placement.box.width).toBeCloseTo(24, 9);
		expect(placement.box.height).toBeCloseTo(200, 9);
		expect(placement.runWidth).toBeCloseTo(200, 9);
		// The baseline starts at the top edge of the box, in its middle: the
		// ascent points right on a page turned a quarter turn.
		expect(placement.origin.x).toBeCloseTo(12, 9);
		expect(placement.origin.y).toBeCloseTo(0, 9);
		expect(placement.em).toBeCloseTo(12, 9);
	});

	it("turns the overlay with a page that is upside down", () => {
		const placement = placeLine(letterCtm(180), LINE, 1);
		expect(placement.angle).toBe(180);
		expect(placement.box.width).toBeCloseTo(200, 9);
		expect(placement.box.height).toBeCloseTo(24, 9);
		// The text runs left, so it starts at the box's RIGHT edge.
		expect(placement.origin.x).toBeCloseTo(200, 9);
		expect(placement.origin.y).toBeCloseTo(12, 9);
	});

	it("turns the overlay with a page that is turned the other way", () => {
		const placement = placeLine(letterCtm(270), LINE, 1);
		expect(placement.angle).toBe(270);
		expect(placement.box.width).toBeCloseTo(24, 9);
		expect(placement.box.height).toBeCloseTo(200, 9);
		// The text runs UP, so it starts at the bottom edge of the box.
		expect(placement.origin.x).toBeCloseTo(12, 9);
		expect(placement.origin.y).toBeCloseTo(200, 9);
	});

	it("translates a page whose MediaBox does not start at the origin", () => {
		// The engine translates the box's origin away, so the same line lands
		// in the same place on the canvas whichever origin the page declares.
		const ctm: Matrix = { a: 1, b: 0, c: 0, d: -1, e: -10, f: 812 };
		const placement = placeLine(ctm, { x: 82, y: 720, width: 200, height: 24 }, 1);
		expect(placement.box.x).toBeCloseTo(72, 9);
		expect(placement.box.y).toBeCloseTo(68, 9);
		expect(placement.origin.y).toBeCloseTo(12, 9);
	});

	it("scales with the canvas", () => {
		// The canvas is displayed smaller than it is rendered, so every number
		// in CSS pixels is the device number times the scale.
		const placement = placeLine(letterCtm(0), LINE, 0.5);
		expect(placement.box.x).toBeCloseTo(36, 9);
		expect(placement.box.width).toBeCloseTo(100, 9);
		expect(placement.em).toBeCloseTo(6, 9);
		expect(placement.origin.y).toBeCloseTo(6, 9);
	});
});

describe("the angle of a turned page", () => {
	it("snaps to the quadrant the page was turned by", () => {
		expect(normaliseAngle(0)).toBe(0);
		expect(normaliseAngle(90)).toBe(90);
		expect(normaliseAngle(180)).toBe(180);
		expect(normaliseAngle(-90)).toBe(270);
		expect(normaliseAngle(270)).toBe(270);
		expect(normaliseAngle(360)).toBe(0);
		expect(normaliseAngle(-180)).toBe(180);
	});

	it("rounds a numerical accident rather than trusting it", () => {
		// The render's transform is always a quarter turn, so a number that is
		// almost one is one.
		expect(normaliseAngle(89.999)).toBe(90);
		expect(normaliseAngle(0.4)).toBe(0);
	});
});
