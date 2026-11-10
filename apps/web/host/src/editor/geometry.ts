/**
 * Page geometry for the editor workspace.
 *
 * ## Why this module exists
 *
 * The editor puts a clickable text layer over the page the engine rendered.
 * Two coordinate systems are involved and they are not the same:
 *
 *  - the **text layer** is in PDF user space — points, y-up, origin at the
 *    `/MediaBox`'s lower-left corner;
 *  - the **canvas** is in device pixels — y-down, origin at the top-left.
 *
 * The mapping between them is a matrix, and the matrix is the ENGINE's: the
 * render op reports the six elements it used. That matters, because the
 * mapping is not a scale-and-flip:
 *
 *  - a page with `/Rotate 90` has its text running *down* the canvas and its
 *    axes swapped;
 *  - a page whose `/MediaBox` starts at (10, 20) rather than (0, 0) is
 *    translated by that origin.
 *
 * A shell that assumed "scale by the DPI, flip y" — which is what the reader
 * does — is wrong on exactly the pages where a misplaced click edits the wrong
 * line. So the editor uses the engine's matrix rather than a second
 * implementation of it.
 *
 * Everything here is pure: the numbers go in, the numbers come out, and the
 * tests can assert the geometry without a browser.
 */

/** PDF's six-element transformation matrix, `[a b c d e f]`. */
export interface Matrix {
	readonly a: number;
	readonly b: number;
	readonly c: number;
	readonly d: number;
	readonly e: number;
	readonly f: number;
}

/** An axis-aligned rectangle. */
export interface Rect {
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
}

/** A point. */
export interface Point {
	readonly x: number;
	readonly y: number;
}

/**
 * Transform a point, in PDF's own convention: `x' = a·x + c·y + e`,
 * `y' = b·x + d·y + f`.
 */
export function applyMatrix(m: Matrix, x: number, y: number): Point {
	return { x: m.a * x + m.c * y + m.e, y: m.b * x + m.d * y + m.f };
}

/**
 * The axis-aligned bounding box of a rectangle after the matrix is applied.
 *
 * The four corners are mapped rather than the rectangle's own edges, because a
 * quarter-turn swaps the axes: mapping the edges would report a box that is
 * the pre-rotation size.
 */
export function mapRect(m: Matrix, rect: Rect): Rect {
	const corners: readonly Point[] = [
		applyMatrix(m, rect.x, rect.y),
		applyMatrix(m, rect.x + rect.width, rect.y),
		applyMatrix(m, rect.x + rect.width, rect.y + rect.height),
		applyMatrix(m, rect.x, rect.y + rect.height),
	];
	let minX = Number.POSITIVE_INFINITY;
	let minY = Number.POSITIVE_INFINITY;
	let maxX = Number.NEGATIVE_INFINITY;
	let maxY = Number.NEGATIVE_INFINITY;
	for (const point of corners) {
		minX = Math.min(minX, point.x);
		minY = Math.min(minY, point.y);
		maxX = Math.max(maxX, point.x);
		maxY = Math.max(maxY, point.y);
	}
	return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}

/**
 * Where one line of the text layer goes on the canvas.
 *
 * `box` is the glyph box's bounding box in CSS pixels; `em` is the font size
 * the overlay text must use; `angle` is the CSS rotation the overlay text
 * needs, in degrees; `origin` is the text's starting point on the baseline,
 * relative to the box's top-left corner.
 */
export interface LinePlacement {
	/** The glyph box's bounding box, in CSS pixels. */
	readonly box: Rect;
	/** The overlay's font size, in CSS pixels. */
	readonly em: number;
	/** The CSS rotation, in degrees: 0, 90, 180 or 270. */
	readonly angle: number;
	/** The baseline start, relative to the box's top-left corner. */
	readonly origin: Point;
	/**
	 * The width the overlay text must span, in CSS pixels — the box's extent
	 * ALONG the text's direction, which is the box's height for a page that is
	 * turned on its side.
	 */
	readonly runWidth: number;
}

/**
 * Place one text-layer line over a rendered page.
 *
 * @param ctm the transform the render used, in device pixels
 * @param rect the line's glyph box, in PDF user space
 * @param cssScale CSS pixels per device pixel
 */
export function placeLine(ctm: Matrix, rect: Rect, cssScale: number): LinePlacement {
	// The glyph box is twice the font size tall (SL-3.TEXT's `glyph_rect`:
	// ±font-size around the baseline), and the baseline runs through its
	// centre. Both facts are the engine's, and the reader relies on them too.
	const emUser = rect.height / 2;
	const scale = Math.hypot(ctm.a, ctm.b);
	const boxDevice = mapRect(ctm, rect);
	const box: Rect = {
		x: boxDevice.x * cssScale,
		y: boxDevice.y * cssScale,
		width: boxDevice.width * cssScale,
		height: boxDevice.height * cssScale,
	};

	// The text's start on the baseline: the box's left edge, at the baseline's
	// height (the box's centre).
	const baseline = applyMatrix(ctm, rect.x, rect.y + rect.height / 2);
	const origin: Point = {
		x: (baseline.x - boxDevice.x) * cssScale,
		y: (baseline.y - boxDevice.y) * cssScale,
	};

	// The direction the text runs, as the matrix maps the user-space +x axis.
	// A page turned on its side runs its text down the canvas, and the
	// overlay has to be turned with it or the two do not line up.
	const angle = normaliseAngle((Math.atan2(ctm.b, ctm.a) * 180) / Math.PI);

	return {
		box,
		em: emUser * scale * cssScale,
		angle,
		origin,
		runWidth: angle === 90 || angle === 270 ? box.height : box.width,
	};
}

/**
 * Snap an angle to the quadrant it belongs to.
 *
 * The render's transform is always a quarter-turn composition, so the angle
 * is 0, 90, 180 or 270; anything else is a numerical accident and is rounded
 * to the nearest quadrant rather than trusted.
 */
export function normaliseAngle(degrees: number): 0 | 90 | 180 | 270 {
	const wrapped = ((degrees % 360) + 360) % 360;
	const quadrant = Math.round(wrapped / 90) % 4;
	switch (quadrant) {
		case 1:
			return 90;
		case 2:
			return 180;
		case 3:
			return 270;
		default:
			return 0;
	}
}
