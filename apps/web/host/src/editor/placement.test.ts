/**
 * Where a placement lands (pdf-plan/40).
 *
 * The expectations are the engine's own arithmetic, worked out by hand:
 *
 * - an upright Letter page (612 × 792 points) at 96 dpi renders 816 × 1056
 *   device pixels, and the transform is `[4/3 0 0 -4/3 0 1056]` — scale by
 *   the DPI, flip y, origin at the media box's lower-left;
 * - the same page with `/Rotate 90` renders 1056 × 816 and the transform is
 *   `[0 4/3 4/3 0 0 0]`: the page is turned, so the user-space y axis is the
 *   canvas's x axis;
 * - a page whose media box starts at (10, 20) has the same transform with
 *   the origin translated away.
 *
 * A placement that lands a few points off is a placement the reader cannot
 * see is wrong, so these numbers are pinned.
 */

import { describe, expect, it } from "vitest";
import type { Matrix } from "./geometry.js";
import {
	type PageGeometry,
	displayRect,
	displayToUser,
	fitImage,
	invert,
	pageGeometry,
	userRect,
	userToDisplay,
} from "./placement.js";

/** The transform for an upright Letter page at 96 dpi. */
const UPRIGHT: Matrix = { a: 4 / 3, b: 0, c: 0, d: -4 / 3, e: 0, f: 1056 };

/** The same page turned a quarter turn clockwise. */
const TURNED: Matrix = { a: 0, b: 4 / 3, c: 4 / 3, d: 0, e: 0, f: 0 };

/** The same page with a media box that starts at (10, 20). */
const OFFSET: Matrix = { a: 4 / 3, b: 0, c: 0, d: -4 / 3, e: -40 / 3, f: 1056 + 80 / 3 };

function upright(): PageGeometry {
	return pageGeometry(UPRIGHT, 816, 1056, 816, 1056, 612, 792);
}

function turned(): PageGeometry {
	return pageGeometry(TURNED, 1056, 816, 1056, 816, 612, 792);
}

describe("the matrix inverse", () => {
	it("inverts a transform", () => {
		const inverse = invert(UPRIGHT);
		expect(inverse).not.toBeNull();
		const m = inverse as Matrix;
		expect(m.a).toBeCloseTo(0.75, 9);
		expect(m.d).toBeCloseTo(-0.75, 9);
		expect(m.e).toBeCloseTo(0, 9);
		expect(m.f).toBeCloseTo(792, 9);
	});

	it("refuses a singular matrix rather than returning nonsense", () => {
		expect(invert({ a: 0, b: 0, c: 0, d: 0, e: 0, f: 0 })).toBeNull();
	});
});

describe("the page's geometry", () => {
	it("reads the rotation off the transform", () => {
		expect(upright().rotation).toBe(0);
		expect(turned().rotation).toBe(90);
	});

	it("reads the device pixels per point off the transform", () => {
		expect(upright().scale).toBeCloseTo(4 / 3, 9);
		expect(turned().scale).toBeCloseTo(4 / 3, 9);
	});
});

describe("a rectangle on the canvas", () => {
	it("is a rectangle in the page's own points on an upright page", () => {
		const rect = userRect(upright(), { x: 0, y: 0, width: 100, height: 50 });
		// The canvas's top-left is the page's top-left: the user space is
		// y-up, so the rectangle sits at the TOP of the page.
		expect(rect.x).toBeCloseTo(0, 9);
		expect(rect.y).toBeCloseTo(792 - 37.5, 9);
		expect(rect.width).toBeCloseTo(75, 9);
		expect(rect.height).toBeCloseTo(37.5, 9);
	});

	it("follows the page's own origin when the media box does not start at zero", () => {
		const geometry = pageGeometry(OFFSET, 816, 1056, 816, 1056, 612, 792);
		const rect = userRect(geometry, { x: 0, y: 0, width: 100, height: 50 });
		expect(rect.x).toBeCloseTo(10, 9);
		expect(rect.y).toBeCloseTo(812 - 37.5, 9);
	});

	it("is turned with the page", () => {
		// On a page turned a quarter turn, the canvas's top-left is the media
		// box's bottom-left corner, and the canvas's x axis is the media's y.
		const rect = userRect(turned(), { x: 0, y: 0, width: 100, height: 50 });
		expect(rect.x).toBeCloseTo(0, 9);
		expect(rect.y).toBeCloseTo(0, 9);
		expect(rect.width).toBeCloseTo(37.5, 9);
		expect(rect.height).toBeCloseTo(75, 9);
	});
});

describe("the display rectangle the overlay writers want", () => {
	it("is the canvas rectangle in points, measured from the page's bottom", () => {
		const rect = displayRect(upright(), { x: 0, y: 0, width: 100, height: 50 });
		expect(rect.x0).toBeCloseTo(0, 9);
		expect(rect.x1).toBeCloseTo(75, 9);
		expect(rect.y0).toBeCloseTo(792 - 37.5, 9);
		expect(rect.y1).toBeCloseTo(792, 9);
	});

	it("follows the page's turn", () => {
		// The canvas's top-left corner is the display's top-left, which on a
		// turned page is the media box's bottom-left.
		const rect = displayRect(turned(), { x: 0, y: 0, width: 100, height: 50 });
		expect(rect.x0).toBeCloseTo(0, 9);
		expect(rect.x1).toBeCloseTo(75, 9);
		expect(rect.y0).toBeCloseTo(612 - 37.5, 9);
		expect(rect.y1).toBeCloseTo(612, 9);
	});
});

describe("the engine's own quarter-turn table", () => {
	it("round-trips through its inverse", () => {
		const points: readonly [number, number][] = [
			[0, 0],
			[612, 792],
			[100, 200],
		];
		for (const rotation of [0, 90, 180, 270] as const) {
			for (const [x, y] of points) {
				const user = displayToUser(rotation, 612, 792, x, y);
				const back = userToDisplay(rotation, 612, 792, user.x, user.y);
				expect(back.x).toBeCloseTo(x, 9);
				expect(back.y).toBeCloseTo(y, 9);
			}
		}
	});

	it("turns a quarter turn clockwise, the way /Rotate 90 does", () => {
		// The page's top-left corner displays at the canvas's top-right.
		expect(displayToUser(90, 612, 792, 612, 0)).toEqual({ x: 612, y: 612 });
	});
});

describe("fitting an image into a rectangle", () => {
	it("keeps the image's own aspect ratio and centres it", () => {
		const fitted = fitImage({ x0: 0, y0: 0, x1: 100, y1: 100 }, 200, 100);
		// The image is twice as wide as it is tall, so it fills the width and
		// leaves the height half-used.
		expect(fitted.width).toBeCloseTo(100, 9);
		expect(fitted.height).toBeCloseTo(50, 9);
		expect(fitted.x).toBeCloseTo(0, 9);
		expect(fitted.y).toBeCloseTo(25, 9);
		expect(fitted.scale).toBeCloseTo(0.5, 9);
	});

	it("refuses an empty image or an empty rectangle", () => {
		const fitted = fitImage({ x0: 0, y0: 0, x1: 10, y1: 10 }, 0, 10);
		expect(fitted.width).toBe(0);
		expect(fitted.height).toBe(0);
	});
});
