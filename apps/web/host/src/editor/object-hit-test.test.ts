/**
 * Click-to-select over the engine's reported object boxes (TEXTEDIT.05/06).
 *
 * The tests derive the click point from the same `canvasRect` the module uses,
 * so a mistake in the geometry cannot hide behind a hand-picked number: a click
 * at the centre of a reported box must select that box, in every rotation.
 */

import { describe, expect, it } from "vitest";
import type { Matrix } from "./geometry.js";
import { objectAt, overlayRects, type PlacedObject } from "./object-hit-test.js";
import { canvasRect, pageGeometry, type PageGeometry } from "./placement.js";

const UPRIGHT: Matrix = { a: 4 / 3, b: 0, c: 0, d: -4 / 3, e: 0, f: 1056 };
const TURNED: Matrix = { a: 0, b: 4 / 3, c: 4 / 3, d: 0, e: 0, f: 0 };

function upright(): PageGeometry {
	return pageGeometry(UPRIGHT, 816, 1056, 816, 1056, 612, 792);
}

function turned(): PageGeometry {
	return pageGeometry(TURNED, 1056, 816, 1056, 816, 612, 792);
}

function image(name: string, x: number, y: number, w: number, h: number): PlacedObject {
	return { id: { name, op_index: 0 }, image: true, rect: { x, y, width: w, height: h } };
}

function path(opIndex: number, x: number, y: number, w: number, h: number): PlacedObject {
	return { id: { name: "", op_index: opIndex }, image: false, rect: { x, y, width: w, height: h } };
}

function centreOf(geometry: PageGeometry, object: PlacedObject): { x: number; y: number } {
	const box = canvasRect(geometry, object.rect);
	return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

describe("object hit testing", () => {
	it("selects the image whose reported box covers the click", () => {
		const geometry = upright();
		const target = image("Im1", 20, 30, 100, 80);
		const other = image("Im2", 300, 400, 40, 40);
		const hit = objectAt(geometry, [other, target], centreOf(geometry, target));
		expect(hit).not.toBeNull();
		expect(hit?.object.id.name).toBe("Im1");
		expect(hit?.index).toBe(1);
	});

	it("returns null when the click is outside every reported box", () => {
		const geometry = upright();
		const objects = [image("Im1", 20, 30, 100, 80)];
		expect(objectAt(geometry, objects, { x: 700, y: 700 })).toBeNull();
	});

	it("picks the smallest box when two objects overlap", () => {
		const geometry = upright();
		const large = path(2, 0, 0, 300, 300);
		const small = image("Im1", 100, 100, 30, 30);
		// Both boxes contain the small image's centre; the image is smaller.
		const hit = objectAt(geometry, [large, small], centreOf(geometry, small));
		expect(hit?.object.image).toBe(true);
		expect(hit?.object.id.name).toBe("Im1");
	});

	it("an image wins an exact-area tie with a vector path", () => {
		const geometry = upright();
		const vector = path(1, 10, 10, 50, 50);
		const picture = image("Im1", 10, 10, 50, 50);
		const hit = objectAt(geometry, [vector, picture], centreOf(geometry, picture));
		expect(hit?.object.image).toBe(true);
	});

	it("hit-tests a rotated page with the same reported boxes", () => {
		const geometry = turned();
		const target = image("Im1", 40, 60, 90, 50);
		const hit = objectAt(geometry, [target], centreOf(geometry, target));
		expect(hit?.object.id.name).toBe("Im1");
	});

	it("draws one overlay rectangle per reported object, in content order", () => {
		const geometry = upright();
		const objects = [image("Im1", 0, 0, 10, 10), path(4, 5, 5, 10, 10)];
		const rects = overlayRects(geometry, objects);
		expect(rects.map((row) => row.object.id.name)).toEqual(["Im1", ""]);
		expect(rects).toHaveLength(2);
		// The boxes are in canvas CSS pixels; a unit-square at the user-space
		// origin occupies the bottom-left of the rendered page.
		expect(rects[0]?.box.x).toBeCloseTo(0, 6);
		expect(rects[0]?.box.width).toBeCloseTo(10 * (4 / 3), 6);
	});
});
