/**
 * Click-to-select for existing image and vector objects (TEXTEDIT.05/06).
 *
 * The engine reports each object's bounding box in the page's own user space.
 * The editor sees CSS pixels on a canvas. [`canvasRect`] already bridges those
 * two spaces exactly — it applies the same transform the render used — so this
 * module does no geometry of its own: it converts every reported box and asks
 * the plain question "is the click inside this rectangle?".
 *
 * A click may land inside more than one box (a small image on a large filled
 * path). The smallest box wins: it is the most specific object the reader
 * could have meant, and the same rule a painter's stack implies. Ties keep
 * content order, so the choice is deterministic.
 *
 * This is a pure module. It creates no DOM and touches no engine, which makes
 * the hit test testable without jsdom or a wasm binary.
 */

import type { AddObjectId } from "./operations.js";
import { canvasRect, type PageGeometry, type Rect } from "./placement.js";

/** One object the engine reported, with its box in PDF user space. */
export interface PlacedObject {
	readonly id: AddObjectId;
	/** Bounding box in PDF user space (points, y-up). */
	readonly rect: Rect;
	/** Whether this is an image (images win a tie against a vector path). */
	readonly image: boolean;
}

/** The result of a hit test. */
export interface Hit<T extends PlacedObject = PlacedObject> {
	readonly index: number;
	readonly object: T;
	/** The object's box in canvas CSS pixels, for drawing the overlay. */
	readonly box: Rect;
}

function contains(box: Rect, x: number, y: number): boolean {
	return x >= box.x && x <= box.x + box.width && y >= box.y && y <= box.y + box.height;
}

function area(box: Rect): number {
	return Math.abs(box.width * box.height);
}

/**
 * Find the object under a canvas point.
 *
 * `point` is in the canvas's CSS pixels, relative to the canvas's top-left
 * corner — the same space the drag-to-place tool reports. Returns `null` when
 * no reported box contains the point, so "nothing was hit" is an honest answer
 * rather than an arbitrary first object.
 */
export function objectAt<T extends PlacedObject>(
	geometry: PageGeometry,
	objects: readonly T[],
	point: { readonly x: number; readonly y: number },
): Hit<T> | null {
	let best: Hit<T> | null = null;
	let bestArea = Number.POSITIVE_INFINITY;
	for (let index = 0; index < objects.length; index += 1) {
		const object = objects[index];
		if (object === undefined) {
			continue;
		}
		const box = canvasRect(geometry, object.rect);
		if (!contains(box, point.x, point.y)) {
			continue;
		}
		const size = area(box);
		// Strictly smaller wins; an equal-size later object does not displace an
		// earlier one, and an image wins an exact tie with a vector path.
		const better =
			best === null ||
			size < bestArea ||
			(size === bestArea && object.image && !best.object.image);
		if (better) {
			best = { index, object, box };
			bestArea = size;
		}
	}
	return best;
}

/**
 * The overlay rectangles to paint for a set of reported objects.
 *
 * Returned in content order with their canvas boxes, so a caller can draw a
 * hairline around every pickable object before the reader clicks one.
 */
export function overlayRects<T extends PlacedObject>(
	geometry: PageGeometry,
	objects: readonly T[],
): { readonly object: T; readonly box: Rect }[] {
	return objects.map((object) => ({ object, box: canvasRect(geometry, object.rect) }));
}
